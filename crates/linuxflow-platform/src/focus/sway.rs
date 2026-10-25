//! Sway: minimal client of the i3-ipc protocol (`get_tree`) on `SWAYSOCK`.
//!
//! Frame: `i3-ipc` magic (6 bytes), `u32` length and `u32` type in native order, then the payload.

use super::IPC_TIMEOUT;
use linuxflow_core::domain::WindowInfo;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

const MAGIC: &[u8; 6] = b"i3-ipc";
const GET_TREE: u32 = 4;
/// Safety bound: a very busy Sway tree stays far below it.
const MAX_REPLY: usize = 32 << 20;

/// Builds an i3-ipc request.
pub(super) fn encode_request(kind: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    out.extend_from_slice(payload);
    out
}

/// Reads a complete answer (header + payload) and returns `(type, payload)`.
pub(super) fn read_reply(stream: &mut impl Read) -> std::io::Result<(u32, Vec<u8>)> {
    let mut header = [0u8; 14];
    stream.read_exact(&mut header)?;
    if &header[..6] != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid i3-ipc magic",
        ));
    }
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let kind = u32::from_ne_bytes([header[10], header[11], header[12], header[13]]);
    if len > MAX_REPLY {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "i3-ipc reply too large",
        ));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok((kind, payload))
}

fn get_tree(socket: &Path) -> std::io::Result<Vec<u8>> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(IPC_TIMEOUT))?;
    stream.set_write_timeout(Some(IPC_TIMEOUT))?;
    stream.write_all(&encode_request(GET_TREE, b""))?;
    let (kind, payload) = read_reply(&mut stream)?;
    if kind != GET_TREE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unexpected i3-ipc reply type",
        ));
    }
    Ok(payload)
}

pub(super) fn active_window(socket: &Path) -> Option<WindowInfo> {
    let payload = get_tree(socket).ok()?;
    parse_tree(&payload)
}

/// Looks for the `focused` node (a window, not a container) in the `get_tree` answer.
pub(super) fn parse_tree(payload: &[u8]) -> Option<WindowInfo> {
    let root: serde_json::Value = serde_json::from_slice(payload).ok()?;
    find_focused(&root, 0)
}

/// Maximum depth (safeguard against a pathological answer).
const MAX_DEPTH: usize = 64;

fn find_focused(node: &serde_json::Value, depth: usize) -> Option<WindowInfo> {
    if depth > MAX_DEPTH {
        return None;
    }
    if node.get("focused").and_then(|f| f.as_bool()) == Some(true) {
        return window_of(node);
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(|c| c.as_array()) {
            if let Some(found) = children.iter().find_map(|c| find_focused(c, depth + 1)) {
                return Some(found);
            }
        }
    }
    None
}

/// Extracts the window from a node; a container without a client (empty workspace) gives `None`.
fn window_of(node: &serde_json::Value) -> Option<WindowInfo> {
    let text = |v: Option<&serde_json::Value>| {
        v.and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let props = node.get("window_properties");
    // `app_id` for native Wayland clients, X11 class for XWayland.
    let app_id = text(node.get("app_id")).or_else(|| text(props.and_then(|p| p.get("class"))));
    let pid = node
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
        title: text(node.get("name")),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    const TREE_WAYLAND: &str = include_str!("fixtures/sway_tree_wayland.json");
    const TREE_XWAYLAND: &str = include_str!("fixtures/sway_tree_xwayland.json");
    const TREE_EMPTY: &str = include_str!("fixtures/sway_tree_empty_workspace.json");

    #[test]
    fn finds_native_wayland_window() {
        let w = parse_tree(TREE_WAYLAND.as_bytes()).expect("window");
        assert_eq!(w.app_id.as_deref(), Some("org.wezfurlong.wezterm"));
        assert_eq!(w.pid, Some(3107));
        assert_eq!(w.title.as_deref(), Some("wezterm — ~/code"));
    }

    #[test]
    fn finds_floating_xwayland_window_by_class() {
        let w = parse_tree(TREE_XWAYLAND.as_bytes()).expect("window");
        assert_eq!(w.app_id.as_deref(), Some("Slack"));
        assert_eq!(w.pid, Some(5522));
    }

    #[test]
    fn empty_workspace_has_no_window() {
        assert_eq!(parse_tree(TREE_EMPTY.as_bytes()), None);
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(parse_tree(b""), None);
        assert_eq!(parse_tree(b"{"), None);
        assert_eq!(parse_tree(b"[1,2]"), None);
    }

    #[test]
    fn frame_roundtrip() {
        let req = encode_request(GET_TREE, b"");
        assert_eq!(&req[..6], b"i3-ipc");
        assert_eq!(req.len(), 14);
        let mut framed = encode_request(GET_TREE, b"{\"a\":1}");
        let (kind, payload) = read_reply(&mut framed.as_slice()).expect("trame");
        assert_eq!((kind, payload.as_slice()), (GET_TREE, &b"{\"a\":1}"[..]));
        framed[0] = b'x';
        assert!(read_reply(&mut framed.as_slice()).is_err());
        // Truncated frame.
        assert!(read_reply(&mut &req[..10]).is_err());
    }

    #[test]
    fn talks_to_a_fake_socket() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("sway.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let mut req = [0u8; 14];
            s.read_exact(&mut req).expect("read");
            assert_eq!(req.as_slice(), encode_request(GET_TREE, b"").as_slice());
            s.write_all(&encode_request(GET_TREE, TREE_WAYLAND.as_bytes()))
                .expect("write");
        });
        let w = active_window(&path).expect("window");
        assert_eq!(w.pid, Some(3107));
        server.join().expect("join");
    }
}
