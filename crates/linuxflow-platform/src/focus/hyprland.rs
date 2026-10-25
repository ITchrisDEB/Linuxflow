//! Hyprland: `j/activewindow` request on the command socket.

use super::IPC_TIMEOUT;
use linuxflow_core::domain::WindowInfo;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// `$XDG_RUNTIME_DIR/hypr/<signature>/.socket.sock`.
pub(super) fn socket_path(runtime_dir: &str, signature: &str) -> PathBuf {
    Path::new(runtime_dir)
        .join("hypr")
        .join(signature)
        .join(".socket.sock")
}

/// Sends a request and reads the answer until Hyprland closes the socket.
pub(super) fn request(socket: &Path, command: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(IPC_TIMEOUT))?;
    stream.set_write_timeout(Some(IPC_TIMEOUT))?;
    stream.write_all(command.as_bytes())?;
    let mut out = Vec::new();
    // Bounds the size: an active window fits in a few hundred bytes.
    stream.take(1 << 20).read_to_end(&mut out)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

pub(super) fn active_window(socket: &Path) -> Option<WindowInfo> {
    let reply = request(socket, "j/activewindow").ok()?;
    parse_active_window(&reply)
}

/// Parses the JSON answer. `{}` (or empty) = no active window.
pub(super) fn parse_active_window(reply: &str) -> Option<WindowInfo> {
    let value: serde_json::Value = serde_json::from_str(reply.trim()).ok()?;
    let obj = value.as_object()?;
    let text = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let app_id = text("class").or_else(|| text("initialClass"));
    let pid = obj
        .get("pid")
        .and_then(|v| v.as_i64())
        .filter(|p| *p > 0)
        .and_then(|p| u32::try_from(p).ok());
    if app_id.is_none() && pid.is_none() {
        return None;
    }
    Some(WindowInfo {
        app_id,
        pid,
        title: text("title"),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    const KITTY: &str = include_str!("fixtures/hyprland_activewindow.json");
    const EMPTY: &str = include_str!("fixtures/hyprland_activewindow_none.json");

    #[test]
    fn parses_active_window() {
        let w = parse_active_window(KITTY).expect("window");
        assert_eq!(w.app_id.as_deref(), Some("kitty"));
        assert_eq!(w.pid, Some(48213));
        assert_eq!(w.title.as_deref(), Some("~/projets — zsh"));
    }

    #[test]
    fn empty_object_means_no_window() {
        assert_eq!(parse_active_window(EMPTY), None);
        assert_eq!(parse_active_window(""), None);
        assert_eq!(parse_active_window("not json"), None);
        assert_eq!(parse_active_window("[]"), None);
    }

    #[test]
    fn falls_back_to_initial_class() {
        let w =
            parse_active_window(r#"{"class":"","initialClass":"Firefox","pid":-1}"#).expect("w");
        assert_eq!(w.app_id.as_deref(), Some("Firefox"));
        assert_eq!(w.pid, None);
    }

    #[test]
    fn talks_to_a_fake_socket() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = socket_path(dir.path().to_str().expect("utf8"), "sig");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).expect("read");
            assert_eq!(&buf[..n], b"j/activewindow");
            s.write_all(KITTY.as_bytes()).expect("write");
        });
        let w = active_window(&path).expect("window");
        assert_eq!(w.app_id.as_deref(), Some("kitty"));
        server.join().expect("join");
    }
}
