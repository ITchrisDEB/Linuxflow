//! Pure evdev <-> X11 keysym / key name translation (US reference layout).
//!
//! Shortcuts are stored as evdev physical codes; the keysym is only used for display and
//! for the portal format. The display faithful to the user's layout (xkbcommon) is
//! the interface's job: here, a stable fallback independent of the layout.

use super::spec::Modifiers;

/// (evdev code, X11 keysym, xkb name)
const KEYS: &[(u16, u32, &str)] = &[
    (1, 0xff1b, "Escape"),
    (2, 0x31, "1"),
    (3, 0x32, "2"),
    (4, 0x33, "3"),
    (5, 0x34, "4"),
    (6, 0x35, "5"),
    (7, 0x36, "6"),
    (8, 0x37, "7"),
    (9, 0x38, "8"),
    (10, 0x39, "9"),
    (11, 0x30, "0"),
    (12, 0x2d, "minus"),
    (13, 0x3d, "equal"),
    (14, 0xff08, "BackSpace"),
    (15, 0xff09, "Tab"),
    (16, 0x71, "q"),
    (17, 0x77, "w"),
    (18, 0x65, "e"),
    (19, 0x72, "r"),
    (20, 0x74, "t"),
    (21, 0x79, "y"),
    (22, 0x75, "u"),
    (23, 0x69, "i"),
    (24, 0x6f, "o"),
    (25, 0x70, "p"),
    (26, 0x5b, "bracketleft"),
    (27, 0x5d, "bracketright"),
    (28, 0xff0d, "Return"),
    (30, 0x61, "a"),
    (31, 0x73, "s"),
    (32, 0x64, "d"),
    (33, 0x66, "f"),
    (34, 0x67, "g"),
    (35, 0x68, "h"),
    (36, 0x6a, "j"),
    (37, 0x6b, "k"),
    (38, 0x6c, "l"),
    (39, 0x3b, "semicolon"),
    (40, 0x27, "apostrophe"),
    (41, 0x60, "grave"),
    (43, 0x5c, "backslash"),
    (44, 0x7a, "z"),
    (45, 0x78, "x"),
    (46, 0x63, "c"),
    (47, 0x76, "v"),
    (48, 0x62, "b"),
    (49, 0x6e, "n"),
    (50, 0x6d, "m"),
    (51, 0x2c, "comma"),
    (52, 0x2e, "period"),
    (53, 0x2f, "slash"),
    (55, 0xffaa, "KP_Multiply"),
    (57, 0x20, "space"),
    (59, 0xffbe, "F1"),
    (60, 0xffbf, "F2"),
    (61, 0xffc0, "F3"),
    (62, 0xffc1, "F4"),
    (63, 0xffc2, "F5"),
    (64, 0xffc3, "F6"),
    (65, 0xffc4, "F7"),
    (66, 0xffc5, "F8"),
    (67, 0xffc6, "F9"),
    (68, 0xffc7, "F10"),
    (74, 0xffad, "KP_Subtract"),
    (78, 0xffab, "KP_Add"),
    (83, 0xffae, "KP_Decimal"),
    (71, 0xffb7, "KP_7"),
    (72, 0xffb8, "KP_8"),
    (73, 0xffb9, "KP_9"),
    (75, 0xffb4, "KP_4"),
    (76, 0xffb5, "KP_5"),
    (77, 0xffb6, "KP_6"),
    (79, 0xffb1, "KP_1"),
    (80, 0xffb2, "KP_2"),
    (81, 0xffb3, "KP_3"),
    (82, 0xffb0, "KP_0"),
    (87, 0xffc8, "F11"),
    (88, 0xffc9, "F12"),
    (96, 0xff8d, "KP_Enter"),
    (98, 0xffaf, "KP_Divide"),
    (99, 0xff61, "Print"),
    (102, 0xff50, "Home"),
    (103, 0xff52, "Up"),
    (104, 0xff55, "Prior"),
    (105, 0xff51, "Left"),
    (106, 0xff53, "Right"),
    (107, 0xff57, "End"),
    (108, 0xff54, "Down"),
    (109, 0xff56, "Next"),
    (110, 0xff63, "Insert"),
    (111, 0xffff, "Delete"),
    (119, 0xff13, "Pause"),
    (183, 0xffca, "F13"),
    (184, 0xffcb, "F14"),
    (185, 0xffcc, "F15"),
    (186, 0xffcd, "F16"),
    (187, 0xffce, "F17"),
    (188, 0xffcf, "F18"),
    (189, 0xffd0, "F19"),
    (190, 0xffd1, "F20"),
    (191, 0xffd2, "F21"),
    (192, 0xffd3, "F22"),
    (193, 0xffd4, "F23"),
    (194, 0xffd5, "F24"),
];

