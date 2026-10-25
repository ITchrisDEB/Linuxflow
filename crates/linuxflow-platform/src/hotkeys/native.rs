//! Translation of a shortcut to the formats of the desktops' NATIVE shortcut managers:
//! KDE (KGlobalAccel: Qt keys) and GNOME (`binding` setting: `<Control><Alt>space`).
//! Pure logic, tested without a desktop.

use super::keymap;
use super::spec::{HotkeySpec, Modifiers};

// `Qt::KeyboardModifier` modifiers as encoded in a `QKeySequence`.
const QT_SHIFT: i32 = 0x0200_0000;
const QT_CTRL: i32 = 0x0400_0000;
const QT_ALT: i32 = 0x0800_0000;
const QT_META: i32 = 0x1000_0000;
const QT_KEYPAD: i32 = 0x2000_0000;

/// Qt code (`Qt::Key`) of a key from its xkb name. `None` if the key is not handled.
pub fn qt_key(xkb: &str) -> Option<i32> {
    // numpad: the ordinary Qt key + `Qt::KeypadModifier`
    if let Some(pad) = xkb.strip_prefix("KP_") {
        let key = match pad {
            "Add" => 0x2b,
            "Subtract" => 0x2d,
            "Multiply" => 0x2a,
            "Divide" => 0x2f,
            "Decimal" => 0x2e,
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => i32::from(d.as_bytes()[0]),
            _ => return None,
        };
        return Some(QT_KEYPAD | key);
    }
    // letters and digits: the ASCII code of the upper-case letter
    let mut chars = xkb.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as i32);
        }
        if c.is_ascii_digit() {
            return Some(c as i32);
        }
    }
    if let Some(n) = xkb.strip_prefix('F').and_then(|n| n.parse::<i32>().ok()) {
        if (1..=35).contains(&n) {
            return Some(0x0100_0030 + n - 1);
        }
    }
    Some(match xkb {
        "space" => 0x20,
        "apostrophe" => 0x27,
        "comma" => 0x2c,
        "minus" => 0x2d,
        "period" => 0x2e,
        "slash" => 0x2f,
        "semicolon" => 0x3b,
        "equal" => 0x3d,
        "bracketleft" => 0x5b,
        "backslash" => 0x5c,
        "bracketright" => 0x5d,
        "grave" => 0x60,
        "Escape" => 0x0100_0000,
        "Tab" => 0x0100_0001,
        "BackSpace" => 0x0100_0003,
        "Return" => 0x0100_0004,
        "Insert" => 0x0100_0006,
        "Delete" => 0x0100_0007,
        "Pause" => 0x0100_0008,
        "Print" => 0x0100_0009,
        "Home" => 0x0100_0010,
        "End" => 0x0100_0011,
        "Left" => 0x0100_0012,
        "Up" => 0x0100_0013,
        "Right" => 0x0100_0014,
        "Down" => 0x0100_0015,
        "Prior" => 0x0100_0016,
        "Next" => 0x0100_0017,
        _ => return None,
    })
}

/// Qt sequence (`QKeySequence` of a single key) of the shortcut; `None` for modifiers alone.
pub fn qt_sequence(spec: &HotkeySpec) -> Option<i32> {
    let key = qt_key(keymap::xkb_name(spec.key?)?)?;
    let mut mods = 0;
    for (m, q) in [
        (Modifiers::CTRL, QT_CTRL),
        (Modifiers::ALT, QT_ALT),
        (Modifiers::SHIFT, QT_SHIFT),
        (Modifiers::SUPER, QT_META),
    ] {
        if spec.modifiers.contains(m) {
            mods |= q;
        }
    }
    Some(mods | key)
}

/// GNOME binding (`<Control><Alt>space`); `None` for modifiers alone.
pub fn gnome_binding(spec: &HotkeySpec) -> Option<String> {
    let key = keymap::xkb_name(spec.key?)?;
    let mut out = String::new();
    for (m, tag) in [
        (Modifiers::CTRL, "<Control>"),
        (Modifiers::ALT, "<Alt>"),
        (Modifiers::SHIFT, "<Shift>"),
        (Modifiers::SUPER, "<Super>"),
    ] {
        if spec.modifiers.contains(m) {
            out.push_str(tag);
        }
    }
    out.push_str(key);
    Some(out)
}

