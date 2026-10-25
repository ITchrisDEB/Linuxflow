//! linuxflow-audio: microphone capture (cpal), mono, 16 kHz resampling (rubato), sound level.
//!
//! - [`CpalRecorder`] implements `linuxflow_core::traits::Recorder`.
//! - [`SyntheticSource`] replaces the microphone to test the whole pipeline without hardware
//!   (`CpalRecorder::with_source`).
//! - [`list_microphones`] enumerates the inputs (the `default` entry is always first).

pub mod capture;
pub mod convert;
pub mod cpal_source;
pub mod level;
pub mod microphones;
pub mod recorder;
pub mod resample;
pub mod synthetic;

pub use capture::{AudioSource, Severity};
pub use cpal_source::{list_microphones, CpalSource};
pub use level::{level_from_block, level_from_rms, LEVEL_BLOCK_SAMPLES};
pub use recorder::CpalRecorder;
pub use synthetic::{SyntheticFormat, SyntheticSource};
