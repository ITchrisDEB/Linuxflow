//! Pure logic of the shortcut recorder: `recording`,
//! `pending` and `bad` states, repeats ignored, modifiers released without an event forgotten.

use linuxflow_i18n::{tr, tr_fmt};
use std::collections::BTreeSet;

use crate::hub::labels::accelerator_keys;

/// Modifiers, in canonical writing order (`Ctrl+Alt+Shift+Super+Key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Super,
}

impl Modifier {
    pub fn token(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
            Modifier::Super => "Super",
        }
    }
}

/// Key pressed, after classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Modifier(Modifier),
    /// Key token (`A`, `5`, `F9`, `Space`, `Up`, `num3`...).
    Key(String),
    /// Unsupported key (name shown in the error).
    Unsupported(String),
}

/// Classifies a key from its X11/GDK name (`Control_L`, `space`, `a`...) and its
/// possible Unicode character.
pub fn press_from_key_name(name: &str, unicode: Option<char>) -> Press {
    let key = |s: &str| Press::Key(s.to_string());
    match name {
        "Control_L" | "Control_R" => return Press::Modifier(Modifier::Ctrl),
        "Alt_L" | "Alt_R" | "Meta_L" | "Meta_R" => return Press::Modifier(Modifier::Alt),
        "Shift_L" | "Shift_R" => return Press::Modifier(Modifier::Shift),
        "Super_L" | "Super_R" | "Hyper_L" | "Hyper_R" => return Press::Modifier(Modifier::Super),
        "space" => return key("Space"),
        "Return" | "KP_Enter" | "ISO_Enter" => return key("Enter"),
        "Tab" | "ISO_Left_Tab" => return key("Tab"),
        "BackSpace" => return key("Backspace"),
        "Delete" => return key("Delete"),
        "Insert" => return key("Insert"),
        "Home" => return key("Home"),
        "End" => return key("End"),
        "Page_Up" => return key("PageUp"),
        "Page_Down" => return key("PageDown"),
        "Up" => return key("Up"),
        "Down" => return key("Down"),
        "Left" => return key("Left"),
        "Right" => return key("Right"),
        "Caps_Lock" => return key("Capslock"),
        // Num Lock off: GTK names the key after its navigation function;
        // it is nevertheless the same physical numpad key.
        "KP_Insert" => return key("num0"),
        "KP_End" => return key("num1"),
        "KP_Down" => return key("num2"),
        "KP_Next" | "KP_Page_Down" => return key("num3"),
        "KP_Left" => return key("num4"),
        "KP_Begin" => return key("num5"),
        "KP_Right" => return key("num6"),
        "KP_Home" => return key("num7"),
        "KP_Up" => return key("num8"),
        "KP_Prior" | "KP_Page_Up" => return key("num9"),
        "KP_Delete" | "KP_Decimal" | "KP_Separator" => return key("numdec"),
        "KP_Add" => return key("numadd"),
        "KP_Subtract" => return key("numsub"),
        "KP_Multiply" => return key("nummult"),
        "KP_Divide" => return key("numdiv"),
        _ => {}
    }
    if let Some(n) = name.strip_prefix('F').and_then(|d| d.parse::<u8>().ok()) {
        if (1..=24).contains(&n) {
            return Press::Key(format!("F{n}"));
        }
    }
    if let Some(d) = name.strip_prefix("KP_").and_then(|d| d.parse::<u8>().ok()) {
        if d <= 9 {
            return Press::Key(format!("num{d}"));
        }
    }
    match unicode {
        Some(c) if c.is_ascii_alphabetic() => Press::Key(c.to_ascii_uppercase().to_string()),
        Some(c) if c.is_ascii_digit() => Press::Key(c.to_string()),
        Some(c) if "-=,./;'\\[]`".contains(c) => Press::Key(c.to_string()),
        Some(c) if !c.is_control() => Press::Unsupported(c.to_string()),
        _ => Press::Unsupported(name.to_string()),
    }
}

