//! Visible texts produced by the orchestration (pill and hub messages).
//! All of them go through `tr()` with an English literal.

use linuxflow_core::dictation::UserMessage;
use linuxflow_i18n::{tr, tr_fmt};

/// Translates a [`UserMessage`] of the dictation state machine.
pub fn user_message(message: &UserMessage) -> String {
    match message {
        UserMessage::ModelDownloading { percent: Some(p) } => {
            tr_fmt("Model downloading · {}%", &[&p.to_string()])
        }
        UserMessage::ModelDownloading { percent: None } => tr("Model downloading…"),
        UserMessage::ModelLoading => tr("Model is loading…"),
        UserMessage::EngineUnavailable => tr("Transcription engine unavailable"),
        UserMessage::NothingHeard => tr("Nothing heard"),
        UserMessage::MicUnavailable { detail: Some(d) } => {
            tr_fmt("Microphone unavailable ({})", &[d])
        }
        UserMessage::MicUnavailable { detail: None } => tr("Microphone unavailable"),
        UserMessage::PasteBlocked => tr("Paste blocked — text copied to the clipboard"),
        UserMessage::PipelineError { message } => tr_fmt("Dictation failed: {}", &[message]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_source_texts() {
        assert_eq!(
            user_message(&UserMessage::ModelDownloading { percent: Some(42) }),
            "Model downloading · 42%"
        );
        assert_eq!(user_message(&UserMessage::NothingHeard), "Nothing heard");
        assert_eq!(
            user_message(&UserMessage::MicUnavailable {
                detail: Some("busy".into())
            }),
            "Microphone unavailable (busy)"
        );
    }
}
