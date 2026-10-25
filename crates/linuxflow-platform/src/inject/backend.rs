//! Contract of the injection backends and associated errors.

use linuxflow_core::domain::PasteMode;

use super::plan::{Key, KeyEvent};

/// Failure of a backend. The messages never contain a token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InjectError {
    /// Backend unusable here (protocol absent, no session, missing tool): try the next one.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Explicit refusal (consent refused, session revoked): do not work around it.
    #[error("blocked: {0}")]
    Blocked(String),
    /// Plain emission error, before any key is confirmed.
    #[error("failed: {0}")]
    Failed(String),
    /// Timeout DURING the emission: it is unknown whether the keys left. Never replay on
    /// another backend (risk of a double paste): the result is `Unknown`.
    #[error("timed out: {0}")]
    TimedOut(String),
}

/// What the backend knows about the delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Events accepted by the server/compositor.
    Sent,
    /// Emitted with no way of confirming acceptance.
    Unconfirmed,
}

/// A way of emitting keys. All the methods are blocking.
pub trait KeyInjector: Send + Sync {
    fn name(&self) -> &'static str;

    /// Cheap probe: `Err(Unavailable)` if the backend cannot work here.
    fn probe(&self) -> Result<(), InjectError>;

    /// Real state of the held keys, if the backend can read it (XQueryKeymap under X11).
    fn held_keys(&self) -> Option<Vec<Key>> {
        None
    }

    /// Releases held keys (no role for backends without state reading).
    fn send_release(&self, _events: &[KeyEvent]) -> Result<(), InjectError> {
        Ok(())
    }

    /// Emits the combination as a single block. `mode` is for backends that do not accept raw events.
    fn send_chord(&self, mode: PasteMode, events: &[KeyEvent]) -> Result<Delivery, InjectError>;
}
