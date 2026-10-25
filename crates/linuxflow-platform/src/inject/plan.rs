//! Pure planning of the paste: list of keyboard events to emit, with no I/O.

use linuxflow_core::domain::PasteMode;

/// Keys used by the paste or to release before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    LeftCtrl,
    RightCtrl,
    LeftShift,
    RightShift,
    LeftAlt,
    RightAlt,
    LeftSuper,
    RightSuper,
    Space,
    Insert,
    /// The letter V: always designated by its keysym, never by a physical position.
    V,
}

impl Key {
    /// Keys to release before pasting (modifiers of the dictation shortcut and space).
    pub const RELEASABLE: [Key; 9] = [
        Key::LeftShift,
        Key::RightShift,
        Key::LeftCtrl,
        Key::RightCtrl,
        Key::LeftAlt,
        Key::RightAlt,
        Key::LeftSuper,
        Key::RightSuper,
        Key::Space,
    ];

    /// X11 keysym (lower case for the letter V).
    pub fn keysym(self) -> u32 {
        match self {
            Key::LeftCtrl => 0xffe3,
            Key::RightCtrl => 0xffe4,
            Key::LeftShift => 0xffe1,
            Key::RightShift => 0xffe2,
            Key::LeftAlt => 0xffe9,
            Key::RightAlt => 0xffea,
            Key::LeftSuper => 0xffeb,
            Key::RightSuper => 0xffec,
            Key::Space => 0x0020,
            Key::Insert => 0xff63,
            Key::V => 0x0076,
        }
    }

    /// evdev code (`KEY_*`) in the QWERTY layout: depends on the layout for `V`.
    pub fn evdev_code(self) -> u16 {
        match self {
            Key::LeftCtrl => 29,
            Key::RightCtrl => 97,
            Key::LeftShift => 42,
            Key::RightShift => 54,
            Key::LeftAlt => 56,
            Key::RightAlt => 100,
            Key::LeftSuper => 125,
            Key::RightSuper => 126,
            Key::Space => 57,
            Key::Insert => 110,
            Key::V => 47,
        }
    }

    /// XKB name (used for the virtual keyboard's keymap).
    pub fn xkb_name(self) -> &'static str {
        match self {
            Key::LeftCtrl => "Control_L",
            Key::RightCtrl => "Control_R",
            Key::LeftShift => "Shift_L",
            Key::RightShift => "Shift_R",
            Key::LeftAlt => "Alt_L",
            Key::RightAlt => "Alt_R",
            Key::LeftSuper => "Super_L",
            Key::RightSuper => "Super_R",
            Key::Space => "space",
            Key::Insert => "Insert",
            Key::V => "v",
        }
    }

    pub fn is_modifier(self) -> bool {
        !matches!(self, Key::Space | Key::Insert | Key::V)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub down: bool,
}

impl KeyEvent {
    pub const fn down(key: Key) -> Self {
        Self { key, down: true }
    }
    pub const fn up(key: Key) -> Self {
        Self { key, down: false }
    }
}

/// Events of the paste combination, to emit as a single block.
pub fn chord_events(mode: PasteMode) -> Vec<KeyEvent> {
    use Key::*;
    match mode {
        PasteMode::CtrlV => vec![
            KeyEvent::down(LeftCtrl),
            KeyEvent::down(V),
            KeyEvent::up(V),
            KeyEvent::up(LeftCtrl),
        ],
        PasteMode::CtrlShiftV => vec![
            KeyEvent::down(LeftCtrl),
            KeyEvent::down(LeftShift),
            KeyEvent::down(V),
            KeyEvent::up(V),
            KeyEvent::up(LeftShift),
            KeyEvent::up(LeftCtrl),
        ],
        PasteMode::ShiftInsert => vec![
            KeyEvent::down(LeftShift),
            KeyEvent::down(Insert),
            KeyEvent::up(Insert),
            KeyEvent::up(LeftShift),
        ],
    }
}

