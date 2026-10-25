//! Monotonic clock shared by all the backends (ms since the first call).

use std::sync::OnceLock;
use std::time::Instant;

pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}
