//! linuxflow-core: pure LinuxFlow logic (no GTK, audio or network dependency).
//!
//! Contract shared by all the crates: `domain`, `traits`, `events`, `error`, `constants`.
//! The other modules are implemented separately.

pub mod constants;
pub mod domain;
pub mod error;
pub mod events;
pub mod traits;

pub mod accelerator;
pub mod dictation;
pub mod format;
pub mod gain;
pub mod history;
pub mod paths;
pub mod pipeline;
pub mod ptt;
pub mod settings;
pub mod stats;
pub mod store;
pub mod vocab;
pub mod wav;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use error::{Error, Result};
