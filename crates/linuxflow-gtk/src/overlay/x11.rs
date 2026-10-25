//! X11 settings of the pill window (EWMH hints, XShape) through `x11rb`.
//!
//! Each function opens its own connection: the properties set stay on the window
//! after the connection is closed. Every error is returned as text (never a panic).

use x11rb::connection::Connection;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, PropMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn atom(conn: &RustConnection, name: &str) -> Res<u32> {
    Ok(conn
        .intern_atom(false, name.as_bytes())
        .map_err(err)?
        .reply()
        .map_err(err)?
        .atom)
}

/// Sets window type, state, desktops, "does not accept focus" and empty input.
/// To call once the window is realised; `apply_state_message` must follow once the window is shown.
pub fn prepare_window(xid: u32) -> Res<()> {
    let (conn, _) = RustConnection::connect(None).map_err(err)?;
    let net_type = atom(&conn, "_NET_WM_WINDOW_TYPE")?;
    let notif = atom(&conn, "_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
    let net_state = atom(&conn, "_NET_WM_STATE")?;
    let states = state_atoms(&conn)?;
    let desktop = atom(&conn, "_NET_WM_DESKTOP")?;

    conn.change_property32(PropMode::REPLACE, xid, net_type, AtomEnum::ATOM, &[notif])
        .map_err(err)?;
    conn.change_property32(PropMode::REPLACE, xid, net_state, AtomEnum::ATOM, &states)
        .map_err(err)?;
    // All desktops.
    conn.change_property32(
        PropMode::REPLACE,
        xid,
        desktop,
        AtomEnum::CARDINAL,
        &[0xFFFF_FFFF],
    )
    .map_err(err)?;
    // WM_HINTS: InputHint (bit 0) with input = False -> does not take focus.
    let mut hints = [0u32; 9];
    hints[0] = 1;
    conn.change_property32(
        PropMode::REPLACE,
        xid,
        AtomEnum::WM_HINTS,
        AtomEnum::WM_HINTS,
        &hints,
    )
    .map_err(err)?;
    set_empty_input_shape_on(&conn, xid)?;
    conn.flush().map_err(err)?;
    Ok(())
}

fn state_atoms(conn: &RustConnection) -> Res<[u32; 4]> {
    Ok([
        atom(conn, "_NET_WM_STATE_ABOVE")?,
        atom(conn, "_NET_WM_STATE_STICKY")?,
        atom(conn, "_NET_WM_STATE_SKIP_TASKBAR")?,
        atom(conn, "_NET_WM_STATE_SKIP_PAGER")?,
    ])
}

/// Asks the window manager to add ABOVE|STICKY|SKIP_TASKBAR|SKIP_PAGER
/// (EWMH method for a window that is already shown).
pub fn apply_state_message(xid: u32) -> Res<()> {
    let (conn, screen) = RustConnection::connect(None).map_err(err)?;
    let root = conn
        .setup()
        .roots
        .get(screen)
        .ok_or("X screen not found")?
        .root;
    let net_state = atom(&conn, "_NET_WM_STATE")?;
    let s = state_atoms(&conn)?;
    for pair in [[s[0], s[1]], [s[2], s[3]]] {
        let ev = ClientMessageEvent::new(32, xid, net_state, [1, pair[0], pair[1], 1, 0]);
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        )
        .map_err(err)?;
    }
    conn.flush().map_err(err)?;
    Ok(())
}

/// Empty input region (XShape): the mouse goes through the window.
pub fn set_empty_input_shape(xid: u32) -> Res<()> {
    let (conn, _) = RustConnection::connect(None).map_err(err)?;
    set_empty_input_shape_on(&conn, xid)?;
    conn.flush().map_err(err)
}

fn set_empty_input_shape_on(conn: &RustConnection, xid: u32) -> Res<()> {
    conn.shape_rectangles(
        shape::SO::SET,
        shape::SK::INPUT,
        x11rb::protocol::xproto::ClipOrdering::UNSORTED,
        xid,
        0,
        0,
        &[],
    )
    .map_err(err)?;
    Ok(())
}

/// Moves the window (screen coordinates).
pub fn move_window(xid: u32, x: i32, y: i32) -> Res<()> {
    let (conn, _) = RustConnection::connect(None).map_err(err)?;
    conn.configure_window(xid, &ConfigureWindowAux::new().x(x).y(y))
        .map_err(err)?;
    conn.flush().map_err(err)
}

/// EWMH state then position (if known), once the window is shown.
pub fn apply_window_state(xid: u32, position: Option<(i32, i32)>) -> Res<()> {
    apply_state_message(xid)?;
    if let Some((x, y)) = position {
        move_window(xid, x, y)?;
    }
    Ok(())
}
