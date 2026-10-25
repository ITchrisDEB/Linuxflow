//! Global shortcut: `HotkeyManager` (platform) -> events -> `PttMachine` (core) ->
//! dictation inputs.

use crate::runtime::Dictator;
use crate::service::{ProbeError, ShortcutInfo};
use linuxflow_core::accelerator::{self, Accelerator, AcceleratorError};
use linuxflow_core::dictation::Input;
use linuxflow_core::domain::{Capabilities, Phase, Settings, ShortcutMode};
use linuxflow_core::ptt::{PttAction, PttMachine};
use linuxflow_core::traits::{HotkeyEvent, HotkeySink, RawKeyEvent};
use linuxflow_i18n::tr;
use linuxflow_platform::hotkeys::{
    plan_chain, AcceleratorSpec, BackendKind, HotkeyManager, HotkeyOptions, HotkeySpec, Modifiers,
};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Adapter: core `Accelerator` to the platform `AcceleratorSpec`.
pub struct AccelSpec(HotkeySpec);

impl AccelSpec {
    /// Refuses a key with no known physical code (otherwise it would become "modifiers only").
    pub fn new(acc: &Accelerator) -> Result<Self, AcceleratorError> {
        let mut m = Modifiers::NONE;
        for (on, bit) in [
            (acc.modifiers.ctrl, Modifiers::CTRL),
            (acc.modifiers.alt, Modifiers::ALT),
            (acc.modifiers.shift, Modifiers::SHIFT),
            (acc.modifiers.super_key, Modifiers::SUPER),
        ] {
            if on {
                m = m.union(bit);
            }
        }
        let key = match &acc.key {
            None => None,
            Some(k) => Some(
                k.to_evdev()
                    .ok_or_else(|| AcceleratorError::KeyNotRecognized(k.name()))?,
            ),
        };
        Ok(Self(HotkeySpec { modifiers: m, key }))
    }
}

impl AcceleratorSpec for AccelSpec {
    fn to_spec(&self) -> HotkeySpec {
        self.0
    }
}

/// Capabilities detected in the background; `wait` blocks (for `timeout` at most) while they are missing.
#[derive(Default)]
pub struct CapsCell {
    value: Mutex<Option<Capabilities>>,
    cv: Condvar,
}

impl CapsCell {
    pub fn set(&self, caps: Capabilities) {
        *lock(&self.value) = Some(caps);
        self.cv.notify_all();
    }

    pub fn get(&self) -> Option<Capabilities> {
        lock(&self.value).clone()
    }

    pub fn wait(&self, timeout: Duration) -> Capabilities {
        let g = lock(&self.value);
        let (g, _) = self
            .cv
            .wait_timeout_while(g, timeout, |v| v.is_none())
            .unwrap_or_else(|e| e.into_inner());
        g.clone().unwrap_or_default()
    }
}

type ManagerFactory = Arc<dyn Fn(Capabilities, HotkeyOptions) -> HotkeyManager + Send + Sync>;

struct Sink {
    machine: Arc<Mutex<Option<PttMachine>>>,
    dictator: Dictator,
}

impl Sink {
    fn act(&self, action: Option<PttAction>) {
        let input = match action {
            Some(PttAction::Start) => Input::Start {
                to_hub: false,
                no_paste: false,
            },
            Some(PttAction::Stop) => Input::Stop,
            Some(PttAction::Cancel) => Input::Cancel,
            Some(PttAction::Toggle) => Input::Toggle { to_hub: false },
            None => return,
        };
        self.dictator.dispatch(input);
    }
}

impl HotkeySink for Sink {
    fn hotkey(&self, event: HotkeyEvent) {
        let a = lock(&self.machine)
            .as_mut()
            .and_then(|m| m.on_hotkey(event));
        self.act(a);
    }
    fn raw_key(&self, event: RawKeyEvent) {
        let a = lock(&self.machine)
            .as_mut()
            .and_then(|m| m.on_raw_key(event));
        self.act(a);
    }
}