/// Canonical form of a GNOME binding to compare those of the system: sorted modifiers, unified
/// names (`Primary` = `Control`, `Mod1` = `Alt`, `Mod4`/`Meta`/`Hyper` = `Super`), lower-case key.
/// minuscules.
pub fn canonical_gnome_binding(binding: &str) -> String {
    let mut mods: Vec<&str> = Vec::new();
    let mut rest = binding.trim();
    while let Some(start) = rest.strip_prefix('<') {
        let Some(end) = start.find('>') else { break };
        let name = &start[..end];
        let canon = match name.to_ascii_lowercase().as_str() {
            "control" | "ctrl" | "primary" | "ctl" => "ctrl",
            "alt" | "mod1" => "alt",
            "shift" => "shift",
            "super" | "mod4" | "meta" | "hyper" | "mod3" => "super",
            _ => "",
        };
        if !canon.is_empty() && !mods.contains(&canon) {
            mods.push(canon);
        }
        rest = &start[end + 1..];
    }
    mods.sort_unstable();
    let key = rest.to_ascii_lowercase();
    format!("{}+{key}", mods.join("+"))
}

/// Command run by GNOME on press: the application itself, which passes `--toggle` to
/// the instance that is already open. Single quotes for paths with spaces.
pub fn gnome_command(exe: &str) -> String {
    let quoted = if exe
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || "/._-+".contains(c)))
    {
        format!("'{}'", exe.replace('\'', "'\\''"))
    } else {
        exe.to_string()
    };
    format!("{quoted} --toggle")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> HotkeySpec {
        HotkeySpec::parse(text).unwrap()
    }

    #[test]
    fn qt_sequences_match_the_values_seen_on_a_real_kde_session() {
        // Ctrl+Alt+Space = 201326624 (value returned by KGlobalAccel for another application's shortcut)
        assert_eq!(qt_sequence(&spec("Ctrl+Alt+Space")), Some(201_326_624));
        // Ctrl+Alt+Shift+F12 = 251658299 (real test)
        assert_eq!(qt_sequence(&spec("Ctrl+Alt+Shift+F12")), Some(251_658_299));
        assert_eq!(qt_sequence(&spec("F9")), Some(0x0100_0038));
        assert_eq!(qt_sequence(&spec("Super+d")), Some(0x1000_0000 | 0x44));
        assert_eq!(qt_sequence(&spec("Ctrl+Shift+a")), Some(0x0600_0000 | 0x41));
    }

    #[test]
    fn modifier_only_shortcuts_have_no_native_form() {
        assert_eq!(qt_sequence(&spec("Ctrl+Super")), None);
        assert_eq!(gnome_binding(&spec("Ctrl+Super")), None);
    }

    #[test]
    fn gnome_bindings_use_angle_brackets_and_keysym_names() {
        assert_eq!(
            gnome_binding(&spec("Ctrl+Alt+Space")).as_deref(),
            Some("<Control><Alt>space")
        );
        assert_eq!(
            gnome_binding(&spec("Super+Shift+F9")).as_deref(),
            Some("<Shift><Super>F9")
        );
        assert_eq!(
            gnome_binding(&spec("Alt+Return")).as_deref(),
            Some("<Alt>Return")
        );
    }

    #[test]
    fn system_bindings_are_compared_in_a_canonical_form() {
        let a = canonical_gnome_binding("<Primary><Alt>space");
        assert_eq!(a, canonical_gnome_binding("<Control><Alt>space"));
        assert_eq!(a, canonical_gnome_binding("<Alt><Ctrl>SPACE"));
        assert_eq!(
            canonical_gnome_binding("<Mod4>d"),
            canonical_gnome_binding("<Super>d")
        );
        assert_ne!(a, canonical_gnome_binding("<Control><Alt>d"));
        assert_eq!(canonical_gnome_binding(""), "+");
    }

    #[test]
    fn the_gnome_command_quotes_paths_with_spaces_and_quotes() {
        assert_eq!(
            gnome_command("/usr/local/bin/linuxflow"),
            "/usr/local/bin/linuxflow --toggle"
        );
        assert_eq!(
            gnome_command("/data/My Apps/LinuxFlow.AppImage"),
            "'/data/My Apps/LinuxFlow.AppImage' --toggle"
        );
        assert_eq!(gnome_command("/a/it's"), "'/a/it'\\''s' --toggle");
    }

    #[test]
    fn numpad_keys_carry_the_keypad_modifier() {
        let plus = HotkeySpec::parse("Ctrl+NumAdd").unwrap();
        assert_eq!(qt_sequence(&plus), Some(QT_CTRL | QT_KEYPAD | 0x2b));
        let three = HotkeySpec::parse("Alt+Num3").unwrap();
        assert_eq!(qt_sequence(&three), Some(QT_ALT | QT_KEYPAD | 0x33));
        assert_eq!(gnome_binding(&plus).as_deref(), Some("<Control>KP_Add"));
    }
}
