//! Keyboard shortcuts: parsing, canonical format, validation and key conversions.
//!
//! Format `Ctrl+Alt+Space`, `Ctrl+Super`, `F9`. No translated string here:
//! the interface translates the `AcceleratorError`s and the labels ("Shift", "Win"…).

use crate::domain::ShortcutMode;
use std::fmt;
use thiserror::Error;

/// evdev codes (`linux/input-event-codes.h`) used here, written by hand (no dependency).
#[allow(missing_docs)]
pub mod evdev {
    pub const KEY_ESC: u16 = 1;
    pub const KEY_1: u16 = 2;
    pub const KEY_2: u16 = 3;
    pub const KEY_3: u16 = 4;
    pub const KEY_4: u16 = 5;
    pub const KEY_5: u16 = 6;
    pub const KEY_6: u16 = 7;
    pub const KEY_7: u16 = 8;
    pub const KEY_8: u16 = 9;
    pub const KEY_9: u16 = 10;
    pub const KEY_0: u16 = 11;
    pub const KEY_MINUS: u16 = 12;
    pub const KEY_EQUAL: u16 = 13;
    pub const KEY_BACKSPACE: u16 = 14;
    pub const KEY_TAB: u16 = 15;
    pub const KEY_Q: u16 = 16;
    pub const KEY_W: u16 = 17;
    pub const KEY_E: u16 = 18;
    pub const KEY_R: u16 = 19;
    pub const KEY_T: u16 = 20;
    pub const KEY_Y: u16 = 21;
    pub const KEY_U: u16 = 22;
    pub const KEY_I: u16 = 23;
    pub const KEY_O: u16 = 24;
    pub const KEY_P: u16 = 25;
    pub const KEY_LEFTBRACE: u16 = 26;
    pub const KEY_RIGHTBRACE: u16 = 27;
    pub const KEY_ENTER: u16 = 28;
    pub const KEY_LEFTCTRL: u16 = 29;
    pub const KEY_A: u16 = 30;
    pub const KEY_S: u16 = 31;
    pub const KEY_D: u16 = 32;
    pub const KEY_F: u16 = 33;
    pub const KEY_G: u16 = 34;
    pub const KEY_H: u16 = 35;
    pub const KEY_J: u16 = 36;
    pub const KEY_K: u16 = 37;
    pub const KEY_L: u16 = 38;
    pub const KEY_SEMICOLON: u16 = 39;
    pub const KEY_APOSTROPHE: u16 = 40;
    pub const KEY_GRAVE: u16 = 41;
    pub const KEY_LEFTSHIFT: u16 = 42;
    pub const KEY_BACKSLASH: u16 = 43;
    pub const KEY_Z: u16 = 44;
    pub const KEY_X: u16 = 45;
    pub const KEY_C: u16 = 46;
    pub const KEY_V: u16 = 47;
    pub const KEY_B: u16 = 48;
    pub const KEY_N: u16 = 49;
    pub const KEY_M: u16 = 50;
    pub const KEY_COMMA: u16 = 51;
    pub const KEY_DOT: u16 = 52;
    pub const KEY_SLASH: u16 = 53;
    pub const KEY_RIGHTSHIFT: u16 = 54;
    pub const KEY_KPASTERISK: u16 = 55;
    pub const KEY_LEFTALT: u16 = 56;
    pub const KEY_SPACE: u16 = 57;
    pub const KEY_CAPSLOCK: u16 = 58;
    pub const KEY_F1: u16 = 59;
    pub const KEY_F2: u16 = 60;
    pub const KEY_F3: u16 = 61;
    pub const KEY_F4: u16 = 62;
    pub const KEY_F5: u16 = 63;
    pub const KEY_F6: u16 = 64;
    pub const KEY_F7: u16 = 65;
    pub const KEY_F8: u16 = 66;
    pub const KEY_F9: u16 = 67;
    pub const KEY_F10: u16 = 68;
    pub const KEY_KPMINUS: u16 = 74;
    pub const KEY_KPPLUS: u16 = 78;
    pub const KEY_KPDOT: u16 = 83;
    pub const KEY_KPSLASH: u16 = 98;
    pub const KEY_KP7: u16 = 71;
    pub const KEY_KP8: u16 = 72;
    pub const KEY_KP9: u16 = 73;
    pub const KEY_KP4: u16 = 75;
    pub const KEY_KP5: u16 = 76;
    pub const KEY_KP6: u16 = 77;
    pub const KEY_KP1: u16 = 79;
    pub const KEY_KP2: u16 = 80;
    pub const KEY_KP3: u16 = 81;
    pub const KEY_KP0: u16 = 82;
    pub const KEY_F11: u16 = 87;
    pub const KEY_F12: u16 = 88;
    pub const KEY_KPENTER: u16 = 96;
    pub const KEY_RIGHTCTRL: u16 = 97;
    pub const KEY_RIGHTALT: u16 = 100;
    pub const KEY_HOME: u16 = 102;
    pub const KEY_UP: u16 = 103;
    pub const KEY_PAGEUP: u16 = 104;
    pub const KEY_LEFT: u16 = 105;
    pub const KEY_RIGHT: u16 = 106;
    pub const KEY_END: u16 = 107;
    pub const KEY_DOWN: u16 = 108;
    pub const KEY_PAGEDOWN: u16 = 109;
    pub const KEY_INSERT: u16 = 110;
    pub const KEY_DELETE: u16 = 111;
    pub const KEY_LEFTMETA: u16 = 125;
    pub const KEY_RIGHTMETA: u16 = 126;
    pub const KEY_F13: u16 = 183;
    pub const KEY_F14: u16 = 184;
    pub const KEY_F15: u16 = 185;
    pub const KEY_F16: u16 = 186;
    pub const KEY_F17: u16 = 187;
    pub const KEY_F18: u16 = 188;
    pub const KEY_F19: u16 = 189;
    pub const KEY_F20: u16 = 190;
    pub const KEY_F21: u16 = 191;
    pub const KEY_F22: u16 = 192;
    pub const KEY_F23: u16 = 193;
    pub const KEY_F24: u16 = 194;
}

