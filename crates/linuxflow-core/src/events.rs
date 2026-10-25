//! Messages pushed from the core to the interface.

use crate::domain::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    History,
    Dictionary,
    Snippets,
    Settings,
    Models,
}

/// State of the pill.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayState {
    pub phase: Phase,
    /// Error message or custom label.
    pub message: Option<String>,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The complete snapshot changed.
    State(Box<AppState>),
    Phase {
        phase: Phase,
        message: Option<String>,
    },
    Overlay(OverlayState),
    /// Microphone level in [0, 1].
    Level(f32),
    /// Text to insert in the hub's test area (dictation started from the hub).
    InsertText(String),
    Navigate(Page),
    Toast(String),
}
