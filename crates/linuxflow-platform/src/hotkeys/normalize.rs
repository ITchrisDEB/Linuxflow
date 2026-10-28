//! Pure normalisation of the raw keyboard events (evdev, XInput2) into `RawKeyEvent`.

use super::keymap;
use linuxflow_core::traits::RawKeyEvent;
use std::collections::{HashMap, HashSet};

pub type DeviceId = u64;

/// Merges several keyboards: a press is only emitted for the first keyboard that presses, a
/// release only for the last one that releases; automatic repeats are dropped.
#[derive(Debug, Default)]
pub struct KeyNormalizer {
    held: HashMap<u16, HashSet<DeviceId>>,
}

/// Codes >= 0x100 are buttons (mouse, game pads), not keyboard keys.
pub fn is_keyboard_key(code: u16) -> bool {
    code > 0 && code < 0x100
}

impl KeyNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// `value`: 0 released, 1 pressed, 2 repeat (evdev convention).
    pub fn handle(
        &mut self,
        dev: DeviceId,
        code: u16,
        value: i32,
        ts_ms: u64,
    ) -> Option<RawKeyEvent> {
        if !is_keyboard_key(code) {
            return None;
        }
        match value {
            1 => {
                let set = self.held.entry(code).or_default();
                let first = set.is_empty();
                set.insert(dev);
                first.then_some(RawKeyEvent {
                    code,
                    down: true,
                    ts_ms,
                })
            }
            0 => {
                let set = self.held.get_mut(&code)?;
                if !set.remove(&dev) {
                    return None;
                }
                if set.is_empty() {
                    self.held.remove(&code);
                    Some(RawKeyEvent {
                        code,
                        down: false,
                        ts_ms,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Keyboard unplugged: releases what it still held.
    pub fn device_removed(&mut self, dev: DeviceId, ts_ms: u64) -> Vec<RawKeyEvent> {
        let mut codes: Vec<u16> = self
            .held
            .iter()
            .filter(|(_, s)| s.contains(&dev))
            .map(|(c, _)| *c)
            .collect();
        codes.sort_unstable();
        codes
            .into_iter()
            .filter_map(|c| self.handle(dev, c, 0, ts_ms))
            .collect()
    }

    /// Releases everything (resume after a suspension) and returns the release events.
    pub fn release_all(&mut self, ts_ms: u64) -> Vec<RawKeyEvent> {
        let mut codes: Vec<u16> = self.held.keys().copied().collect();
        codes.sort_unstable();
        self.held.clear();
        codes
            .into_iter()
            .map(|code| RawKeyEvent {
                code,
                down: false,
                ts_ms,
            })
            .collect()
    }
}

/// Raw XInput2 event -> (evdev code, value). `detail` is an X11 keycode.
pub fn from_xi2(detail: u32, pressed: bool, is_repeat: bool) -> Option<(u16, i32)> {
    let code = keymap::x11_keycode_to_evdev(detail)?;
    Some((code, if is_repeat { 2 } else { i32::from(pressed) }))
}

/// Is an evdev device a keyboard? (has at least A, Z and Enter, and is not ignored)
pub fn looks_like_keyboard(
    has_key: impl Fn(u16) -> bool,
    name: &str,
    ignore_names: &[String],
) -> bool {
    let lower = name.to_lowercase();
    if ignore_names
        .iter()
        .any(|n| !n.is_empty() && lower.contains(n.as_str()))
    {
        return false;
    }
    [30u16, 44, 28].iter().all(|c| has_key(*c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_and_buttons_ignored() {
        let mut n = KeyNormalizer::new();
        assert!(n.handle(1, 30, 1, 10).unwrap().down);
        assert_eq!(n.handle(1, 30, 2, 20), None);
        assert_eq!(n.handle(1, 0x110, 1, 20), None, "BTN_LEFT");
        assert!(!n.handle(1, 30, 0, 30).unwrap().down);
        assert_eq!(n.handle(1, 30, 0, 40), None, "release without a press");
    }

    #[test]
    fn deux_claviers() {
        let mut n = KeyNormalizer::new();
        assert!(n.handle(1, 29, 1, 1).is_some());
        assert_eq!(n.handle(2, 29, 1, 2), None);
        assert_eq!(
            n.handle(1, 29, 0, 3),
            None,
            "l'autre clavier tient encore Ctrl"
        );
        let up = n.handle(2, 29, 0, 4).unwrap();
        assert!(!up.down && up.ts_ms == 4);
    }

    #[test]
    fn debranchement_relache() {
        let mut n = KeyNormalizer::new();
        n.handle(1, 56, 1, 1);
        n.handle(1, 57, 1, 2);
        let ev = n.device_removed(1, 9);
        assert_eq!(ev.len(), 2);
        assert!(ev.iter().all(|e| !e.down && e.ts_ms == 9));
        assert!(n.device_removed(1, 10).is_empty());
    }

    #[test]
    fn release_all() {
        let mut n = KeyNormalizer::new();
        n.handle(1, 29, 1, 1);
        n.handle(2, 42, 1, 1);
        assert_eq!(n.release_all(5).len(), 2);
        assert!(n.release_all(6).is_empty());
    }

    #[test]
    fn xi2() {
        assert_eq!(from_xi2(65, true, false), Some((57, 1)));
        assert_eq!(from_xi2(65, true, true), Some((57, 2)));
        assert_eq!(from_xi2(65, false, false), Some((57, 0)));
        assert_eq!(from_xi2(3, true, false), None);
    }

    #[test]
    fn classification_clavier() {
        let kb = |c: u16| [30, 44, 28, 57].contains(&c);
        let ignore = vec!["linuxflow".to_string()];
        assert!(looks_like_keyboard(
            kb,
            "AT Translated Set 2 keyboard",
            &ignore
        ));
        assert!(!looks_like_keyboard(
            kb,
            "LinuxFlow virtual keyboard",
            &ignore
        ));
        assert!(!looks_like_keyboard(|c| c == 0x110, "mouse", &ignore));
    }
}