/// On-screen label of a numpad key (`num3` → `Num3`, `numadd` → `Num+`);
/// `None` for any other key.
pub fn numpad_label(token: &str) -> Option<String> {
    Some(match token {
        "numadd" => "Num+".into(),
        "numsub" => "Num-".into(),
        "nummult" => "Num*".into(),
        "numdiv" => "Num/".into(),
        "numdec" => "Num.".into(),
        t if t.len() == 4 && t.starts_with("num") && t.as_bytes()[3].is_ascii_digit() => {
            format!("Num{}", &t[3..])
        }
        _ => return None,
    })
}

/// Parsing or validation error. No translated text: the interface chooses the message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AcceleratorError {
    /// No key ("Press at least one combination").
    #[error("empty accelerator")]
    Empty,
    /// Several normal keys in the same shortcut.
    #[error("more than one non-modifier key")]
    MultipleKeys,
    /// Modifiers alone: at least two are needed.
    #[error("modifier-only shortcut needs at least two modifiers")]
    ModifiersOnlyNeedTwo,
    /// Normal key without a modifier (except F1-F12).
    #[error("shortcut needs a modifier (or an F1-F12 key)")]
    NeedsModifier,
    /// Unknown key in hold mode (push-to-talk). Contains the entered name.
    #[error("key not recognised in push-to-talk mode: {0}")]
    KeyNotRecognized(String),
    /// Combination that the system or nearly every application already uses (copy,
    /// paste, Alt+Tab…): grabbing it globally would make it unusable. Contains its display.
    #[error("shortcut reserved by the system or by most applications: {0}")]
    Reserved(String),
}

/// One of the four modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Super,
}

impl Modifier {
    /// Canonical display order.
    pub const ORDER: [Modifier; 4] = [
        Modifier::Ctrl,
        Modifier::Alt,
        Modifier::Shift,
        Modifier::Super,
    ];

    /// Canonical name (English).
    pub fn name(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
            Modifier::Super => "Super",
        }
    }

    /// Recognised aliases, case-insensitive.
    pub fn from_name(s: &str) -> Option<Modifier> {
        match s.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" => Some(Modifier::Ctrl),
            "alt" => Some(Modifier::Alt),
            "shift" => Some(Modifier::Shift),
            "super" | "meta" | "cmd" | "command" => Some(Modifier::Super),
            _ => None,
        }
    }

    /// Modifier matching an evdev code (left or right).
    pub fn from_evdev(code: u16) -> Option<Modifier> {
        match code {
            evdev::KEY_LEFTCTRL | evdev::KEY_RIGHTCTRL => Some(Modifier::Ctrl),
            evdev::KEY_LEFTALT | evdev::KEY_RIGHTALT => Some(Modifier::Alt),
            evdev::KEY_LEFTSHIFT | evdev::KEY_RIGHTSHIFT => Some(Modifier::Shift),
            evdev::KEY_LEFTMETA | evdev::KEY_RIGHTMETA => Some(Modifier::Super),
            _ => None,
        }
    }

    /// Modifier matching an X11/GTK keysym name (`Control_L`, `Super_R`…).
    pub fn from_keysym_name(s: &str) -> Option<Modifier> {
        match s {
            "Control_L" | "Control_R" => Some(Modifier::Ctrl),
            "Alt_L" | "Alt_R" | "ISO_Level3_Shift" => Some(Modifier::Alt),
            "Shift_L" | "Shift_R" => Some(Modifier::Shift),
            "Super_L" | "Super_R" | "Meta_L" | "Meta_R" | "Hyper_L" | "Hyper_R" => {
                Some(Modifier::Super)
            }
            _ => None,
        }
    }
}

/// Set of modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl Modifiers {
    pub fn contains(&self, m: Modifier) -> bool {
        match m {
            Modifier::Ctrl => self.ctrl,
            Modifier::Alt => self.alt,
            Modifier::Shift => self.shift,
            Modifier::Super => self.super_key,
        }
    }

    pub fn insert(&mut self, m: Modifier) {
        match m {
            Modifier::Ctrl => self.ctrl = true,
            Modifier::Alt => self.alt = true,
            Modifier::Shift => self.shift = true,
            Modifier::Super => self.super_key = true,
        }
    }

    pub fn count(&self) -> usize {
        Modifier::ORDER
            .iter()
            .filter(|m| self.contains(**m))
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// Modifiers present, in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = Modifier> + '_ {
        Modifier::ORDER.into_iter().filter(|m| self.contains(*m))
    }
}

