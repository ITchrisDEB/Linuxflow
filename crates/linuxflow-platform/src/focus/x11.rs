//! X11 : EWMH (`_NET_ACTIVE_WINDOW`) via x11rb.

use linuxflow_core::domain::WindowInfo;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

type BoxError = Box<dyn std::error::Error>;

/// Window class from a `WM_CLASS` property: `instance\0class\0`. The class is kept
/// (second string), otherwise the instance.
pub(super) fn parse_wm_class(raw: &[u8]) -> Option<String> {
    let mut parts = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned());
    let instance = parts.next()?;
    Some(parts.next().unwrap_or(instance))
}

/// Process name from `/proc/<pid>/comm` (fallback when the window has no `WM_CLASS`).
fn process_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let comm = comm.trim();
    (!comm.is_empty()).then(|| comm.to_owned())
}

pub(super) fn active_window() -> Option<WindowInfo> {
    query().ok().flatten()
}

fn atom(conn: &RustConnection, name: &str) -> Result<Atom, BoxError> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn query() -> Result<Option<WindowInfo>, BoxError> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or("no X11 screen")?
        .root;

    let active = atom(&conn, "_NET_ACTIVE_WINDOW")?;
    let reply = conn
        .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)?
        .reply()?;
    let window: Window = match reply.value32().and_then(|mut v| v.next()) {
        Some(w) if w != 0 => w,
        _ => return Ok(None),
    };

    let class = conn
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?
        .reply()
        .ok()
        .and_then(|r| parse_wm_class(&r.value));

    let pid_atom = atom(&conn, "_NET_WM_PID")?;
    let pid = conn
        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)?
        .reply()
        .ok()
        .and_then(|r| r.value32().and_then(|mut v| v.next()))
        .filter(|p| *p > 0);

    let name_atom = atom(&conn, "_NET_WM_NAME")?;
    let utf8 = atom(&conn, "UTF8_STRING")?;
    let mut title = conn
        .get_property(false, window, name_atom, utf8, 0, 256)?
        .reply()
        .ok()
        .map(|r| String::from_utf8_lossy(&r.value).into_owned())
        .filter(|t| !t.is_empty());
    if title.is_none() {
        title = conn
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)?
            .reply()
            .ok()
            .map(|r| String::from_utf8_lossy(&r.value).into_owned())
            .filter(|t| !t.is_empty());
    }

    let app_id = class.or_else(|| pid.and_then(process_name));
    Ok(Some(WindowInfo { app_id, pid, title }))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn wm_class_uses_second_string() {
        assert_eq!(
            parse_wm_class(b"gnome-terminal-server\0Gnome-terminal\0").as_deref(),
            Some("Gnome-terminal")
        );
        assert_eq!(parse_wm_class(b"kitty\0kitty\0").as_deref(), Some("kitty"));
    }

    #[test]
    fn wm_class_falls_back_to_instance() {
        assert_eq!(parse_wm_class(b"xterm").as_deref(), Some("xterm"));
        assert_eq!(parse_wm_class(b"xterm\0").as_deref(), Some("xterm"));
    }

    #[test]
    fn wm_class_empty_is_none() {
        assert_eq!(parse_wm_class(b""), None);
        assert_eq!(parse_wm_class(b"\0\0"), None);
    }

    #[test]
    fn process_name_of_unknown_pid_is_none() {
        assert_eq!(process_name(u32::MAX), None);
    }

    /// Needs an X11 server: `LINUXFLOW_TEST_DESKTOP=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_x11_does_not_panic() {
        if std::env::var("LINUXFLOW_TEST_DESKTOP").as_deref() != Ok("1") {
            return;
        }
        let _ = active_window();
    }
}
