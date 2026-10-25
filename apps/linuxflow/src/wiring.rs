//! Assembly of the real components (outside GTK): stores, engine, pipeline, dictation, shortcuts.

use crate::adapters::{AppFormatter, StoreHints, StoreHistory, UnavailablePaster};
use crate::clipboard_gdk::GdkClipboard;
use crate::engine::{EngineHost, WorkerLauncher};
use crate::engine_ctl::EngineController;
use crate::hotkeys::{CapsCell, HotkeyController};
use crate::library::{DictionaryStore, SnippetStore};
use crate::logging::redact;
use crate::models::ModelsService;
use crate::runtime::{Dictator, DictatorDeps, EventSink, SystemClock, ThreadExecutor, TokioTimers};
use crate::service::{AppService, ServiceDeps};
use crate::system::{RealSystem, UiCommand};
use linuxflow_audio::CpalRecorder;
use linuxflow_core::domain::Settings;
use linuxflow_core::events::Event;
use linuxflow_core::history::HistoryStore;
use linuxflow_core::paths::{AppPaths, SystemEnv};
use linuxflow_core::pipeline::{Pipeline, PipelineDeps};
use linuxflow_core::settings::SettingsStore;
use linuxflow_core::store::WarnFn;
use linuxflow_core::traits::{Clipboard, Paster};
use linuxflow_models::{Catalog, Config};
use linuxflow_platform::clipboard::{build_clipboard, ClipboardBackend, ClipboardConfig};
use linuxflow_platform::focus::SystemActiveWindow;
use linuxflow_platform::inject::portal::{PortalConfig, PortalSession};
use linuxflow_platform::inject::{
    build_paster, inject_chain, InjectBackendKind, InjectConfig, SessionEnv,
};
use linuxflow_platform::per_app::PerAppTable;
use linuxflow_stt::Hardware;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

/// (Optional) TOML file that overrides the per-application paste table.
pub const PER_APP_FILE: &str = "per-app.toml";

/// Sending the events to the GTK thread.
pub struct ChannelSink(pub async_channel::Sender<Event>);

impl EventSink for ChannelSink {
    fn send(&self, event: Event) {
        let _ = self.0.try_send(event);
    }
}

pub struct Options {
    pub display_name: Option<String>,
    pub no_engine: bool,
}

/// Result of the assembly.
pub struct Core {
    pub service: Arc<AppService>,
    pub dictator: Dictator,
    pub engine: Arc<EngineController>,
    pub hotkeys: Arc<HotkeyController>,
    pub caps: Arc<CapsCell>,
    pub tray_available: Arc<AtomicBool>,
    pub settings: Arc<SettingsStore>,
    pub paths: AppPaths,
    /// Tokio runtime (timers, downloads), in its own threads.
    pub runtime: tokio::runtime::Runtime,
}

pub fn warn_logger() -> WarnFn {
    Arc::new(|m| tracing::warn!("{}", redact(m)))
}

/// Models folder for a given setting (environment overrides included).
pub fn models_dir_for(custom: Option<&std::path::Path>) -> PathBuf {
    AppPaths::resolve_with(&SystemEnv, custom)
        .map(|p| p.models_dir)
        .unwrap_or_else(|_| PathBuf::from("models"))
}

pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
}