/// "Normal" (non-modifier) key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Lettre majuscule `'A'..='Z'`.
    Letter(char),
    /// Digit `0..=9` of the main row.
    Digit(u8),
    /// Function key `F1..=F24`.
    F(u8),
    /// Numpad `num0..=num9`.
    Numpad(u8),
    /// Numpad operations: `numadd`, `numsub`, `nummult`, `numdiv`, `numdec`.
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadDecimal,
    Space,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Minus,
    Equal,
    Comma,
    Period,
    Slash,
    Semicolon,
    Quote,
    Backslash,
    BracketLeft,
    BracketRight,
    Backquote,
    CapsLock,
    /// Unrecognised name, kept as is (lossless round trip; refused in hold mode).
    Unknown(String),
}

/// `evdev <-> Key` table. The first code listed for a key is its canonical code; the
/// following ones (e.g. numpad Enter) are only used for reading.
fn evdev_table() -> Vec<(u16, Key)> {
    use evdev::*;
    let mut t: Vec<(u16, Key)> = Vec::with_capacity(130);
    let letters = [
        (KEY_A, 'A'),
        (KEY_B, 'B'),
        (KEY_C, 'C'),
        (KEY_D, 'D'),
        (KEY_E, 'E'),
        (KEY_F, 'F'),
        (KEY_G, 'G'),
        (KEY_H, 'H'),
        (KEY_I, 'I'),
        (KEY_J, 'J'),
        (KEY_K, 'K'),
        (KEY_L, 'L'),
        (KEY_M, 'M'),
        (KEY_N, 'N'),
        (KEY_O, 'O'),
        (KEY_P, 'P'),
        (KEY_Q, 'Q'),
        (KEY_R, 'R'),
        (KEY_S, 'S'),
        (KEY_T, 'T'),
        (KEY_U, 'U'),
        (KEY_V, 'V'),
        (KEY_W, 'W'),
        (KEY_X, 'X'),
        (KEY_Y, 'Y'),
        (KEY_Z, 'Z'),
    ];
    t.extend(letters.iter().map(|(c, l)| (*c, Key::Letter(*l))));
    let digits = [
        KEY_0, KEY_1, KEY_2, KEY_3, KEY_4, KEY_5, KEY_6, KEY_7, KEY_8, KEY_9,
    ];
    t.extend(
        digits
            .iter()
            .enumerate()
            .map(|(i, c)| (*c, Key::Digit(i as u8))),
    );
    let fkeys = [
        KEY_F1, KEY_F2, KEY_F3, KEY_F4, KEY_F5, KEY_F6, KEY_F7, KEY_F8, KEY_F9, KEY_F10, KEY_F11,
        KEY_F12, KEY_F13, KEY_F14, KEY_F15, KEY_F16, KEY_F17, KEY_F18, KEY_F19, KEY_F20, KEY_F21,
        KEY_F22, KEY_F23, KEY_F24,
    ];
    t.extend(
        fkeys
            .iter()
            .enumerate()
            .map(|(i, c)| (*c, Key::F(i as u8 + 1))),
    );
    let kp = [
        KEY_KP0, KEY_KP1, KEY_KP2, KEY_KP3, KEY_KP4, KEY_KP5, KEY_KP6, KEY_KP7, KEY_KP8, KEY_KP9,
    ];
    t.extend(
        kp.iter()
            .enumerate()
            .map(|(i, c)| (*c, Key::Numpad(i as u8))),
    );
    t.extend([
        (KEY_SPACE, Key::Space),
        (KEY_ENTER, Key::Enter),
        (KEY_KPENTER, Key::Enter),
        (KEY_KPPLUS, Key::NumpadAdd),
        (KEY_KPMINUS, Key::NumpadSubtract),
        (KEY_KPASTERISK, Key::NumpadMultiply),
        (KEY_KPSLASH, Key::NumpadDivide),
        (KEY_KPDOT, Key::NumpadDecimal),
        (KEY_TAB, Key::Tab),
        (KEY_BACKSPACE, Key::Backspace),
        (KEY_DELETE, Key::Delete),
        (KEY_INSERT, Key::Insert),
        (KEY_HOME, Key::Home),
        (KEY_END, Key::End),
        (KEY_PAGEUP, Key::PageUp),
        (KEY_PAGEDOWN, Key::PageDown),
        (KEY_UP, Key::Up),
        (KEY_DOWN, Key::Down),
        (KEY_LEFT, Key::Left),
        (KEY_RIGHT, Key::Right),
        (KEY_MINUS, Key::Minus),
        (KEY_EQUAL, Key::Equal),
        (KEY_COMMA, Key::Comma),
        (KEY_DOT, Key::Period),
        (KEY_SLASH, Key::Slash),
        (KEY_SEMICOLON, Key::Semicolon),
        (KEY_APOSTROPHE, Key::Quote),
        (KEY_BACKSLASH, Key::Backslash),
        (KEY_LEFTBRACE, Key::BracketLeft),
        (KEY_RIGHTBRACE, Key::BracketRight),
        (KEY_GRAVE, Key::Backquote),
        (KEY_CAPSLOCK, Key::CapsLock),
    ]);
    t
}

/// `F` followed by 1 or 2 digits (original regex `^F\d{1,2}$`, case-insensitive).
fn is_function_name(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() == 2 || b.len() == 3)
        && (b[0] == b'F' || b[0] == b'f')
        && b[1..].iter().all(u8::is_ascii_digit)
}