pub struct HotkeyController {
    caps: Arc<CapsCell>,
    dictator: Dictator,
    make_manager: ManagerFactory,
    manager: Mutex<Option<Arc<HotkeyManager>>>,
    machine: Arc<Mutex<Option<PttMachine>>>,
    info: Mutex<ShortcutInfo>,
    suspended: Mutex<bool>,
    apply_lock: Mutex<()>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

impl HotkeyController {
    pub fn new(
        caps: Arc<CapsCell>,
        dictator: Dictator,
        make_manager: ManagerFactory,
        on_change: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            caps,
            dictator: dictator.clone(),
            make_manager,
            manager: Mutex::new(None),
            machine: Arc::default(),
            info: Mutex::new(ShortcutInfo::default()),
            suspended: Mutex::new(false),
            apply_lock: Mutex::new(()),
            on_change,
        });
        // Keeps the PTT machine synchronised with the dictation phase.
        let weak = Arc::downgrade(&this);
        dictator.on_phase(move |p| {
            if let Some(s) = weak.upgrade() {
                s.sync_phase(p);
            }
        });
        this
    }

    /// Production: the real platform manager.
    pub fn system_factory() -> ManagerFactory {
        Arc::new(HotkeyManager::new)
    }

    pub fn sync_phase(&self, phase: Phase) {
        if let Some(m) = lock(&self.machine).as_mut() {
            m.set_phase(phase);
        }
    }

    pub fn info(&self) -> ShortcutInfo {
        lock(&self.info).clone()
    }

    pub fn suspend(&self, suspended: bool) {
        *lock(&self.suspended) = suspended;
        if let Some(m) = lock(&self.manager).as_ref() {
            m.suspend(suspended);
        }
        if let Some(m) = lock(&self.machine).as_mut() {
            m.reset();
        }
    }

    fn options(settings: &Settings) -> HotkeyOptions {
        HotkeyOptions {
            allow_evdev: settings
                .extra
                .get("allowEvdev")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            ..HotkeyOptions::default()
        }
    }

    /// (Re)starts the shortcut on a thread (the portal may ask for consent).
    pub fn apply(self: &Arc<Self>, settings: &Settings) {
        let this = self.clone();
        let settings = settings.clone();
        let spawned = std::thread::Builder::new()
            .name("linuxflow-hotkey".into())
            .spawn(move || this.apply_blocking(&settings));
        if let Err(e) = spawned {
            tracing::warn!("hotkey thread could not start: {e}");
        }
    }

    /// Blocking version of [`HotkeyController::apply`] (tests).
    pub fn apply_blocking(&self, settings: &Settings) {
        let _serial = lock(&self.apply_lock);
        if let Some(old) = lock(&self.manager).take() {
            old.stop();
        }
        *lock(&self.machine) = None;
        *lock(&self.info) = ShortcutInfo::default();
        let caps = self.caps.wait(Duration::from_secs(10));
        let acc = match accelerator::parse(&settings.shortcut) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("invalid shortcut in settings: {e}");
                (self.on_change)();
                return;
            }
        };
        let spec = match AccelSpec::new(&acc) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("unsupported key in shortcut: {e}");
                (self.on_change)();
                return;
            }
        };
        // Unknown key in hold mode: forced fallback to toggle mode (not persisted).
        let mode = match accelerator::validate(&acc, settings.mode) {
            Err(AcceleratorError::Reserved(a)) => {
                // Never grab Ctrl+C, Alt+Tab… globally, even if the settings file asks for it.
                tracing::warn!(shortcut = %a, "reserved shortcut not registered");
                *lock(&self.info) = ShortcutInfo::default();
                (self.on_change)();
                return;
            }
            Err(AcceleratorError::KeyNotRecognized(_)) => ShortcutMode::Toggle,
            _ => settings.mode,
        };
        let manager = Arc::new((self.make_manager)(caps, Self::options(settings)));
        manager.suspend(*lock(&self.suspended));
        let sink = Arc::new(Sink {
            machine: self.machine.clone(),
            dictator: self.dictator.clone(),
        });
        let status = manager.start(&spec, mode, sink);
        if status.ok {
            if let Ok(mut m) = PttMachine::new(&acc, mode, status.supports_release) {
                m.set_phase(self.dictator.phase());
                *lock(&self.machine) = Some(m);
            }
        }
        tracing::info!(backend = ?status.backend, ok = status.ok, "shortcut: {}", status.message);
        *lock(&self.info) = ShortcutInfo {
            ok: status.ok,
            desktop_trigger: (status.ok
                && status.backend == BackendKind::Portal
                && !status.trigger_description.is_empty())
            .then(|| status.trigger_description.clone()),
            ptt_available: status.ok && status.supports_release,
            message: (!status.ok && !status.message.is_empty()).then(|| status.message.clone()),
        };
        *lock(&self.manager) = Some(manager);
        (self.on_change)();
    }

    pub fn stop(&self) {
        if let Some(m) = lock(&self.manager).take() {
            m.stop();
        }
        *lock(&self.machine) = None;
    }

    /// Can the shortcut be taken? Real attempt under X11 (detects a conflict), availability check of
    /// the mechanism elsewhere (a portal cannot be probed without consent).
    pub fn probe(&self, acc: &Accelerator, mode: ShortcutMode) -> Result<(), ProbeError> {
        let caps = self.caps.wait(Duration::from_secs(3));
        let spec = AccelSpec::new(acc)
            .map_err(|e| ProbeError::Unavailable(crate::service::accelerator_error_text(&e)))?;
        let options = HotkeyOptions::default();
        let chain = plan_chain(&caps, &options, &spec.to_spec());
        if chain.first().is_none_or(|k| *k == BackendKind::None) {
            return Err(ProbeError::Unavailable(tr(
                "No global shortcut mechanism is available here. Bind a desktop shortcut to the LinuxFlow command line instead.",
            )));
        }
        // Native managers (KDE, GNOME): the conflict is known BEFORE the key is registered, and
        // the message names the application or the action that owns it.
        let native = match chain.first() {
            Some(BackendKind::KGlobalAccel) => {
                linuxflow_platform::hotkeys::kglobalaccel::probe(&spec.to_spec())
            }
            Some(BackendKind::GnomeKeys) => {
                linuxflow_platform::hotkeys::gnome::probe(&spec.to_spec())
            }
            Some(BackendKind::SwayKeys) => {
                linuxflow_platform::hotkeys::compositor::sway_probe(&spec.to_spec())
            }
            Some(BackendKind::HyprlandKeys) => {
                linuxflow_platform::hotkeys::compositor::hyprland_probe(&spec.to_spec())
            }
            _ => Ok(()),
        };
        if let Err(message) = native {
            return Err(ProbeError::Unavailable(message));
        }
        if chain.first() == Some(&BackendKind::X11) {
            struct Null;
            impl HotkeySink for Null {
                fn hotkey(&self, _: HotkeyEvent) {}
                fn raw_key(&self, _: RawKeyEvent) {}
            }
            let tmp = (self.make_manager)(caps, options);
            let status = tmp.start(&spec, mode, Arc::new(Null));
            tmp.stop();
            if !status.ok {
                return Err(ProbeError::Conflict);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::runtime::{DictatorDeps, EventSink};
    use crate::testkit::*;
    use linuxflow_core::domain::EngineInfo;
    use linuxflow_core::pipeline::{Pipeline, PipelineDeps};
    use linuxflow_core::testing::*;
    use linuxflow_platform::hotkeys::{
        BackendError, BackendFactory, BackendReport, HotkeyBackend, StartRequest,
    };

    struct FakeBackend {
        sink: Arc<Mutex<Option<Arc<dyn HotkeySink>>>>,
        report: Result<BackendReport, BackendError>,
        started: Arc<Mutex<Vec<(HotkeySpec, ShortcutMode)>>>,
    }
    impl HotkeyBackend for FakeBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::Portal
        }
        fn start(
            &mut self,
            req: &StartRequest,
            sink: Arc<dyn HotkeySink>,
        ) -> Result<BackendReport, BackendError> {
            self.started.lock().unwrap().push((req.spec, req.mode));
            *self.sink.lock().unwrap() = Some(sink);
            self.report.clone()
        }
        fn stop(&mut self) {}
    }
    struct Factory {
        sink: Arc<Mutex<Option<Arc<dyn HotkeySink>>>>,
        report: Result<BackendReport, BackendError>,
        started: Arc<Mutex<Vec<(HotkeySpec, ShortcutMode)>>>,
    }
    impl BackendFactory for Factory {
        fn create(&self, kind: BackendKind, _: &HotkeyOptions) -> Option<Box<dyn HotkeyBackend>> {
            (kind == BackendKind::Portal).then(|| {
                Box::new(FakeBackend {
                    sink: self.sink.clone(),
                    report: self.report.clone(),
                    started: self.started.clone(),
                }) as Box<dyn HotkeyBackend>
            })
        }
    }

    struct Rig {
        ctl: Arc<HotkeyController>,
        dictator: Dictator,
        sink: Arc<Mutex<Option<Arc<dyn HotkeySink>>>>,
        started: Arc<Mutex<Vec<(HotkeySpec, ShortcutMode)>>>,
        recorder: FakeRecorder,
        changes: Arc<Mutex<u32>>,
    }

    fn rig(release: bool, ok: bool) -> Rig {
        let recorder = FakeRecorder::new();
        recorder.set_silence_ms(1000);
        let stt = FakeStt::new();
        stt.push_text("x y");
        let clock = FakeClock::new(1000);
        let pipeline = Pipeline::new(PipelineDeps {
            stt: Arc::new(stt),
            clipboard: Arc::new(FakeClipboard::new()),
            paster: Arc::new(FakePaster::new()),
            window: Arc::new(FakeWindow::new()),
            clock: Arc::new(clock.clone()),
            formatter: Arc::new(FakeFormatter::new()),
            history: Arc::new(FakeHistory::new()),
            paste_modes: Arc::new(FakePasteModes::new()),
            hints: Arc::new(FakeHints::new()),
        });
        let events: Arc<dyn EventSink> = Arc::new(VecSink::default());
        let dictator = Dictator::new(DictatorDeps {
            recorder: Arc::new(recorder.clone()),
            pipeline: Arc::new(pipeline),
            clipboard: Arc::new(FakeClipboard::new()),
            clock: Arc::new(clock),
            settings: Arc::new(Settings::default),
            events,
            timers: Arc::new(NoTimers),
            executor: Arc::new(InlineExec),
        });
        dictator.engine_status(EngineInfo {
            ready: true,
            ..EngineInfo::default()
        });
        let sink = Arc::new(Mutex::new(None));
        let started = Arc::new(Mutex::new(Vec::new()));
        let report = if ok {
            Ok(BackendReport {
                supports_release: release,
                provides_hotkey_events: true,
                ..BackendReport::default()
            })
        } else {
            Err(BackendError::new(
                linuxflow_platform::hotkeys::BackendErrorKind::Denied,
                "denied",
            ))
        };
        let (s2, st2, r2) = (sink.clone(), started.clone(), report.clone());
        let caps = Arc::new(CapsCell::default());
        caps.set(Capabilities {
            session: "wayland".into(),
            portal_global_shortcuts: true,
            ..Capabilities::default()
        });
        let changes = Arc::new(Mutex::new(0));
        let c2 = changes.clone();
        let ctl = HotkeyController::new(
            caps,
            dictator.clone(),
            Arc::new(move |caps, opts| {
                HotkeyManager::with_factory(
                    caps,
                    opts,
                    Box::new(Factory {
                        sink: s2.clone(),
                        report: r2.clone(),
                        started: st2.clone(),
                    }),
                )
            }),
            Arc::new(move || *c2.lock().unwrap() += 1),
        );
        Rig {
            ctl,
            dictator,
            sink,
            started,
            recorder,
            changes,
        }
    }

    fn fire(r: &Rig, ev: HotkeyEvent) {
        let s = r.sink.lock().unwrap().clone().unwrap();
        s.hotkey(ev);
    }

    #[test]
    fn toggle_mode_starts_then_stops_dictation() {
        let r = rig(true, true);
        r.ctl.apply_blocking(&Settings::default());
        assert!(r.ctl.info().ok);
        assert!(r.ctl.info().ptt_available);
        assert_eq!(*r.changes.lock().unwrap(), 1);
        fire(&r, HotkeyEvent::Activated { ts_ms: 0 });
        assert_eq!(r.dictator.phase(), Phase::Listening);
        fire(&r, HotkeyEvent::Activated { ts_ms: 5000 });
        assert_eq!(r.dictator.phase(), Phase::Done);
        assert_eq!(r.started.lock().unwrap()[0].1, ShortcutMode::Toggle);
    }

    #[test]
    fn push_to_talk_hold_and_release() {
        let r = rig(true, true);
        let s = Settings {
            mode: ShortcutMode::Ptt,
            ..Settings::default()
        };
        r.ctl.apply_blocking(&s);
        fire(&r, HotkeyEvent::Activated { ts_ms: 0 });
        assert_eq!(r.dictator.phase(), Phase::Listening);
        fire(&r, HotkeyEvent::Deactivated { ts_ms: 2000 });
        assert_eq!(r.dictator.phase(), Phase::Done);
    }

    #[test]
    fn push_to_talk_without_release_falls_back_to_toggle() {
        let r = rig(false, true);
        let s = Settings {
            mode: ShortcutMode::Ptt,
            ..Settings::default()
        };
        r.ctl.apply_blocking(&s);
        assert!(!r.ctl.info().ptt_available);
        fire(&r, HotkeyEvent::Activated { ts_ms: 0 });
        fire(&r, HotkeyEvent::Deactivated { ts_ms: 10 });
        assert_eq!(r.dictator.phase(), Phase::Listening);
        fire(&r, HotkeyEvent::Activated { ts_ms: 3000 });
        assert_eq!(r.dictator.phase(), Phase::Done);
    }

    #[test]
    fn suspended_shortcut_ignores_events() {
        let r = rig(true, true);
        r.ctl.apply_blocking(&Settings::default());
        r.ctl.suspend(true);
        fire(&r, HotkeyEvent::Activated { ts_ms: 0 });
        assert_eq!(r.dictator.phase(), Phase::Idle);
        r.ctl.suspend(false);
        fire(&r, HotkeyEvent::Activated { ts_ms: 10 });
        assert_eq!(r.dictator.phase(), Phase::Listening);
        assert!(r.recorder.calls().len() == 1);
    }

    #[test]
    fn backend_failure_reports_not_ok() {
        let r = rig(true, false);
        r.ctl.apply_blocking(&Settings::default());
        assert!(!r.ctl.info().ok);
        assert!(!r.ctl.info().ptt_available);
        assert_eq!(*r.changes.lock().unwrap(), 1);
    }

    #[test]
    fn adapter_maps_modifiers_and_keys() {
        let acc = accelerator::parse("Ctrl+Alt+Space").unwrap();
        let spec = AccelSpec::new(&acc).unwrap().to_spec();
        assert_eq!(spec.display(), "Ctrl+Alt+Space");
        let acc = accelerator::parse("Ctrl+Super").unwrap();
        let spec = AccelSpec::new(&acc).unwrap().to_spec();
        assert!(spec.is_modifier_only());
        assert_eq!(spec.modifiers, Modifiers::CTRL.union(Modifiers::SUPER));
        let acc = accelerator::parse("Ctrl+Unobtainium").unwrap();
        assert!(AccelSpec::new(&acc).is_err());
    }

    #[test]
    fn probe_reports_missing_mechanism() {
        let r = rig(true, true);
        let acc = accelerator::parse("Ctrl+Alt+K").unwrap();
        assert!(r.ctl.probe(&acc, ShortcutMode::Toggle).is_ok());
        // Without a portal: no mechanism.
        let caps = Arc::new(CapsCell::default());
        caps.set(Capabilities {
            session: "wayland".into(),
            ..Capabilities::default()
        });
        let ctl = HotkeyController::new(
            caps,
            r.dictator.clone(),
            HotkeyController::system_factory(),
            Arc::new(|| {}),
        );
        assert!(matches!(
            ctl.probe(&acc, ShortcutMode::Toggle),
            Err(ProbeError::Unavailable(_))
        ));
    }

    #[test]
    fn caps_cell_wait_times_out_to_default() {
        let c = CapsCell::default();
        assert_eq!(c.wait(Duration::from_millis(10)), Capabilities::default());
        c.set(Capabilities {
            session: "x11".into(),
            ..Capabilities::default()
        });
        assert_eq!(c.wait(Duration::from_millis(10)).session, "x11");
    }
}
