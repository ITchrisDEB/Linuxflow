//! Control message of the pill (one JSON line per message to the child process).

use linuxflow_core::domain::Phase;
use linuxflow_core::events::{Event, OverlayState};
use serde::{Deserialize, Serialize};

/// Command sent to the pill. All the fields are optional; the absent ones
/// change nothing. `phase = idle` hides the pill.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayMsg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    /// Error message or custom label (already translated by the application).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Microphone level in [0, 1].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<f32>,
    /// Extension: `true` shows, `false` hides (without changing phase).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
}

impl OverlayMsg {
    pub fn phase(phase: Phase) -> Self {
        Self {
            phase: Some(phase),
            ..Self::default()
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            phase: Some(Phase::Error),
            message: Some(message.into()),
            ..Self::default()
        }
    }

    pub fn done(latency_ms: u64) -> Self {
        Self {
            phase: Some(Phase::Done),
            latency_ms: Some(latency_ms),
            ..Self::default()
        }
    }

    pub fn level(level: f32) -> Self {
        Self {
            level: Some(level),
            ..Self::default()
        }
    }

    pub fn hide() -> Self {
        Self {
            phase: Some(Phase::Idle),
            ..Self::default()
        }
    }

    /// Converts a core event; only `Overlay` and `Level` concern the pill.
    pub fn from_event(event: &Event) -> Option<Self> {
        match event {
            Event::Overlay(state) => Some(Self::from(state)),
            Event::Level(level) => Some(Self::level(*level)),
            _ => None,
        }
    }

    /// One JSON line (without a trailing newline).
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

impl From<&OverlayState> for OverlayMsg {
    fn from(state: &OverlayState) -> Self {
        Self {
            phase: Some(state.phase),
            message: state.message.clone(),
            latency_ms: state.latency_ms,
            level: None,
            visible: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_full_message() {
        let msg = OverlayMsg {
            phase: Some(Phase::Done),
            message: Some("ok".into()),
            latency_ms: Some(600),
            level: Some(0.5),
            visible: Some(true),
        };
        let line = msg.to_line().expect("serialisation");
        assert!(!line.contains('\n'));
        assert_eq!(OverlayMsg::from_line(&line).expect("lecture"), msg);
    }

    #[test]
    fn wire_format_is_stable() {
        assert_eq!(
            OverlayMsg::done(600).to_line().expect("ligne"),
            r#"{"phase":"done","latency_ms":600}"#
        );
        assert_eq!(
            OverlayMsg::level(0.25).to_line().expect("ligne"),
            r#"{"level":0.25}"#
        );
        assert_eq!(
            OverlayMsg::hide().to_line().expect("ligne"),
            r#"{"phase":"idle"}"#
        );
    }

    #[test]
    fn missing_and_null_fields_are_accepted() {
        let msg = OverlayMsg::from_line(r#"{"phase":"error","message":"Oups","latency_ms":null}"#)
            .expect("lecture");
        assert_eq!(msg, OverlayMsg::error("Oups"));
        assert_eq!(
            OverlayMsg::from_line("{}").expect("vide"),
            OverlayMsg::default()
        );
        // An unknown field (newer version of the parent) does not break the child.
        assert!(OverlayMsg::from_line(r#"{"phase":"listening","x":1}"#).is_ok());
        assert!(OverlayMsg::from_line("not json").is_err());
    }

    #[test]
    fn converts_core_events() {
        let state = OverlayState {
            phase: Phase::Error,
            message: Some("Nope".into()),
            latency_ms: None,
        };
        assert_eq!(
            OverlayMsg::from_event(&Event::Overlay(state)),
            Some(OverlayMsg::error("Nope"))
        );
        assert_eq!(
            OverlayMsg::from_event(&Event::Level(0.7)),
            Some(OverlayMsg::level(0.7))
        );
        assert_eq!(OverlayMsg::from_event(&Event::Toast("x".into())), None);
    }
}
