//! Description of a shortcut on the platform side.
//!
//! `linuxflow_core::accelerator` did not exist when this module was written:
//! the input of `HotkeyManager::start` is therefore the `AcceleratorSpec` trait, which the integration
//! implements for the core type (`fn to_spec(&self) -> HotkeySpec`). The keys are
//! physical evdev codes (`KEY_*`).

use super::keymap;

/// Set of modifiers (independent of left/right).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const NONE: Modifiers = Modifiers(0);
    pub const CTRL: Modifiers = Modifiers(1);
    pub const ALT: Modifiers = Modifiers(2);
    pub const SHIFT: Modifiers = Modifiers(4);
    pub const SUPER: Modifiers = Modifiers(8);

    pub fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn union(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 | other.0)
    }
}

/// Shortcut: modifiers + optional physical key (`None` = modifiers alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeySpec {
    pub modifiers: Modifiers,
    /// evdev code of the non-modifier key.
    pub key: Option<u16>,
}

/// Source of a shortcut (implemented by the core accelerator type at integration).
pub trait AcceleratorSpec {
    fn to_spec(&self) -> HotkeySpec;
}

impl AcceleratorSpec for HotkeySpec {
    fn to_spec(&self) -> HotkeySpec {
        *self
    }
}

/// Function keys F1 to F24 (evdev codes).
fn is_function_key(code: u16) -> bool {
    matches!(code, 59..=68 | 87 | 88 | 183..=194)
}

impl HotkeySpec {
    pub fn is_modifier_only(&self) -> bool {
        self.key.is_none()
    }

    /// A shortcut must contain at least one key.
    pub fn validate(&self) -> Result<(), String> {
        if self.key.is_none() && self.modifiers.is_empty() {
            return Err("empty shortcut".into());
        }
        if let Some(k) = self.key {
            if keymap::modifier_of_code(k).is_some() {
                return Err("a modifier cannot be the main key".into());
            }
            // A key that types text (letter, space, Enter…) alone or with Shift would be grabbed
            // globally: no application would receive it any more. Only the
            // function (F1 to F24) are allowed without Ctrl, Alt or Super.
            let non_shift = self.modifiers.contains(Modifiers::CTRL)
                || self.modifiers.contains(Modifiers::ALT)
                || self.modifiers.contains(Modifiers::SUPER);
            if !non_shift && !is_function_key(k) {
                return Err(
                    "a typing key needs Ctrl, Alt or Super (only F1 to F24 may be used alone)"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// Parses `Ctrl+Alt+Space`, `Super`, `Ctrl+Shift+F5`... (case-insensitive).
    pub fn parse(text: &str) -> Result<HotkeySpec, String> {
        let mut modifiers = Modifiers::NONE;
        let mut key = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            let lower = part.to_lowercase();
            let m = match lower.as_str() {
                "ctrl" | "control" | "ctl" => Modifiers::CTRL,
                "alt" | "option" => Modifiers::ALT,
                "shift" => Modifiers::SHIFT,
                "super" | "meta" | "win" | "logo" | "cmd" => Modifiers::SUPER,
                _ => Modifiers::NONE,
            };
            if !m.is_empty() {
                modifiers = modifiers.union(m);
            } else if key.is_none() {
                key = Some(
                    keymap::code_from_name(part).ok_or_else(|| format!("unknown key: {part}"))?,
                );
            } else {
                return Err("only one main key is allowed".into());
            }
        }
        let spec = HotkeySpec { modifiers, key };
        spec.validate()?;
        Ok(spec)
    }

    /// Readable text (`Ctrl+Alt+Space`), always in the same order.
    pub fn display(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (m, label) in [
            (Modifiers::CTRL, "Ctrl"),
            (Modifiers::ALT, "Alt"),
            (Modifiers::SHIFT, "Shift"),
            (Modifiers::SUPER, "Super"),
        ] {
            if self.modifiers.contains(m) {
                parts.push(label.into());
            }
        }
        if let Some(k) = self.key {
            parts.push(keymap::key_label(k));
        }
        parts.join("+")
    }

    /// Preferred trigger in the format of the XDG shortcuts specification
    /// (`CTRL+ALT+space`); `None` for modifiers alone (not reliable through the portal).
    pub fn portal_trigger(&self) -> Option<String> {
        let key = keymap::xkb_name(self.key?)?;
        let mut parts: Vec<&str> = Vec::new();
        for (m, n) in [
            (Modifiers::CTRL, "CTRL"),
            (Modifiers::ALT, "ALT"),
            (Modifiers::SHIFT, "SHIFT"),
            (Modifiers::SUPER, "LOGO"),
        ] {
            if self.modifiers.contains(m) {
                parts.push(n);
            }
        }
        parts.push(key);
        Some(parts.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_keys_alone_or_with_shift_are_refused() {
        // Regression: `a`, `Space`, `Return`, `Shift+a` were accepted; under X11 the key
        // would have been grabbed globally and would no longer reach the applications.
        for bad in ["a", "Space", "Return", "Shift+a", "Shift+Space", "1"] {
            assert!(HotkeySpec::parse(bad).is_err(), "{bad} must be refused");
        }
        for good in [
            "Ctrl+a",
            "Alt+Space",
            "Super+d",
            "Ctrl+Shift+a",
            "F9",
            "Shift+F5",
            "F24",
        ] {
            assert!(HotkeySpec::parse(good).is_ok(), "{good} must be accepted");
        }
    }

    #[test]
    fn parse_et_affichage() {
        let s = HotkeySpec::parse("ctrl+Alt+space").unwrap();
        assert_eq!(s.modifiers, Modifiers::CTRL.union(Modifiers::ALT));
        assert_eq!(s.key, Some(57));
        assert_eq!(s.display(), "Ctrl+Alt+Space");
        assert_eq!(s.portal_trigger().as_deref(), Some("CTRL+ALT+space"));
    }

    #[test]
    fn modificateurs_seuls() {
        let s = HotkeySpec::parse("Ctrl+Super").unwrap();
        assert!(s.is_modifier_only());
        assert_eq!(s.portal_trigger(), None);
        assert_eq!(s.display(), "Ctrl+Super");
    }

    #[test]
    fn invalides() {
        assert!(HotkeySpec::parse("").is_err());
        assert!(HotkeySpec::parse("Ctrl+A+B").is_err());
        assert!(HotkeySpec::parse("Ctrl+Nope").is_err());
        assert!(HotkeySpec {
            modifiers: Modifiers::NONE,
            key: Some(29)
        }
        .validate()
        .is_err());
    }

    #[test]
    fn touche_seule_et_fonction() {
        let s = HotkeySpec::parse("F9").unwrap();
        assert_eq!(s.key, Some(67));
        assert_eq!(HotkeySpec::parse("Shift+F13").unwrap().key, Some(183));
    }
}
