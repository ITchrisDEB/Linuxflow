//! RemoteDesktop portal session (and associated Clipboard), shared between the key injector
//! and the clipboard backend. No session is opened before the first use (or a call
//! explicit call to [`PortalSession::ensure`]): opening triggers the user's consent.
//!
//! The restore token is stored with 0600 in the data folder provided by the caller.
//! It is never displayed, logged or included in an error message.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ashpd::desktop::clipboard::Clipboard;
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
use ashpd::desktop::{PersistMode, Session};
use ashpd::enumflags2::BitFlags;
use futures_util::StreamExt;
use linuxflow_core::domain::PasteMode;

use super::backend::{Delivery, InjectError, KeyInjector};
use super::plan::KeyEvent;

/// File name of the token in the data folder.
pub const TOKEN_FILE_NAME: &str = "portal-restore-token";
/// Text MIME types announced to the portal.
pub const TEXT_MIMES: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];

// --- Restore token (0600) -------------------------------------------------------------

/// Reads the token; tightens the permissions if they were too wide. `None` if absent/empty.
pub fn read_token(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    if let Ok(meta) = file.metadata() {
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    let token = text.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Writes the token with 0600 (temporary file created with 0600 then renamed). The parent folder is
/// created with 0700; the temporary file, which contains the token, never remains after a failure.
pub fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
    }
    let tmp: PathBuf = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// --- Configuration ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PortalConfig {
    /// Data folder (provided by the caller) where to store the token; `None`: no persistence.
    pub data_dir: Option<PathBuf>,
    /// Also requests access to the portal clipboard (before the session starts).
    pub clipboard: bool,
    /// Maximum delay to wait for the consent.
    pub consent_timeout: Duration,
}

impl Default for PortalConfig {
    fn default() -> Self {
        Self {
            data_dir: None,
            clipboard: false,
            consent_timeout: Duration::from_secs(90),
        }
    }
}

/// Message of a portal session closed or revoked during use.
const REVOKED: &str = "portal session closed or revoked";

// --- Session ----------------------------------------------------------------------------------

struct Live {
    rd: RemoteDesktop,
    session: Session<RemoteDesktop>,
    clipboard: Option<Clipboard>,
}

type Content = Mutex<HashMap<String, Vec<u8>>>;

/// State of the connection to the portal. The lock is NEVER held during the consent
/// (up to 90 s): the other callers wait on the condition, with a maximum delay.
enum LiveState {
    Idle,
    Connecting,
    Ready(Arc<Live>),
}

pub struct PortalSession {
    /// Kept to stop it cleanly in `Drop` (dropping it in an asynchronous context panics).
    rt: Option<tokio::runtime::Runtime>,
    handle: tokio::runtime::Handle,
    config: PortalConfig,
    live: Mutex<LiveState>,
    ready: std::sync::Condvar,
    content: Arc<Content>,
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

fn map_err(err: ashpd::Error) -> InjectError {
    use ashpd::desktop::ResponseError;
    match err {
        ashpd::Error::Response(ResponseError::Cancelled) => {
            InjectError::Blocked("portal request refused or cancelled".to_string())
        }
        ashpd::Error::PortalNotFound(_) | ashpd::Error::RequiresVersion(..) => {
            InjectError::Unavailable("portal interface not available".to_string())
        }
        other => InjectError::Failed(format!("portal: {other}")),
    }
}

/// Releases (best effort, errors ignored) keys that may have stayed pressed.
async fn release_all(live: &Live, keysyms: &[i32]) {
    for keysym in keysyms.iter().rev() {
        let _ = live
            .rd
            .notify_keyboard_keysym(
                &live.session,
                *keysym,
                KeyState::Released,
                Default::default(),
            )
            .await;
    }
}

impl PortalSession {
    pub fn new(config: PortalConfig) -> Result<Arc<Self>, InjectError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("linuxflow-portal")
            .enable_all()
            .build()
            .map_err(|e| InjectError::Unavailable(format!("runtime: {e}")))?;
        let handle = rt.handle().clone();
        Ok(Arc::new(Self {
            rt: Some(rt),
            handle,
            config,
            live: Mutex::new(LiveState::Idle),
            ready: std::sync::Condvar::new(),
            content: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    fn token_path(&self) -> Option<PathBuf> {
        self.config
            .data_dir
            .as_ref()
            .map(|d| d.join(TOKEN_FILE_NAME))
    }

    /// Runs a future on the private runtime and waits for its result (usable from any
    /// which thread, including from another tokio runtime).
    fn run<T: Send + 'static>(
        &self,
        timeout: Duration,
        fut: impl std::future::Future<Output = Result<T, InjectError>> + Send + 'static,
    ) -> Result<T, InjectError> {
        self.run_with(
            timeout,
            fut,
            InjectError::TimedOut("portal did not answer in time".to_string()),
        )
    }

    /// Like `run`, with the error to return on expiry. On expiry the task is CANCELLED:
    /// no orphan session that would open later with a second consent dialog.
    fn run_with<T: Send + 'static>(
        &self,
        timeout: Duration,
        fut: impl std::future::Future<Output = Result<T, InjectError>> + Send + 'static,
        on_timeout: InjectError,
    ) -> Result<T, InjectError> {
        let (tx, rx) = mpsc::channel();
        let task = self.handle.spawn(async move {
            let _ = tx.send(fut.await);
        });
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                task.abort();
                Err(on_timeout)
            }
        }
    }

