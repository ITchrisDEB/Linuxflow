//! Model manager errors.
//!
//! `Display` is technical (logs). `user_message()` gives the translated, actionable text
//! meant for the user.

use linuxflow_i18n::{tr, tr_fmt};

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("offline mode: no network request allowed")]
    Offline,
    #[error("unknown model {0:?}")]
    UnknownModel(String),
    #[error("another download of this file is already running")]
    AlreadyDownloading,
    #[error("not enough disk space: {needed} bytes needed, {available} available")]
    InsufficientSpace { needed: u64, available: u64 },
    #[error("download cancelled")]
    Cancelled,
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("HTTP status {0}")]
    Http(u16),
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected server response: {0}")]
    Protocol(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid file: {0}")]
    InvalidFile(String),
    #[error("model not installed")]
    NotInstalled,
    #[error("model is busy (download running or in use)")]
    Busy,
}

pub type Result<T> = std::result::Result<T, ModelError>;

/// Formats a size in bytes readably (decimal units).
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

impl ModelError {
    /// Translated message for the user, with the steps to follow.
    pub fn user_message(&self) -> String {
        match self {
            ModelError::Offline => tr("Offline mode is on: nothing can be downloaded. Choose a local model file or turn offline mode off."),
            ModelError::UnknownModel(_) => tr("This model is not in the catalog."),
            ModelError::AlreadyDownloading => tr("This model is already being downloaded."),
            ModelError::InsufficientSpace { needed, available } => tr_fmt(
                "Not enough disk space: {} needed, {} available. Free some space or pick a lighter model.",
                &[&format_bytes(*needed), &format_bytes(*available)],
            ),
            ModelError::Cancelled => tr("Download cancelled."),
            ModelError::ChecksumMismatch { .. } => tr("The downloaded file is corrupted (checksum mismatch). Try again or change the download source."),
            ModelError::Http(403) | ModelError::Http(404) | ModelError::Http(401) => tr_fmt(
                "The server refused or does not have this file (HTTP {}). Check the download source.",
                &[&self.http_code()],
            ),
            ModelError::Http(_) => tr_fmt("The server answered with an error (HTTP {}). Try again later.", &[&self.http_code()]),
            ModelError::Network(_) | ModelError::Protocol(_) => tr("Network problem. Check your connection (and proxy settings) and try again."),
            ModelError::Io(e) => match e.kind() {
                std::io::ErrorKind::PermissionDenied => tr("Permission denied in the models folder. Choose another folder."),
                std::io::ErrorKind::StorageFull => tr("The disk is full. Free some space and try again."),
                _ => tr("A file error occurred in the models folder."),
            },
            ModelError::InvalidFile(_) => tr("This file cannot be used as a model: it does not match the expected one."),
            ModelError::NotInstalled => tr("This model is not installed."),
            ModelError::Busy => tr("This model is busy. Stop the download or switch model first."),
        }
    }

    fn http_code(&self) -> String {
        match self {
            ModelError::Http(c) => c.to_string(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_readable() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(487_601_967), "487.6 MB");
        assert_eq!(format_bytes(3_095_033_483), "3.1 GB");
    }

    #[test]
    fn user_messages_are_actionable() {
        let m = ModelError::InsufficientSpace {
            needed: 2_000_000_000,
            available: 1_000_000,
        }
        .user_message();
        assert!(m.contains("2.0 GB") && m.contains("1.0 MB"), "{m}");
        let m = ModelError::Http(404).user_message();
        assert!(m.contains("404"), "{m}");
    }
}
