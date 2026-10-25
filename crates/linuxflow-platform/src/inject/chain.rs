//! Execution of the paste: choice of the first usable backend, release of the modifiers,
//! delay, then the combination as a block. The backends are behind a trait (tested with fakes).

use std::sync::Arc;
use std::time::Duration;

use linuxflow_core::domain::{PasteMode, PasteResult};
use linuxflow_core::traits::Paster;

use super::backend::{Delivery, InjectError, KeyInjector};
use super::modifiers::{
    wait_physical_release, ModifierState, ReleasePolicy, Sleeper, ThreadSleeper,
};
use super::plan::{plan_paste, KeyEvent, PastePlan};

/// Pause between the forced release (X11) and the combination (30 ms).
pub const RELEASE_SETTLE_MS: u64 = 30;

pub struct PasteChain {
    backends: Vec<Box<dyn KeyInjector>>,
    modifiers: Option<Arc<dyn ModifierState>>,
    sleeper: Arc<dyn Sleeper>,
    policy: ReleasePolicy,
}

impl PasteChain {
    pub fn new(backends: Vec<Box<dyn KeyInjector>>) -> Self {
        Self {
            backends,
            modifiers: None,
            sleeper: Arc::new(ThreadSleeper),
            policy: ReleasePolicy::default(),
        }
    }

