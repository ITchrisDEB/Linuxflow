//! `HotkeyManager`: chooses the backend chain at run time and exposes a single state.

use super::backend::{BackendError, BackendFactory, HotkeyBackend, StartRequest, SystemBackends};
use super::select::{plan_chain, BackendKind, HotkeyOptions};
use super::spec::AcceleratorSpec;
use linuxflow_core::domain::{Capabilities, ShortcutMode};
use linuxflow_core::traits::{HotkeyEvent, HotkeySink, RawKeyEvent};
use linuxflow_i18n::{tr, tr_fmt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyStatus {
    pub backend: BackendKind,
    pub ok: bool,
    pub message: String,
    pub supports_release: bool,
    pub supports_modifier_only: bool,
    pub trigger_description: String,
    pub provides_hotkey_events: bool,
    pub provides_raw_keys: bool,
}

impl HotkeyStatus {
    fn stopped() -> Self {
        Self {
            backend: BackendKind::None,
            ok: false,
            message: tr("The global shortcut is not started."),
            supports_release: false,
            supports_modifier_only: false,
            trigger_description: String::new(),
            provides_hotkey_events: false,
            provides_raw_keys: false,
        }
    }
}

/// Drops the events during the suspension, whatever the backend.
struct GatedSink {
    inner: Arc<dyn HotkeySink>,
    suspended: Arc<AtomicBool>,
}

impl HotkeySink for GatedSink {
    fn hotkey(&self, event: HotkeyEvent) {
        if !self.suspended.load(Ordering::SeqCst) {
            self.inner.hotkey(event);
        }
    }
    fn raw_key(&self, event: RawKeyEvent) {
        if !self.suspended.load(Ordering::SeqCst) {
            self.inner.raw_key(event);
        }
    }
}

struct State {
    backend: Option<Box<dyn HotkeyBackend>>,
    status: HotkeyStatus,
}

pub struct HotkeyManager {
    caps: Capabilities,
    opts: HotkeyOptions,
    factory: Box<dyn BackendFactory>,
    suspended: Arc<AtomicBool>,
    state: Mutex<State>,
}

fn backend_label(kind: BackendKind) -> String {
    match kind {
        BackendKind::KGlobalAccel => tr("KDE's shortcut service"),
        BackendKind::GnomeKeys => tr("GNOME's keyboard shortcuts"),
        BackendKind::SwayKeys => tr("Sway's key bindings"),
        BackendKind::HyprlandKeys => tr("Hyprland's key bindings"),
        BackendKind::Portal => tr("the desktop shortcut portal"),
        BackendKind::X11 => tr("X11 key grabs"),
        BackendKind::Evdev => tr("keyboard devices"),
        BackendKind::None => tr("no backend"),
    }
}

impl HotkeyManager {
    pub fn new(caps: Capabilities, opts: HotkeyOptions) -> Self {
        Self::with_factory(caps, opts, Box::new(SystemBackends))
    }

    pub fn with_factory(
        caps: Capabilities,
        opts: HotkeyOptions,
        factory: Box<dyn BackendFactory>,
    ) -> Self {
        Self {
            caps,
            opts,
            factory,
            suspended: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(State {
                backend: None,
                status: HotkeyStatus::stopped(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts the shortcut (blocking: call it from a worker thread, the portal may
    /// show a consent request). Never returns an error: without a backend, the state
    /// is `BackendKind::None` and `message` explains what to do.
    pub fn start(
        &self,
        accel: &dyn AcceleratorSpec,
        mode: ShortcutMode,
        sink: Arc<dyn HotkeySink>,
    ) -> HotkeyStatus {
        self.stop();
        let spec = accel.to_spec();
        if let Err(e) = spec.validate() {
            let mut st = HotkeyStatus::stopped();
            st.message = tr_fmt("Invalid shortcut: {}", &[&e]);
            self.lock().status = st.clone();
            return st;
        }
        let gated: Arc<dyn HotkeySink> = Arc::new(GatedSink {
            inner: sink,
            suspended: self.suspended.clone(),
        });
        let req = StartRequest { spec, mode };
        let mut failures: Vec<String> = Vec::new();
        for kind in plan_chain(&self.caps, &self.opts, &spec) {
            if kind == BackendKind::None {
                break;
            }
            let Some(mut backend) = self.factory.create(kind, &self.opts) else {
                continue;
            };
            match backend.start(&req, gated.clone()) {
                Ok(rep) => {
                    backend.set_suspended(self.suspended.load(Ordering::SeqCst));
                    let mut message =
                        tr_fmt("Shortcut active through {}.", &[&backend_label(kind)]);
                    if let Some(n) = rep.note {
                        message = format!("{message} {n}");
                    }
                    let trigger = if rep.trigger_description.is_empty() {
                        spec.display()
                    } else {
                        rep.trigger_description
                    };
                    let st = HotkeyStatus {
                        backend: kind,
                        ok: true,
                        message,
                        supports_release: rep.supports_release,
                        supports_modifier_only: rep.supports_modifier_only,
                        trigger_description: trigger,
                        provides_hotkey_events: rep.provides_hotkey_events,
                        provides_raw_keys: rep.provides_raw_keys,
                    };
                    let mut g = self.lock();
                    g.backend = Some(backend);
                    g.status = st.clone();
                    return st;
                }
                Err(BackendError { kind: k, message }) => {
                    tracing::warn!(backend = ?kind, error_kind = ?k, "hotkey backend failed");
                    failures.push(message);
                    // Conflict reported by a native manager (KDE, GNOME): stop here. Falling back
                    // on the portal would make the desktop SILENTLY assign another key.
                    if k == super::backend::BackendErrorKind::Conflict
                        && matches!(
                            kind,
                            BackendKind::KGlobalAccel
                                | BackendKind::GnomeKeys
                                | BackendKind::SwayKeys
                                | BackendKind::HyprlandKeys
                        )
                    {
                        break;
                    }
                }
            }
        }
        let mut st = HotkeyStatus::stopped();
        st.trigger_description = spec.display();
        st.message = if !failures.is_empty() {
            failures.join(" ")
        } else if spec.is_modifier_only() && !self.opts.allow_evdev {
            tr("Shortcuts made only of modifier keys need keyboard-device access, which is disabled. Choose a shortcut with a regular key, or enable it in the settings.")
        } else {
            tr("No global shortcut mechanism is available here. Bind a desktop shortcut to the LinuxFlow command line instead.")
        };
        self.lock().status = st.clone();
        st
    }

    pub fn stop(&self) {
        let mut g = self.lock();
        if let Some(mut b) = g.backend.take() {
            b.stop();
        }
        g.status = HotkeyStatus::stopped();
    }

    /// Suspends or resumes the shortcut (for example while it is being recorded).
    pub fn suspend(&self, suspended: bool) {
        self.suspended.store(suspended, Ordering::SeqCst);
        if let Some(b) = self.lock().backend.as_mut() {
            b.set_suspended(suspended);
        }
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> HotkeyStatus {
        let state = self.lock();
        let mut status = state.status.clone();
        if status.ok && state.backend.as_ref().is_some_and(|b| !b.is_alive()) {
            status.ok = false;
            status.message = tr(
                "The shortcut stopped working (the desktop's shortcut service restarted). Choose the shortcut again to re-enable it.",
            );
        }
        status
    }
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::super::backend::{BackendErrorKind, BackendReport};
    use super::super::spec::HotkeySpec;
    use super::*;
    use crate::capabilities::{detect, mock::MockEnv};
    use std::collections::HashMap;

    #[derive(Default)]
    struct Log {
        started: Vec<BackendKind>,
        stopped: Vec<BackendKind>,
        suspended: Vec<bool>,
        sinks: Vec<Arc<dyn HotkeySink>>,
        dead: bool,
    }

    struct Fake {
        kind: BackendKind,
        result: Result<BackendReport, BackendError>,
        log: Arc<Mutex<Log>>,
    }

    impl HotkeyBackend for Fake {
        fn kind(&self) -> BackendKind {
            self.kind
        }
        fn start(
            &mut self,
            _: &StartRequest,
            sink: Arc<dyn HotkeySink>,
        ) -> Result<BackendReport, BackendError> {
            let mut l = self.log.lock().unwrap();
            l.started.push(self.kind);
            l.sinks.push(sink);
            self.result.clone()
        }
        fn stop(&mut self) {
            self.log.lock().unwrap().stopped.push(self.kind);
        }
        fn set_suspended(&mut self, s: bool) {
            self.log.lock().unwrap().suspended.push(s);
        }
        fn is_alive(&self) -> bool {
            !self.log.lock().unwrap().dead
        }
    }

    struct FakeFactory {
        results: HashMap<BackendKind, Result<BackendReport, BackendError>>,
        log: Arc<Mutex<Log>>,
    }

    impl BackendFactory for FakeFactory {
        fn create(&self, kind: BackendKind, _: &HotkeyOptions) -> Option<Box<dyn HotkeyBackend>> {
            let result = self.results.get(&kind)?.clone();
            Some(Box::new(Fake {
                kind,
                result,
                log: self.log.clone(),
            }))
        }
    }

    #[derive(Default)]
    struct Collect(Mutex<Vec<String>>);
    impl HotkeySink for Collect {
        fn hotkey(&self, e: HotkeyEvent) {
            self.0.lock().unwrap().push(format!("{e:?}"));
        }
        fn raw_key(&self, e: RawKeyEvent) {
            self.0.lock().unwrap().push(format!("raw{}", e.code));
        }
    }

    fn ok(trigger: &str) -> Result<BackendReport, BackendError> {
        Ok(BackendReport {
            supports_release: true,
            supports_modifier_only: false,
            trigger_description: trigger.into(),
            provides_hotkey_events: true,
            provides_raw_keys: false,
            note: None,
        })
    }

    fn err(kind: BackendErrorKind, m: &str) -> Result<BackendReport, BackendError> {
        Err(BackendError::new(kind, m))
    }

    fn manager(
        env: MockEnv,
        opts: HotkeyOptions,
        results: Vec<(BackendKind, Result<BackendReport, BackendError>)>,
    ) -> (HotkeyManager, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let f = FakeFactory {
            results: results.into_iter().collect(),
            log: log.clone(),
        };
        (
            HotkeyManager::with_factory(detect(&env), opts, Box::new(f)),
            log,
        )
    }

    fn spec(t: &str) -> HotkeySpec {
        HotkeySpec::parse(t).unwrap()
    }

    #[test]
    fn a_native_conflict_is_reported_and_never_hidden_by_the_portal() {
        // Under KDE, falling back to the portal would silently make the desktop assign ANOTHER key.
        let (m, log) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![
                (
                    BackendKind::KGlobalAccel,
                    err(
                        BackendErrorKind::Conflict,
                        "Ctrl+Alt+Space is already used by Claude",
                    ),
                ),
                (BackendKind::Portal, ok("Ctrl+Num++")),
            ],
        );
        let st = m.start(
            &spec("Ctrl+Alt+Space"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert!(!st.ok);
        assert!(st.message.contains("Claude"), "{}", st.message);
        assert_eq!(log.lock().unwrap().started, vec![BackendKind::KGlobalAccel]);
    }

    #[test]
    fn a_native_backend_that_is_missing_falls_back_to_the_portal() {
        let (m, log) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![
                (
                    BackendKind::KGlobalAccel,
                    err(BackendErrorKind::Unavailable, "service absent"),
                ),
                (BackendKind::Portal, ok("Ctrl+Alt+Space")),
            ],
        );
        let st = m.start(
            &spec("Ctrl+Alt+Space"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert!(st.ok);
        assert_eq!(st.backend, BackendKind::Portal);
        assert_eq!(
            log.lock().unwrap().started,
            vec![BackendKind::KGlobalAccel, BackendKind::Portal]
        );
    }

    #[test]
    fn a_dead_backend_is_reported_instead_of_staying_ok() {
        // Regression: if the portal streams end (service restarted), `ok` stayed true:
        // dead shortcut without a single message.
        let (m, log) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![(BackendKind::Portal, ok("Meta+Space"))],
        );
        let st = m.start(
            &spec("Ctrl+Alt+Space"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert!(st.ok);
        assert!(m.status().ok);
        log.lock().unwrap().dead = true;
        let after = m.status();
        assert!(!after.ok);
        assert!(!after.message.is_empty());
    }

    #[test]
    fn portail_choisi_sur_kde() {
        let (m, log) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![(BackendKind::Portal, ok("Meta+Space"))],
        );
        let st = m.start(
            &spec("Ctrl+Alt+Space"),
            ShortcutMode::Ptt,
            Arc::new(Collect::default()),
        );
        assert!(st.ok && st.backend == BackendKind::Portal && st.supports_release);
        assert_eq!(
            st.trigger_description, "Meta+Space",
            "the portal decides the trigger"
        );
        assert_eq!(log.lock().unwrap().started, vec![BackendKind::Portal]);
        assert_eq!(m.status(), st);
    }

    #[test]
    fn repli_sur_evdev_si_portail_refuse() {
        let opts = HotkeyOptions {
            allow_evdev: true,
            ..Default::default()
        };
        let (m, log) = manager(
            MockEnv::kde(),
            opts,
            vec![
                (
                    BackendKind::Portal,
                    err(BackendErrorKind::Denied, "An app id is required."),
                ),
                (BackendKind::Evdev, ok("")),
            ],
        );
        let st = m.start(
            &spec("F9"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert_eq!(st.backend, BackendKind::Evdev);
        assert_eq!(st.trigger_description, "F9");
        assert_eq!(
            log.lock().unwrap().started,
            vec![BackendKind::Portal, BackendKind::Evdev]
        );
    }

    #[test]
    fn portail_refuse_sans_evdev_donne_aucun_backend() {
        let (m, _) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![
                (
                    BackendKind::Portal,
                    err(BackendErrorKind::Denied, "Refused."),
                ),
                (BackendKind::Evdev, ok("")),
            ],
        );
        let st = m.start(
            &spec("F9"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert!(!st.ok && st.backend == BackendKind::None);
        assert!(st.message.contains("Refused."));
    }

    #[test]
    fn x11_conflit_badaccess() {
        let (m, _) = manager(
            MockEnv::x11(),
            HotkeyOptions::default(),
            vec![(
                BackendKind::X11,
                err(BackendErrorKind::Conflict, "Key already in use."),
            )],
        );
        let st = m.start(
            &spec("Ctrl+Alt+Space"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert!(!st.ok);
        assert!(st.message.contains("already in use"));
    }

    #[test]
    fn aucun_backend_explique_quoi_faire() {
        let (m, log) = manager(MockEnv::empty(), HotkeyOptions::default(), vec![]);
        let st = m.start(
            &spec("F9"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert!(!st.ok && st.backend == BackendKind::None && !st.message.is_empty());
        assert!(log.lock().unwrap().started.is_empty());
    }

    #[test]
    fn modificateurs_seuls_wayland_sans_evdev() {
        let (m, _) = manager(
            MockEnv::gnome(),
            HotkeyOptions::default(),
            vec![(BackendKind::Portal, ok(""))],
        );
        let st = m.start(
            &spec("Ctrl+Super"),
            ShortcutMode::Ptt,
            Arc::new(Collect::default()),
        );
        assert!(!st.ok);
        assert!(st.message.contains("modifier"));
    }

    #[test]
    fn raccourci_invalide() {
        let (m, _) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![(BackendKind::Portal, ok(""))],
        );
        let bad = HotkeySpec {
            modifiers: Default::default(),
            key: None,
        };
        assert!(
            !m.start(&bad, ShortcutMode::Toggle, Arc::new(Collect::default()))
                .ok
        );
    }

    #[test]
    fn suspension_filtre_les_evenements_et_previent_le_backend() {
        let (m, log) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![(BackendKind::Portal, ok(""))],
        );
        let sink = Arc::new(Collect::default());
        m.start(&spec("F9"), ShortcutMode::Toggle, sink.clone());
        let backend_sink = log.lock().unwrap().sinks[0].clone();
        backend_sink.hotkey(HotkeyEvent::Activated { ts_ms: 1 });
        m.suspend(true);
        assert!(m.is_suspended());
        backend_sink.hotkey(HotkeyEvent::Deactivated { ts_ms: 2 });
        backend_sink.raw_key(RawKeyEvent {
            code: 1,
            down: true,
            ts_ms: 3,
        });
        m.suspend(false);
        backend_sink.raw_key(RawKeyEvent {
            code: 2,
            down: true,
            ts_ms: 4,
        });
        assert_eq!(sink.0.lock().unwrap().len(), 2);
        assert_eq!(
            log.lock().unwrap().suspended,
            vec![false, true, false],
            "synchro initiale puis suspension et reprise"
        );
    }

    #[test]
    fn stop_et_redemarrage() {
        let (m, log) = manager(
            MockEnv::kde(),
            HotkeyOptions::default(),
            vec![(BackendKind::Portal, ok(""))],
        );
        m.start(
            &spec("F9"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        m.start(
            &spec("F10"),
            ShortcutMode::Toggle,
            Arc::new(Collect::default()),
        );
        assert_eq!(
            log.lock().unwrap().stopped.len(),
            1,
            "the old backend is stopped"
        );
        m.stop();
        assert_eq!(log.lock().unwrap().stopped.len(), 2);
        assert!(!m.status().ok);
        drop(m);
    }
}