    async fn connect(
        config: PortalConfig,
        token_path: Option<PathBuf>,
        content: Arc<Content>,
    ) -> Result<Live, InjectError> {
        let rd = RemoteDesktop::new().await.map_err(map_err)?;
        let session = rd
            .create_session(Default::default())
            .await
            .map_err(map_err)?;
        let clipboard = if config.clipboard {
            match Clipboard::new().await {
                Ok(c) => {
                    c.request(&session, Default::default())
                        .await
                        .map_err(map_err)?;
                    Some(c)
                }
                Err(_) => None,
            }
        } else {
            None
        };
        let token = token_path.as_deref().and_then(read_token);
        let options = ashpd::desktop::remote_desktop::SelectDevicesOptions::default()
            .set_devices(Some(BitFlags::from(DeviceType::Keyboard)))
            .set_persist_mode(Some(PersistMode::ExplicitlyRevoked))
            .set_restore_token(token.as_deref());
        rd.select_devices(&session, options)
            .await
            .map_err(map_err)?
            .response()
            .map_err(map_err)?;
        let started = rd
            .start(&session, None, Default::default())
            .await
            .map_err(map_err)?
            .response()
            .map_err(map_err)?;
        if !started.devices().contains(DeviceType::Keyboard) {
            return Err(InjectError::Blocked(
                "keyboard access not granted".to_string(),
            ));
        }
        // The token is single-use: the new one is saved at every session.
        if let (Some(path), Some(new_token)) = (token_path.as_deref(), started.restore_token()) {
            if let Err(e) = write_token(path, new_token) {
                tracing::warn!("portal: cannot store restore token: {}", e.kind());
            }
        }
        let live = Live {
            rd,
            session,
            clipboard,
        };
        let _ = &content;
        Ok(live)
    }

    /// Opens the session if needed (triggers the consent at first use).
    pub fn ensure(&self) -> Result<(), InjectError> {
        self.live_handle().map(|_| ())
    }

