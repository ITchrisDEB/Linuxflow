//! GlobalShortcuts portal backend (ashpd). The application must be registered with the
//! portal (visible `.desktop` file), otherwise the session is refused ("An app id is required").
//! No persistent permission is requested by this module.

use super::backend::{BackendError, BackendErrorKind, BackendReport, HotkeyBackend, StartRequest};
use super::clock::now_ms;
use super::select::BackendKind;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::zbus::Connection;
use futures_util::StreamExt;
use linuxflow_core::constants::APP_ID;
use linuxflow_core::traits::{HotkeyEvent, HotkeySink};
use linuxflow_i18n::tr;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// Shortcut identifier. A desktop (KDE, GNOME) only takes the requested key into account for a
/// NEW identifier: once assigned, it no longer changes from the application. The identifier
/// therefore depends on the chosen key, which makes it possible to change it from LinuxFlow's settings.
fn shortcut_id(trigger: Option<&str>) -> String {
    match trigger {
        Some(t) => {
            let slug: String = t
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            format!("toggle-{slug}")
        }
        None => "toggle".to_string(),
    }
}
/// The portal may show a consent request: generous delay.
const START_TIMEOUT: Duration = Duration::from_secs(30);

pub struct PortalBackend {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Turns false if the event streams end without our having asked for it.
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl PortalBackend {
    pub fn new() -> Self {
        Self {
            stop: None,
            thread: None,
            alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

impl Default for PortalBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn denied(msg: impl Into<String>) -> BackendError {
    BackendError::new(BackendErrorKind::Denied, msg)
}

async fn run(
    trigger: Option<String>,
    sink: Arc<dyn HotkeySink>,
    ready: mpsc::Sender<Result<BackendReport, BackendError>>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
    alive: Arc<std::sync::atomic::AtomicBool>,
) {
    let id = shortcut_id(trigger.as_deref());
    let setup = async {
        let conn = Connection::session().await.map_err(|_| {
            BackendError::new(
                BackendErrorKind::Unavailable,
                tr("The session D-Bus is not reachable."),
            )
        })?;
        // Registration of the application (useful outside Flatpak); a failure is not fatal.
        if let Ok(id) = ashpd::AppID::from_str(APP_ID) {
            let _ = ashpd::register_host_app_with_connection(conn.clone(), id).await;
        }
        let proxy = GlobalShortcuts::with_connection(conn).await.map_err(|_| {
            BackendError::new(
                BackendErrorKind::Unavailable,
                tr("The shortcut portal is not available."),
            )
        })?;
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(|e| {
                denied(format!(
                    "{} ({e})",
                    tr("The shortcut portal refused the session.")
                ))
            })?;
        let desc = tr("Start or stop dictation");
        let shortcut =
            NewShortcut::new(id.as_str(), desc.as_str()).preferred_trigger(trigger.as_deref());
        let bound = proxy
            .bind_shortcuts(&session, &[shortcut], None, Default::default())
            .await
            .map_err(|e| {
                denied(format!(
                    "{} ({e})",
                    tr("The shortcut portal refused the binding.")
                ))
            })?
            .response()
            .map_err(|e| {
                denied(format!(
                    "{} ({e})",
                    tr("The shortcut portal refused the binding.")
                ))
            })?;
        let description = bound
            .shortcuts()
            .iter()
            .find(|s| s.id() == id)
            .map(|s| s.trigger_description().to_string())
            .ok_or_else(|| denied(tr("The shortcut portal did not bind the shortcut.")))?;
        if description.trim().is_empty() {
            // The desktop kept the request but attached no key to it (already taken, or
            // dialog cancelled): the shortcut would never fire.
            return Err(BackendError::new(
                BackendErrorKind::Conflict,
                tr("The desktop did not assign the key you chose (it is probably already used by another application). Choose another shortcut."),
            ));
        }
        let activated = proxy
            .receive_activated()
            .await
            .map_err(|e| denied(e.to_string()))?;
        let deactivated = proxy
            .receive_deactivated()
            .await
            .map_err(|e| denied(e.to_string()))?;
        Ok::<_, BackendError>((session, description, activated, deactivated))
    };
    let (session, description, mut activated, mut deactivated) = match setup.await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(BackendReport {
        supports_release: true,
        supports_modifier_only: false,
        trigger_description: description,
        provides_hotkey_events: true,
        provides_raw_keys: false,
        note: Some(tr(
            "If the key you chose is taken, the desktop assigns another one: record a different shortcut.",
        )),
    }));
    loop {
        tokio::select! {
            _ = &mut stop => break,
            Some(ev) = activated.next() => {
                if ev.shortcut_id() == id {
                    sink.hotkey(HotkeyEvent::Activated { ts_ms: now_ms() });
                }
            }
            Some(ev) = deactivated.next() => {
                if ev.shortcut_id() == id {
                    sink.hotkey(HotkeyEvent::Deactivated { ts_ms: now_ms() });
                }
            }
            else => {
                // Both streams ended without a stop request: the desktop's shortcut service
                // restarted. The shortcut is dead: it is reported instead of staying "OK".
                alive.store(false, std::sync::atomic::Ordering::SeqCst);
                break;
            }
        }
    }
    let _ = session.close().await;
}

impl HotkeyBackend for PortalBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Portal
    }

    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn start(
        &mut self,
        req: &StartRequest,
        sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError> {
        self.stop();
        let trigger = req.spec.portal_trigger();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        self.alive.store(true, std::sync::atomic::Ordering::SeqCst);
        let alive = self.alive.clone();
        let thread = std::thread::Builder::new()
            .name("lf-portal-hotkey".into())
            .spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    rt.block_on(run(trigger, sink, ready_tx, stop_rx, alive));
                }
            })
            .map_err(|e| BackendError::new(BackendErrorKind::Failed, e.to_string()))?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(report)) => {
                self.stop = Some(stop_tx);
                self.thread = Some(thread);
                Ok(report)
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = stop_tx.send(());
                Err(BackendError::new(
                    BackendErrorKind::Failed,
                    tr("The shortcut portal did not answer in time."),
                ))
            }
        }
    }

    fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for PortalBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::spec::HotkeySpec;
    use linuxflow_core::domain::ShortcutMode;

    #[test]
    fn the_shortcut_id_follows_the_chosen_key() {
        assert_eq!(shortcut_id(Some("CTRL+ALT+space")), "toggle-ctrl-alt-space");
        assert_eq!(shortcut_id(Some("CTRL+SHIFT+F9")), "toggle-ctrl-shift-f9");
        assert_ne!(
            shortcut_id(Some("CTRL+ALT+d")),
            shortcut_id(Some("CTRL+ALT+space"))
        );
        assert_eq!(shortcut_id(None), "toggle");
    }

    struct Null;
    impl HotkeySink for Null {
        fn hotkey(&self, _: HotkeyEvent) {}
        fn raw_key(&self, _: linuxflow_core::traits::RawKeyEvent) {}
    }

    /// Interacts with the real portal (may show a dialog): never started automatically.
    #[test]
    #[ignore = "real portal; set LINUXFLOW_TEST_PORTALS=1"]
    fn portail_reel() {
        if std::env::var("LINUXFLOW_TEST_PORTALS").as_deref() != Ok("1") {
            return;
        }
        let mut b = PortalBackend::new();
        let req = StartRequest {
            spec: HotkeySpec::parse("Ctrl+Alt+Space").unwrap(),
            mode: ShortcutMode::Toggle,
        };
        let r = b.start(&req, Arc::new(Null));
        println!("{r:?}");
        b.stop();
    }
}