/// Real paster and clipboard, sharing the portal session when needed.
fn build_output(paths: &AppPaths, settings: &Settings) -> (Arc<dyn Clipboard>, Arc<dyn Paster>) {
    let env = SessionEnv::from_env();
    let portal = if inject_chain(&env, false).contains(&InjectBackendKind::Portal) {
        PortalSession::new(PortalConfig {
            data_dir: Some(paths.data_dir.clone()),
            clipboard: true,
            ..PortalConfig::default()
        })
        .ok()
    } else {
        None
    };
    let flag = |k: &str| {
        settings
            .extra
            .get(k)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let app_backend: Arc<dyn ClipboardBackend> = Arc::new(GdkClipboard);
    let clipboard = build_clipboard(
        &env,
        ClipboardConfig {
            also_primary: false,
            portal: portal.clone(),
            app_backend: Some(app_backend),
        },
    );
    let inject = InjectConfig {
        data_dir: Some(paths.data_dir.clone()),
        enable_uinput: flag("enableUinput"),
        ..InjectConfig::default()
    };
    let paster: Arc<dyn Paster> = match build_paster(&env, &inject, None, portal) {
        Ok(chain) => Arc::new(chain),
        Err(e) => {
            tracing::warn!("paste injector unavailable: {e}");
            Arc::new(UnavailablePaster(e.to_string()))
        }
    };
    (Arc::new(clipboard), paster)
}

/// Assembles everything. Starts nothing: see `AppService::startup`.
pub fn build_core(
    paths: AppPaths,
    opts: Options,
    events_tx: async_channel::Sender<Event>,
    ui_tx: async_channel::Sender<UiCommand>,
    ui_clipboard: Arc<dyn Clipboard>,
) -> Result<Core, String> {
    let warn = warn_logger();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("linuxflow-tokio")
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let settings = Arc::new(SettingsStore::open(paths.settings_file(), warn.clone()));
    let dictionary = Arc::new(DictionaryStore::open(paths.dictionary_file(), warn.clone()));
    let snippets = Arc::new(SnippetStore::open(paths.snippets_file(), warn.clone()));
    let history = Arc::new(HistoryStore::open(paths.history_file(), warn));
    let initial = settings.get();
    let events: Arc<dyn EventSink> = Arc::new(ChannelSink(events_tx));

    // Models and engine.
    let host = Arc::new(EngineHost::new());
    let models = ModelsService::new(
        runtime.handle().clone(),
        Catalog::load_from_env(),
        Config::from_env(),
        paths.models_dir.clone(),
        host.clone(),
    );
    let s_for_engine = settings.clone();
    let engine = Arc::new(EngineController::new(
        host.clone(),
        models.clone(),
        Arc::new(WorkerLauncher),
        Arc::new(move || s_for_engine.get()),
        Arc::new(|| Hardware::detect_isolated(Duration::from_secs(10))),
        opts.no_engine,
        exe_dir(),
    ));

    // Outputs (clipboard, paste), active window, per-application table.
    let (clipboard, paster) = build_output(&paths, &initial);
    let per_app = PerAppTable::load(Some(&paths.config_dir.join(PER_APP_FILE)));
    let clock = Arc::new(SystemClock);
    let pipeline = Pipeline::new(PipelineDeps {
        stt: host.clone(),
        clipboard: clipboard.clone(),
        paster,
        window: Arc::new(SystemActiveWindow::detect()),
        clock: clock.clone(),
        formatter: Arc::new(AppFormatter {
            dictionary: dictionary.clone(),
            snippets: snippets.clone(),
        }),
        history: Arc::new(StoreHistory {
            history: history.clone(),
            settings: settings.clone(),
        }),
        paste_modes: Arc::new(per_app),
        hints: Arc::new(StoreHints(dictionary.clone())),
    });
    let s_for_dictator = settings.clone();
    let dictator = Dictator::new(DictatorDeps {
        recorder: Arc::new(CpalRecorder::new()),
        pipeline: Arc::new(pipeline),
        clipboard,
        clock: clock.clone(),
        settings: Arc::new(move || s_for_dictator.get()),
        events: events.clone(),
        timers: Arc::new(TokioTimers::new(runtime.handle().clone())),
        executor: Arc::new(ThreadExecutor::new()),
    });

    // System (shortcuts, capabilities...).
    let caps = Arc::new(CapsCell::default());
    let tray_available = Arc::new(AtomicBool::new(false));
    let system = Arc::new(RealSystem {
        hotkeys: OnceLock::new(),
        caps: caps.clone(),
        tray_available: tray_available.clone(),
        paths: paths.clone(),
        models: models.clone(),
        ui_clipboard,
        ui_tx,
        versions: gtk_versions(),
    });
    let service = AppService::new(ServiceDeps {
        version: env!("CARGO_PKG_VERSION").to_string(),
        display_name: opts.display_name.or_else(default_display_name),
        settings: settings.clone(),
        dictionary,
        snippets,
        history,
        clock,
        dictator: dictator.clone(),
        engine: engine.clone(),
        models,
        system: system.clone(),
        events,
        models_dir_for: Arc::new(models_dir_for),
    });
    let weak: Weak<AppService> = Arc::downgrade(&service);
    let hotkeys = HotkeyController::new(
        caps.clone(),
        dictator.clone(),
        HotkeyController::system_factory(),
        Arc::new(move || {
            if let Some(s) = weak.upgrade() {
                s.push_state();
            }
        }),
    );
    let _ = system.hotkeys.set(hotkeys.clone());

    // Capability detection in the background.
    let caps_bg = caps.clone();
    let spawned = std::thread::Builder::new()
        .name("linuxflow-capabilities".into())
        .spawn(move || {
            caps_bg.set(linuxflow_platform::capabilities::detect(
                &linuxflow_platform::capabilities::SystemEnv,
            ));
        });
    if let Err(e) = spawned {
        tracing::warn!("capabilities detection could not start: {e}");
        caps.set(Default::default());
    }

    Ok(Core {
        service,
        dictator,
        engine,
        hotkeys,
        caps,
        tray_available,
        settings,
        paths,
        runtime,
    })
}

/// Versions of GTK and libadwaita loaded (display only).
pub fn gtk_versions() -> (String, String) {
    (
        format!(
            "{}.{}.{}",
            gtk4::major_version(),
            gtk4::minor_version(),
            gtk4::micro_version()
        ),
        format!(
            "{}.{}.{}",
            libadwaita::major_version(),
            libadwaita::minor_version(),
            libadwaita::micro_version()
        ),
    )
}

/// First name for the greeting ("Hello, Sam"): the first word of the account name
/// (separated by space, dot, hyphen or underscore). Read in place, never written or sent.
fn default_display_name() -> Option<String> {
    first_name_of(
        &std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .ok()?,
    )
}

fn first_name_of(login: &str) -> Option<String> {
    login
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-'))
        .find(|p| !p.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod display_name_tests {
    use super::first_name_of;

    #[test]
    fn the_first_word_of_the_account_name_is_used() {
        assert_eq!(first_name_of("sam").as_deref(), Some("sam"));
        assert_eq!(first_name_of("sam.dupont").as_deref(), Some("sam"));
        assert_eq!(first_name_of("jean-michel_x").as_deref(), Some("jean"));
        assert_eq!(first_name_of("_x").as_deref(), Some("x"));
        assert_eq!(first_name_of("").as_deref(), None);
        assert_eq!(first_name_of("...").as_deref(), None);
    }
}