    fn live_handle(&self) -> Result<Arc<Live>, InjectError> {
        let poisoned = || InjectError::Failed("lock poisoned".into());
        let mut guard = self.live.lock().map_err(|_| poisoned())?;
        loop {
            match &*guard {
                LiveState::Ready(live) => return Ok(live.clone()),
                LiveState::Connecting => {
                    // Another caller is waiting for the consent: we wait with a ceiling.
                    let (g, waited) = self
                        .ready
                        .wait_timeout(guard, self.config.consent_timeout)
                        .map_err(|_| poisoned())?;
                    guard = g;
                    if waited.timed_out() && matches!(&*guard, LiveState::Connecting) {
                        return Err(InjectError::Unavailable(
                            "portal connection still pending".to_string(),
                        ));
                    }
                }
                LiveState::Idle => {
                    *guard = LiveState::Connecting;
                    break;
                }
            }
        }
        drop(guard); // jamais de verrou tenu pendant le consentement

        let fut = Self::connect(self.config.clone(), self.token_path(), self.content.clone());
        let result = self.run_with(
            self.config.consent_timeout,
            fut,
            InjectError::Unavailable("portal consent timed out".to_string()),
        );
        let mut guard = self.live.lock().map_err(|_| poisoned())?;
        match result {
            Ok(live) => {
                let live = Arc::new(live);
                *guard = LiveState::Ready(live.clone());
                self.ready.notify_all();
                drop(guard);
                if live.clipboard.is_some() {
                    self.spawn_transfer_server(live.clone());
                }
                Ok(live)
            }
            Err(e) => {
                *guard = LiveState::Idle;
                self.ready.notify_all();
                Err(e)
            }
        }
    }

    /// Serves the `SelectionTransfer` requests with the current content.
    fn spawn_transfer_server(&self, live: Arc<Live>) {
        let content = self.content.clone();
        self.handle.spawn(async move {
            let Some(clip_proxy) = live.clipboard.as_ref() else {
                return;
            };
            let Ok(stream) = clip_proxy
                .receive_selection_transfer::<RemoteDesktop>()
                .await
            else {
                return;
            };
            let mut stream = Box::pin(stream);
            while let Some((_session, mime, serial)) = stream.next().await {
                let data = content.lock().ok().and_then(|c| c.get(&mime).cloned());
                let ok = match data {
                    Some(bytes) => match clip_proxy.selection_write(&live.session, serial).await {
                        Ok(fd) => {
                            let fd: std::os::fd::OwnedFd = fd.into();
                            tokio::task::spawn_blocking(move || {
                                let mut file = std::fs::File::from(fd);
                                file.write_all(&bytes).is_ok()
                            })
                            .await
                            .unwrap_or(false)
                        }
                        Err(_) => false,
                    },
                    None => false,
                };
                let _ = clip_proxy
                    .selection_write_done(&live.session, serial, ok)
                    .await;
            }
        });
    }

    fn drop_live(&self) {
        if let Ok(mut g) = self.live.lock() {
            *g = LiveState::Idle;
        }
    }

    /// Runs a portal operation; if it fails because the session is gone (the portal service crashed or restarted,
    /// which xdg-desktop-portal-gnome 46 does now and then), the session is abandoned and the operation is repeated
    /// once on a new one (the restore token means no new dialog). A refusal or an absent portal is not retried.
    fn with_recovery<T>(
        &self,
        mut op: impl FnMut() -> Result<T, InjectError>,
    ) -> Result<T, InjectError> {
        match op() {
            Err(InjectError::Failed(_)) => {
                self.drop_live();
                op()
            }
            other => other,
        }
    }

    /// Sends (keysym, pressed) pairs to the portal. A closed or revoked session gives `Blocked`.
    /// On failure or timeout in the middle of the sequence, the keys already pressed are
    /// released (best effort): a Ctrl must never stay stuck in the session.
    pub fn notify_keysyms(&self, keys: Vec<(i32, bool)>) -> Result<(), InjectError> {
        match self.notify_keysyms_once(keys.clone()) {
            // Closed or revoked session (the desktop lets the user stop it at any time): the session
            // is abandoned, a new one is opened (restore token: no new dialog) and
            // the sequence is sent again only once, so as not to lose the dictation.
            Err(InjectError::Blocked(m)) if m == REVOKED => self.notify_keysyms_once(keys),
            other => other,
        }
    }