/// (evdev code, keysym, modifier) for the modifier keys.
const MODIFIER_KEYS: &[(u16, u32, Modifiers)] = &[
    (29, 0xffe3, Modifiers::CTRL),
    (97, 0xffe4, Modifiers::CTRL),
    (42, 0xffe1, Modifiers::SHIFT),
    (54, 0xffe2, Modifiers::SHIFT),
    (56, 0xffe9, Modifiers::ALT),
    (100, 0xffea, Modifiers::ALT),
    (125, 0xffeb, Modifiers::SUPER),
    (126, 0xffec, Modifiers::SUPER),
];

pub const KEY_NUMLOCK: u16 = 69;
pub const KEY_SCROLLLOCK: u16 = 70;

pub fn modifier_of_code(code: u16) -> Option<Modifiers> {
    MODIFIER_KEYS
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, _, m)| *m)
}

pub fn evdev_to_keysym(code: u16) -> Option<u32> {
    KEYS.iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, s, _)| *s)
        .or_else(|| {
            MODIFIER_KEYS
                .iter()
                .find(|(c, _, _)| *c == code)
                .map(|(_, s, _)| *s)
        })
}

/// A keysym can match a physical key of the US layout; ASCII upper-case letters
/// are brought back to lower case.
pub fn keysym_to_evdev(sym: u32) -> Option<u16> {
    let sym = if (0x41..=0x5a).contains(&sym) {
        sym + 0x20
    } else {
        sym
    };
    KEYS.iter()
        .find(|(_, s, _)| *s == sym)
        .map(|(c, _, _)| *c)
        .or_else(|| {
            MODIFIER_KEYS
                .iter()
                .find(|(_, s, _)| *s == sym)
                .map(|(c, _, _)| *c)
        })
}

/// xkb name of the keysym (`space`, `a`, `F5`) as expected by the portal.
pub fn xkb_name(code: u16) -> Option<&'static str> {
    KEYS.iter().find(|(c, _, _)| *c == code).map(|(_, _, n)| *n)
}

