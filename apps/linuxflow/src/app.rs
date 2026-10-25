//! GTK application: single instance (`adw::Application`), hub, pill, tray, orderly shutdown.

use crate::cli::{Cli, RemoteAction};
use crate::clipboard_gdk::GdkClipboard;
use crate::system::UiCommand;
use crate::wiring::{build_core, Core, Options};
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use linuxflow_core::constants::{APP_ID, APP_NAME};
use linuxflow_core::domain::{AppState, MicrophoneInfo};
use linuxflow_core::events::{Event, Page};
use linuxflow_core::paths::AppPaths;
use linuxflow_core::traits::Controller;
use linuxflow_gtk::hub::{CallbackStateStore, HubWindow};
use linuxflow_gtk::overlay::{
    choose_mode_auto_with, create_host, ChildOverlay, NullOverlay, OverlayHost, OverlayMode,
    OverlayMsg, WORKER_ARG,
};
use linuxflow_i18n::{tr, tr_fmt};
use linuxflow_platform::tray::{advice, Tray, TrayCommand, TrayHandle, TrayModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Should the hub be shown? (`first`: first launch of the primary instance.)
pub fn should_show_hub(
    actions: &[RemoteAction],
    first: bool,
    show_hub_at_start: bool,
    tray_available: bool,
) -> bool {
    let asked = actions
        .iter()
        .any(|a| matches!(a, RemoteAction::ShowHub(_)));
    if !asked {
        return false;
    }
    let explicit_page = actions
        .iter()
        .any(|a| matches!(a, RemoteAction::ShowHub(Some(_))));
    // First launch without options: the "show at start" setting decides, unless no icon would
    // let the user find the application again.
    !first || explicit_page || show_hub_at_start || !tray_available
}

struct Ui {
    app: adw::Application,
    core: Core,
    controller: Arc<dyn Controller>,
    hub: RefCell<Option<HubWindow>>,
    overlay: RefCell<Box<dyn OverlayHost>>,
    tray: RefCell<Option<TrayHandle>>,
    mics: RefCell<Vec<MicrophoneInfo>>,
    last_state: RefCell<Option<AppState>>,
    quitting: Cell<bool>,
    first_command: Cell<bool>,
    tray_hint_shown: Cell<bool>,
}

/// Does the Wayland compositor offer `zwlr_layer_shell_v1`? (registry read, no GTK)
fn compositor_has_layer_shell() -> bool {
    use linuxflow_platform::capabilities::{Env, SystemEnv};
    SystemEnv
        .wayland_globals()
        .is_some_and(|g| g.iter().any(|g| g.interface == "zwlr_layer_shell_v1"))
}

fn make_overlay() -> Box<dyn OverlayHost> {
    let mode = choose_mode_auto_with(&compositor_has_layer_shell);
    if mode == OverlayMode::ChildXWayland {
        // The child process speaks the resolved interface language.
        return match std::env::current_exe() {
            Ok(exe) => Box::new(ChildOverlay::with_command(
                exe,
                vec![WORKER_ARG.into()],
                vec![
                    ("GDK_BACKEND".into(), "x11".into()),
                    ("LANGUAGE".into(), linuxflow_i18n::current_language()),
                ],
            )),
            Err(_) => Box::new(NullOverlay),
        };
    }
    create_host(mode)
}

impl Ui {
    fn new_hub(self: &Rc<Self>) -> HubWindow {
        let hub = HubWindow::new(&self.app, self.controller.clone());
        let dir = self.core.paths.state_dir.clone();
        let (load_dir, save_dir) = (dir.clone(), dir);
        hub.set_state_store(Rc::new(CallbackStateStore {
            load: Box::new(move || {
                linuxflow_core::store::read_text_lenient(&load_dir.join("ui-page"))
                    .map(|s| s.trim().to_string())
            }),
            save: Box::new(move |page| {
                let _ = linuxflow_core::store::write_atomic(
                    &save_dir.join("ui-page"),
                    page.as_bytes(),
                    linuxflow_core::store::FileMode::Private,
                );
            }),
        }));
        hub
    }

    fn show_hub(self: &Rc<Self>, page: Option<Page>) {
        if self.hub.borrow().is_none() {
            let hub = self.new_hub();
            *self.hub.borrow_mut() = Some(hub);
        }
        if let Some(hub) = self.hub.borrow().as_ref() {
            if let Some(p) = page {
                hub.handle_event(Event::Navigate(p));
            }
            hub.present();
        }
        self.refresh_microphones();
        self.maybe_tray_hint();
    }

    fn handle_event(&self, ev: Event) {
        match &ev {
            Event::State(s) => {
                *self.last_state.borrow_mut() = Some((**s).clone());
                self.update_tray();
            }
            Event::Phase { phase, .. } => {
                if let Some(st) = self.last_state.borrow_mut().as_mut() {
                    st.phase = *phase;
                }
                self.update_tray();
            }
            _ => {}
        }
        if let Some(hub) = self.hub.borrow().as_ref() {
            hub.handle_event(ev.clone());
        }
        if let Some(msg) = OverlayMsg::from_event(&ev) {
            self.overlay.borrow().push(msg);
        }
    }

    fn update_tray(&self) {
        let tray = self.tray.borrow();
        let (Some(tray), Some(state)) = (tray.as_ref(), self.last_state.borrow().clone()) else {
            return;
        };
        tray.update(TrayModel::from_app_state(
            &state,
            self.mics.borrow().clone(),
        ));
    }

    fn refresh_microphones(self: &Rc<Self>) {
        let controller = self.controller.clone();
        let this = Rc::downgrade(self);
        linuxflow_gtk::hub::ctx::run_blocking(
            move || controller.list_microphones(),
            move |list| {
                if let Some(ui) = this.upgrade() {
                    *ui.mics.borrow_mut() = list;
                    ui.update_tray();
                }
            },
        );
    }

    /// A single help toast when the tray icon is missing.
    fn maybe_tray_hint(&self) {
        let visible = self
            .hub
            .borrow()
            .as_ref()
            .is_some_and(|h| h.window().is_visible());
        let available = self.tray.borrow().as_ref().is_some_and(|t| t.available());
        if !visible || available || self.tray_hint_shown.get() {
            return;
        }
        if self.core.settings.get().tray_hint_dismissed {
            return;
        }
        let caps = self.controller.capabilities();
        let Some(a) = advice(&caps) else { return };
        self.tray_hint_shown.set(true);
        self.handle_event(Event::Toast(tr_fmt("{} {}", &[&a.title, &a.body])));
        if let Err(e) = self
            .controller
            .set_settings(serde_json::json!({"trayHintDismissed": true}))
        {
            tracing::warn!("tray hint flag not saved: {e}");
        }
    }

    fn show_about(&self) {
        let mut b = adw::AboutWindow::builder()
            .application_name(APP_NAME)
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .comments(tr("Voice dictation, 100% local"))
            .developer_name("ITchrisDEB")
            .copyright("Copyright © 2026 ITchrisDEB")
            .website("https://github.com/ITchrisDEB/Linuxflow")
            .license_type(gtk::License::MitX11);
        if let Some(hub) = self.hub.borrow().as_ref() {
            b = b.transient_for(hub.window());
        }
        b.build().present();
    }

    fn tray_command(self: &Rc<Self>, cmd: TrayCommand) {
        let c = &self.controller;
        let set = |patch: serde_json::Value| {
            if let Err(e) = c.set_settings(patch) {
                self.handle_event(Event::Toast(e.to_string()));
            }
        };
        match cmd {
            TrayCommand::OpenHub => self.show_hub(None),
            TrayCommand::OpenPage(p) => self.show_hub(Some(p)),
            TrayCommand::ToggleDictation => {
                c.dictation_toggle(false);
            }
            TrayCommand::CancelDictation => {
                c.dictation_cancel();
            }
            TrayCommand::SetLanguage(l) => set(serde_json::json!({"language": l})),
            TrayCommand::SetMicrophone(m) => set(serde_json::json!({"microphoneId": m})),
            TrayCommand::SetAutoPaste(b) => set(serde_json::json!({"autoPaste": b})),
            TrayCommand::SetLaunchAtLogin(b) => set(serde_json::json!({"launchAtLogin": b})),
            TrayCommand::About => self.show_about(),
            TrayCommand::Quit => self.quit(),
        }
    }

    fn remote_action(self: &Rc<Self>, a: &RemoteAction) {
        let c = &self.controller;
        match a {
            RemoteAction::Toggle => {
                c.dictation_toggle(false);
            }
            RemoteAction::Start => {
                self.core.dictator.start(false, false);
            }
            RemoteAction::Stop => {
                self.core.dictator.stop();
            }
            RemoteAction::Cancel => {
                c.dictation_cancel();
            }
            RemoteAction::Quit => self.quit(),
            RemoteAction::ShowHub(_) => {} // handled with should_show_hub
        }
    }

    /// Rebuilds the windows after a language change.
    fn rebuild(self: &Rc<Self>) {
        let was_visible = self
            .hub
            .borrow()
            .as_ref()
            .is_some_and(|h| h.window().is_visible());
        if let Some(old) = self.hub.borrow_mut().take() {
            old.window().destroy();
        }
        *self.overlay.borrow_mut() = make_overlay();
        if was_visible {
            self.show_hub(None);
        }
        self.update_tray();
    }

    /// Orderly shutdown: shortcuts, dictation, engine, tray, pill, windows.
    fn quit(&self) {
        if self.quitting.replace(true) {
            return;
        }
        tracing::info!("shutting down");
        self.core.hotkeys.stop();
        self.core.dictator.cancel();
        // A clipboard restoration scheduled just before shutdown must not be lost.
        self.core.dictator.flush_pending();
        self.core.engine.host().shutdown();
        if let Some(t) = self.tray.borrow_mut().take() {
            t.shutdown();
        }
        *self.overlay.borrow_mut() = Box::new(NullOverlay);
        if let Some(h) = self.hub.borrow_mut().take() {
            h.window().destroy();
        }
        self.app.quit();
    }
}

/// Entry point of the graphical application. Returns the exit code.
pub fn run(cli: Cli, paths: AppPaths, args: Vec<std::ffi::OsString>) -> glib::ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // Probe of the single instance with a `gio::Application` (no display needed):
    // an existing instance receives the command line; without an instance, quit, cancel or
    // stop start nothing.
    {
        let probe = gtk::gio::Application::builder()
            .application_id(APP_ID)
            .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
            .build();
        if let Err(e) = probe.register(gtk::gio::Cancellable::NONE) {
            tracing::warn!("application registration failed: {e}");
        }
        if probe.is_remote() {
            return probe.run_with_args(&args);
        }
        if cli.is_noop_without_instance() {
            return glib::ExitCode::SUCCESS;
        }
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    if let Err(e) = app.register(gtk::gio::Cancellable::NONE) {
        tracing::warn!("application registration failed: {e}");
    }
    if app.is_remote() {
        // Another instance appeared in the meantime.
        return app.run_with_args(&args);
    }

    let (events_tx, events_rx) = async_channel::unbounded::<Event>();
    let (ui_tx, ui_rx) = async_channel::unbounded::<UiCommand>();
    let core = match build_core(
        paths,
        Options {
            display_name: cli.display_name.clone(),
            no_engine: cli.no_engine || std::env::var_os("LINUXFLOW_NO_ENGINE").is_some(),
        },
        events_tx,
        ui_tx.clone(),
        Arc::new(GdkClipboard),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{}",
                linuxflow_i18n::tr_fmt("LinuxFlow could not start: {}", &[&e])
            );
            return glib::ExitCode::FAILURE;
        }
    };
    let controller: Arc<dyn Controller> = core.service.clone();
    let ui = Rc::new(Ui {
        app: app.clone(),
        core,
        controller,
        hub: RefCell::new(None),
        overlay: RefCell::new(make_overlay()),
        tray: RefCell::new(None),
        mics: RefCell::new(Vec::new()),
        last_state: RefCell::new(None),
        quitting: Cell::new(false),
        first_command: Cell::new(true),
        tray_hint_shown: Cell::new(false),
    });
    let hold = app.hold();

    // Core events towards the views, interface commands.
    let u = ui.clone();
    glib::spawn_future_local(async move {
        while let Ok(ev) = events_rx.recv().await {
            u.handle_event(ev);
        }
    });
    let u = ui.clone();
    glib::spawn_future_local(async move {
        while let Ok(cmd) = ui_rx.recv().await {
            match cmd {
                UiCommand::ShowHub(p) => u.show_hub(p),
                UiCommand::RebuildUi => u.rebuild(),
                UiCommand::About => u.show_about(),
                UiCommand::Quit => u.quit(),
            }
        }
    });

    // Tray.
    let (tray_tx, tray_rx) = async_channel::unbounded::<TrayCommand>();
    let handle = Tray::spawn(tray_tx);
    let availability = handle.subscribe();
    *ui.tray.borrow_mut() = Some(handle);
    let u = ui.clone();
    glib::spawn_future_local(async move {
        while let Ok(cmd) = tray_rx.recv().await {
            u.tray_command(cmd);
        }
    });
    let u = ui.clone();
    glib::spawn_future_local(async move {
        while let Ok(a) = availability.recv().await {
            let ok = a == linuxflow_platform::tray::TrayAvailability::Available;
            u.core.tray_available.store(ok, Ordering::SeqCst);
            u.core.service.push_state();
            u.update_tray();
            u.maybe_tray_hint();
        }
    });
    let u = ui.clone();
    glib::timeout_add_seconds_local_once(6, move || u.maybe_tray_hint());

    // Signals: clean shutdown.
    crate::signals::install();
    let u = ui.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        if crate::signals::take_requested() {
            u.quit();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });

    // Command line (first and following launches).
    let u = ui.clone();
    app.connect_command_line(move |_, cmdline| {
        let args = cmdline.arguments();
        let cli = match crate::cli::parse_from(args) {
            Ok(c) => c,
            Err(exit) => {
                // The arguments were already validated by the calling process (main).
                tracing::warn!("invalid forwarded command line: {}", exit.text.trim());
                return glib::ExitCode::from(exit.code.clamp(0, 255) as u8);
            }
        };
        let first = u.first_command.replace(false);
        let actions = cli.actions();
        let settings = u.core.settings.get();
        let tray_ok = u.core.tray_available.load(Ordering::SeqCst);
        for a in &actions {
            u.remote_action(a);
        }
        if should_show_hub(&actions, first, settings.show_hub_at_start, tray_ok) {
            let page = actions.iter().find_map(|a| match a {
                RemoteAction::ShowHub(p) => *p,
                _ => None,
            });
            u.show_hub(page);
        }
        glib::ExitCode::SUCCESS
    });
    let u = ui.clone();
    app.connect_shutdown(move |_| u.quit());

    // Starting the services: shortcut, engine, monitoring.
    ui.core.service.startup();
    ui.refresh_microphones();
    let code = app.run_with_args(&args);
    drop(hold);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_visibility_rules() {
        let none: Vec<RemoteAction> = vec![];
        let show = vec![RemoteAction::ShowHub(None)];
        let page = vec![RemoteAction::ShowHub(Some(Page::Settings))];
        // --background: nothing to show.
        assert!(!should_show_hub(&none, true, true, true));
        // First launch: the setting decides, unless there is no icon.
        assert!(should_show_hub(&show, true, true, true));
        assert!(!should_show_hub(&show, true, false, true));
        assert!(should_show_hub(&show, true, false, false));
        // Explicit page and later launches: always.
        assert!(should_show_hub(&page, true, false, true));
        assert!(should_show_hub(&show, false, false, true));
        // Dictation actions alone: never.
        assert!(!should_show_hub(
            &[RemoteAction::Toggle],
            false,
            true,
            false
        ));
    }
}