/// Letter or digit of the physical QWERTY layout for an evdev code (`keycode - 8` under X11
/// and Wayland). Used as a fallback when the active layout is not Latin.
pub fn qwerty_from_evdev(code: u32) -> Option<char> {
    const ROW_TOP: &str = "QWERTYUIOP"; // 16..=25
    const ROW_HOME: &str = "ASDFGHJKL"; // 30..=38
    const ROW_BOTTOM: &str = "ZXCVBNM"; // 44..=50
    match code {
        2..=10 => char::from_digit(code - 1, 10),
        11 => Some('0'),
        16..=25 => ROW_TOP.chars().nth((code - 16) as usize),
        30..=38 => ROW_HOME.chars().nth((code - 30) as usize),
        44..=50 => ROW_BOTTOM.chars().nth((code - 44) as usize),
        _ => None,
    }
}

/// Canonical shortcut from the modifiers and the key.
pub fn accelerator_from(mods: &BTreeSet<Modifier>, key: Option<&str>) -> String {
    let mut parts: Vec<&str> = mods.iter().map(|m| m.token()).collect();
    if let Some(k) = key {
        parts.push(k);
    }
    parts.join("+")
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F')
        .is_some_and(|d| !d.is_empty() && d.len() <= 2 && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Local check of a shortcut (before `Controller::shortcut_test`). `None` = acceptable.
pub fn check_accelerator(acc: &str) -> Option<String> {
    let parts: Vec<&str> = acc.split('+').filter(|p| !p.is_empty()).collect();
    let is_mod = |p: &&str| matches!(*p, "Ctrl" | "Alt" | "Shift" | "Super");
    let mods = parts.iter().filter(|p| is_mod(p)).count();
    let key = parts.iter().find(|p| !is_mod(p));
    if parts.is_empty() {
        return Some(tr("Press at least one key combination."));
    }
    match key {
        None if mods >= 2 => None,
        None => Some(tr(
            "With modifier keys only, at least two are needed (for example Ctrl + Super).",
        )),
        Some(k) if mods == 0 && !is_function_key(k) => Some(tr(
            "Add Ctrl, Alt, Shift or Super (or use a function key such as F9).",
        )),
        Some(_) => match linuxflow_core::accelerator::parse(acc) {
            Ok(a) if linuxflow_core::accelerator::is_reserved(&a) => Some(tr_fmt(
                "{} is used by the system or by most applications. Choose another shortcut.",
                &[&a.to_string()],
            )),
            _ => None,
        },
    }
}

/// Does the key assigned by the desktop ("Ctrl+Alt+Space", "Press Ctrl+Alt+Space"…) match
/// the requested one (`Ctrl+Alt+Space`)? Comparison tolerant of the desktop's wording: every requested
/// element must appear among those of the description.
pub fn trigger_matches(requested: &str, desktop: &str) -> bool {
    fn canon(token: &str) -> String {
        match token.to_lowercase().as_str() {
            "control" => "ctrl".into(),
            "meta" | "win" | "logo" | "windows" => "super".into(),
            "return" => "enter".into(),
            "escape" => "esc".into(),
            other => other.to_string(),
        }
    }
    let have: Vec<String> = desktop
        .split(|c: char| c == '+' || c.is_whitespace() || c == '<' || c == '>')
        .filter(|t| !t.is_empty())
        .map(canon)
        .collect();
    requested
        .split('+')
        .filter(|t| !t.is_empty())
        .map(canon)
        .all(|t| have.contains(&t))
}

/// Appearance of the key area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewStyle {
    Normal,
    Recording,
    Pending,
    Bad,
}

impl ViewStyle {
    pub fn css_class(self) -> Option<&'static str> {
        match self {
            ViewStyle::Normal => None,
            ViewStyle::Recording => Some("recording"),
            ViewStyle::Pending => Some("pending"),
            ViewStyle::Bad => Some("bad"),
        }
    }
}

/// What the widget must display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderView {
    pub keys: Vec<String>,
    pub placeholder: String,
    pub style: ViewStyle,
    pub hint: String,
    pub hint_bad: bool,
    pub recording: bool,
    pub confirm_enabled: bool,
    pub cancel_visible: bool,
}

/// Result of processing a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressOutcome {
    Ignored,
    Updated,
}

