//! Pure planning of the `XGrabKey`s: one grab per Num/Caps/Scroll Lock combination.

use super::keymap;
use super::spec::{HotkeySpec, Modifiers};

pub const MASK_SHIFT: u16 = 1;
pub const MASK_LOCK: u16 = 2;
pub const MASK_CONTROL: u16 = 4;
pub const MASK_MOD1: u16 = 8;
pub const MASK_MOD2: u16 = 16;
pub const MASK_MOD4: u16 = 64;

/// X11 masks of the locks (0 = lock absent from the modifier table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockMasks {
    pub num: u16,
    pub caps: u16,
    pub scroll: u16,
}

impl Default for LockMasks {
    fn default() -> Self {
        Self {
            num: MASK_MOD2,
            caps: MASK_LOCK,
            scroll: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GrabPlan {
    pub keycode: u8,
    pub modifiers: u16,
}

/// X11 mask of the modifiers of the shortcut.
pub fn modifier_mask(m: Modifiers) -> u16 {
    let mut mask = 0;
    if m.contains(Modifiers::CTRL) {
        mask |= MASK_CONTROL;
    }
    if m.contains(Modifiers::ALT) {
        mask |= MASK_MOD1;
    }
    if m.contains(Modifiers::SHIFT) {
        mask |= MASK_SHIFT;
    }
    if m.contains(Modifiers::SUPER) {
        mask |= MASK_MOD4;
    }
    mask
}

/// Deduces the Num/Scroll Lock masks from the `GetModifierMapping` table
/// (`keycodes` = `8 * keycodes_per_modifier` codes, row i = bit `1 << i`).
pub fn lock_masks_from_mapping(keycodes_per_modifier: usize, keycodes: &[u8]) -> LockMasks {
    let mut out = LockMasks {
        num: 0,
        caps: MASK_LOCK,
        scroll: 0,
    };
    if keycodes_per_modifier == 0 {
        return LockMasks::default();
    }
    let num = keymap::evdev_to_x11_keycode(keymap::KEY_NUMLOCK);
    let scroll = keymap::evdev_to_x11_keycode(keymap::KEY_SCROLLLOCK);
    for (row, chunk) in keycodes.chunks(keycodes_per_modifier).enumerate().take(8) {
        let bit = 1u16 << row;
        if num.is_some_and(|k| chunk.contains(&k)) {
            out.num = bit;
        }
        if scroll.is_some_and(|k| chunk.contains(&k)) {
            out.scroll = bit;
        }
    }
    out
}

/// De-duplicated, sorted list of the grabs to set. `Err` if the shortcut has no key
/// main key (modifiers alone go through XInput2, not through a grab).
pub fn plan_grabs(spec: &HotkeySpec, locks: LockMasks) -> Result<Vec<GrabPlan>, String> {
    let code = spec
        .key
        .ok_or_else(|| "modifier-only shortcuts cannot be grabbed".to_string())?;
    let keycode = keymap::evdev_to_x11_keycode(code)
        .ok_or_else(|| "key code out of the X11 range".to_string())?;
    let base = modifier_mask(spec.modifiers);
    let lock_bits: Vec<u16> = [locks.num, locks.caps, locks.scroll]
        .into_iter()
        .filter(|m| *m != 0)
        .collect();
    let mut plans = Vec::new();
    for subset in 0..(1u32 << lock_bits.len()) {
        let mut mask = base;
        for (i, bit) in lock_bits.iter().enumerate() {
            if subset & (1 << i) != 0 {
                mask |= bit;
            }
        }
        plans.push(GrabPlan {
            keycode,
            modifiers: mask,
        });
    }
    plans.sort();
    plans.dedup();
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_grabs_with_three_locks() {
        let s = HotkeySpec::parse("Ctrl+Alt+Space").unwrap();
        let locks = LockMasks {
            num: MASK_MOD2,
            caps: MASK_LOCK,
            scroll: 128,
        };
        let p = plan_grabs(&s, locks).unwrap();
        assert_eq!(p.len(), 8);
        assert!(p.iter().all(|g| g.keycode == 65));
        let base = MASK_CONTROL | MASK_MOD1;
        assert!(p.iter().any(|g| g.modifiers == base));
        assert!(p
            .iter()
            .any(|g| g.modifiers == base | MASK_MOD2 | MASK_LOCK | 128));
    }

    #[test]
    fn default_four_grabs_and_deduplication() {
        let s = HotkeySpec::parse("F9").unwrap();
        assert_eq!(plan_grabs(&s, LockMasks::default()).unwrap().len(), 4);
        let same = LockMasks {
            num: 16,
            caps: 16,
            scroll: 0,
        };
        assert_eq!(plan_grabs(&s, same).unwrap().len(), 2);
        let none = LockMasks {
            num: 0,
            caps: 0,
            scroll: 0,
        };
        assert_eq!(
            plan_grabs(&s, none).unwrap(),
            vec![GrabPlan {
                keycode: 75,
                modifiers: 0
            }]
        );
    }

    #[test]
    fn modifier_only_refused() {
        let s = HotkeySpec::parse("Ctrl+Super").unwrap();
        assert!(plan_grabs(&s, LockMasks::default()).is_err());
    }

    #[test]
    fn modifier_masks() {
        assert_eq!(
            modifier_mask(Modifiers::SHIFT.union(Modifiers::SUPER)),
            MASK_SHIFT | MASK_MOD4
        );
    }

    #[test]
    fn modifier_table() {
        // 2 codes per row: Shift, Lock, Control, Mod1, Mod2 (NumLock = 77), Mod3, Mod4, Mod5 (ScrollLock = 78).
        let mut t = vec![0u8; 16];
        t[8] = 77;
        t[14] = 78;
        let m = lock_masks_from_mapping(2, &t);
        assert_eq!((m.num, m.scroll, m.caps), (16, 128, 2));
        assert_eq!(lock_masks_from_mapping(0, &[]), LockMasks::default());
    }
}
