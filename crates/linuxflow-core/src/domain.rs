//! Shared domain types. The JSON keys of the settings are camelCase.

use crate::constants::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Idle,
    Listening,
    Transcribing,
    Formatting,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShortcutMode {
    #[default]
    Toggle,
    Ptt,
}

/// Colour theme of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follows the light or dark preference of the desktop (the default).
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl Theme {
    /// Offered values, in display order.
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    /// Value of the setting (`system`, `light`, `dark`).
    pub fn setting(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// Boost for a microphone that is too weak, applied at recording time before transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MicBoost {
    /// No gain.
    Off,
    /// Gain adapted to each recording (see `gain`): default setting, can be turned off.
    #[default]
    Auto,
    /// Fixed gain of +6 dB.
    Low,
    /// Fixed gain of +12 dB.
    Medium,
    /// Fixed gain of +20 dB.
    High,
}

impl MicBoost {
    /// Offered values, in display order.
    pub const ALL: [MicBoost; 5] = [
        MicBoost::Off,
        MicBoost::Auto,
        MicBoost::Low,
        MicBoost::Medium,
        MicBoost::High,
    ];

    /// Amplitude factor of the fixed gain (`None` for `Off` and `Auto`).
    pub fn fixed_factor(self) -> Option<f32> {
        match self {
            MicBoost::Low => Some(2.0),
            MicBoost::Medium => Some(4.0),
            MicBoost::High => Some(10.0),
            MicBoost::Off | MicBoost::Auto => None,
        }
    }

    /// Value of the setting (`off`, `auto`, `low`, `medium`, `high`).
    pub fn setting(self) -> &'static str {
        match self {
            MicBoost::Off => "off",
            MicBoost::Auto => "auto",
            MicBoost::Low => "low",
            MicBoost::Medium => "medium",
            MicBoost::High => "high",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub word: String,
    #[serde(default)]
    pub sounds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub id: String,
    pub trigger: String,
    pub expansion: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasteDisposition {
    Pasted,
    Hub,
    Clipboard,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    /// Horodatage ISO 8601.
    pub ts: String,
    pub text: String,
    pub raw: String,
    pub words: u32,
    pub audio_ms: u64,
    pub stt_ms: u64,
    pub latency_ms: u64,
    pub language: Option<String>,
    pub app: Option<String>,
    pub pasted: PasteDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub words_today: u32,
    pub total_words: u32,
    pub dictations: u32,
    pub avg_wpm: f64,
    pub avg_latency_ms: f64,
    pub minutes_spoken: f64,
}

/// User settings (`settings.json`). Unknown keys are kept (`extra`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub shortcut: String,
    pub mode: ShortcutMode,
    /// `auto` or Whisper language code (`fr`, `en`, ...).
    pub language: String,
    pub auto_paste: bool,
    pub launch_at_login: bool,
    pub microphone_id: String,
    /// Boost for a weak microphone (automatic by default, can be turned off in Settings).
    pub mic_boost: MicBoost,
    pub paste_delay_ms: u64,
    pub restore_clipboard_ms: u64,
    /// Models folder (override). `modelCache`: old name.
    #[serde(alias = "modelCache")]
    pub model_dir: Option<PathBuf>,
    pub max_dictation_sec: u64,
    pub show_hub_at_start: bool,
    pub vocab_seeded: bool,
    /// Identifier of the model chosen in the catalogue (None = automatic).
    pub selected_model: Option<String>,
    /// Forced inference device (None = automatic).
    pub device: Option<String>,
    pub history_enabled: bool,
    pub history_limit: usize,
    /// Interface language (None = system locale).
    pub ui_language: Option<String>,
    /// Colour theme: follows the desktop by default, or always light / always dark.
    pub theme: Theme,
    /// True once the first-launch wizard is finished.
    pub first_run_done: bool,
    /// Global shortcut active (false = suspended from the tray menu, useful when gaming).
    pub shortcut_enabled: bool,
    /// True once the "tray icon missing" help has been dismissed by the user.
    pub tray_hint_dismissed: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: DEFAULT_SHORTCUT.into(),
            mode: ShortcutMode::Toggle,
            language: "auto".into(),
            auto_paste: true,
            launch_at_login: false,
            microphone_id: "default".into(),
            mic_boost: MicBoost::Auto,
            paste_delay_ms: DEFAULT_PASTE_DELAY_MS,
            restore_clipboard_ms: DEFAULT_RESTORE_CLIPBOARD_MS,
            model_dir: None,
            max_dictation_sec: DEFAULT_MAX_DICTATION_SEC,
            show_hub_at_start: true,
            vocab_seeded: false,
            selected_model: None,
            device: None,
            history_enabled: true,
            history_limit: HISTORY_MAX,
            ui_language: None,
            theme: Theme::System,
            first_run_done: false,
            shortcut_enabled: true,
            tray_hint_dismissed: false,
            extra: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnginePhase {
    #[default]
    Starting,
    Downloading,
    Loading,
    Ready,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub model: String,
    pub done: u64,
    /// 0 if the size is unknown.
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Gpu,
    #[default]
    Cpu,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EngineInfo {
    pub phase: EnginePhase,
    pub ready: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub device: DeviceKind,
    /// Readable name of the device (e.g. GPU name), never used as an identifier.
    pub device_name: Option<String>,
    pub model: Option<String>,
    /// Quantisation / precision (e.g. `f16`, `q5_0`).
    pub precision: Option<String>,
    pub load_ms: Option<u64>,
    pub download: Option<DownloadProgress>,
}

/// Complete snapshot of the state, pushed to the views.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub version: String,
    /// First name for the greeting (optional, never written or sent).
    pub user: Option<String>,
    pub settings: Settings,
    pub dictionary: Vec<DictionaryEntry>,
    pub snippets: Vec<Snippet>,
    /// At most `HISTORY_TO_UI` entries, most recent first.
    pub history: Vec<HistoryEntry>,
    pub stats: Stats,
    pub engine: EngineInfo,
    pub ptt_available: bool,
    pub shortcut_ok: bool,
    /// Key assigned by the desktop when it chooses it itself (shortcuts portal).
    #[serde(default)]
    pub shortcut_desktop_trigger: Option<String>,
    /// Precise reason why the shortcut failed (for example "already used by Claude"), if known.
    #[serde(default)]
    pub shortcut_message: Option<String>,
    pub shortcut_suspended: bool,
    pub phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasteMode {
    CtrlV,
    CtrlShiftV,
    ShiftInsert,
}

/// Result of a paste attempt (contract specific to LinuxFlow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteResult {
    Pasted,
    Blocked,
    Failed(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowInfo {
    /// `app_id` (Wayland) or class (X11), in lower case.
    pub app_id: Option<String>,
    pub pid: Option<u32>,
    pub title: Option<String>,
}

/// Mono 16 kHz audio, f32 in [-1, 1].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Recording {
    pub samples: Vec<f32>,
}

impl Recording {
    pub fn duration_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / SAMPLE_RATE_HZ as u64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeRequest {
    pub audio: Recording,
    /// `None` = automatic detection.
    pub language: Option<String>,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transcription {
    pub text: String,
    pub language: Option<String>,
    pub stt_ms: u64,
    pub audio_ms: u64,
}

// ---- Types used by the UI <-> core contract (`traits::Controller`) ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneInfo {
    /// Stable identifier (device name); `default` = default microphone.
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    NotInstalled,
    Downloading {
        done: u64,
        total: u64,
    },
    Paused {
        done: u64,
        total: u64,
    },
    Installed,
    /// Installed but with no known checksum (warning).
    Unverified,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStatus {
    pub id: String,
    pub name: String,
    pub download_bytes: u64,
    pub disk_bytes: u64,
    /// Estimate of the memory needed (RAM or VRAM), in bytes.
    pub memory_bytes: u64,
    pub state: ModelState,
    /// Recommended for the detected machine.
    pub recommended: bool,
    /// Model currently used by the engine.
    pub in_use: bool,
}

/// Report of the capabilities detected at run time. Every field has a fallback.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// `wayland`, `x11` or `unknown`.
    pub session: String,
    pub desktop: String,
    pub portal_global_shortcuts: bool,
    pub portal_remote_desktop: bool,
    pub portal_clipboard: bool,
    pub layer_shell: bool,
    pub data_control: bool,
    pub vulkan_available: bool,
    pub vulkan_devices: Vec<String>,
    pub pipewire: bool,
    pub tray_watcher: bool,
    pub uinput_writable: bool,
    pub input_group: bool,
    pub gtk_version: String,
    pub adw_version: String,
    /// Help text per component with a real problem: (component, message to show).
    pub hints: Vec<(String, String)>,
}

impl Capabilities {
    pub fn is_wayland(&self) -> bool {
        self.session == "wayland"
    }

    pub fn is_x11(&self) -> bool {
        self.session == "x11"
    }

    /// The desktop offers its own shortcut manager, which LinuxFlow drives without the GlobalShortcuts portal.
    pub fn has_native_shortcuts(&self) -> bool {
        self.is_wayland() && matches!(self.desktop.as_str(), "kde" | "gnome" | "sway" | "hyprland")
    }

    /// A way to register the dictation shortcut exists (portal, desktop manager or X11 grab).
    pub fn can_register_shortcut(&self) -> bool {
        self.portal_global_shortcuts || self.has_native_shortcuts() || self.is_x11()
    }

    /// Text can reach the clipboard (portal, data-control protocol or the X11 selection).
    pub fn can_copy_text(&self) -> bool {
        self.portal_clipboard || self.data_control || self.is_x11()
    }
}