/// Releases of the keys really held (order of `Key::RELEASABLE`, without duplicates).
pub fn release_events(held: &[Key]) -> Vec<KeyEvent> {
    Key::RELEASABLE
        .iter()
        .filter(|k| held.contains(k))
        .map(|k| KeyEvent::up(*k))
        .collect()
}

/// Complete sequence decided before any emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastePlan {
    pub mode: PasteMode,
    /// Releases to emit first (empty if nothing is held or if the state is unknown).
    pub release: Vec<KeyEvent>,
    /// Delay before the combination.
    pub delay_ms: u64,
    pub chord: Vec<KeyEvent>,
}

pub fn plan_paste(mode: PasteMode, held: &[Key], delay_ms: u64) -> PastePlan {
    PastePlan {
        mode,
        release: release_events(held),
        delay_ms,
        chord: chord_events(mode),
    }
}

/// Checks that a sequence is balanced: every pressed key is released, in reverse order.
pub fn is_balanced(events: &[KeyEvent]) -> bool {
    let mut stack: Vec<Key> = Vec::new();
    for e in events {
        if e.down {
            stack.push(e.key);
        } else if stack.pop() != Some(e.key) {
            return false;
        }
    }
    stack.is_empty()
}

/// Releases to emit (reverse order of the press) for the keys pressed after having attempted
/// the first `attempted` events: used for the cleanup after an error in the middle of a sequence.
pub fn unwind_events(events: &[KeyEvent], attempted: usize) -> Vec<KeyEvent> {
    let mut down: Vec<Key> = Vec::new();
    for e in events.iter().take(attempted) {
        if e.down {
            down.push(e.key);
        } else if let Some(pos) = down.iter().rposition(|k| *k == e.key) {
            down.remove(pos);
        }
    }
    down.into_iter().rev().map(KeyEvent::up).collect()
}

/// Failure of an emission: `sent` events were accepted before the error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitFailure<E> {
    pub error: E,
    pub sent: usize,
}