impl Key {
    /// Parses a key name (case-insensitive). An unknown name becomes `Key::Unknown`.
    pub fn from_name(s: &str) -> Key {
        let lower = s.to_ascii_lowercase();
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Key::Letter(c.to_ascii_uppercase());
            }
            if let Some(d) = c.to_digit(10) {
                return Key::Digit(d as u8);
            }
            let punct = match c {
                '-' => Some(Key::Minus),
                '=' => Some(Key::Equal),
                ',' => Some(Key::Comma),
                '.' => Some(Key::Period),
                '/' => Some(Key::Slash),
                ';' => Some(Key::Semicolon),
                '\'' => Some(Key::Quote),
                '\\' => Some(Key::Backslash),
                '[' => Some(Key::BracketLeft),
                ']' => Some(Key::BracketRight),
                '`' => Some(Key::Backquote),
                _ => None,
            };
            if let Some(k) = punct {
                return k;
            }
        }
        if is_function_name(s) {
            if let Ok(n) = s[1..].parse::<u8>() {
                if (1..=24).contains(&n) {
                    return Key::F(n);
                }
            }
            return Key::Unknown(s.to_string());
        }
        if let Some(rest) = lower.strip_prefix("num") {
            if rest.len() == 1 {
                if let Some(d) = rest.chars().next().and_then(|c| c.to_digit(10)) {
                    return Key::Numpad(d as u8);
                }
            }
        }
        match lower.as_str() {
            "space" => Key::Space,
            "enter" | "return" => Key::Enter,
            "tab" => Key::Tab,
            "backspace" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "insert" | "ins" => Key::Insert,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "capslock" => Key::CapsLock,
            // Inherited aliases.
            "numadd" => Key::NumpadAdd,
            "numsub" => Key::NumpadSubtract,
            "nummult" => Key::NumpadMultiply,
            "numdiv" => Key::NumpadDivide,
            "numdec" => Key::NumpadDecimal,
            "plus" => Key::Equal,
            "minus" => Key::Minus,
            "comma" => Key::Comma,
            "period" => Key::Period,
            "slash" => Key::Slash,
            "semicolon" => Key::Semicolon,
            "quote" => Key::Quote,
            "backslash" => Key::Backslash,
            _ => Key::Unknown(s.to_string()),
        }
    }

    /// Canonical (English) name, the one of the `Ctrl+Alt+Space` format.
    pub fn name(&self) -> String {
        match self {
            Key::Letter(c) => c.to_string(),
            Key::Digit(d) => d.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Numpad(d) => format!("num{d}"),
            Key::NumpadAdd => "numadd".into(),
            Key::NumpadSubtract => "numsub".into(),
            Key::NumpadMultiply => "nummult".into(),
            Key::NumpadDivide => "numdiv".into(),
            Key::NumpadDecimal => "numdec".into(),
            Key::Space => "Space".into(),
            Key::Enter => "Enter".into(),
            Key::Tab => "Tab".into(),
            Key::Backspace => "Backspace".into(),
            Key::Delete => "Delete".into(),
            Key::Insert => "Insert".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::PageUp => "PageUp".into(),
            Key::PageDown => "PageDown".into(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
            Key::Left => "Left".into(),
            Key::Right => "Right".into(),
            Key::Minus => "-".into(),
            Key::Equal => "=".into(),
            Key::Comma => ",".into(),
            Key::Period => ".".into(),
            Key::Slash => "/".into(),
            Key::Semicolon => ";".into(),
            Key::Quote => "'".into(),
            Key::Backslash => "\\".into(),
            Key::BracketLeft => "[".into(),
            Key::BracketRight => "]".into(),
            Key::Backquote => "`".into(),
            Key::CapsLock => "Capslock".into(),
            Key::Unknown(s) => s.clone(),
        }
    }

    /// True for any recognised key (everything except `Unknown`).
    pub fn is_known(&self) -> bool {
        !matches!(self, Key::Unknown(_))
    }

    /// True for `F` + 1 to 2 digits: exempt from the modifier requirement (like the
    /// `F\d{1,2}` regex, which therefore also accepts F13-F99).
    pub fn is_function_key(&self) -> bool {
        match self {
            Key::F(_) => true,
            Key::Unknown(s) => is_function_name(s),
            _ => false,
        }
    }

    /// Canonical evdev code of the key.
    pub fn to_evdev(&self) -> Option<u16> {
        evdev_table()
            .into_iter()
            .find(|(_, k)| k == self)
            .map(|(c, _)| c)
    }

    /// Key matching an evdev code (`None` for modifiers and unsupported keys).
    pub fn from_evdev(code: u16) -> Option<Key> {
        evdev_table()
            .into_iter()
            .find(|(c, _)| *c == code)
            .map(|(_, k)| k)
    }

    /// X11/GTK keysym name (`space`, `Return`, `Page_Up`…).
    pub fn to_keysym_name(&self) -> Option<String> {
        let s = match self {
            Key::Letter(c) => c.to_ascii_lowercase().to_string(),
            Key::Digit(d) => d.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Numpad(d) => format!("KP_{d}"),
            Key::NumpadAdd => "KP_Add".into(),
            Key::NumpadSubtract => "KP_Subtract".into(),
            Key::NumpadMultiply => "KP_Multiply".into(),
            Key::NumpadDivide => "KP_Divide".into(),
            Key::NumpadDecimal => "KP_Decimal".into(),
            Key::Space => "space".into(),
            Key::Enter => "Return".into(),
            Key::Tab => "Tab".into(),
            Key::Backspace => "BackSpace".into(),
            Key::Delete => "Delete".into(),
            Key::Insert => "Insert".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::PageUp => "Page_Up".into(),
            Key::PageDown => "Page_Down".into(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
            Key::Left => "Left".into(),
            Key::Right => "Right".into(),
            Key::Minus => "minus".into(),
            Key::Equal => "equal".into(),
            Key::Comma => "comma".into(),
            Key::Period => "period".into(),
            Key::Slash => "slash".into(),
            Key::Semicolon => "semicolon".into(),
            Key::Quote => "apostrophe".into(),
            Key::Backslash => "backslash".into(),
            Key::BracketLeft => "bracketleft".into(),
            Key::BracketRight => "bracketright".into(),
            Key::Backquote => "grave".into(),
            Key::CapsLock => "Caps_Lock".into(),
            Key::Unknown(_) => return None,
        };
        Some(s)
    }

    /// Key matching a keysym name (case-sensitive except letters: GTK gives
    /// `A` when Shift is held).
    pub fn from_keysym_name(s: &str) -> Option<Key> {
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Some(Key::Letter(c.to_ascii_uppercase()));
            }
            if let Some(d) = c.to_digit(10) {
                return Some(Key::Digit(d as u8));
            }
        }
        if let Some(rest) = s.strip_prefix("KP_") {
            let mut it = rest.chars();
            if let (Some(c), None) = (it.next(), it.next()) {
                if let Some(d) = c.to_digit(10) {
                    return Some(Key::Numpad(d as u8));
                }
            }
        }
        if s.len() >= 2 && s.starts_with('F') && s[1..].bytes().all(|b| b.is_ascii_digit()) {
            return match s[1..].parse::<u8>() {
                Ok(n) if (1..=24).contains(&n) => Some(Key::F(n)),
                _ => None,
            };
        }
        let k = match s {
            "space" => Key::Space,
            "Return" | "KP_Enter" => Key::Enter,
            "KP_Add" => Key::NumpadAdd,
            "KP_Subtract" => Key::NumpadSubtract,
            "KP_Multiply" => Key::NumpadMultiply,
            "KP_Divide" => Key::NumpadDivide,
            "KP_Decimal" => Key::NumpadDecimal,
            "Tab" | "ISO_Left_Tab" => Key::Tab,
            "BackSpace" => Key::Backspace,
            "Delete" => Key::Delete,
            "Insert" => Key::Insert,
            "Home" => Key::Home,
            "End" => Key::End,
            "Page_Up" | "Prior" => Key::PageUp,
            "Page_Down" | "Next" => Key::PageDown,
            "Up" => Key::Up,
            "Down" => Key::Down,
            "Left" => Key::Left,
            "Right" => Key::Right,
            "minus" => Key::Minus,
            "equal" => Key::Equal,
            "comma" => Key::Comma,
            "period" => Key::Period,
            "slash" => Key::Slash,
            "semicolon" => Key::Semicolon,
            "apostrophe" => Key::Quote,
            "backslash" => Key::Backslash,
            "bracketleft" => Key::BracketLeft,
            "bracketright" => Key::BracketRight,
            "grave" => Key::Backquote,
            "Caps_Lock" => Key::CapsLock,
            _ => return None,
        };
        Some(k)
    }

    /// True if this evdev code designates this key (numpad Enter included).
    pub fn matches_evdev(&self, code: u16) -> bool {
        Key::from_evdev(code).as_ref() == Some(self)
    }
}

