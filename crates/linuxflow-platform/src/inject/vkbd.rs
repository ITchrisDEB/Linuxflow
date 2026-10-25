//! Wayland virtual keyboard (`zwp_virtual_keyboard_v1`, wlroots compositors). A minimal XKB keymap
//! minimal one, which belongs to us, is sent before any key: the paste therefore does not depend on
//! the user's layout.

use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::sync::Mutex;
use std::time::Instant;

use linuxflow_core::domain::PasteMode;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

use super::backend::{Delivery, InjectError, KeyInjector};
use super::plan::{Key, KeyEvent};

/// Standard XKB modifier masks (Shift, Control, Mod1 = Alt, Mod4 = Super).
const MOD_SHIFT: u32 = 1;
const MOD_CTRL: u32 = 4;
const MOD_ALT: u32 = 8;
const MOD_SUPER: u32 = 64;

const ALL_KEYS: [Key; 11] = [
    Key::LeftCtrl,
    Key::RightCtrl,
    Key::LeftShift,
    Key::RightShift,
    Key::LeftAlt,
    Key::RightAlt,
    Key::LeftSuper,
    Key::RightSuper,
    Key::Space,
    Key::Insert,
    Key::V,
];

fn keycode_name(key: Key) -> String {
    format!("<K{}>", key.evdev_code())
}

/// Text of the XKB keymap: keycodes = evdev codes + 8, letter V as `v`/`V`.
pub fn keymap_text() -> String {
    let mut s = String::from(
        "xkb_keymap {\nxkb_keycodes \"linuxflow\" {\n minimum = 8;\n maximum = 255;\n",
    );
    for key in ALL_KEYS {
        s.push_str(&format!(
            " {} = {};\n",
            keycode_name(key),
            key.evdev_code() + 8
        ));
    }
    s.push_str("};\nxkb_types \"linuxflow\" { include \"complete\" };\n");
    s.push_str("xkb_compat \"linuxflow\" { include \"complete\" };\n");
    s.push_str("xkb_symbols \"linuxflow\" {\n");
    for key in ALL_KEYS {
        if key == Key::V {
            s.push_str(&format!(" key {} {{ [ v, V ] }};\n", keycode_name(key)));
        } else {
            s.push_str(&format!(
                " key {} {{ [ {} ] }};\n",
                keycode_name(key),
                key.xkb_name()
            ));
        }
    }
    s.push_str(&format!(
        " modifier_map Shift {{ {}, {} }};\n modifier_map Control {{ {}, {} }};\n modifier_map Mod1 {{ {}, {} }};\n modifier_map Mod4 {{ {}, {} }};\n}};\n}};\n",
        keycode_name(Key::LeftShift),
        keycode_name(Key::RightShift),
        keycode_name(Key::LeftCtrl),
        keycode_name(Key::RightCtrl),
        keycode_name(Key::LeftAlt),
        keycode_name(Key::RightAlt),
        keycode_name(Key::LeftSuper),
        keycode_name(Key::RightSuper),
    ));
    s
}

fn mod_bit(key: Key) -> u32 {
    match key {
        Key::LeftShift | Key::RightShift => MOD_SHIFT,
        Key::LeftCtrl | Key::RightCtrl => MOD_CTRL,
        Key::LeftAlt | Key::RightAlt => MOD_ALT,
        Key::LeftSuper | Key::RightSuper => MOD_SUPER,
        _ => 0,
    }
}

/// Protocol operation to emit (the virtual keyboard requires an explicit `modifiers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VkOp {
    Key { evdev: u32, pressed: bool },
    Modifiers { depressed: u32 },
}

/// Translates events into operations, tracking the mask of the pressed modifiers.
pub fn ops_for(events: &[KeyEvent]) -> Vec<VkOp> {
    let mut mask = 0u32;
    let mut ops = Vec::new();
    for e in events {
        let bit = mod_bit(e.key);
        if bit != 0 && e.down {
            ops.push(VkOp::Key {
                evdev: u32::from(e.key.evdev_code()),
                pressed: true,
            });
            mask |= bit;
            ops.push(VkOp::Modifiers { depressed: mask });
        } else if bit != 0 {
            mask &= !bit;
            ops.push(VkOp::Modifiers { depressed: mask });
            ops.push(VkOp::Key {
                evdev: u32::from(e.key.evdev_code()),
                pressed: false,
            });
        } else {
            ops.push(VkOp::Key {
                evdev: u32::from(e.key.evdev_code()),
                pressed: e.down,
            });
        }
    }
    ops
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

struct Live {
    // Kept for the lifetime of the keyboard.
    _conn: Connection,
    queue: EventQueue<State>,
    keyboard: ZwpVirtualKeyboardV1,
    started: Instant,
}

pub struct VirtualKeyboardInjector {
    live: Mutex<Option<Live>>,
}

impl Default for VirtualKeyboardInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualKeyboardInjector {
    pub fn new() -> Self {
        Self {
            live: Mutex::new(None),
        }
    }

    fn connect() -> Result<Live, InjectError> {
        let un = |m: String| InjectError::Unavailable(m);
        let conn = Connection::connect_to_env().map_err(|e| un(format!("wayland: {e}")))?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).map_err(|e| un(format!("wayland: {e}")))?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| un("wl_seat absent".to_string()))?;
        let manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| un("zwp_virtual_keyboard_manager_v1 absent".to_string()))?;
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());

        // Keymap in an anonymous file, sent before any key.
        let text = keymap_text();
        let mut file = tempfile::tempfile().map_err(|e| un(format!("keymap: {e}")))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.write_all(&[0]))
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
            .map_err(|e| un(format!("keymap: {e}")))?;
        let size =
            u32::try_from(text.len() + 1).map_err(|_| un("keymap trop grand".to_string()))?;
        keyboard.keymap(1, file.as_fd(), size); // 1 = XKB_KEYMAP_FORMAT_TEXT_V1
        queue
            .roundtrip(&mut State)
            .map_err(|e| un(format!("wayland: {e}")))?;
        Ok(Live {
            _conn: conn,
            queue,
            keyboard,
            started: Instant::now(),
        })
    }
}