    fn notify_keysyms_once(&self, keys: Vec<(i32, bool)>) -> Result<(), InjectError> {
        let live = self.live_handle()?;
        let pressed: Vec<i32> = keys
            .iter()
            .filter(|(_, down)| *down)
            .map(|(k, _)| *k)
            .collect();
        let live_for_send = live.clone();
        let pressed_in_task = pressed.clone();
        let result = self.run(Duration::from_secs(5), async move {
            let live = live_for_send;
            let pressed = pressed_in_task;
            for (keysym, down) in keys {
                let state = if down {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                };
                if live
                    .rd
                    .notify_keyboard_keysym(&live.session, keysym, state, Default::default())
                    .await
                    .is_err()
                {
                    release_all(&live, &pressed).await;
                    return Err(InjectError::Blocked(REVOKED.to_string()));
                }
            }
            Ok(())
        });
        match &result {
            Err(InjectError::TimedOut(_)) => {
                // The task was cancelled on the way: everything that may have been pressed is released.
                let live = live.clone();
                self.handle
                    .spawn(async move { release_all(&live, &pressed).await });
            }
            Err(InjectError::Blocked(_)) => self.drop_live(),
            _ => {}
        }
        result
    }

    /// Announces a new selection (CLIPBOARD) served by this process.
    pub fn clipboard_set(&self, items: Vec<(String, Vec<u8>)>) -> Result<(), InjectError> {
        self.with_recovery(|| self.clipboard_set_once(items.clone()))
    }

    fn clipboard_set_once(&self, items: Vec<(String, Vec<u8>)>) -> Result<(), InjectError> {
        if !self.config.clipboard {
            return Err(InjectError::Unavailable(
                "clipboard portal not requested".into(),
            ));
        }
        let live = self.live_handle()?;
        if live.clipboard.is_none() {
            return Err(InjectError::Unavailable("clipboard portal absent".into()));
        }
        let mimes: Vec<String> = items.iter().map(|(m, _)| m.clone()).collect();
        if let Ok(mut c) = self.content.lock() {
            c.clear();
            c.extend(items);
        }
        self.run(Duration::from_secs(5), async move {
            let Some(clip) = live.clipboard.as_ref() else {
                return Err(InjectError::Unavailable("clipboard portal absent".into()));
            };
            let refs: Vec<&str> = mimes.iter().map(String::as_str).collect();
            clip.set_selection(
                &live.session,
                ashpd::desktop::clipboard::SetSelectionOptions::default().set_mime_types(&refs),
            )
            .await
            .map_err(map_err)
        })
    }

    /// Reads the current selection in the requested MIME type (if the portal allows it).
    pub fn clipboard_read(&self, mime: &str) -> Result<Vec<u8>, InjectError> {
        self.with_recovery(|| self.clipboard_read_once(mime))
    }

    fn clipboard_read_once(&self, mime: &str) -> Result<Vec<u8>, InjectError> {
        if !self.config.clipboard {
            return Err(InjectError::Unavailable(
                "clipboard portal not requested".into(),
            ));
        }
        let live = self.live_handle()?;
        let mime = mime.to_string();
        self.run(Duration::from_secs(5), async move {
            let Some(clip) = live.clipboard.as_ref() else {
                return Err(InjectError::Unavailable("clipboard portal absent".into()));
            };
            let fd = clip
                .selection_read(&live.session, &mime)
                .await
                .map_err(map_err)?;
            let fd: std::os::fd::OwnedFd = fd.into();
            tokio::task::spawn_blocking(move || {
                let mut buf = Vec::new();
                std::fs::File::from(fd)
                    .take(64 * 1024 * 1024)
                    .read_to_end(&mut buf)
                    .map(|_| buf)
                    .map_err(|e| InjectError::Failed(format!("read: {e}")))
            })
            .await
            .map_err(|e| InjectError::Failed(e.to_string()))?
        })
    }

    pub fn clipboard_enabled(&self) -> bool {
        self.config.clipboard
    }
}

/// Keysyms to send for a sequence of events.
pub fn keysym_stream(events: &[KeyEvent]) -> Vec<(i32, bool)> {
    events
        .iter()
        .map(|e| (e.key.keysym() as i32, e.down))
        .collect()
}