/// Shortcut: modifiers and an optional normal key (absent = modifiers only).
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct Accelerator {
    pub modifiers: Modifiers,
    pub key: Option<Key>,
}

impl Accelerator {
    pub fn is_modifier_only(&self) -> bool {
        self.key.is_none()
    }

    /// Canonical tokens in English (`["Ctrl", "Alt", "Space"]`); the interface localises them.
    pub fn display_tokens(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .modifiers
            .iter()
            .map(|m| m.name().to_string())
            .collect();
        if let Some(k) = &self.key {
            v.push(k.name());
        }
        v
    }
}

impl fmt::Display for Accelerator {
    /// Canonical form: `Ctrl+Alt+Shift+Super+Key`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_tokens().join("+"))
    }
}

/// Parses `Ctrl+Alt+Space` (modifier and key aliases are case-insensitive).
pub fn parse(s: &str) -> Result<Accelerator, AcceleratorError> {
    let mut acc = Accelerator::default();
    let mut any = false;
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        any = true;
        if let Some(m) = Modifier::from_name(part) {
            acc.modifiers.insert(m);
        } else if acc.key.is_some() {
            return Err(AcceleratorError::MultipleKeys);
        } else {
            acc.key = Some(Key::from_name(part));
        }
    }
    if !any {
        return Err(AcceleratorError::Empty);
    }
    Ok(acc)
}

