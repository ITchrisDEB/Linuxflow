//! Clean shutdown on SIGINT / SIGTERM: the handler sets a flag, the GTK loop reads it.

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signum: libc::c_int) {
    // The only safe operation in a signal handler: an atomic store.
    REQUESTED.store(true, Ordering::SeqCst);
}

/// Installs the handlers (to be called once).
pub fn install() {
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: `on_signal` is an `extern "C" fn(c_int)` that only does an atomic store
        // (async-signal-safe); `signal` is called with a valid signal number.
        unsafe {
            libc::signal(
                sig,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
}

/// True (only once) if a shutdown signal was received.
pub fn take_requested() -> bool {
    REQUESTED.swap(false, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_is_consumed_once() {
        assert!(!take_requested());
        on_signal(libc::SIGTERM);
        assert!(take_requested());
        assert!(!take_requested());
    }
}