/// Emits the events one by one; at the first error, releases as best it can (errors ignored) all
/// the keys already pressed, including the one whose emission failed (it may have left).
pub fn emit_with_unwind<E>(
    events: &[KeyEvent],
    mut emit: impl FnMut(KeyEvent) -> Result<(), E>,
) -> Result<(), EmitFailure<E>> {
    for (i, event) in events.iter().enumerate() {
        if let Err(error) = emit(*event) {
            for up in unwind_events(events, i + 1) {
                let _ = emit(up);
            }
            return Err(EmitFailure { error, sent: i });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn ctrl_v_sequence() {
        assert_eq!(
            chord_events(PasteMode::CtrlV),
            vec![
                KeyEvent::down(Key::LeftCtrl),
                KeyEvent::down(Key::V),
                KeyEvent::up(Key::V),
                KeyEvent::up(Key::LeftCtrl)
            ]
        );
    }

    #[test]
    fn ctrl_shift_v_sequence() {
        let ev = chord_events(PasteMode::CtrlShiftV);
        assert_eq!(ev.len(), 6);
        assert_eq!(ev[0], KeyEvent::down(Key::LeftCtrl));
        assert_eq!(ev[1], KeyEvent::down(Key::LeftShift));
        assert_eq!(ev[2], KeyEvent::down(Key::V));
        assert_eq!(ev[5], KeyEvent::up(Key::LeftCtrl));
    }

    #[test]
    fn shift_insert_sequence_does_not_use_letter_v() {
        let ev = chord_events(PasteMode::ShiftInsert);
        assert!(ev.iter().all(|e| e.key != Key::V));
        assert_eq!(ev[1], KeyEvent::down(Key::Insert));
        assert_eq!(ev.len(), 4);
    }

    #[test]
    fn all_chords_are_balanced() {
        for mode in [
            PasteMode::CtrlV,
            PasteMode::CtrlShiftV,
            PasteMode::ShiftInsert,
        ] {
            assert!(is_balanced(&chord_events(mode)), "{mode:?}");
        }
    }

    #[test]
    fn balance_detects_errors() {
        assert!(!is_balanced(&[KeyEvent::down(Key::V)]));
        assert!(!is_balanced(&[KeyEvent::up(Key::V)]));
        assert!(!is_balanced(&[
            KeyEvent::down(Key::LeftCtrl),
            KeyEvent::down(Key::V),
            KeyEvent::up(Key::LeftCtrl),
            KeyEvent::up(Key::V)
        ]));
    }

    #[test]
    fn release_only_held_keys_in_stable_order() {
        let held = [
            Key::Space,
            Key::LeftCtrl,
            Key::RightAlt,
            Key::V,
            Key::LeftCtrl,
        ];
        assert_eq!(
            release_events(&held),
            vec![
                KeyEvent::up(Key::LeftCtrl),
                KeyEvent::up(Key::RightAlt),
                KeyEvent::up(Key::Space)
            ]
        );
        assert!(release_events(&[]).is_empty());
    }

    #[test]
    fn plan_combines_release_delay_and_chord() {
        let plan = plan_paste(PasteMode::CtrlV, &[Key::LeftShift], 120);
        assert_eq!(plan.release, vec![KeyEvent::up(Key::LeftShift)]);
        assert_eq!(plan.delay_ms, 120);
        assert_eq!(plan.chord, chord_events(PasteMode::CtrlV));
        assert!(plan_paste(PasteMode::CtrlV, &[], 0).release.is_empty());
    }

    #[test]
    fn key_tables_are_consistent() {
        // Letter V: lower-case keysym, never confused with a physical code.
        assert_eq!(Key::V.keysym(), 'v' as u32);
        assert_eq!(Key::Insert.keysym(), 0xff63);
        assert_eq!(Key::LeftCtrl.evdev_code(), 29);
        assert_eq!(Key::Insert.evdev_code(), 110);
        let mut codes: Vec<u16> = [
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
        ]
        .iter()
        .map(|k| k.evdev_code())
        .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 11);
        assert!(Key::LeftShift.is_modifier());
        assert!(!Key::V.is_modifier());
    }

    #[test]
    fn unwind_releases_pressed_keys_in_reverse_order() {
        let ev = chord_events(PasteMode::CtrlShiftV);
        // Failure after "Ctrl↓ Shift↓" (2 attempted): release Shift then Ctrl.
        assert_eq!(
            unwind_events(&ev, 2),
            vec![KeyEvent::up(Key::LeftShift), KeyEvent::up(Key::LeftCtrl)]
        );
        // Complete sequence: nothing left to release.
        assert!(unwind_events(&ev, ev.len()).is_empty());
        assert!(unwind_events(&ev, 0).is_empty());
        // V already released: only the modifiers remain.
        assert_eq!(
            unwind_events(&ev, 4),
            vec![KeyEvent::up(Key::LeftShift), KeyEvent::up(Key::LeftCtrl)]
        );
    }

    #[test]
    fn emit_with_unwind_cleans_up_after_error() {
        let ev = chord_events(PasteMode::CtrlV);
        let mut log: Vec<KeyEvent> = Vec::new();
        let mut calls = 0;
        let result = emit_with_unwind(&ev, |e| {
            calls += 1;
            log.push(e);
            // Failure on the 2nd event (V↓); the cleanup releases go through.
            if calls == 2 {
                Err("boom")
            } else {
                Ok(())
            }
        });
        assert_eq!(
            result,
            Err(EmitFailure {
                error: "boom",
                sent: 1
            })
        );
        assert_eq!(
            log,
            vec![
                KeyEvent::down(Key::LeftCtrl),
                KeyEvent::down(Key::V),
                KeyEvent::up(Key::V),
                KeyEvent::up(Key::LeftCtrl)
            ]
        );
    }

    #[test]
    fn emit_with_unwind_ignores_cleanup_errors_and_succeeds_normally() {
        let ev = chord_events(PasteMode::ShiftInsert);
        assert_eq!(emit_with_unwind(&ev, |_| Ok::<(), ()>(())), Ok(()));
        let r = emit_with_unwind(&ev, |_| Err::<(), &str>("dead"));
        assert_eq!(
            r,
            Err(EmitFailure {
                error: "dead",
                sent: 0
            })
        );
    }
}
