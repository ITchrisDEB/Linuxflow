//! X11 injection through the XTEST extension (`fake_input`). Reads the real keyboard state (XQueryKeymap)
//! to release the modifiers that are still held.

use std::sync::Mutex;

use linuxflow_core::domain::PasteMode;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use super::backend::{Delivery, InjectError, KeyInjector};
use super::plan::{Key, KeyEvent};

/// X11 keyboard table (copy of `GetKeyboardMapping`), independent of the connection for the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardMap {
    pub min_keycode: u8,
    pub keysyms_per_keycode: usize,
    pub keysyms: Vec<u32>,
}

impl KeyboardMap {
    /// Keycode carrying `keysym` in the first two columns (group 1, without/with Shift),
    /// i.e. on the default active layout. Independent of the physical layout.
    pub fn keycode_for(&self, keysym: u32) -> Option<u8> {
        if self.keysyms_per_keycode == 0 {
            return None;
        }
        let cols = self.keysyms_per_keycode.min(2);
        // Looks first in column 0 then in column 1 (upper-case V).
        for col in 0..cols {
            for (idx, chunk) in self.keysyms.chunks(self.keysyms_per_keycode).enumerate() {
                if chunk.get(col) == Some(&keysym) {
                    let code = usize::from(self.min_keycode) + idx;
                    return u8::try_from(code).ok();
                }
            }
        }
        // The letter V may only exist in upper case (column 1): second pass on 'V'.
        if keysym == u32::from(b'v') {
            return self.keycode_for(u32::from(b'V'));
        }
        None
    }

    /// Held keys from a `QueryKeymap` (32 bytes, one bit per keycode).
    pub fn held_keys(&self, bitmap: &[u8; 32]) -> Vec<Key> {
        Key::RELEASABLE
            .iter()
            .copied()
            .filter(|key| {
                self.keycode_for(key.keysym())
                    .is_some_and(|code| bitmap[usize::from(code / 8)] & (1 << (code % 8)) != 0)
            })
            .collect()
    }
}

struct Conn {
    conn: RustConnection,
    root: u32,
    max_keycode: u8,
    map: KeyboardMap,
}

pub struct XTestInjector {
    conn: Mutex<Option<Conn>>,
}

impl Default for XTestInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl XTestInjector {
    pub fn new() -> Self {
        Self {
            conn: Mutex::new(None),
        }
    }

    fn connect() -> Result<Conn, InjectError> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| InjectError::Unavailable(format!("X11 : {e}")))?;
        let setup = conn.setup();
        let root = setup
            .roots
            .get(screen_num)
            .map(|s| s.root)
            .ok_or_else(|| InjectError::Unavailable("X11: screen not found".to_string()))?;
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        if conn
            .xtest_get_version(2, 2)
            .map_err(unavailable)?
            .reply()
            .is_err()
        {
            return Err(InjectError::Unavailable(
                "extension XTEST absente".to_string(),
            ));
        }
        let map = Self::read_map(&conn, min, max)?;
        Ok(Conn {
            conn,
            root,
            max_keycode: max,
            map,
        })
    }

    /// Reads the CURRENT keyboard layout. Read again at every paste: a layout change
    /// (`setxkbmap`, XKB group) must not make the injector send a letter other than V.
    fn read_map(conn: &RustConnection, min: u8, max: u8) -> Result<KeyboardMap, InjectError> {
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(unavailable)?
            .reply()
            .map_err(unavailable)?;
        Ok(KeyboardMap {
            min_keycode: min,
            keysyms_per_keycode: usize::from(reply.keysyms_per_keycode),
            keysyms: reply.keysyms,
        })
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&Conn) -> Result<T, InjectError>,
    ) -> Result<T, InjectError> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| InjectError::Failed("poisoned X11 lock".into()))?;
        if guard.is_none() {
            *guard = Some(Self::connect()?);
        }
        let result = match guard.as_ref() {
            Some(c) => f(c),
            None => Err(InjectError::Unavailable("no X11 connection".into())),
        };
        if matches!(result, Err(InjectError::Failed(_))) {
            // Connection possibly dead: reconnect at the next call.
            *guard = None;
        }
        result
    }

    fn emit(c: &Conn, events: &[KeyEvent]) -> Result<(), InjectError> {
        // Current layout; if reading fails, fall back to the one of the connection.
        let fresh = Self::read_map(&c.conn, c.map.min_keycode, c.max_keycode).ok();
        let map = fresh.as_ref().unwrap_or(&c.map);
        for e in events {
            let keysym = e.key.keysym();
            let code = map
                .keycode_for(keysym)
                // Fallback on the QWERTY physical code ONLY for the function keys and the
                // modifiers (present in every layout). A letter absent from the
                // layout gives a plain error: never another letter at random.
                .or_else(|| {
                    (keysym >= 0xff00)
                        .then(|| u8::try_from(e.key.evdev_code() + 8).ok())
                        .flatten()
                })
                .ok_or_else(|| {
                    InjectError::Failed("key not found in the current keyboard layout".into())
                })?;
            let kind = if e.down {
                KEY_PRESS_EVENT
            } else {
                KEY_RELEASE_EVENT
            };
            c.conn
                .xtest_fake_input(kind, code, 0, c.root, 0, 0, 0)
                .map_err(failed)?;
        }
        c.conn.flush().map_err(failed)?;
        // Round trip: surfaces the server's asynchronous errors.
        c.conn
            .get_input_focus()
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        Ok(())
    }
}

