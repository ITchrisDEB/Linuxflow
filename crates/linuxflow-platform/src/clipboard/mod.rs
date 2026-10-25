//! Clipboard: chain of backends chosen at run time (Wayland data-control, X11, Clipboard
//! portal, GTK backend provided by the application), best-effort backup/restore of all the
//! MIME types and "served" detection when the backend allows it.
//!
//! `SystemClipboard` implements the `linuxflow_core::traits::Clipboard` contract (text) and adds
//! `snapshot()` / `restore()` / `restore_if_ours()` / `wait_served()`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use linuxflow_core::error::{Error, Result as CoreResult};
use linuxflow_core::traits::Clipboard;

use crate::inject::env::{Compositor, SessionEnv};
use crate::inject::portal::PortalSession;

pub mod portal;
pub mod wayland;
pub mod x11;

/// Cap of the backup (all types together): beyond it, the backup is marked incomplete.
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Selection {
    Clipboard,
    Primary,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipError {
    /// Backend unusable here: try the next one.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("failed: {0}")]
    Failed(String),
}

/// A clipboard content in one MIME type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipItem {
    pub mime: String,
    pub data: Vec<u8>,
}

/// Content of a selection: empty `items` = empty clipboard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelectionData {
    pub items: Vec<ClipItem>,
    /// Some types could not be read (size, error): partial restoration.
    pub incomplete: bool,
}

/// Best-effort backup. `None` for a selection = not backed up (do not restore it).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub clipboard: Option<SelectionData>,
    pub primary: Option<SelectionData>,
}

impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.clipboard.is_none() && self.primary.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedStatus {
    /// An application read our content.
    Served,
    /// Nobody read our content within the delay.
    TimedOut,
    /// The backend cannot detect it: use a fixed delay.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    /// The content is no longer ours (the user copied something else): nothing was touched.
    SkippedChanged,
    /// Nothing to restore (no backup).
    NothingToRestore,
    Failed,
}

/// Extension point: a way of reading/writing the clipboard. The application can provide a
/// GTK backend (`gdk::Clipboard`) for sessions without data-control (GNOME).
pub trait ClipboardBackend: Send + Sync {
    fn name(&self) -> &'static str;

    fn get_text(&self, selection: Selection) -> std::result::Result<Option<String>, ClipError>;

    /// Puts `text` in all the requested selections and serves it as long as it is not replaced.
    fn set_text(&self, text: &str, selections: &[Selection]) -> std::result::Result<(), ClipError>;

    fn supports_primary(&self) -> bool {
        false
    }

    /// Reads all the MIME types of the selection.
    fn snapshot_selection(
        &self,
        _selection: Selection,
    ) -> std::result::Result<SelectionData, ClipError> {
        Err(ClipError::Unavailable("snapshot not supported".into()))
    }

    /// Puts back a saved content (empty list: empties the selection).
    fn restore_selection(
        &self,
        _selection: Selection,
        _data: &SelectionData,
    ) -> std::result::Result<(), ClipError> {
        Err(ClipError::Unavailable("restore not supported".into()))
    }

    /// Waits until an application has read the last content set.
    fn wait_served(&self, _timeout: Duration) -> ServedStatus {
        ServedStatus::Unknown
    }
}

/// Known backends, for the order of attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardBackendKind {
    DataControl,
    X11,
    Portal,
    /// Backend provided by the application (GTK).
    App,
}

/// Order of attempts for a session (pure, testable with a simulated environment).
pub fn clipboard_chain(
    env: &SessionEnv,
    portal_clipboard: bool,
    has_app_backend: bool,
) -> Vec<ClipboardBackendKind> {
    use ClipboardBackendKind::*;
    let mut chain = if env.is_wayland() {
        match env.compositor() {
            // Mutter offers no data-control: portal (consent) then GTK.
            Compositor::Gnome => {
                if portal_clipboard {
                    vec![Portal]
                } else {
                    vec![]
                }
            }
            _ => vec![DataControl],
        }
    } else if env.is_x11() {
        vec![X11]
    } else {
        vec![DataControl, X11]
    };
    // XWayland: the X11 clipboard is synchronised with Wayland, last resort before GTK.
    if env.is_wayland()
        && env.x_display
        && !chain.contains(&X11)
        && env.compositor() != Compositor::Gnome
    {
        chain.push(X11);
    }
    if has_app_backend {
        chain.push(App);
    }
    chain
}

/// Build settings.
#[derive(Default)]
pub struct ClipboardConfig {
    /// Also fills PRIMARY (Shift+Insert paste in some terminals) and restores it.
    pub also_primary: bool,
    /// Portal session shared with the injector (requested with `clipboard: true`).
    pub portal: Option<Arc<PortalSession>>,
    /// GTK backend of the application.
    pub app_backend: Option<Arc<dyn ClipboardBackend>>,
}

pub fn build_clipboard(env: &SessionEnv, config: ClipboardConfig) -> SystemClipboard {
    let portal_ok = config
        .portal
        .as_ref()
        .is_some_and(|p| p.clipboard_enabled());
    let mut backends: Vec<Arc<dyn ClipboardBackend>> = Vec::new();
    for kind in clipboard_chain(env, portal_ok, config.app_backend.is_some()) {
        match kind {
            ClipboardBackendKind::DataControl => {
                backends.push(Arc::new(wayland::WaylandClipboard::new()))
            }
            ClipboardBackendKind::X11 => backends.push(Arc::new(x11::X11Clipboard::new())),
            ClipboardBackendKind::Portal => {
                if let Some(p) = config.portal.as_ref() {
                    backends.push(Arc::new(portal::PortalClipboard::new(p.clone())));
                }
            }
            ClipboardBackendKind::App => {
                if let Some(b) = config.app_backend.as_ref() {
                    backends.push(b.clone());
                }
            }
        }
    }
    SystemClipboard::new(backends, config.also_primary)
}

