//! Wait for the physical release of the modifiers (under Wayland) and injectable timing.

use std::time::Duration;

use super::plan::Key;

/// Physical key state, provided by the caller (passive evdev). Absent: fallback delay.
pub trait ModifierState: Send + Sync {
    /// Keys currently held (at least the modifiers and the space).
    fn held(&self) -> Vec<Key>;
}

/// Injectable timing to test without really waiting.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration);
}

/// Real implementation: `std::thread::sleep`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Bounds of the fallback delay (without physical state): 150 to 300 ms.
pub const FALLBACK_MIN_MS: u64 = 150;
pub const FALLBACK_MAX_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleasePolicy {
    /// No polling of the physical state.
    pub poll_ms: u64,
    /// Fallback delay when the physical state is unknown (bounded to 150-300 ms).
    pub fallback_ms: u64,
    /// Wait ceiling when the physical state is known.
    pub cap_ms: u64,
}

impl Default for ReleasePolicy {
    fn default() -> Self {
        Self {
            poll_ms: 10,
            fallback_ms: 200,
            cap_ms: 1000,
        }
    }
}

impl ReleasePolicy {
    pub fn fallback_clamped(&self) -> u64 {
        self.fallback_ms.clamp(FALLBACK_MIN_MS, FALLBACK_MAX_MS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// No key held any more (or already the case).
    Released,
    /// Ceiling reached with keys still held.
    TimedOut,
    /// Physical state unknown: only the fallback delay was waited.
    Fallback,
}

/// Waits until the dictation shortcut is physically released.
pub fn wait_physical_release(
    state: Option<&dyn ModifierState>,
    policy: &ReleasePolicy,
    sleeper: &dyn Sleeper,
) -> ReleaseOutcome {
    let Some(state) = state else {
        sleeper.sleep(Duration::from_millis(policy.fallback_clamped()));
        return ReleaseOutcome::Fallback;
    };
    let poll = policy.poll_ms.max(1);
    let mut waited = 0u64;
    loop {
        let held = state.held();
        if !held.iter().any(|k| Key::RELEASABLE.contains(k)) {
            return ReleaseOutcome::Released;
        }
        if waited >= policy.cap_ms {
            return ReleaseOutcome::TimedOut;
        }
        sleeper.sleep(Duration::from_millis(poll));
        waited += poll;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeSleeper {
        pub total_ms: AtomicU64,
    }
    impl Sleeper for FakeSleeper {
        fn sleep(&self, d: Duration) {
            self.total_ms
                .fetch_add(d.as_millis() as u64, Ordering::SeqCst);
        }
    }

    /// Holds keys for `n` polls then releases them.
    struct Scripted(Mutex<Vec<Vec<Key>>>);
    impl ModifierState for Scripted {
        fn held(&self) -> Vec<Key> {
            let mut script = self.0.lock().unwrap();
            if script.is_empty() {
                Vec::new()
            } else {
                script.remove(0)
            }
        }
    }

    #[test]
    fn unknown_state_uses_clamped_fallback() {
        let s = FakeSleeper::default();
        let out = wait_physical_release(None, &ReleasePolicy::default(), &s);
        assert_eq!(out, ReleaseOutcome::Fallback);
        assert_eq!(s.total_ms.load(Ordering::SeqCst), 200);

        let s = FakeSleeper::default();
        let low = ReleasePolicy {
            fallback_ms: 5,
            ..Default::default()
        };
        wait_physical_release(None, &low, &s);
        assert_eq!(s.total_ms.load(Ordering::SeqCst), FALLBACK_MIN_MS);

        let s = FakeSleeper::default();
        let high = ReleasePolicy {
            fallback_ms: 5000,
            ..Default::default()
        };
        wait_physical_release(None, &high, &s);
        assert_eq!(s.total_ms.load(Ordering::SeqCst), FALLBACK_MAX_MS);
    }

    #[test]
    fn already_released_returns_immediately() {
        let s = FakeSleeper::default();
        let st = Scripted(Mutex::new(vec![]));
        assert_eq!(
            wait_physical_release(Some(&st), &ReleasePolicy::default(), &s),
            ReleaseOutcome::Released
        );
        assert_eq!(s.total_ms.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn waits_until_physical_release() {
        let s = FakeSleeper::default();
        let st = Scripted(Mutex::new(vec![
            vec![Key::LeftCtrl, Key::LeftAlt],
            vec![Key::LeftAlt],
            vec![Key::Space],
        ]));
        assert_eq!(
            wait_physical_release(Some(&st), &ReleasePolicy::default(), &s),
            ReleaseOutcome::Released
        );
        assert_eq!(s.total_ms.load(Ordering::SeqCst), 30);
    }

    #[test]
    fn stops_at_cap_when_keys_stay_held() {
        struct Stuck;
        impl ModifierState for Stuck {
            fn held(&self) -> Vec<Key> {
                vec![Key::LeftShift]
            }
        }
        let s = FakeSleeper::default();
        let policy = ReleasePolicy {
            cap_ms: 100,
            ..Default::default()
        };
        assert_eq!(
            wait_physical_release(Some(&Stuck), &policy, &s),
            ReleaseOutcome::TimedOut
        );
        assert_eq!(s.total_ms.load(Ordering::SeqCst), 100);
    }

    #[test]
    fn non_releasable_keys_do_not_block() {
        struct OnlyV;
        impl ModifierState for OnlyV {
            fn held(&self) -> Vec<Key> {
                vec![Key::V, Key::Insert]
            }
        }
        let s = FakeSleeper::default();
        assert_eq!(
            wait_physical_release(Some(&OnlyV), &ReleasePolicy::default(), &s),
            ReleaseOutcome::Released
        );
    }
}
