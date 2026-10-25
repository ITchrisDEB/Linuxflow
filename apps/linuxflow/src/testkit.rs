//! Shared test doubles (orchestration, service, application).

use crate::engine::{Engine, EngineHost, EngineLauncher, EnginePlan};
use crate::engine_ctl::EngineController;
use crate::library::{DictionaryStore, SnippetStore};
use crate::models::ModelsService;
use crate::runtime::{Dictator, DictatorDeps, EventSink, Executor, Lane, TimerHost};
use crate::service::{AppService, Folder, ProbeError, ServiceDeps, ShortcutInfo, System};
use linuxflow_core::accelerator::Accelerator;
use linuxflow_core::dictation::TimerId;
use linuxflow_core::domain::*;
use linuxflow_core::error::{Result, SttError};
use linuxflow_core::events::Event;
use linuxflow_core::history::HistoryStore;
use linuxflow_core::pipeline::{HistorySink, Pipeline, PipelineDeps};
use linuxflow_core::settings::SettingsStore;
use linuxflow_core::testing::*;
use linuxflow_core::traits::SpeechToText;
use linuxflow_models::{Catalog, Config};
use linuxflow_stt::Hardware;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct VecSink(pub Mutex<Vec<Event>>);
impl EventSink for VecSink {
    fn send(&self, e: Event) {
        self.0.lock().unwrap().push(e);
    }
}
impl VecSink {
    pub fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }
}

pub struct NoTimers;
impl TimerHost for NoTimers {
    fn schedule(&self, _: TimerId, _: u64, _: Box<dyn FnOnce() + Send>) {}
    fn cancel(&self, _: TimerId) {}
    fn after(&self, _: u64, _: Box<dyn FnOnce() + Send>) {}
}

pub struct InlineExec;
impl Executor for InlineExec {
    fn run(&self, _: Lane, job: Box<dyn FnOnce() + Send>) {
        job();
    }
}

/// Simulated system: records the calls.
#[derive(Default)]
pub struct FakeSystem {
    pub calls: Mutex<Vec<String>>,
    pub status: Mutex<ShortcutInfo>,
    pub probe: Mutex<Option<ProbeError>>,
    pub autostart_fails: Mutex<bool>,
    pub open_fails: Mutex<bool>,
}
impl FakeSystem {
    pub fn log(&self, s: impl Into<String>) {
        self.calls.lock().unwrap().push(s.into());
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}
impl System for FakeSystem {
    fn apply_shortcut(&self, s: &Settings) {
        self.log(format!("apply_shortcut {} {:?}", s.shortcut, s.mode));
    }
    fn suspend_shortcut(&self, b: bool) {
        self.log(format!("suspend {b}"));
    }
    fn shortcut_status(&self) -> ShortcutInfo {
        self.status.lock().unwrap().clone()
    }
    fn probe_shortcut(
        &self,
        _: &Accelerator,
        _: ShortcutMode,
    ) -> std::result::Result<(), ProbeError> {
        match self.probe.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn set_autostart(&self, enabled: bool) -> std::io::Result<()> {
        self.log(format!("autostart {enabled}"));
        if *self.autostart_fails.lock().unwrap() {
            Err(std::io::Error::other("no"))
        } else {
            Ok(())
        }
    }
    fn microphones(&self) -> Vec<MicrophoneInfo> {
        vec![MicrophoneInfo {
            id: "default".into(),
            label: "Default".into(),
        }]
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            session: "x11".into(),
            ..Capabilities::default()
        }
    }
    fn open_folder(&self, f: Folder) -> std::result::Result<(), String> {
        self.log(format!("open {f:?}"));
        if *self.open_fails.lock().unwrap() {
            Err("cannot open".into())
        } else {
            Ok(())
        }
    }
    fn copy_text(&self, text: &str) -> Result<()> {
        self.log(format!("copy {text}"));
        Ok(())
    }
    fn free_disk_bytes(&self, _: &Path) -> Option<u64> {
        Some(42)
    }
    fn ui_language_changed(&self) {
        self.log("ui_language_changed");
    }
    fn request_quit(&self) {
        self.log("quit");
    }
}

struct FakeEngine(EngineInfo);
impl SpeechToText for FakeEngine {
    fn info(&self) -> EngineInfo {
        self.0.clone()
    }
    fn transcribe(&self, _: &TranscribeRequest) -> std::result::Result<Transcription, SttError> {
        Err(SttError::NotReady)
    }
}
impl Engine for FakeEngine {
    fn shutdown(&self) {}
}

#[derive(Default)]
pub struct FakeLauncher(pub Mutex<Vec<EnginePlan>>);
impl EngineLauncher for FakeLauncher {
    fn launch(&self, plan: &EnginePlan) -> Arc<dyn Engine> {
        self.0.lock().unwrap().push(plan.clone());
        Arc::new(FakeEngine(EngineInfo {
            phase: EnginePhase::Ready,
            ready: true,
            model: Some(plan.model_id.clone()),
            ..EngineInfo::default()
        }))
    }
}

pub fn test_catalog() -> Catalog {
    Catalog::from_json(
        r#"{"endpoint":"http://127.0.0.1:9","models":[
        {"id":"tiny","name":"Tiny","kind":"stt","file":"tiny.bin","download_bytes":4,
         "urls":["http://127.0.0.1:9/t"],"usage":"light","rank":1},
        {"id":"silero-vad","name":"VAD","kind":"vad","file":"vad.bin","download_bytes":3,
         "urls":["http://127.0.0.1:9/v"],"required":true}]}"#,
    )
    .unwrap()
}