/// Nearly universal combinations (editing, windows, virtual console, lock) that a
/// dictation shortcut must not grab: the desktop or application that owns them would lose
/// or, worse, they would stop working everywhere.
pub fn is_reserved(acc: &Accelerator) -> bool {
    let Some(key) = &acc.key else {
        return false;
    };
    let k = key.name().to_ascii_lowercase();
    let ctrl = acc.modifiers.contains(Modifier::Ctrl);
    let alt = acc.modifiers.contains(Modifier::Alt);
    let shift = acc.modifiers.contains(Modifier::Shift);
    let sup = acc.modifiers.contains(Modifier::Super);
    let only = |ctrl_: bool, alt_: bool, shift_: bool, sup_: bool| {
        (ctrl, alt, shift, sup) == (ctrl_, alt_, shift_, sup_)
    };
    let fn_number = k
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .unwrap_or(0);
    (only(true, false, false, false)
        && matches!(
            k.as_str(),
            "c" | "v" | "x" | "z" | "y" | "a" | "s" | "q" | "w"
        ))
        || (only(true, false, true, false) && matches!(k.as_str(), "c" | "v"))
        || (only(false, true, false, false) && (k == "tab" || fn_number == 4))
        || (only(true, true, false, false) && (k == "delete" || (1..=12).contains(&fn_number)))
        || (only(false, false, false, true) && k == "l")
}

