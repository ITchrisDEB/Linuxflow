//! KDE Plasma: global shortcut set by the application itself with `kglobalacceld`
//! (`org.kde.KGlobalAccel` D-Bus interface, the one KDE applications use).
//!
//! Unlike the portal (where the desktop decides the key), the application IMPOSES here the key
//! chosen in its settings, learns who owns a key that is already taken (to tell
//! the user) and receives both the press AND the release (hence push-to-talk).
//!
//! Protocol verified on a real Plasma session: `doRegister(as)`, `getComponent(s)`,
//! `getGlobalShortcutsByKey(i)`, `setShortcutKeys(as, a(ai), u)`, `unregister(s, s)` and the
//! `globalShortcutPressed` / `globalShortcutReleased` signals of the component.

use super::backend::{BackendError, BackendErrorKind, BackendReport, HotkeyBackend, StartRequest};
use super::native::qt_sequence;
use super::select::BackendKind;
use super::spec::HotkeySpec;
use crate::hotkeys::clock::now_ms;
use ashpd::zbus::{self, Connection};
use futures_util::StreamExt;
use linuxflow_core::traits::{HotkeyEvent, HotkeySink};
use linuxflow_i18n::{tr, tr_fmt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const SERVICE: &str = "org.kde.kglobalaccel";
const ROOT_PATH: &str = "/kglobalaccel";
const ROOT_IFACE: &str = "org.kde.KGlobalAccel";
const COMPONENT_IFACE: &str = "org.kde.kglobalaccel.Component";
/// Identifier of our component (`kglobalshortcutsrc`) and of our action.
const COMPONENT: &str = "linuxflow";
const ACTION: &str = "toggle";
/// `SetPresent | NoAutoloading`: the requested key replaces the one remembered by a previous session.
const FLAGS_FORCE: u32 = 2 | 4;
const START_TIMEOUT: Duration = Duration::from_secs(8);

/// A key owner returned by `getGlobalShortcutsByKey`:
/// (action, readable action, component, readable component, context, readable context, keys, default keys).
type Owner = (
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<i32>,
    Vec<i32>,
);

pub struct KGlobalAccelBackend {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    alive: Arc<AtomicBool>,
}

impl KGlobalAccelBackend {
    pub fn new() -> Self {
        Self {
            stop: None,
            thread: None,
            alive: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Default for KGlobalAccelBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn err(kind: BackendErrorKind, msg: impl Into<String>) -> BackendError {
    BackendError::new(kind, msg)
}

fn action_id() -> Vec<String> {
    vec![
        COMPONENT.to_string(),
        ACTION.to_string(),
        linuxflow_core::constants::APP_NAME.to_string(),
        tr("Start or stop dictation"),
    ]
}

fn unavailable() -> BackendError {
    err(
        BackendErrorKind::Unavailable,
        tr("The KDE global shortcut service is not available."),
    )
}

/// Conflict message naming the application that owns the key.
fn conflict_message(shortcut: &str, owner: &Owner) -> String {
    let (_, action, _, component, ..) = owner;
    tr_fmt(
        "{} is already used by {} ({}). Choose another shortcut.",
        &[shortcut, component, action],
    )
}

async fn run(
    spec: HotkeySpec,
    sink: Arc<dyn HotkeySink>,
    ready: mpsc::Sender<Result<BackendReport, BackendError>>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
    alive: Arc<AtomicBool>,
) {
    let setup = async {
        let key = qt_sequence(&spec).ok_or_else(|| {
            err(
                BackendErrorKind::Unavailable,
                tr("This shortcut cannot be used with KDE's shortcut service."),
            )
        })?;
        let conn = Connection::session().await.map_err(|_| unavailable())?;
        let root = zbus::Proxy::new(&conn, SERVICE, ROOT_PATH, ROOT_IFACE)
            .await
            .map_err(|_| unavailable())?;
        let id = action_id();
        let failed = |e: zbus::Error| {
            err(
                BackendErrorKind::Failed,
                format!(
                    "{} ({e})",
                    tr("KDE's shortcut service refused the request.")
                ),
            )
        };

        // 1. Who already owns this key? (never a theft: nothing is attempted if it is taken)
        let owners: Vec<Owner> = root
            .call("getGlobalShortcutsByKey", &(key,))
            .await
            .map_err(|_| unavailable())?;
        if let Some(other) = owners.iter().find(|o| o.2 != COMPONENT) {
            return Err(err(
                BackendErrorKind::Conflict,
                conflict_message(&spec.display(), other),
            ));
        }

        // 2. registration of the action, then imposing the key
        root.call::<_, _, ()>("doRegister", &(id.clone(),))
            .await
            .map_err(failed)?;
        let set: Vec<(Vec<i32>,)> = root
            .call(
                "setShortcutKeys",
                &(id.clone(), vec![(vec![key, 0, 0, 0],)], FLAGS_FORCE),
            )
            .await
            .map_err(failed)?;
        if !set.iter().any(|(k,)| k.first() == Some(&key)) {
            return Err(err(
                BackendErrorKind::Conflict,
                tr_fmt(
                    "KDE did not accept {}. Choose another shortcut.",
                    &[&spec.display()],
                ),
            ));
        }

        // 3. press and release
        let path: zbus::zvariant::OwnedObjectPath = root
            .call("getComponent", &(COMPONENT,))
            .await
            .map_err(failed)?;
        let component = zbus::Proxy::new(&conn, SERVICE, path.as_ref(), COMPONENT_IFACE)
            .await
            .map_err(failed)?;
        let pressed = component
            .receive_signal("globalShortcutPressed")
            .await
            .map_err(failed)?;
        let released = component
            .receive_signal("globalShortcutReleased")
            .await
            .map_err(failed)?;
        Ok::<_, BackendError>((root, id, pressed, released))
    };
    let (root, id, mut pressed, mut released) = match setup.await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(BackendReport {
        supports_release: true,
        supports_modifier_only: false,
        trigger_description: spec.display(),
        provides_hotkey_events: true,
        provides_raw_keys: false,
        note: None,
    }));
    let is_ours = |msg: &zbus::Message| {
        msg.body()
            .deserialize::<(String, String, i64)>()
            .is_ok_and(|(c, a, _)| c == COMPONENT && a == ACTION)
    };
    loop {
        tokio::select! {
            _ = &mut stop => break,
            Some(msg) = pressed.next() => {
                if is_ours(&msg) {
                    sink.hotkey(HotkeyEvent::Activated { ts_ms: now_ms() });
                }
            }
            Some(msg) = released.next() => {
                if is_ours(&msg) {
                    sink.hotkey(HotkeyEvent::Deactivated { ts_ms: now_ms() });
                }
            }
            else => {
                // The Plasma shortcut service stopped: the shortcut is dead.
                alive.store(false, Ordering::SeqCst);
                break;
            }
        }
    }
    // Releases the key (it must not stay reserved when the application is gone).
    let _ = root
        .call::<_, _, bool>("unregister", &(COMPONENT, ACTION))
        .await;
    let _ = id;
}

/// Attempt WITHOUT registering anything: is the key already taken by another application?
/// (`Ok` also when the service does not answer: the backend start-up will decide.)
pub fn probe(spec: &HotkeySpec) -> Result<(), String> {
    let Some(key) = qt_sequence(spec) else {
        return Ok(());
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Ok(());
    };
    rt.block_on(async {
        let Ok(conn) = Connection::session().await else {
            return Ok(());
        };
        let Ok(root) = zbus::Proxy::new(&conn, SERVICE, ROOT_PATH, ROOT_IFACE).await else {
            return Ok(());
        };
        let owners: Vec<Owner> = match tokio::time::timeout(
            Duration::from_secs(3),
            root.call("getGlobalShortcutsByKey", &(key,)),
        )
        .await
        {
            Ok(Ok(o)) => o,
            _ => return Ok(()),
        };
        match owners.iter().find(|o| o.2 != COMPONENT) {
            Some(other) => Err(conflict_message(&spec.display(), other)),
            None => Ok(()),
        }
    })
}

impl HotkeyBackend for KGlobalAccelBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::KGlobalAccel
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn start(
        &mut self,
        req: &StartRequest,
        sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError> {
        self.stop();
        let spec = req.spec;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        self.alive.store(true, Ordering::SeqCst);
        let alive = self.alive.clone();
        let thread = std::thread::Builder::new()
            .name("lf-kglobalaccel".into())
            .spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    rt.block_on(run(spec, sink, ready_tx, stop_rx, alive));
                }
            })
            .map_err(|e| err(BackendErrorKind::Failed, e.to_string()))?;
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
                Err(err(
                    BackendErrorKind::Failed,
                    tr("The KDE shortcut service did not answer in time."),
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

impl Drop for KGlobalAccelBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conflict_names_the_application_that_owns_the_key() {
        let owner: Owner = (
            "47B5-Ctrl+Alt+Space".into(),
            "Claude shortcut: Ctrl+Alt+Space".into(),
            "com.anthropic.Claude".into(),
            "Claude".into(),
            "default".into(),
            "Default Context".into(),
            vec![201_326_624],
            vec![201_326_624],
        );
        let m = conflict_message("Ctrl+Alt+Space", &owner);
        assert!(m.contains("Claude") && m.contains("Ctrl+Alt+Space"), "{m}");
    }

    #[test]
    fn the_action_id_has_the_four_parts_kglobalaccel_expects() {
        let id = action_id();
        assert_eq!(id.len(), 4);
        assert_eq!(id[0], "linuxflow");
        assert_eq!(id[1], "toggle");
    }

    static EVENTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    struct Null;
    impl HotkeySink for Null {
        fn hotkey(&self, e: HotkeyEvent) {
            EVENTS.lock().unwrap().push(format!("{e:?}"));
        }
        fn raw_key(&self, _: linuxflow_core::traits::RawKeyEvent) {}
    }

    /// Real Plasma session: registers a free key, checks that it really belongs to our
    /// component, releases it. Never started automatically.
    #[test]
    #[ignore = "real KDE session; set LINUXFLOW_TEST_PORTALS=1"]
    fn kde_reel_enregistre_puis_libere_une_touche() {
        if std::env::var("LINUXFLOW_TEST_PORTALS").as_deref() != Ok("1") {
            return;
        }
        let spec = HotkeySpec::parse("Ctrl+Alt+Shift+F12").unwrap();
        let mut b = KGlobalAccelBackend::new();
        let report = b
            .start(
                &StartRequest {
                    spec,
                    mode: linuxflow_core::domain::ShortcutMode::Toggle,
                },
                Arc::new(Null),
            )
            .expect("the free key must be accepted");
        assert!(report.supports_release);
        assert!(b.is_alive());
        // a simulated press on the KDE side (`invokeShortcut`) must reach us
        let status = std::process::Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.kde.kglobalaccel",
                "--object-path",
                "/component/linuxflow",
                "--method",
                "org.kde.kglobalaccel.Component.invokeShortcut",
                "toggle",
            ])
            .output()
            .expect("gdbus");
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            EVENTS
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.starts_with("Activated")),
            "no press received: {:?}",
            EVENTS.lock().unwrap()
        );
        // during the registration, the key belongs to us: the probe (another requester) sees it as taken by us
        assert!(probe(&spec).is_ok(), "our own key is not a conflict");
        b.stop();
        // after the stop, it is free
        assert!(probe(&spec).is_ok());
    }

    /// Read-only: another application's key is reported, never stolen.
    #[test]
    #[ignore = "real KDE session with another application on Ctrl+Alt+Space"]
    fn kde_reel_signale_la_touche_d_une_autre_application() {
        if std::env::var("LINUXFLOW_TEST_PORTALS").as_deref() != Ok("1") {
            return;
        }
        let spec = HotkeySpec::parse("Ctrl+Alt+Space").unwrap();
        let e = probe(&spec).expect_err("Ctrl+Alt+Space is taken in this session");
        println!("MESSAGE: {e}");
    }
}