/// Complete service with all the components simulated.
/// The interface language is global to the process: the tests that change it, or that expect
/// English texts, hold this guard so that they never overlap. It also forces English, so that
/// the result does not depend on the system locale or on a catalogue installed on the machine.
pub fn language_guard() -> std::sync::MutexGuard<'static, ()> {
    static LANGUAGE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    linuxflow_i18n::set_language(Some("en"));
    guard
}

pub struct ServiceRig {
    pub _rt: tokio::runtime::Runtime,
    pub dir: tempfile::TempDir,
    pub service: Arc<AppService>,
    pub system: Arc<FakeSystem>,
    pub events: Arc<VecSink>,
    pub launcher: Arc<FakeLauncher>,
    pub recorder: FakeRecorder,
    pub stt: FakeStt,
    pub clock: FakeClock,
    pub history_sink: Arc<HistoryStore>,
    pub settings: Arc<SettingsStore>,
}

pub fn service_rig(display_name: Option<&str>, no_engine: bool) -> ServiceRig {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join("config");
    let models_dir = dir.path().join("models");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::create_dir_all(&models_dir).unwrap();
    let warn: linuxflow_core::store::WarnFn = Arc::new(|_| {});
    let settings = Arc::new(SettingsStore::open(
        cfg_dir.join("settings.json"),
        warn.clone(),
    ));
    let dictionary = Arc::new(DictionaryStore::open(
        cfg_dir.join("dictionary.json"),
        warn.clone(),
    ));
    let snippets = Arc::new(SnippetStore::open(
        cfg_dir.join("snippets.json"),
        warn.clone(),
    ));
    let history = Arc::new(HistoryStore::open(dir.path().join("history.json"), warn));
    let clock = FakeClock::new(1_700_000_000_000);
    let events = Arc::new(VecSink::default());
    let host = Arc::new(EngineHost::new());
    let models = ModelsService::new(
        rt.handle().clone(),
        test_catalog(),
        Config {
            offline: true,
            ..Config::default()
        },
        models_dir.clone(),
        host.clone(),
    );
    let launcher = Arc::new(FakeLauncher::default());
    let s2 = settings.clone();
    let engine = Arc::new(EngineController::new(
        host.clone(),
        models.clone(),
        launcher.clone(),
        Arc::new(move || s2.get()),
        Arc::new(|| Hardware {
            gpus: vec![],
            cpu_features: ["sse4.2", "avx", "avx2", "bmi2", "fma", "f16c"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ram_bytes: Some(8 << 30),
            vulkan_error: None,
        }),
        no_engine,
        None,
    ));
    let recorder = FakeRecorder::new();
    let stt = FakeStt::new();
    struct HistoryAdapter(Arc<HistoryStore>, Arc<SettingsStore>);
    impl HistorySink for HistoryAdapter {
        fn add(&self, e: HistoryEntry) {
            let _ = self.0.add(e, &self.1.get());
        }
    }
    let pipeline = Pipeline::new(PipelineDeps {
        stt: Arc::new(stt.clone()),
        clipboard: Arc::new(FakeClipboard::new()),
        paster: Arc::new(FakePaster::new()),
        window: Arc::new(FakeWindow::new()),
        clock: Arc::new(clock.clone()),
        formatter: Arc::new(FakeFormatter::new()),
        history: Arc::new(HistoryAdapter(history.clone(), settings.clone())),
        paste_modes: Arc::new(FakePasteModes::new()),
        hints: Arc::new(FakeHints::new()),
    });
    let s3 = settings.clone();
    let dictator = Dictator::new(DictatorDeps {
        recorder: Arc::new(recorder.clone()),
        pipeline: Arc::new(pipeline),
        clipboard: Arc::new(FakeClipboard::new()),
        clock: Arc::new(clock.clone()),
        settings: Arc::new(move || s3.get()),
        events: events.clone(),
        timers: Arc::new(NoTimers),
        executor: Arc::new(InlineExec),
    });
    let system = Arc::new(FakeSystem::default());
    let md = models_dir.clone();
    let service = AppService::new(ServiceDeps {
        version: "9.9.9".into(),
        display_name: display_name.map(str::to_string),
        settings: settings.clone(),
        dictionary,
        snippets,
        history: history.clone(),
        clock: Arc::new(clock.clone()),
        dictator,
        engine,
        models,
        system: system.clone(),
        events: events.clone(),
        models_dir_for: Arc::new(move |custom: Option<&Path>| -> PathBuf {
            custom.map(Path::to_path_buf).unwrap_or_else(|| md.clone())
        }),
    });
    ServiceRig {
        _rt: rt,
        dir,
        service,
        system,
        events,
        launcher,
        recorder,
        stt,
        clock,
        history_sink: history,
        settings,
    }
}