/// Validation rules (`localCheck` + `shortcut-test`, excluding the real registration
/// attempt, which depends on the backend).
pub fn validate(acc: &Accelerator, mode: ShortcutMode) -> Result<(), AcceleratorError> {
    let n = acc.modifiers.count();
    match &acc.key {
        None => {
            if n == 0 {
                return Err(AcceleratorError::Empty);
            }
            if n < 2 {
                return Err(AcceleratorError::ModifiersOnlyNeedTwo);
            }
        }
        Some(k) => {
            if n == 0 && !k.is_function_key() {
                return Err(AcceleratorError::NeedsModifier);
            }
            if is_reserved(acc) {
                return Err(AcceleratorError::Reserved(acc.to_string()));
            }
            if mode == ShortcutMode::Ptt && !k.is_known() {
                return Err(AcceleratorError::KeyNotRecognized(k.name()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn p(s: &str) -> Accelerator {
        parse(s).expect("parse")
    }

    #[test]
    fn reserved_combinations_are_refused_in_both_modes() {
        for text in [
            "Ctrl+C",
            "Ctrl+V",
            "Ctrl+Shift+V",
            "Alt+Tab",
            "Alt+F4",
            "Ctrl+Alt+Delete",
            "Ctrl+Alt+F2",
            "Super+L",
        ] {
            for mode in [ShortcutMode::Toggle, ShortcutMode::Ptt] {
                assert!(
                    matches!(validate(&p(text), mode), Err(AcceleratorError::Reserved(_))),
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn usual_dictation_shortcuts_are_not_reserved() {
        for text in [
            "Ctrl+Alt+Space",
            "Ctrl+Super",
            "F9",
            "Ctrl+Shift+Space",
            "Super+Space",
            "Ctrl+Alt+D",
            "Alt+Space",
        ] {
            assert_eq!(validate(&p(text), ShortcutMode::Toggle), Ok(()), "{text}");
        }
    }

    #[test]
    fn alias_modificateurs() {
        for (alias, m) in [
            ("ctrl", Modifier::Ctrl),
            ("Control", Modifier::Ctrl),
            ("CommandOrControl", Modifier::Ctrl),
            ("CMDORCTRL", Modifier::Ctrl),
            ("alt", Modifier::Alt),
            ("SHIFT", Modifier::Shift),
            ("super", Modifier::Super),
            ("Meta", Modifier::Super),
            ("cmd", Modifier::Super),
            ("Command", Modifier::Super),
        ] {
            let a = p(&format!("{alias}+x"));
            assert!(a.modifiers.contains(m), "{alias}");
            assert_eq!(a.modifiers.count(), 1, "{alias}");
        }
    }

    #[test]
    fn ordre_canonique() {
        assert_eq!(
            p("super+shift+alt+ctrl+space").to_string(),
            "Ctrl+Alt+Shift+Super+Space"
        );
        assert_eq!(p("space+ctrl").to_string(), "Ctrl+Space");
        assert_eq!(p("Win").to_string(), "Win"); // unknown, kept
        assert_eq!(p(" Ctrl + Super ").to_string(), "Ctrl+Super");
        assert_eq!(p("ctrl++alt").to_string(), "Ctrl+Alt");
    }

    #[test]
    fn vide_et_plusieurs_touches() {
        assert_eq!(parse(""), Err(AcceleratorError::Empty));
        assert_eq!(parse(" + "), Err(AcceleratorError::Empty));
        assert_eq!(parse("Ctrl+A+B"), Err(AcceleratorError::MultipleKeys));
    }

    #[test]
    fn numpad_keys_have_readable_labels() {
        assert_eq!(numpad_label("numadd").as_deref(), Some("Num+"));
        assert_eq!(numpad_label("nummult").as_deref(), Some("Num*"));
        assert_eq!(numpad_label("num3").as_deref(), Some("Num3"));
        assert_eq!(numpad_label("Space"), None);
        assert_eq!(numpad_label("number"), None);
    }

    #[test]
    fn aller_retour_toutes_les_touches() {
        let mut names: Vec<String> = Vec::new();
        names.extend(('A'..='Z').map(|c| c.to_string()));
        names.extend((0..=9).map(|d| d.to_string()));
        names.extend((1..=24).map(|n| format!("F{n}")));
        names.extend((0..=9).map(|n| format!("num{n}")));
        names.extend(["numadd", "numsub", "nummult", "numdiv", "numdec"].map(String::from));
        for n in [
            "Space",
            "Enter",
            "Tab",
            "Backspace",
            "Delete",
            "Insert",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Up",
            "Down",
            "Left",
            "Right",
            "-",
            "=",
            ",",
            ".",
            "/",
            ";",
            "'",
            "\\",
            "[",
            "]",
            "`",
            "Capslock",
        ] {
            names.push(n.to_string());
        }
        assert_eq!(names.len(), 26 + 10 + 24 + 10 + 5 + 26);
        for n in &names {
            let a = p(&format!("Ctrl+Alt+{n}"));
            assert!(a.key.as_ref().unwrap().is_known(), "{n}");
            assert_eq!(a.to_string(), format!("Ctrl+Alt+{n}"));
            assert_eq!(p(&a.to_string()), a);
            // Without a modifier too.
            assert_eq!(p(n).to_string(), *n);
        }
    }

    #[test]
    fn casse_et_alias_de_touches() {
        assert_eq!(p("ctrl+a").to_string(), "Ctrl+A");
        assert_eq!(p("ctrl+f5").to_string(), "Ctrl+F5");
        assert_eq!(p("ctrl+NUM3").to_string(), "Ctrl+num3");
        assert_eq!(p("ctrl+return").to_string(), "Ctrl+Enter");
        assert_eq!(p("ctrl+esc").to_string(), "Ctrl+esc");
        assert_eq!(p("ctrl+CAPSLOCK").to_string(), "Ctrl+Capslock");
        assert_eq!(p("ctrl+plus").to_string(), "Ctrl+=");
        assert_eq!(p("ctrl+pageup").to_string(), "Ctrl+PageUp");
    }

    #[test]
    fn modificateurs_seuls() {
        let a = p("ctrl+super");
        assert!(a.is_modifier_only());
        assert_eq!(a.to_string(), "Ctrl+Super");
        assert_eq!(a.display_tokens(), vec!["Ctrl", "Super"]);
    }

    #[test]
    fn validation_modificateurs_seuls() {
        for mode in [ShortcutMode::Toggle, ShortcutMode::Ptt] {
            assert_eq!(
                validate(&p("ctrl"), mode),
                Err(AcceleratorError::ModifiersOnlyNeedTwo)
            );
            assert_eq!(
                validate(&p("super"), mode),
                Err(AcceleratorError::ModifiersOnlyNeedTwo)
            );
            assert_eq!(validate(&p("ctrl+super"), mode), Ok(()));
            assert_eq!(validate(&p("ctrl+alt+shift"), mode), Ok(()));
            assert_eq!(
                validate(&Accelerator::default(), mode),
                Err(AcceleratorError::Empty)
            );
        }
    }

    #[test]
    fn validation_touche_sans_modificateur() {
        for mode in [ShortcutMode::Toggle, ShortcutMode::Ptt] {
            assert_eq!(
                validate(&p("A"), mode),
                Err(AcceleratorError::NeedsModifier)
            );
            assert_eq!(
                validate(&p("Space"), mode),
                Err(AcceleratorError::NeedsModifier)
            );
            assert_eq!(
                validate(&p("5"), mode),
                Err(AcceleratorError::NeedsModifier)
            );
            for n in 1..=12 {
                assert_eq!(validate(&p(&format!("F{n}")), mode), Ok(()), "F{n}");
            }
            assert_eq!(validate(&p("ctrl+K"), mode), Ok(()));
            assert_eq!(validate(&p("shift+Space"), mode), Ok(()));
        }
    }

    #[test]
    fn validation_f13_et_au_dela() {
        // The original regex `F\d{1,2}` accepts F13..F99 without a modifier.
        assert_eq!(validate(&p("F24"), ShortcutMode::Ptt), Ok(()));
        assert_eq!(validate(&p("F13"), ShortcutMode::Toggle), Ok(()));
        // F30 is unknown: accepted in toggle mode, refused in ptt.
        assert_eq!(validate(&p("F30"), ShortcutMode::Toggle), Ok(()));
        assert_eq!(
            validate(&p("F30"), ShortcutMode::Ptt),
            Err(AcceleratorError::KeyNotRecognized("F30".into()))
        );
    }

    #[test]
    fn validation_touche_inconnue_en_ptt() {
        let a = p("ctrl+Esc");
        assert_eq!(validate(&a, ShortcutMode::Toggle), Ok(()));
        assert_eq!(
            validate(&a, ShortcutMode::Ptt),
            Err(AcceleratorError::KeyNotRecognized("Esc".into()))
        );
        // The modifier rule comes before the recognised-key rule.
        assert_eq!(
            validate(&p("Esc"), ShortcutMode::Ptt),
            Err(AcceleratorError::NeedsModifier)
        );
    }

    #[test]
    fn evdev_aller_retour() {
        let mut keys: Vec<Key> = Vec::new();
        keys.extend(('A'..='Z').map(Key::Letter));
        keys.extend((0..=9).map(Key::Digit));
        keys.extend((1..=24).map(Key::F));
        keys.extend((0..=9).map(Key::Numpad));
        keys.extend([
            Key::NumpadAdd,
            Key::NumpadSubtract,
            Key::NumpadMultiply,
            Key::NumpadDivide,
            Key::NumpadDecimal,
        ]);
        for n in [
            "Space",
            "Enter",
            "Tab",
            "Backspace",
            "Delete",
            "Insert",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Up",
            "Down",
            "Left",
            "Right",
            "-",
            "=",
            ",",
            ".",
            "/",
            ";",
            "'",
            "\\",
            "[",
            "]",
            "`",
            "Capslock",
        ] {
            keys.push(Key::from_name(n));
        }
        let mut seen = std::collections::HashSet::new();
        for k in keys {
            let code = k.to_evdev().unwrap_or_else(|| panic!("no code for {k:?}"));
            assert!(seen.insert(code), "code {code} en double");
            assert_eq!(Key::from_evdev(code), Some(k.clone()));
            assert!(k.matches_evdev(code));
        }
    }

    #[test]
    fn evdev_valeurs_connues() {
        assert_eq!(Key::Letter('A').to_evdev(), Some(30));
        assert_eq!(Key::Letter('Q').to_evdev(), Some(16));
        assert_eq!(Key::Digit(0).to_evdev(), Some(11));
        assert_eq!(Key::F(1).to_evdev(), Some(59));
        assert_eq!(Key::F(12).to_evdev(), Some(88));
        assert_eq!(Key::F(13).to_evdev(), Some(183));
        assert_eq!(Key::Space.to_evdev(), Some(57));
        assert_eq!(Key::Numpad(0).to_evdev(), Some(82));
        assert_eq!(Key::Backquote.to_evdev(), Some(41));
        assert_eq!(Key::Unknown("Esc".into()).to_evdev(), None);
        assert_eq!(Key::from_evdev(evdev::KEY_KPENTER), Some(Key::Enter));
        assert!(Key::Enter.matches_evdev(evdev::KEY_KPENTER));
        assert_eq!(Key::from_evdev(evdev::KEY_LEFTCTRL), None);
        assert_eq!(Key::from_evdev(evdev::KEY_ESC), None);
    }

    #[test]
    fn modificateurs_evdev() {
        use evdev::*;
        for (code, m) in [
            (KEY_LEFTCTRL, Modifier::Ctrl),
            (KEY_RIGHTCTRL, Modifier::Ctrl),
            (KEY_LEFTALT, Modifier::Alt),
            (KEY_RIGHTALT, Modifier::Alt),
            (KEY_LEFTSHIFT, Modifier::Shift),
            (KEY_RIGHTSHIFT, Modifier::Shift),
            (KEY_LEFTMETA, Modifier::Super),
            (KEY_RIGHTMETA, Modifier::Super),
        ] {
            assert_eq!(Modifier::from_evdev(code), Some(m));
        }
        assert_eq!(Modifier::from_evdev(KEY_A), None);
        assert_eq!(Modifier::from_evdev(KEY_CAPSLOCK), None);
    }

    #[test]
    fn keysym_aller_retour() {
        let mut keys: Vec<Key> = Vec::new();
        keys.extend(('A'..='Z').map(Key::Letter));
        keys.extend((0..=9).map(Key::Digit));
        keys.extend((1..=24).map(Key::F));
        keys.extend((0..=9).map(Key::Numpad));
        keys.extend([
            Key::NumpadAdd,
            Key::NumpadSubtract,
            Key::NumpadMultiply,
            Key::NumpadDivide,
            Key::NumpadDecimal,
        ]);
        for n in [
            "Space",
            "Enter",
            "Tab",
            "Backspace",
            "Delete",
            "Insert",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Up",
            "Down",
            "Left",
            "Right",
            "-",
            "=",
            ",",
            ".",
            "/",
            ";",
            "'",
            "\\",
            "[",
            "]",
            "`",
            "Capslock",
        ] {
            keys.push(Key::from_name(n));
        }
        for k in keys {
            let name = k.to_keysym_name().expect("keysym");
            assert_eq!(Key::from_keysym_name(&name), Some(k.clone()), "{name}");
        }
    }

    #[test]
    fn keysym_valeurs_connues() {
        assert_eq!(Key::Space.to_keysym_name().as_deref(), Some("space"));
        assert_eq!(Key::Enter.to_keysym_name().as_deref(), Some("Return"));
        assert_eq!(Key::Letter('A').to_keysym_name().as_deref(), Some("a"));
        assert_eq!(Key::from_keysym_name("A"), Some(Key::Letter('A')));
        assert_eq!(Key::from_keysym_name("KP_Enter"), Some(Key::Enter));
        assert_eq!(Key::from_keysym_name("F25"), None);
        assert_eq!(Key::from_keysym_name("Escape"), None);
        assert_eq!(Key::Unknown("x".into()).to_keysym_name(), None);
        assert_eq!(
            Modifier::from_keysym_name("Control_R"),
            Some(Modifier::Ctrl)
        );
        assert_eq!(Modifier::from_keysym_name("Super_L"), Some(Modifier::Super));
        assert_eq!(Modifier::from_keysym_name("Shift_L"), Some(Modifier::Shift));
        assert_eq!(Modifier::from_keysym_name("a"), None);
    }
}
