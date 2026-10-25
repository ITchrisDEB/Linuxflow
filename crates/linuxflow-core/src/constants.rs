//! Named constants. None of them is a local value.

pub const APP_NAME: &str = "LinuxFlow";
/// Name of the XDG folders and prefix of the environment variables (`LINUXFLOW_*`).
pub const APP_DIR_NAME: &str = "linuxflow";
/// Application identifier. Single source of truth.
pub const APP_ID: &str = "io.github.ITchrisDEB.LinuxFlow";

pub const SAMPLE_RATE_HZ: u32 = 16_000;
pub const MIN_AUDIO_MS: u64 = 250;
pub const HINTS_MAX: usize = 80;
/// Identifier of the small speech-detection model (a Whisper helper, never offered as a choice).
pub const VAD_MODEL_ID: &str = "silero-vad";
pub const HISTORY_MAX: usize = 2000;
pub const HISTORY_TO_UI: usize = 300;

pub const PTT_TAP_MS: u64 = 300;
pub const PTT_SPOILED_CANCEL_MS: u64 = 700;
pub const PTT_GHOST_KEY_MS: u64 = 8000;

pub const IDLE_DONE_MS: u64 = 900;
pub const IDLE_EMPTY_MS: u64 = 1200;
pub const IDLE_FLASH_MS: u64 = 1600;
pub const IDLE_MIC_ERROR_MS: u64 = 2000;
pub const IDLE_ERROR_MS: u64 = 2200;
pub const OVERLAY_HIDE_DELAY_MS: u64 = 420;

pub const OVERLAY_BARS: usize = 22;
pub const DEFAULT_SHORTCUT: &str = "Ctrl+Alt+Space";
pub const DEFAULT_PASTE_DELAY_MS: u64 = 60;
pub const DEFAULT_RESTORE_CLIPBOARD_MS: u64 = 400;
pub const DEFAULT_MAX_DICTATION_SEC: u64 = 300;
