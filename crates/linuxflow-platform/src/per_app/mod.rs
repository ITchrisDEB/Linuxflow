//! Per-application table: paste mode (Ctrl+V, Ctrl+Shift+V, Shift+Insert) and delay.
//!
//! The key is the `app_id` (Wayland) or the class (X11)
//! in lower case. An embedded table gives the defaults; a user TOML file
//! of the same format overrides them field by field.

use std::collections::HashMap;
use std::path::Path;

use linuxflow_core::domain::{PasteMode, WindowInfo};
use serde::Deserialize;

/// Table embedded in the binary.
const EMBEDDED_TABLE: &str = include_str!("default_table.toml");

/// Contract for resolving the paste mode: the core pipeline trait (single point).
pub use linuxflow_core::pipeline::PasteModeResolver;

/// Error reading a table.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PerAppError {
    #[error("table par application invalide : {0}")]
    Parse(String),
    #[error("unknown paste mode \"{mode}\" for the application \"{app}\"")]
    UnknownMode { app: String, mode: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    apps: HashMap<String, RawEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    mode: Option<String>,
    delay_ms: Option<u64>,
}

/// Setting of an application; an absent field falls back to the caller's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppPaste {
    pub mode: Option<PasteMode>,
    pub delay_ms: Option<u64>,
}

/// Table `app_id` in lower case -> setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerAppTable {
    entries: HashMap<String, AppPaste>,
}

/// Normalises a key: lower case, spaces removed, `.desktop` suffix ignored.
fn normalize_key(key: &str) -> String {
    let lower = key.trim().to_lowercase();
    lower.strip_suffix(".desktop").unwrap_or(&lower).to_string()
}

pub fn parse_mode(text: &str) -> Option<PasteMode> {
    match text.trim().to_lowercase().as_str() {
        "ctrl_v" => Some(PasteMode::CtrlV),
        "ctrl_shift_v" => Some(PasteMode::CtrlShiftV),
        "shift_insert" => Some(PasteMode::ShiftInsert),
        _ => None,
    }
}

pub fn mode_to_str(mode: PasteMode) -> &'static str {
    match mode {
        PasteMode::CtrlV => "ctrl_v",
        PasteMode::CtrlShiftV => "ctrl_shift_v",
        PasteMode::ShiftInsert => "shift_insert",
    }
}

fn parse_layer(text: &str) -> Result<HashMap<String, AppPaste>, PerAppError> {
    let raw: RawFile = toml::from_str(text).map_err(|e| PerAppError::Parse(e.to_string()))?;
    let mut out = HashMap::new();
    for (app, entry) in raw.apps {
        let mode = match entry.mode {
            None => None,
            Some(text) => Some(parse_mode(&text).ok_or_else(|| PerAppError::UnknownMode {
                app: app.clone(),
                mode: text.clone(),
            })?),
        };
        let slot: &mut AppPaste = out.entry(normalize_key(&app)).or_default();
        slot.mode = mode.or(slot.mode);
        slot.delay_ms = entry.delay_ms.or(slot.delay_ms);
    }
    Ok(out)
}

impl PerAppTable {
    /// Embedded table only.
    pub fn embedded() -> Self {
        // The embedded table is tested; on an unlikely error we start again from an empty table.
        let entries = parse_layer(EMBEDDED_TABLE).unwrap_or_default();
        Self { entries }
    }

    /// Embedded table overridden by a user TOML text.
    pub fn from_user_toml(user: &str) -> Result<Self, PerAppError> {
        let mut table = Self::embedded();
        table.apply_layer(parse_layer(user)?);
        Ok(table)
    }

    /// Loads the embedded table then the user file if it exists. An unreadable or invalid
    /// file is ignored (warning in the log): the application must start.
    pub fn load(user_file: Option<&Path>) -> Self {
        let mut table = Self::embedded();
        let Some(path) = user_file else {
            return table;
        };
        match std::fs::read_to_string(path) {
            Ok(text) => match parse_layer(&text) {
                Ok(layer) => table.apply_layer(layer),
                Err(err) => tracing::warn!("per-application table ignored: {err}"),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("table par application illisible : {err}"),
        }
        table
    }

    fn apply_layer(&mut self, layer: HashMap<String, AppPaste>) {
        for (key, over) in layer {
            let slot = self.entries.entry(key).or_default();
            slot.mode = over.mode.or(slot.mode);
            slot.delay_ms = over.delay_ms.or(slot.delay_ms);
        }
    }

    /// Setting of an application (key already normalised or not).
    pub fn get(&self, app_id: &str) -> Option<&AppPaste> {
        self.entries.get(&normalize_key(app_id))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for PerAppTable {
    fn default() -> Self {
        Self::embedded()
    }
}

impl PasteModeResolver for PerAppTable {
    fn resolve(&self, window: Option<&WindowInfo>, default_delay_ms: u64) -> (PasteMode, u64) {
        let entry = window
            .and_then(|w| w.app_id.as_deref())
            .and_then(|id| self.get(id));
        match entry {
            Some(app) => (
                app.mode.unwrap_or(PasteMode::CtrlV),
                app.delay_ms.unwrap_or(default_delay_ms),
            ),
            None => (PasteMode::CtrlV, default_delay_ms),
        }
    }
}

#[cfg(test)]
mod tests;