/// Short label for the interface: `A`, `Space`, `F5`, `Ctrl`, `Key 200` if unknown.
pub fn key_label(code: u16) -> String {
    if let Some(m) = modifier_of_code(code) {
        let base = if m == Modifiers::CTRL {
            "Ctrl"
        } else if m == Modifiers::SHIFT {
            "Shift"
        } else if m == Modifiers::ALT {
            "Alt"
        } else {
            "Super"
        };
        return base.into();
    }
    match xkb_name(code) {
        Some(n) if n.chars().count() == 1 => n.to_uppercase(),
        Some("minus") => "-".into(),
        Some("equal") => "=".into(),
        Some("bracketleft") => "[".into(),
        Some("bracketright") => "]".into(),
        Some("semicolon") => ";".into(),
        Some("apostrophe") => "'".into(),
        Some("grave") => "`".into(),
        Some("backslash") => "\\".into(),
        Some("comma") => ",".into(),
        Some("period") => ".".into(),
        Some("slash") => "/".into(),
        Some("KP_Add") => "NumAdd".into(),
        Some("KP_Subtract") => "NumSub".into(),
        Some("KP_Multiply") => "NumMul".into(),
        Some("KP_Divide") => "NumDiv".into(),
        Some("KP_Decimal") => "NumDot".into(),
        Some(n) if n.starts_with("KP_") && n.len() == 4 => format!("Num{}", &n[3..]),
        Some("Prior") => "PageUp".into(),
        Some("Next") => "PageDown".into(),
        Some("Return") => "Enter".into(),
        Some("BackSpace") => "Backspace".into(),
        Some("Print") => "PrintScreen".into(),
        Some(n) => {
            let mut c = n.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
        None => format!("Key {code}"),
    }
}

/// Finds a key by its label or xkb name (case-insensitive).
pub fn code_from_name(name: &str) -> Option<u16> {
    let lower = name.trim().to_lowercase();
    // numpad labels (`Num+`, `Num3`): those of `key_label`
    let numpad = match lower.as_str() {
        "numadd" => Some("kp_add".to_string()),
        "numsub" => Some("kp_subtract".to_string()),
        "nummul" => Some("kp_multiply".to_string()),
        "numdiv" => Some("kp_divide".to_string()),
        "numdot" => Some("kp_decimal".to_string()),
        s if s.len() == 4 && s.starts_with("num") && s.as_bytes()[3].is_ascii_digit() => {
            Some(format!("kp_{}", &s[3..]))
        }
        _ => None,
    };
    let lower = numpad.unwrap_or(lower);
    let alias = match lower.as_str() {
        "enter" => "return",
        "esc" => "escape",
        "pageup" | "pgup" => "prior",
        "pagedown" | "pgdn" => "next",
        "del" => "delete",
        "ins" => "insert",
        "printscreen" | "prtsc" => "print",
        "backspace" => "backspace",
        "-" => "minus",
        "=" => "equal",
        "[" => "bracketleft",
        "]" => "bracketright",
        ";" => "semicolon",
        "'" => "apostrophe",
        "`" => "grave",
        "\\" => "backslash",
        "," => "comma",
        "." => "period",
        "/" => "slash",
        other => other,
    };
    KEYS.iter()
        .find(|(_, _, n)| n.to_lowercase() == alias)
        .map(|(c, _, _)| *c)
}

/// X11 code (`detail` of an event) -> evdev code: the evdev/libinput driver offsets by 8.
pub fn x11_keycode_to_evdev(keycode: u32) -> Option<u16> {
    keycode
        .checked_sub(8)
        .and_then(|c| u16::try_from(c).ok())
        .filter(|c| *c > 0)
}

/// evdev code -> X11 code (limited to 255 by the protocol).
pub fn evdev_to_x11_keycode(code: u16) -> Option<u8> {
    u8::try_from(u32::from(code) + 8).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour_sur_toute_la_table() {
        for (code, sym, _) in KEYS {
            assert_eq!(evdev_to_keysym(*code), Some(*sym));
            assert_eq!(keysym_to_evdev(*sym), Some(*code), "code {code}");
        }
        for (code, sym, _) in MODIFIER_KEYS {
            assert_eq!(keysym_to_evdev(*sym), Some(*code));
        }
    }

    #[test]
    fn pas_de_doublons() {
        let mut codes: Vec<u16> = KEYS
            .iter()
            .map(|k| k.0)
            .chain(MODIFIER_KEYS.iter().map(|k| k.0))
            .collect();
        codes.sort_unstable();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n);
        let mut syms: Vec<u32> = KEYS
            .iter()
            .map(|k| k.1)
            .chain(MODIFIER_KEYS.iter().map(|k| k.1))
            .collect();
        syms.sort_unstable();
        let n = syms.len();
        syms.dedup();
        assert_eq!(syms.len(), n);
    }

    #[test]
    fn valeurs_connues() {
        assert_eq!(evdev_to_keysym(57), Some(0x20));
        assert_eq!(keysym_to_evdev(0x41), Some(30), "A majuscule -> KEY_A");
        assert_eq!(evdev_to_x11_keycode(57), Some(65));
        assert_eq!(x11_keycode_to_evdev(65), Some(57));
        assert_eq!(x11_keycode_to_evdev(8), None);
        assert_eq!(evdev_to_x11_keycode(300), None);
    }

    #[test]
    fn libelles_et_noms() {
        assert_eq!(key_label(30), "A");
        assert_eq!(key_label(57), "Space");
        assert_eq!(key_label(104), "PageUp");
        assert_eq!(key_label(29), "Ctrl");
        assert_eq!(key_label(250), "Key 250");
        assert_eq!(code_from_name("PgDn"), Some(109));
        assert_eq!(code_from_name("enter"), Some(28));
        assert_eq!(code_from_name("."), Some(52));
        assert_eq!(code_from_name("zzz"), None);
        for (code, _, _) in KEYS {
            assert_eq!(
                code_from_name(&key_label(*code)),
                Some(*code),
                "label of code {code}"
            );
        }
    }
}