#[derive(Debug, Default)]
pub struct Recorder {
    recording: bool,
    /// Held keys: (hardware identifier, possible modifier).
    held: Vec<(u32, Option<Modifier>)>,
    mods: BTreeSet<Modifier>,
    key: Option<String>,
    pending: Option<String>,
    error: Option<String>,
    /// Key actually assigned by the desktop (portal), if known.
    desktop_trigger: Option<String>,
    /// Precise reason for the failure of the current shortcut (named conflict), if the system gives it.
    failure: Option<String>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Did the desktop assign a different key than the requested one?
    fn desktop_differs(&self, current: &str) -> bool {
        self.desktop_trigger
            .as_deref()
            .is_some_and(|d| !trigger_matches(current, d))
    }

    pub fn desktop_trigger(&self) -> Option<&str> {
        self.desktop_trigger.as_deref()
    }

    pub fn set_failure(&mut self, message: Option<String>) {
        self.failure = message;
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn set_desktop_trigger(&mut self, trigger: Option<String>) {
        self.desktop_trigger = trigger;
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn pending(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    /// Starts the recording: the pending shortcut is forgotten.
    pub fn start(&mut self) {
        self.recording = true;
        self.held.clear();
        self.error = None;
        self.pending = None;
        self.mods.clear();
        self.key = None;
    }

    /// Stops listening while keeping the pending shortcut.
    pub fn stop(&mut self) {
        self.recording = false;
        self.held.clear();
        self.error = None;
    }

    /// Cancels everything (listening and pending shortcut).
    pub fn discard(&mut self) {
        self.stop();
        self.pending = None;
        self.mods.clear();
        self.key = None;
    }

    /// The pending shortcut has been registered.
    pub fn committed(&mut self) {
        self.discard();
    }

    /// A key is pressed. `active` = modifiers really active according to the system.
    pub fn press(&mut self, id: u32, press: Press, active: &BTreeSet<Modifier>) -> PressOutcome {
        if !self.recording || self.held.iter().any(|(h, _)| *h == id) {
            return PressOutcome::Ignored; // not recording, or automatic repeat
        }
        // Forgets the modifiers released without an event (e.g. Super opening a menu).
        self.held.retain(|(h, m)| match m {
            Some(m) if *h != id => active.contains(m),
            _ => true,
        });
        if self.held.is_empty() {
            // A new combination replaces the previous one.
            self.mods.clear();
            self.key = None;
        }
        match press {
            Press::Modifier(m) => {
                self.held.push((id, Some(m)));
                self.mods.insert(m);
            }
            Press::Key(k) => {
                self.held.push((id, None));
                self.key = Some(k);
            }
            Press::Unsupported(name) => {
                self.held.push((id, None));
                self.error = Some(tr_fmt("The key “{}” is not supported.", &[&name]));
                return PressOutcome::Updated;
            }
        }
        self.error = None;
        self.pending = Some(accelerator_from(&self.mods, self.key.as_deref()));
        PressOutcome::Updated
    }

    pub fn release(&mut self, id: u32) {
        self.held.retain(|(h, _)| *h != id);
    }

    /// The window loses focus: the held keys are forgotten.
    pub fn blur(&mut self) {
        self.held.clear();
    }

    /// Description of the display for the `current` shortcut (setting) and its `shortcut_ok` state.
    pub fn view(&self, current: &str, shortcut_ok: bool) -> RecorderView {
        let pending = self.pending.as_deref().filter(|p| *p != current);
        let problem = pending.and_then(check_accelerator);
        let (keys, style, hint) = if self.recording {
            (
                self.pending.as_deref().map(accelerator_keys).unwrap_or_default(),
                ViewStyle::Recording,
                problem.clone().unwrap_or_else(|| {
                    tr("Press the combination you want (you can start over). Click Stop when it is right, then Confirm.")
                }),
            )
        } else if let Some(p) = pending {
            let bad = problem.is_some() || self.error.is_some();
            (
                accelerator_keys(p),
                if bad {
                    ViewStyle::Bad
                } else {
                    ViewStyle::Pending
                },
                self.error
                    .clone()
                    .or_else(|| problem.clone())
                    .unwrap_or_else(|| tr("New shortcut ready: click Confirm to use it.")),
            )
        } else {
            (
                accelerator_keys(current),
                if shortcut_ok && !self.desktop_differs(current) {
                    ViewStyle::Normal
                } else {
                    ViewStyle::Bad
                },
                self.error.clone().unwrap_or_else(|| {
                    if let (true, Some(key)) = (shortcut_ok, &self.desktop_trigger) {
                        if trigger_matches(current, key) {
                            tr("Shortcut active. Click Configure to choose another one.")
                        } else {
                            tr_fmt(
                                "“{}” is already used by another application: your desktop assigned “{}” instead. Click Configure to choose another shortcut.",
                                &[&accelerator_keys(current).join(" + "), key],
                            )
                        }
                    } else if shortcut_ok {
                        tr("Shortcut active. Click Configure to choose another one.")
                    } else if let Some(why) = &self.failure {
                        why.clone()
                    } else {
                        tr("This shortcut does not work (already used by another application?). Choose another one.")
                    }
                }),
            )
        };
        RecorderView {
            keys,
            placeholder: if self.recording {
                tr("Press your keys…")
            } else {
                "—".to_string()
            },
            style,
            hint,
            hint_bad: self.error.is_some()
                || (problem.is_some() && !self.recording)
                || ((!shortcut_ok || self.desktop_differs(current))
                    && pending.is_none()
                    && !self.recording),
            recording: self.recording,
            confirm_enabled: pending.is_some() && problem.is_none(),
            cancel_visible: self.recording || pending.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(list: &[Modifier]) -> BTreeSet<Modifier> {
        list.iter().copied().collect()
    }

    fn ctrl() -> Press {
        Press::Modifier(Modifier::Ctrl)
    }

    #[test]
    fn numpad_keys_are_the_same_with_numlock_off() {
        for (name, token) in [
            ("KP_Insert", "num0"),
            ("KP_End", "num1"),
            ("KP_Down", "num2"),
            ("KP_Next", "num3"),
            ("KP_Left", "num4"),
            ("KP_Begin", "num5"),
            ("KP_Right", "num6"),
            ("KP_Home", "num7"),
            ("KP_Up", "num8"),
            ("KP_Prior", "num9"),
            ("KP_Delete", "numdec"),
            ("KP_Separator", "numdec"),
        ] {
            assert_eq!(press_from_key_name(name, None), Press::Key(token.into()));
        }
    }

    #[test]
    fn numpad_operator_keys_are_recorded() {
        for (name, ch, token) in [
            ("KP_Add", '+', "numadd"),
            ("KP_Subtract", '-', "numsub"),
            ("KP_Multiply", '*', "nummult"),
            ("KP_Divide", '/', "numdiv"),
            ("KP_Decimal", '.', "numdec"),
        ] {
            assert_eq!(
                press_from_key_name(name, Some(ch)),
                Press::Key(token.into())
            );
        }
    }

    #[test]
    fn classifies_keys() {
        assert_eq!(press_from_key_name("Control_R", None), ctrl());
        assert_eq!(
            press_from_key_name("Super_L", None),
            Press::Modifier(Modifier::Super)
        );
        assert_eq!(
            press_from_key_name("space", Some(' ')),
            Press::Key("Space".into())
        );
        assert_eq!(press_from_key_name("a", Some('a')), Press::Key("A".into()));
        assert_eq!(press_from_key_name("A", Some('A')), Press::Key("A".into()));
        assert_eq!(press_from_key_name("5", Some('5')), Press::Key("5".into()));
        assert_eq!(press_from_key_name("F9", None), Press::Key("F9".into()));
        assert_eq!(
            press_from_key_name("F25", None),
            Press::Unsupported("F25".into())
        );
        assert_eq!(
            press_from_key_name("KP_3", Some('3')),
            Press::Key("num3".into())
        );
        assert_eq!(
            press_from_key_name("Page_Up", None),
            Press::Key("PageUp".into())
        );
        assert_eq!(
            press_from_key_name("semicolon", Some(';')),
            Press::Key(";".into())
        );
        assert_eq!(
            press_from_key_name("Cyrillic_a", Some('а')),
            Press::Unsupported("а".into())
        );
        assert_eq!(
            press_from_key_name("Escape", Some('\u{1b}')),
            Press::Unsupported("Escape".into())
        );
    }

    #[test]
    fn evdev_fallback() {
        assert_eq!(qwerty_from_evdev(30), Some('A'));
        assert_eq!(qwerty_from_evdev(16), Some('Q'));
        assert_eq!(qwerty_from_evdev(50), Some('M'));
        assert_eq!(qwerty_from_evdev(2), Some('1'));
        assert_eq!(qwerty_from_evdev(11), Some('0'));
        assert_eq!(qwerty_from_evdev(57), None);
    }

    #[test]
    fn local_checks() {
        assert!(check_accelerator("").is_some());
        assert!(check_accelerator("A").is_some());
        assert!(check_accelerator("F9").is_none());
        assert!(check_accelerator("Ctrl").is_some());
        assert!(check_accelerator("Ctrl+Super").is_none());
        assert!(check_accelerator("Ctrl+Alt+Space").is_none());
        assert!(check_accelerator("Space").is_some());
    }

    #[test]
    fn records_a_chord_and_ignores_repeats() {
        let mut r = Recorder::new();
        assert_eq!(
            r.press(1, ctrl(), &mods(&[])),
            PressOutcome::Ignored,
            "hors enregistrement"
        );
        r.start();
        assert_eq!(r.press(1, ctrl(), &mods(&[])), PressOutcome::Updated);
        assert_eq!(
            r.press(2, Press::Modifier(Modifier::Alt), &mods(&[Modifier::Ctrl])),
            PressOutcome::Updated
        );
        assert_eq!(
            r.press(
                3,
                Press::Key("Space".into()),
                &mods(&[Modifier::Ctrl, Modifier::Alt])
            ),
            PressOutcome::Updated
        );
        assert_eq!(r.pending(), Some("Ctrl+Alt+Space"));
        // Automatic repeat of the held key.
        assert_eq!(
            r.press(
                3,
                Press::Key("Space".into()),
                &mods(&[Modifier::Ctrl, Modifier::Alt])
            ),
            PressOutcome::Ignored
        );
        assert_eq!(r.pending(), Some("Ctrl+Alt+Space"));
    }

    #[test]
    fn new_chord_replaces_the_previous_one() {
        let mut r = Recorder::new();
        r.start();
        r.press(1, ctrl(), &mods(&[]));
        r.press(3, Press::Key("A".into()), &mods(&[Modifier::Ctrl]));
        r.release(3);
        r.release(1);
        r.press(5, Press::Modifier(Modifier::Alt), &mods(&[]));
        r.press(6, Press::Key("B".into()), &mods(&[Modifier::Alt]));
        assert_eq!(r.pending(), Some("Alt+B"));
    }

    #[test]
    fn forgets_modifiers_released_without_event() {
        let mut r = Recorder::new();
        r.start();
        r.press(1, Press::Modifier(Modifier::Super), &mods(&[]));
        // Super was released without an event: the system state no longer contains it.
        r.press(2, Press::Key("A".into()), &mods(&[]));
        // held is not empty at the start (Super removed => empty): new combination "A" alone.
        assert_eq!(r.pending(), Some("A"));
    }

    #[test]
    fn unsupported_key_sets_an_error_but_keeps_pending() {
        let mut r = Recorder::new();
        r.start();
        r.press(1, ctrl(), &mods(&[]));
        r.press(
            2,
            Press::Unsupported("Escape".into()),
            &mods(&[Modifier::Ctrl]),
        );
        assert!(r.error().is_some_and(|e| e.contains("Escape")));
        assert_eq!(r.pending(), Some("Ctrl"));
    }

    #[test]
    fn blur_releases_held_keys() {
        let mut r = Recorder::new();
        r.start();
        r.press(1, ctrl(), &mods(&[]));
        r.blur();
        // The same key can be pressed again (it is no longer considered a repeat).
        assert_eq!(r.press(1, ctrl(), &mods(&[])), PressOutcome::Updated);
    }

    #[test]
    fn view_states() {
        let mut r = Recorder::new();
        let v = r.view("Ctrl+Alt+Space", true);
        assert_eq!(v.style, ViewStyle::Normal);
        assert_eq!(v.keys, vec!["Ctrl", "Alt", "Space"]);
        assert!(!v.confirm_enabled && !v.cancel_visible && !v.hint_bad);

        let v = r.view("Ctrl+Alt+Space", false);
        assert_eq!(v.style, ViewStyle::Bad);
        assert!(v.hint_bad);

        r.start();
        let v = r.view("Ctrl+Alt+Space", true);
        assert_eq!(v.style, ViewStyle::Recording);
        assert!(v.keys.is_empty() && v.cancel_visible && v.recording);

        r.press(1, ctrl(), &mods(&[]));
        let v = r.view("Ctrl+Alt+Space", true);
        assert!(!v.confirm_enabled, "Ctrl alone is invalid");
        r.press(2, Press::Key("K".into()), &mods(&[Modifier::Ctrl]));
        r.stop();
        let v = r.view("Ctrl+Alt+Space", true);
        assert_eq!(v.style, ViewStyle::Pending);
        assert!(v.confirm_enabled && v.cancel_visible);
        assert_eq!(v.keys, vec!["Ctrl", "K"]);

        r.set_error(Some("taken".into()));
        let v = r.view("Ctrl+Alt+Space", true);
        assert_eq!(v.style, ViewStyle::Bad);
        assert_eq!(v.hint, "taken");

        r.discard();
        assert_eq!(r.view("Ctrl+Alt+Space", true).style, ViewStyle::Normal);
    }

    #[test]
    fn pending_equal_to_current_is_not_pending() {
        let mut r = Recorder::new();
        r.start();
        r.press(1, ctrl(), &mods(&[]));
        r.press(
            2,
            Press::Modifier(Modifier::Super),
            &mods(&[Modifier::Ctrl]),
        );
        r.stop();
        let v = r.view("Ctrl+Super", true);
        assert!(!v.confirm_enabled, "identique au raccourci actuel");
    }

    #[test]
    fn reserved_combination_is_flagged_while_recording() {
        assert!(check_accelerator("Ctrl+V").is_some());
        assert!(check_accelerator("Alt+Tab").is_some());
        assert!(check_accelerator("Ctrl+Alt+Space").is_none());
    }

    #[test]
    fn a_different_desktop_key_is_reported_with_both_keys() {
        let mut r = Recorder::new();
        assert!(!r.view("Ctrl+Alt+Space", true).hint_bad);
        r.set_desktop_trigger(Some("Ctrl+Shift+D".into()));
        let v = r.view("Ctrl+Alt+Space", true);
        assert!(v.hint.contains("Ctrl+Shift+D"), "{}", v.hint);
        assert!(v.hint_bad);
    }

    #[test]
    fn the_desktop_key_is_compared_loosely_but_a_different_key_is_a_conflict() {
        assert!(trigger_matches("Ctrl+Alt+Space", "Ctrl+Alt+Space"));
        assert!(trigger_matches("Ctrl+Alt+Space", "Press Ctrl+Alt+Space"));
        assert!(trigger_matches("Ctrl+Alt+Space", "<Ctrl><Alt>space"));
        assert!(trigger_matches("Super+D", "Meta+D"));
        assert!(trigger_matches("Ctrl+Shift+F9", "Ctrl+Shift+F9"));
        assert!(!trigger_matches("Ctrl+Alt+Space", "Ctrl+Num++"));
        assert!(!trigger_matches("Ctrl+Alt+Space", "Ctrl+Alt+D"));
    }

    #[test]
    fn a_conflicting_desktop_key_turns_the_hint_red_and_names_both_keys() {
        let mut r = Recorder::new();
        r.set_desktop_trigger(Some("Ctrl+Num++".into()));
        let v = r.view("Ctrl+Alt+Space", true);
        assert!(v.hint_bad);
        assert_eq!(v.style, ViewStyle::Bad);
        assert!(
            v.hint.contains("Ctrl+Num++") && v.hint.contains("Space"),
            "{}",
            v.hint
        );
        r.set_desktop_trigger(Some("Ctrl+Alt+Space".into()));
        let ok = r.view("Ctrl+Alt+Space", true);
        assert!(!ok.hint_bad);
        assert_eq!(ok.style, ViewStyle::Normal);
    }

    #[test]
    fn the_precise_failure_reason_replaces_the_generic_hint() {
        let mut r = Recorder::new();
        assert!(r
            .view("Ctrl+Alt+Space", false)
            .hint
            .contains("already used by another application?"));
        r.set_failure(Some(
            "Ctrl+Alt+Space is already used by Claude (Claude shortcut).".into(),
        ));
        let v = r.view("Ctrl+Alt+Space", false);
        assert!(v.hint.contains("Claude"), "{}", v.hint);
        assert!(v.hint_bad);
    }
}
