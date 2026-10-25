//! Erreurs communes.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON invalide : {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Stt(#[from] SttError),
}

impl Error {
    pub fn msg(text: impl Into<String>) -> Self {
        Error::Message(text.into())
    }
}

/// Transcription engine errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SttError {
    /// The engine is not ready yet (model missing, downloading or loading).
    #[error("the transcription engine is not ready")]
    NotReady,
    /// No usable model is installed.
    #[error("no model installed")]
    NoModel,
    /// Transcription failure (message meant for the log, not for the end user).
    #[error("transcription failure: {0}")]
    Failed(String),
}