    /// Physical keyboard state (evdev) provided by the caller, for Wayland sessions.
    pub fn with_modifier_state(mut self, state: Arc<dyn ModifierState>) -> Self {
        self.modifiers = Some(state);
        self
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    pub fn with_policy(mut self, policy: ReleasePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Names of the backends of the chain, in order of attempt.
    pub fn backend_names(&self) -> Vec<&'static str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    fn sleep_ms(&self, ms: u64) {
        if ms > 0 {
            self.sleeper.sleep(Duration::from_millis(ms));
        }
    }

    fn run(&self, mode: PasteMode, delay_ms: u64) -> PasteResult {
        let mut waited_physical = false;
        let mut delayed = false;
        let mut failures: Vec<String> = Vec::new();

        for backend in &self.backends {
            if let Err(err) = backend.probe() {
                tracing::debug!("paste: {} ignored ({err})", backend.name());
                failures.push(format!("{} {err}", backend.name()));
                continue;
            }

            // Release of the modifiers: real state (X11) otherwise wait for the physical release.
            let mut release = Vec::new();
            match backend.held_keys() {
                Some(held) => {
                    release = plan_paste(mode, &held, delay_ms).release;
                }
                None if !waited_physical => {
                    waited_physical = true;
                    let outcome = wait_physical_release(
                        self.modifiers.as_deref(),
                        &self.policy,
                        &*self.sleeper,
                    );
                    tracing::debug!("paste: waiting for the modifiers -> {outcome:?}");
                }
                None => {}
            }
            if !delayed {
                delayed = true;
                self.sleep_ms(delay_ms);
            }
            if !release.is_empty() {
                match backend.send_release(&release) {
                    Ok(()) => self.sleep_ms(RELEASE_SETTLE_MS),
                    Err(InjectError::Blocked(m)) => return blocked(backend.name(), &m),
                    Err(InjectError::TimedOut(m)) => return timed_out(backend.name(), &m),
                    Err(err) => {
                        failures.push(format!("{} {err}", backend.name()));
                        continue;
                    }
                }
            }

            let PastePlan { chord, .. } = plan_paste(mode, &[], delay_ms);
            match backend.send_chord(mode, &chord) {
                Ok(Delivery::Sent) => return PasteResult::Pasted,
                Ok(Delivery::Unconfirmed) => return PasteResult::Unknown,
                Err(InjectError::Blocked(m)) => return blocked(backend.name(), &m),
                Err(InjectError::TimedOut(m)) => return timed_out(backend.name(), &m),
                Err(err) => {
                    tracing::debug!("paste: {} failed ({err})", backend.name());
                    // The combination may have been sent halfway: everything it presses is released, otherwise a
                    // Ctrl could stay stuck in the session (best effort, errors ignored).
                    let release = release_events(&chord);
                    if !release.is_empty() {
                        let _ = backend.send_release(&release);
                    }
                    failures.push(format!("{} {err}", backend.name()));
                }
            }
        }

        if failures.is_empty() {
            PasteResult::Failed("no input backend available".to_string())
        } else {
            PasteResult::Failed(failures.join("; "))
        }
    }
}

/// "Released" events of all the keys that a combination presses, in reverse order.
fn release_events(chord: &[KeyEvent]) -> Vec<KeyEvent> {
    chord
        .iter()
        .rev()
        .filter(|e| e.down)
        .map(|e| KeyEvent::up(e.key))
        .collect()
}

/// Timeout while sending: it is unknown whether the paste happened. We DO NOT REPLAY on a
/// another backend (possible double paste): the text stays in the clipboard.
fn timed_out(name: &str, message: &str) -> PasteResult {
    tracing::warn!("paste: timeout with {name} ({message}): unknown result, no new attempt");
    PasteResult::Unknown
}

fn blocked(name: &str, message: &str) -> PasteResult {
    tracing::info!("paste blocked by {name}: {message}");
    PasteResult::Blocked
}

impl Paster for PasteChain {
    fn paste(&self, mode: PasteMode, delay_ms: u64) -> PasteResult {
        self.run(mode, delay_ms)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::inject::plan::{chord_events, Key};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Log(Mutex<Vec<String>>);
    impl Log {
        fn push(&self, s: impl Into<String>) {
            self.0.lock().unwrap().push(s.into());
        }
        fn take(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    struct FakeSleeper {
        log: Arc<Log>,
        total: AtomicU64,
    }
    impl Sleeper for FakeSleeper {
        fn sleep(&self, d: Duration) {
            self.total.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
            self.log.push(format!("sleep {}", d.as_millis()));
        }
    }

    struct Fake {
        name: &'static str,
        probe: Result<(), InjectError>,
        held: Option<Vec<Key>>,
        chord: Result<Delivery, InjectError>,
        log: Arc<Log>,
    }
    impl KeyInjector for Fake {
        fn name(&self) -> &'static str {
            self.name
        }
        fn probe(&self) -> Result<(), InjectError> {
            self.probe.clone()
        }
        fn held_keys(&self) -> Option<Vec<Key>> {
            self.held.clone()
        }
        fn send_release(&self, events: &[KeyEvent]) -> Result<(), InjectError> {
            self.log
                .push(format!("{}:release {}", self.name, events.len()));
            Ok(())
        }
        fn send_chord(
            &self,
            mode: PasteMode,
            events: &[KeyEvent],
        ) -> Result<Delivery, InjectError> {
            assert_eq!(events, chord_events(mode).as_slice());
            self.log.push(format!("{}:chord {mode:?}", self.name));
            self.chord.clone()
        }
    }

    fn fake(name: &'static str, log: &Arc<Log>, chord: Result<Delivery, InjectError>) -> Box<Fake> {
        Box::new(Fake {
            name,
            probe: Ok(()),
            held: None,
            chord,
            log: log.clone(),
        })
    }

    fn chain(backends: Vec<Box<dyn KeyInjector>>, log: &Arc<Log>) -> PasteChain {
        PasteChain::new(backends).with_sleeper(Arc::new(FakeSleeper {
            log: log.clone(),
            total: AtomicU64::new(0),
        }))
    }

    #[test]
    fn send_timeout_is_unknown_and_never_replayed_on_another_backend() {
        // Regression: after a simple send timeout, the chain moved on to the next backend: risk of
        // DOUBLE paste and a stuck modifier. Now: unknown result, a single attempt.
        let log = Arc::new(Log::default());
        let c = chain(
            vec![
                fake("portal", &log, Err(InjectError::TimedOut("slow".into()))),
                fake("uinput", &log, Ok(Delivery::Sent)),
            ],
            &log,
        );
        assert_eq!(c.paste(PasteMode::CtrlV, 0), PasteResult::Unknown);
        let calls = log.take();
        assert!(calls.iter().any(|l| l == "portal:chord CtrlV"), "{calls:?}");
        assert!(
            !calls.iter().any(|l| l.starts_with("uinput:")),
            "the next backend must not be tried: {calls:?}"
        );
    }

    #[test]
    fn a_failed_chord_releases_the_keys_it_pressed() {
        let log = Arc::new(Log::default());
        let c = chain(
            vec![fake(
                "a",
                &log,
                Err(InjectError::Failed("half sent".into())),
            )],
            &log,
        );
        assert!(matches!(
            c.paste(PasteMode::CtrlV, 0),
            PasteResult::Failed(_)
        ));
        let calls = log.take();
        // Ctrl and V were pressed: both are released after the failure.
        assert!(calls.iter().any(|l| l == "a:release 2"), "{calls:?}");
    }

    #[test]
    fn a_plain_failure_still_falls_through_to_the_next_backend() {
        let log = Arc::new(Log::default());
        let c = chain(
            vec![
                fake("a", &log, Err(InjectError::Failed("boom".into()))),
                fake("b", &log, Ok(Delivery::Sent)),
            ],
            &log,
        );
        assert_eq!(c.paste(PasteMode::CtrlV, 0), PasteResult::Pasted);
    }

    #[test]
    fn pasted_after_fallback_wait_then_delay_then_chord() {
        let log = Arc::new(Log::default());
        let c = chain(vec![fake("a", &log, Ok(Delivery::Sent))], &log);
        assert_eq!(c.paste(PasteMode::CtrlV, 120), PasteResult::Pasted);
        assert_eq!(log.take(), vec!["sleep 200", "sleep 120", "a:chord CtrlV"]);
    }

    #[test]
    fn x11_style_backend_releases_real_held_keys_without_waiting() {
        let log = Arc::new(Log::default());
        let mut b = fake("xtest", &log, Ok(Delivery::Sent));
        b.held = Some(vec![Key::LeftCtrl, Key::Space]);
        let c = chain(vec![b], &log);
        assert_eq!(c.paste(PasteMode::CtrlShiftV, 60), PasteResult::Pasted);
        assert_eq!(
            log.take(),
            vec![
                "sleep 60",
                "xtest:release 2",
                "sleep 30",
                "xtest:chord CtrlShiftV"
            ]
        );
    }

    #[test]
    fn x11_nothing_held_sends_no_release() {
        let log = Arc::new(Log::default());
        let mut b = fake("xtest", &log, Ok(Delivery::Sent));
        b.held = Some(vec![]);
        let c = chain(vec![b], &log);
        assert_eq!(c.paste(PasteMode::CtrlV, 0), PasteResult::Pasted);
        assert_eq!(log.take(), vec!["xtest:chord CtrlV"]);
    }

    #[test]
    fn unavailable_backend_falls_through_to_next() {
        let log = Arc::new(Log::default());
        let mut first = fake("a", &log, Ok(Delivery::Sent));
        first.probe = Err(InjectError::Unavailable("no".into()));
        let c = chain(vec![first, fake("b", &log, Ok(Delivery::Sent))], &log);
        assert_eq!(c.paste(PasteMode::ShiftInsert, 0), PasteResult::Pasted);
        assert_eq!(log.take(), vec!["sleep 200", "b:chord ShiftInsert"]);
    }

    #[test]
    fn failed_backend_falls_through_and_delay_not_repeated() {
        let log = Arc::new(Log::default());
        let c = chain(
            vec![
                fake("a", &log, Err(InjectError::Failed("boom".into()))),
                fake("b", &log, Ok(Delivery::Sent)),
            ],
            &log,
        );
        assert_eq!(c.paste(PasteMode::CtrlV, 100), PasteResult::Pasted);
        assert_eq!(
            log.take(),
            // `a` failed: its keys are released before trying `b`.
            vec![
                "sleep 200",
                "sleep 100",
                "a:chord CtrlV",
                "a:release 2",
                "b:chord CtrlV"
            ]
        );
    }

    #[test]
    fn blocked_stops_the_chain() {
        let log = Arc::new(Log::default());
        let c = chain(
            vec![
                fake("portal", &log, Err(InjectError::Blocked("denied".into()))),
                fake("uinput", &log, Ok(Delivery::Sent)),
            ],
            &log,
        );
        assert_eq!(c.paste(PasteMode::CtrlV, 0), PasteResult::Blocked);
        assert!(!log.take().iter().any(|l| l.starts_with("uinput")));
    }

    #[test]
    fn unconfirmed_delivery_is_unknown() {
        let log = Arc::new(Log::default());
        let c = chain(vec![fake("hypr", &log, Ok(Delivery::Unconfirmed))], &log);
        assert_eq!(c.paste(PasteMode::CtrlV, 0), PasteResult::Unknown);
    }

    #[test]
    fn all_failing_reports_failed_with_reasons() {
        let log = Arc::new(Log::default());
        let mut a = fake("a", &log, Ok(Delivery::Sent));
        a.probe = Err(InjectError::Unavailable("absent".into()));
        let c = chain(
            vec![a, fake("b", &log, Err(InjectError::Failed("x".into())))],
            &log,
        );
        match c.paste(PasteMode::CtrlV, 0) {
            PasteResult::Failed(msg) => {
                assert!(msg.contains("a ") && msg.contains("b "), "{msg}");
            }
            other => panic!("attendu Failed, obtenu {other:?}"),
        }
    }

    #[test]
    fn empty_chain_fails() {
        let log = Arc::new(Log::default());
        let c = chain(vec![], &log);
        assert!(matches!(
            c.paste(PasteMode::CtrlV, 0),
            PasteResult::Failed(_)
        ));
    }

    #[test]
    fn physical_state_is_waited_when_provided() {
        struct Held(Mutex<u32>);
        impl ModifierState for Held {
            fn held(&self) -> Vec<Key> {
                let mut n = self.0.lock().unwrap();
                if *n > 0 {
                    *n -= 1;
                    vec![Key::LeftCtrl]
                } else {
                    vec![]
                }
            }
        }
        let log = Arc::new(Log::default());
        let c = chain(vec![fake("a", &log, Ok(Delivery::Sent))], &log)
            .with_modifier_state(Arc::new(Held(Mutex::new(2))));
        assert_eq!(c.paste(PasteMode::CtrlV, 50), PasteResult::Pasted);
        assert_eq!(
            log.take(),
            vec!["sleep 10", "sleep 10", "sleep 50", "a:chord CtrlV"]
        );
    }
}