/// Key injector through the RemoteDesktop portal (keysyms: independent of the layout).
pub struct PortalInjector {
    session: Arc<PortalSession>,
}

impl PortalInjector {
    pub fn new(session: Arc<PortalSession>) -> Self {
        Self { session }
    }
}

impl KeyInjector for PortalInjector {
    fn name(&self) -> &'static str {
        "portal-remote-desktop"
    }

    fn probe(&self) -> Result<(), InjectError> {
        self.session.ensure()
    }

    fn send_chord(&self, _mode: PasteMode, events: &[KeyEvent]) -> Result<Delivery, InjectError> {
        self.session.notify_keysyms(keysym_stream(events))?;
        Ok(Delivery::Sent)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::inject::plan::{chord_events, Key};

    #[test]
    fn dropping_the_session_inside_an_async_context_does_not_panic() {
        // Regression: `drop` of the internal tokio runtime from an asynchronous context panicked
        // ("Cannot drop a runtime in a context where blocking is not allowed").
        let session = PortalSession::new(PortalConfig::default()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async move {
            drop(session);
        });
    }

    #[test]
    fn a_lost_session_is_retried_once_and_a_refusal_is_not() {
        let session = PortalSession::new(PortalConfig::default()).unwrap();
        // The portal went away: the first call fails, the second one (new session) succeeds.
        let mut calls = 0;
        let r = session.with_recovery(|| {
            calls += 1;
            if calls == 1 {
                Err(InjectError::Failed("portal: connection closed".into()))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(r.unwrap(), 2);
        // Still failing: one retry only, the error is returned.
        let mut calls = 0;
        let r: Result<(), _> = session.with_recovery(|| {
            calls += 1;
            Err(InjectError::Failed("portal: gone".into()))
        });
        assert!(r.is_err());
        assert_eq!(calls, 2);
        // A refusal, an absent portal or a timeout is never replayed.
        for e in [
            InjectError::Blocked("refused".into()),
            InjectError::Unavailable("absent".into()),
            InjectError::TimedOut("slow".into()),
        ] {
            let mut calls = 0;
            let r: Result<(), _> = session.with_recovery(|| {
                calls += 1;
                Err(e.clone())
            });
            assert!(r.is_err());
            assert_eq!(calls, 1, "{e:?} must not be retried");
        }
    }

    #[test]
    fn token_directory_is_created_0700_and_no_temp_file_survives_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("data");
        let path = sub.join(TOKEN_FILE_NAME);
        write_token(&path, "abc").unwrap();
        let mode = std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "token folder too open");

        // Rename failure: the destination is a non-empty folder.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir_all(blocked.join("inner")).unwrap();
        assert!(write_token(&blocked, "secret-token").is_err());
        assert!(
            !blocked.with_extension("tmp").exists(),
            "the temporary file containing the token was left behind"
        );
    }

    #[test]
    fn token_round_trip_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(TOKEN_FILE_NAME);
        write_token(&path, "abc123\n").unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("abc123"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Overwrite: always 0600, a single file.
        write_token(&path, "def").unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("def"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn read_token_tightens_loose_permissions_and_handles_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        assert_eq!(read_token(&path), None);
        std::fs::write(&path, "tok").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("tok"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&path, "  \n").unwrap();
        assert_eq!(read_token(&path), None);
    }

    #[test]
    fn keysym_stream_uses_keysyms_not_scancodes() {
        let s = keysym_stream(&chord_events(PasteMode::CtrlV));
        assert_eq!(
            s,
            vec![(0xffe3, true), (0x76, true), (0x76, false), (0xffe3, false)]
        );
        assert_eq!(Key::V.keysym() as i32, 0x76);
    }

    #[test]
    fn error_mapping_distinguishes_refusal_from_absence() {
        use ashpd::desktop::ResponseError;
        assert!(matches!(
            map_err(ashpd::Error::Response(ResponseError::Cancelled)),
            InjectError::Blocked(_)
        ));
        assert!(matches!(
            map_err(ashpd::Error::NoResponse),
            InjectError::Failed(_)
        ));
    }
}
