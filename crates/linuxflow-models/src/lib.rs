//! linuxflow-models: catalogue, recommendation and download of the models.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod catalog;
pub mod config;
pub mod error;
pub mod manager;
pub mod recommend;
mod sys;

pub use catalog::{Catalog, Languages, ModelEntry, ModelKind, Usage};
pub use config::Config;
pub use error::{ModelError, Result};
pub use manager::{
    FreeSpaceFn, Integrity, ModelManager, ProgressCallback, ProgressPhase, ProgressUpdate,
};
pub use recommend::{recommend, recommend_in, HardwareSummary, Reason, Recommendation};