/// System clipboard: first success of the chain.
pub struct SystemClipboard {
    backends: Vec<Arc<dyn ClipboardBackend>>,
    also_primary: bool,
    /// Index of the backend that set the last text.
    last_writer: Mutex<Option<usize>>,
}

impl SystemClipboard {
    pub fn new(backends: Vec<Arc<dyn ClipboardBackend>>, also_primary: bool) -> Self {
        Self {
            backends,
            also_primary,
            last_writer: Mutex::new(None),
        }
    }

    pub fn backend_names(&self) -> Vec<&'static str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    fn selections(&self) -> Vec<Selection> {
        if self.also_primary {
            vec![Selection::Clipboard, Selection::Primary]
        } else {
            vec![Selection::Clipboard]
        }
    }

    /// Best-effort backup of all the MIME types of CLIPBOARD (and PRIMARY if enabled).
    pub fn snapshot(&self) -> Snapshot {
        let take = |sel: Selection| {
            self.backends
                .iter()
                .find_map(|b| b.snapshot_selection(sel).ok())
        };
        Snapshot {
            clipboard: take(Selection::Clipboard),
            primary: if self.also_primary {
                take(Selection::Primary)
            } else {
                None
            },
        }
    }

    /// Puts the backup back unconditionally. Returns `true` if everything that was saved is restored.
    pub fn restore(&self, snapshot: &Snapshot) -> bool {
        let mut ok = true;
        for (sel, data) in [
            (Selection::Clipboard, &snapshot.clipboard),
            (Selection::Primary, &snapshot.primary),
        ] {
            let Some(data) = data else { continue };
            let done = self
                .backends
                .iter()
                .any(|b| b.restore_selection(sel, data).is_ok());
            ok &= done;
        }
        ok
    }

    /// Rule: only restore if the content is still ours.
    pub fn restore_if_ours(&self, snapshot: &Snapshot, ours: &str) -> RestoreOutcome {
        if snapshot.is_empty() {
            return RestoreOutcome::NothingToRestore;
        }
        match self.get_text() {
            Ok(Some(current)) if current == ours => {
                if self.restore(snapshot) {
                    RestoreOutcome::Restored
                } else {
                    RestoreOutcome::Failed
                }
            }
            Ok(_) => RestoreOutcome::SkippedChanged,
            Err(_) => RestoreOutcome::Failed,
        }
    }

    /// "Served" detection of the last text set; `Unknown`: the caller uses a fixed delay.
    pub fn wait_served(&self, timeout: Duration) -> ServedStatus {
        let idx = self.last_writer.lock().ok().and_then(|g| *g);
        match idx.and_then(|i| self.backends.get(i)) {
            Some(b) => b.wait_served(timeout),
            None => ServedStatus::Unknown,
        }
    }
}

impl Clipboard for SystemClipboard {
    fn get_text(&self) -> CoreResult<Option<String>> {
        let mut last = None;
        for b in &self.backends {
            match b.get_text(Selection::Clipboard) {
                Ok(v) => return Ok(v),
                Err(e) => last = Some(format!("{} {e}", b.name())),
            }
        }
        Err(Error::msg(
            last.unwrap_or_else(|| "no clipboard backend".to_string()),
        ))
    }

    fn set_text(&self, text: &str) -> CoreResult<()> {
        let selections = self.selections();
        let mut last = None;
        for (i, b) in self.backends.iter().enumerate() {
            let wanted: Vec<Selection> = selections
                .iter()
                .copied()
                .filter(|s| *s == Selection::Clipboard || b.supports_primary())
                .collect();
            match b.set_text(text, &wanted) {
                Ok(()) => {
                    if let Ok(mut g) = self.last_writer.lock() {
                        *g = Some(i);
                    }
                    return Ok(());
                }
                Err(e) => last = Some(format!("{} {e}", b.name())),
            }
        }
        Err(Error::msg(
            last.unwrap_or_else(|| "no clipboard backend".to_string()),
        ))
    }
}

/// Reading a pipe with a delay and a cap: a source that does not answer does not block the caller.
///
/// Reading WITHOUT a thread: wait for the descriptor to be ready with `poll` (bounded by the remaining
/// delay) then read what is ready. On expiry nothing stays in the background: no
/// thread or descriptor leaks when the selection owner never answers.
pub(crate) fn read_limited(
    mut reader: impl std::io::Read + std::os::fd::AsRawFd,
    limit: usize,
    timeout: Duration,
) -> std::result::Result<(Vec<u8>, bool), ClipError> {
    let deadline = std::time::Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(ClipError::Failed("read timed out".into()));
        }
        let mut pfd = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let wait_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: `pfd` is a valid `pollfd` for the duration of the call, `nfds` is 1.
        let ready = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if ready < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(ClipError::Failed(format!("read: {err}")));
        }
        if ready == 0 {
            return Err(ClipError::Failed("read timed out".into()));
        }
        // Ready (data or close): the read no longer blocks.
        let want = (limit + 1 - buf.len()).min(chunk.len());
        match reader.read(&mut chunk[..want]) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > limit {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ClipError::Failed(format!("read: {e}"))),
        }
    }
    let truncated = buf.len() > limit;
    if truncated {
        buf.truncate(limit);
    }
    Ok((buf, truncated))
}

#[cfg(test)]
mod tests;
