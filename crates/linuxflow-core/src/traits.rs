//! Injectable interfaces: they make it possible to test all the logic without hardware.
//! All the methods are blocking; they are called from worker threads.

use crate::domain::*;
use crate::error::{Result, SttError};

pub trait SpeechToText: Send + Sync {
    fn info(&self) -> EngineInfo;
    fn transcribe(
        &self,
        request: &TranscribeRequest,
    ) -> std::result::Result<Transcription, SttError>;
}

/// Microphone capture. `start` returns when the capture has started; `stop` returns mono 16 kHz audio.
pub trait Recorder: Send + Sync {
    fn start(&self, device: Option<&str>, max_sec: u64) -> Result<()>;
    fn stop(&self) -> Result<Recording>;
    fn cancel(&self);
    /// Receives a sound level in [0, 1] about 10 to 30 times per second.
    fn set_level_sink(&self, sink: Box<dyn Fn(f32) + Send + Sync>);
}

pub trait Clipboard: Send + Sync {
    fn get_text(&self) -> Result<Option<String>>;
    fn set_text(&self, text: &str) -> Result<()>;
}

pub trait Paster: Send + Sync {
    fn paste(&self, mode: PasteMode, delay_ms: u64) -> PasteResult;
}

pub trait ActiveWindow: Send + Sync {
    fn current(&self) -> Option<WindowInfo>;
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
    /// ISO 8601 timestamp (UTC).
    fn now_iso(&self) -> String;
    /// Local date `YYYY-MM-DD` (for "words today").
    fn today_local(&self) -> String;
}

/// Event of a global shortcut provided by a portal or X11 (press/release).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Activated { ts_ms: u64 },
    Deactivated { ts_ms: u64 },
}

/// Raw physical key (evdev / XInput2), for push-to-talk and modifiers alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawKeyEvent {
    /// Code evdev (`KEY_*`).
    pub code: u16,
    pub down: bool,
    pub ts_ms: u64,
}

pub trait HotkeySink: Send + Sync {
    fn hotkey(&self, event: HotkeyEvent);
    fn raw_key(&self, event: RawKeyEvent);
}

/// Commands that the interface can emit.
/// Implemented by the `linuxflow` binary (application service). All the methods are blocking
/// and short; long operations (download) return immediately and push `Event`s.
pub trait Controller: Send + Sync {
    fn state(&self) -> AppState;
    /// Applies a partial JSON patch to the settings (shallow merge) and persists.
    fn set_settings(&self, patch: serde_json::Value) -> Result<()>;
    /// Validates a shortcut (`Ctrl+Alt+Space`) without registering it. `Err` = translated message.
    fn shortcut_test(&self, accelerator: &str) -> std::result::Result<(), String>;
    fn shortcut_suspend(&self, suspended: bool);

    fn dictionary_add(&self, word: &str, sounds: &str) -> Result<()>;
    fn dictionary_update(&self, previous_word: &str, word: &str, sounds: &str) -> Result<()>;
    fn dictionary_remove(&self, word: &str) -> Result<()>;
    fn dictionary_restore_defaults(&self) -> Result<()>;
    fn format_preview(&self, text: &str) -> String;

    fn snippet_add(&self, trigger: &str, expansion: &str) -> Result<()>;
    fn snippet_remove(&self, id: &str) -> Result<()>;
    fn history_delete(&self, id: &str) -> Result<()>;
    fn history_clear(&self) -> Result<()>;

    /// `to_hub`: the text goes into the hub's test area instead of being pasted.
    fn dictation_toggle(&self, to_hub: bool) -> Phase;
    fn dictation_cancel(&self) -> Phase;
    fn copy_text(&self, text: &str) -> Result<()>;
    /// Opens a known folder (logs, data). Never an arbitrary path.
    fn open_logs_dir(&self);
    fn open_data_dir(&self);
    fn list_microphones(&self) -> Vec<MicrophoneInfo>;
    fn capabilities(&self) -> Capabilities;

    fn models(&self) -> Vec<ModelStatus>;
    fn model_download(&self, id: &str) -> Result<()>;
    fn model_pause(&self, id: &str) -> Result<()>;
    fn model_resume(&self, id: &str) -> Result<()>;
    fn model_cancel(&self, id: &str) -> Result<()>;
    fn model_delete(&self, id: &str) -> Result<()>;
    fn model_verify(&self, id: &str) -> Result<()>;
    fn model_use(&self, id: &str) -> Result<()>;
    fn model_use_local_file(&self, path: &std::path::Path) -> Result<()>;
    /// Finishes the first-launch wizard.
    fn complete_first_run(&self, shortcut: &str, model_id: Option<&str>) -> Result<()>;

    /// Effective models folder (overrides resolved). Empty by default = standard location.
    fn models_dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::new()
    }
    /// Opens the models folder (allow-list). By default: no effect.
    fn open_models_dir(&self) {}
    /// Free disk space at the models location, if known.
    fn free_disk_bytes(&self) -> Option<u64> {
        None
    }
    /// Restarts the transcription engine (tray menu, settings page). By default: no effect.
    fn restart_engine(&self) {}

    fn quit(&self);
}