fn unavailable(e: impl std::fmt::Display) -> InjectError {
    InjectError::Unavailable(format!("X11 : {e}"))
}

fn failed(e: impl std::fmt::Display) -> InjectError {
    InjectError::Failed(format!("X11 : {e}"))
}

impl KeyInjector for XTestInjector {
    fn name(&self) -> &'static str {
        "xtest"
    }

    fn probe(&self) -> Result<(), InjectError> {
        self.with_conn(|_| Ok(()))
    }

    fn held_keys(&self) -> Option<Vec<Key>> {
        self.with_conn(|c| {
            let reply = c
                .conn
                .query_keymap()
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            Ok(c.map.held_keys(&reply.keys))
        })
        .ok()
    }

    fn send_release(&self, events: &[KeyEvent]) -> Result<(), InjectError> {
        self.with_conn(|c| Self::emit(c, events))
    }

    fn send_chord(&self, _mode: PasteMode, events: &[KeyEvent]) -> Result<Delivery, InjectError> {
        self.with_conn(|c| Self::emit(c, events))
            .map(|()| Delivery::Sent)
    }
}

// Avoids an unused-import warning if `xproto` is only used for constants.
#[allow(dead_code)]
type _Xproto = xproto::Window;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Fake layout: keycode 8 + idx. 2 columns.
    fn map(rows: &[(u32, u32)]) -> KeyboardMap {
        KeyboardMap {
            min_keycode: 8,
            keysyms_per_keycode: 2,
            keysyms: rows.iter().flat_map(|(a, b)| [*a, *b]).collect(),
        }
    }

    #[test]
    fn a_letter_missing_from_the_layout_is_not_replaced_by_a_qwerty_code() {
        // Layout without `v` (for example non-Latin only): keycode_for returns None, and only
        // the function keys/modifiers (keysym >= 0xff00) are entitled to the physical fallback.
        let cyrillic = map(&[(0x6c4, 0x6e4), (0x6c3, 0x6e3)]);
        assert_eq!(cyrillic.keycode_for(0x76), None);
    }

    #[test]
    fn finds_v_wherever_the_layout_puts_it() {
        // QWERTY: v at position 3; Dvorak: v elsewhere.
        let qwerty = map(&[
            (0, 0),
            (u32::from(b'c'), u32::from(b'C')),
            (u32::from(b'v'), u32::from(b'V')),
        ]);
        assert_eq!(qwerty.keycode_for(0x76), Some(10));
        let dvorak = map(&[
            (u32::from(b'v'), u32::from(b'V')),
            (u32::from(b'c'), u32::from(b'C')),
        ]);
        assert_eq!(dvorak.keycode_for(0x76), Some(8));
    }

    #[test]
    fn falls_back_to_uppercase_only_entry() {
        let m = map(&[(0, u32::from(b'V'))]);
        assert_eq!(m.keycode_for(0x76), Some(8));
    }

    #[test]
    fn missing_keysym_is_none() {
        assert_eq!(map(&[(1, 2)]).keycode_for(0x76), None);
        let empty = KeyboardMap {
            min_keycode: 8,
            keysyms_per_keycode: 0,
            keysyms: vec![],
        };
        assert_eq!(empty.keycode_for(0x76), None);
    }

    #[test]
    fn held_keys_reads_the_bitmap() {
        // keycode 8 = Control_L, keycode 9 = Shift_L
        let m = map(&[(0xffe3, 0), (0xffe1, 0)]);
        let mut bitmap = [0u8; 32];
        assert!(m.held_keys(&bitmap).is_empty());
        bitmap[1] |= 1; // keycode 8
        assert_eq!(m.held_keys(&bitmap), vec![Key::LeftCtrl]);
        bitmap[1] |= 2; // keycode 9
        assert_eq!(m.held_keys(&bitmap), vec![Key::LeftShift, Key::LeftCtrl]);
    }
}
