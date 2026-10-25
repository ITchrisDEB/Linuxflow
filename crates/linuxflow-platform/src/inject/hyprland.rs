//! Hyprland: `dispatch sendshortcut` sent directly over Hyprland's command socket
//! (the same protocol as `hyprctl`, without running any external program), without a virtual device.
//!
//! Protocol (the one of `hyprctl`): connect to
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`, write `/<command>` (the slash
//! stands in for the options), read the answer until Hyprland closes the socket.
//! A successful command answers `ok`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use linuxflow_core::domain::PasteMode;

use super::backend::{Delivery, InjectError, KeyInjector};
use super::plan::KeyEvent;

/// Exchange with Hyprland (replaced in the tests).
pub trait HyprIpc: Send + Sync {
    /// Sends the command (without the leading slash) and returns the answer, or a message
    /// d'erreur.
    fn command(&self, command: &str) -> Result<String, String>;
    /// `true` if the session's command socket exists.
    fn available(&self) -> bool;
}

/// Message returned when Hyprland does not answer within the delay.
pub const RUN_TIMED_OUT: &str = "timed out";
const IPC_DEADLINE: Duration = Duration::from_secs(2);
/// Maximum size of an answer (a `dispatch` answers a few bytes).
const MAX_REPLY: u64 = 1 << 20;

/// Command socket of the current Hyprland session, from the environment.
pub fn socket_from_env() -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|v| !v.is_empty())?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: `getuid` has no precondition and cannot fail.
            PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
        });
    Some(runtime.join("hypr").join(signature).join(".socket.sock"))
}

/// Command socket client (production).
#[derive(Debug, Clone)]
pub struct SocketIpc {
    path: Option<PathBuf>,
}

impl SocketIpc {
    pub fn from_env() -> Self {
        Self {
            path: socket_from_env(),
        }
    }

    /// Explicit socket (tests).
    pub fn with_path(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }
}

impl SocketIpc {
    /// JSON request (`j/<command>`), for example `binds` or `activewindow`.
    pub fn json(&self, command: &str) -> Result<String, String> {
        self.exchange(&format!("j/{command}"))
    }

    fn exchange(&self, request: &str) -> Result<String, String> {
        let path = self.path.as_ref().ok_or("no Hyprland socket")?;
        let timed_out = |e: &std::io::Error| {
            matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
        };
        let fail = |e: std::io::Error| {
            if timed_out(&e) {
                RUN_TIMED_OUT.to_string()
            } else {
                e.to_string()
            }
        };
        let mut stream = UnixStream::connect(path).map_err(fail)?;
        stream.set_read_timeout(Some(IPC_DEADLINE)).map_err(fail)?;
        stream.set_write_timeout(Some(IPC_DEADLINE)).map_err(fail)?;
        stream.write_all(request.as_bytes()).map_err(fail)?;
        let mut reply = Vec::new();
        stream
            .take(MAX_REPLY)
            .read_to_end(&mut reply)
            .map_err(fail)?;
        Ok(String::from_utf8_lossy(&reply).trim().to_string())
    }
}

impl HyprIpc for SocketIpc {
    fn command(&self, command: &str) -> Result<String, String> {
        self.exchange(&format!("/{command}"))
    }

    fn available(&self) -> bool {
        self.path.as_ref().is_some_and(|p| p.exists())
    }
}

/// Argument of `sendshortcut` (`MODS,key,window`): the key name is resolved by the
/// compositor according to the active layout.
pub fn shortcut_arg(mode: PasteMode) -> &'static str {
    match mode {
        PasteMode::CtrlV => "CTRL,V,activewindow",
        PasteMode::CtrlShiftV => "CTRL SHIFT,V,activewindow",
        PasteMode::ShiftInsert => "SHIFT,Insert,activewindow",
    }
}

pub struct HyprlandInjector<I: HyprIpc = SocketIpc> {
    ipc: I,
    in_hyprland: bool,
}

impl HyprlandInjector<SocketIpc> {
    pub fn new(in_hyprland: bool) -> Self {
        Self {
            ipc: SocketIpc::from_env(),
            in_hyprland,
        }
    }
}

impl<I: HyprIpc> HyprlandInjector<I> {
    pub fn with_ipc(ipc: I, in_hyprland: bool) -> Self {
        Self { ipc, in_hyprland }
    }
}

impl<I: HyprIpc> KeyInjector for HyprlandInjector<I> {
    fn name(&self) -> &'static str {
        "hyprland-sendshortcut"
    }

    fn probe(&self) -> Result<(), InjectError> {
        if !self.in_hyprland {
            return Err(InjectError::Unavailable("not a Hyprland session".into()));
        }
        if !self.ipc.available() {
            return Err(InjectError::Unavailable(
                "Hyprland command socket not found".into(),
            ));
        }
        Ok(())
    }

    fn send_chord(&self, mode: PasteMode, _events: &[KeyEvent]) -> Result<Delivery, InjectError> {
        let command = format!("dispatch sendshortcut {}", shortcut_arg(mode));
        match self.ipc.command(&command) {
            Ok(out) if out.trim().eq_ignore_ascii_case("ok") => Ok(Delivery::Sent),
            // Unexpected answer: the command went through but without confirmation.
            Ok(_) => Ok(Delivery::Unconfirmed),
            // No answer: it is unknown whether the shortcut left, hence neither a retry nor a plain failure.
            Err(e) if e == RUN_TIMED_OUT => Err(InjectError::TimedOut("Hyprland".to_string())),
            Err(e) => Err(InjectError::Failed(format!("Hyprland: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;

    struct Fake {
        calls: Mutex<Vec<String>>,
        output: Result<String, String>,
        available: bool,
    }
    impl HyprIpc for Fake {
        fn command(&self, command: &str) -> Result<String, String> {
            self.calls.lock().unwrap().push(command.to_string());
            self.output.clone()
        }
        fn available(&self) -> bool {
            self.available
        }
    }

    fn fake(output: Result<String, String>, available: bool) -> Fake {
        Fake {
            calls: Mutex::new(vec![]),
            output,
            available,
        }
    }

    #[test]
    fn talks_the_hyprctl_protocol_over_a_real_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".socket.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 128];
            let n = s.read(&mut buf).unwrap();
            assert_eq!(
                &buf[..n],
                b"/dispatch sendshortcut CTRL,V,activewindow".as_slice()
            );
            s.write_all(b"ok").unwrap();
        });
        let ipc = SocketIpc::with_path(path);
        assert!(ipc.available());
        let inj = HyprlandInjector::with_ipc(ipc, true);
        assert_eq!(inj.send_chord(PasteMode::CtrlV, &[]), Ok(Delivery::Sent));
        server.join().unwrap();
    }

    #[test]
    fn a_silent_hyprland_times_out_instead_of_freezing_the_paste_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".socket.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (_s, _) = listener.accept().unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(6)); // keeps the connection open, without answering
        });
        let started = std::time::Instant::now();
        let out = SocketIpc::with_path(path).command("dispatch sendshortcut X");
        assert_eq!(out, Err(RUN_TIMED_OUT.to_string()));
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(release);
        server.join().unwrap();
    }

    #[test]
    fn a_missing_socket_is_unavailable_and_a_failed_connection_is_a_plain_error() {
        let dir = tempfile::tempdir().unwrap();
        let ipc = SocketIpc::with_path(dir.path().join("absent.sock"));
        assert!(!ipc.available());
        assert!(ipc.command("dispatch x").is_err());
        let none = SocketIpc { path: None };
        assert!(!none.available());
        assert!(none.command("x").is_err());
    }

    #[test]
    fn a_timed_out_command_is_a_timeout_error_not_a_plain_failure() {
        let inj = HyprlandInjector::with_ipc(fake(Err(RUN_TIMED_OUT.to_string()), true), true);
        let err = inj.send_chord(PasteMode::CtrlV, &[]).unwrap_err();
        assert!(matches!(err, InjectError::TimedOut(_)), "{err:?}");
    }

    #[test]
    fn arguments_per_mode() {
        assert_eq!(shortcut_arg(PasteMode::CtrlV), "CTRL,V,activewindow");
        assert_eq!(
            shortcut_arg(PasteMode::CtrlShiftV),
            "CTRL SHIFT,V,activewindow"
        );
        assert_eq!(
            shortcut_arg(PasteMode::ShiftInsert),
            "SHIFT,Insert,activewindow"
        );
    }

    #[test]
    fn sends_expected_command_and_reports_sent() {
        let b = HyprlandInjector::with_ipc(fake(Ok("ok".into()), true), true);
        assert_eq!(b.probe(), Ok(()));
        assert_eq!(b.send_chord(PasteMode::CtrlShiftV, &[]), Ok(Delivery::Sent));
        let calls = b.ipc.calls.lock().unwrap();
        assert_eq!(calls[0], "dispatch sendshortcut CTRL SHIFT,V,activewindow");
    }

    #[test]
    fn unexpected_output_is_unconfirmed_and_error_is_failed() {
        let b = HyprlandInjector::with_ipc(fake(Ok("Invalid dispatcher".into()), true), true);
        assert_eq!(
            b.send_chord(PasteMode::CtrlV, &[]),
            Ok(Delivery::Unconfirmed)
        );
        let b = HyprlandInjector::with_ipc(fake(Err("boom".into()), true), true);
        assert!(matches!(
            b.send_chord(PasteMode::CtrlV, &[]),
            Err(InjectError::Failed(_))
        ));
    }

    #[test]
    fn probe_requires_session_and_socket() {
        let b = HyprlandInjector::with_ipc(fake(Ok("ok".into()), true), false);
        assert!(matches!(b.probe(), Err(InjectError::Unavailable(_))));
        let b = HyprlandInjector::with_ipc(fake(Ok("ok".into()), false), true);
        assert!(matches!(b.probe(), Err(InjectError::Unavailable(_))));
    }
}