impl KeyInjector for VirtualKeyboardInjector {
    fn name(&self) -> &'static str {
        "wayland-virtual-keyboard"
    }

    fn probe(&self) -> Result<(), InjectError> {
        let mut guard = self
            .live
            .lock()
            .map_err(|_| InjectError::Failed("lock poisoned".into()))?;
        if guard.is_none() {
            *guard = Some(Self::connect()?);
        }
        Ok(())
    }

    fn send_chord(&self, _mode: PasteMode, events: &[KeyEvent]) -> Result<Delivery, InjectError> {
        let mut guard = self
            .live
            .lock()
            .map_err(|_| InjectError::Failed("lock poisoned".into()))?;
        if guard.is_none() {
            *guard = Some(Self::connect()?);
        }
        let Some(live) = guard.as_mut() else {
            return Err(InjectError::Unavailable("no virtual keyboard".into()));
        };
        let time = u32::try_from(live.started.elapsed().as_millis()).unwrap_or(u32::MAX);
        for op in ops_for(events) {
            match op {
                VkOp::Key { evdev, pressed } => live.keyboard.key(time, evdev, u32::from(pressed)),
                VkOp::Modifiers { depressed } => live.keyboard.modifiers(depressed, 0, 0, 0),
            }
        }
        let result = live.queue.roundtrip(&mut State);
        if let Err(e) = result {
            *guard = None;
            return Err(InjectError::Failed(format!("wayland: {e}")));
        }
        Ok(Delivery::Sent)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::inject::plan::chord_events;
    use linuxflow_core::domain::PasteMode;

    #[test]
    fn keymap_defines_every_key_with_evdev_plus_8() {
        let text = keymap_text();
        assert!(text.contains("<K29> = 37;"), "Ctrl_L");
        assert!(text.contains("<K110> = 118;"), "Insert");
        assert!(text.contains("key <K47> { [ v, V ] };"));
        assert!(text.contains("Control_L"));
        assert!(text.contains("modifier_map Control { <K29>, <K97> };"));
        assert!(text.starts_with("xkb_keymap {") && text.trim_end().ends_with("};"));
        assert_eq!(text.matches('{').count(), text.matches('}').count());
    }

    #[test]
    fn ctrl_v_ops_set_and_clear_modifier_mask() {
        let ops = ops_for(&chord_events(PasteMode::CtrlV));
        assert_eq!(
            ops,
            vec![
                VkOp::Key {
                    evdev: 29,
                    pressed: true
                },
                VkOp::Modifiers {
                    depressed: MOD_CTRL
                },
                VkOp::Key {
                    evdev: 47,
                    pressed: true
                },
                VkOp::Key {
                    evdev: 47,
                    pressed: false
                },
                VkOp::Modifiers { depressed: 0 },
                VkOp::Key {
                    evdev: 29,
                    pressed: false
                },
            ]
        );
    }

    #[test]
    fn ctrl_shift_v_accumulates_mask() {
        let ops = ops_for(&chord_events(PasteMode::CtrlShiftV));
        assert!(ops.contains(&VkOp::Modifiers {
            depressed: MOD_CTRL | MOD_SHIFT
        }));
        assert_eq!(
            ops.last(),
            Some(&VkOp::Key {
                evdev: 29,
                pressed: false
            })
        );
    }

    #[test]
    fn shift_insert_never_sends_letter_v() {
        let ops = ops_for(&chord_events(PasteMode::ShiftInsert));
        assert!(!ops.iter().any(|o| matches!(o, VkOp::Key { evdev: 47, .. })));
        assert!(ops.contains(&VkOp::Key {
            evdev: 110,
            pressed: true
        }));
    }
}
