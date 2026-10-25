//! Contract of the shortcut backends. The real backends (portal, X11, evdev) implement it;
//! the tests use fakes.

use super::select::{BackendKind, HotkeyOptions};
use super::spec::HotkeySpec;
use linuxflow_core::domain::ShortcutMode;
use linuxflow_core::traits::HotkeySink;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct StartRequest {
    pub spec: HotkeySpec,
    pub mode: ShortcutMode,
}

/// What the started backend can do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackendReport {
    pub supports_release: bool,
    pub supports_modifier_only: bool,
    /// Text of the effective trigger (the portal's may differ from the request).
    pub trigger_description: String,
    /// Emits `HotkeyEvent`s (Activated/Deactivated).
    pub provides_hotkey_events: bool,
    /// Emits `RawKeyEvent`s (the core PTT machine then reconstructs the shortcut).
    pub provides_raw_keys: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendErrorKind {
    Unavailable,
    Denied,
    /// Key already taken by another application (`BadAccess`).
    Conflict,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError {
    pub kind: BackendErrorKind,
    pub message: String,
}

impl BackendError {
    pub fn new(kind: BackendErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub trait HotkeyBackend: Send {
    fn kind(&self) -> BackendKind;
    /// Starts (blocking, short); the events then arrive on `sink` from a thread.
    fn start(
        &mut self,
        req: &StartRequest,
        sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError>;
    fn stop(&mut self);
    /// Temporarily releases the shortcut (e.g. while recording a new one). Optional.
    fn set_suspended(&mut self, _suspended: bool) {}
    /// False when the backend no longer receives anything although it was not stopped (the desktop's
    /// shortcut service restarted, D-Bus streams ended). Default: always alive.
    fn is_alive(&self) -> bool {
        true
    }
}

/// Backend factory (real or simulated).
pub trait BackendFactory: Send + Sync {
    fn create(&self, kind: BackendKind, opts: &HotkeyOptions) -> Option<Box<dyn HotkeyBackend>>;
}

/// Real factory.
pub struct SystemBackends;

impl BackendFactory for SystemBackends {
    fn create(&self, kind: BackendKind, opts: &HotkeyOptions) -> Option<Box<dyn HotkeyBackend>> {
        match kind {
            BackendKind::KGlobalAccel => {
                Some(Box::new(super::kglobalaccel::KGlobalAccelBackend::new()))
            }
            BackendKind::GnomeKeys => Some(Box::new(super::gnome::GnomeKeysBackend::new())),
            BackendKind::SwayKeys => Some(Box::new(super::compositor::SwayKeysBackend::new())),
            BackendKind::HyprlandKeys => {
                Some(Box::new(super::compositor::HyprlandKeysBackend::new()))
            }
            BackendKind::Portal => Some(Box::new(super::portal::PortalBackend::new())),
            BackendKind::X11 => Some(Box::new(super::x11::X11Backend::new())),
            BackendKind::Evdev => Some(Box::new(super::evdev_backend::EvdevBackend::new(
                opts.ignore_device_names.clone(),
            ))),
            BackendKind::None => None,
        }
    }
}
