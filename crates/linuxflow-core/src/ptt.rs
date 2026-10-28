//! State machine of the shortcut and of push-to-talk.
//!
//! Pure, deterministic logic: time comes from the events (`ts_ms`), there is no internal clock.
//! The machine consumes raw keys (evdev/XI2) or portal events and produces
//! `PttAction`s; the caller applies them to the dictation and can resynchronise the phase with
//! `set_phase` (end of dictation by timer, error, start from the status bar…).

use crate::accelerator::{Accelerator, AcceleratorError, Key, Modifier, Modifiers};
use crate::constants::{PTT_GHOST_KEY_MS, PTT_SPOILED_CANCEL_MS, PTT_TAP_MS};
use crate::domain::{Phase, ShortcutMode};
use crate::traits::{HotkeyEvent, RawKeyEvent};
use std::collections::BTreeMap;

/// Action requested from the dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttAction {
    /// Start listening.
    Start,
    /// Stop and transcribe.
    Stop,
    /// Abandon without transcribing (the combination was not meant for us).
    Cancel,
    /// Toggle (toggle mode, or fallback without release).
    Toggle,
}

/// State machine of a shortcut.
#[derive(Debug, Clone)]
pub struct PttMachine {
    mods: Modifiers,
    key: Option<Key>,
    mode: ShortcutMode,
    supports_release: bool,

    /// Pressed keys (evdev code -> instant of the first press).
    down: BTreeMap<u16, u64>,
    combo_down: bool,
    pressed_at: u64,
    hands_free: bool,
    spoiled: bool,
    /// Press in progress on the portal side (`Activated` received, `Deactivated` not yet).
    portal_down: bool,
    phase: Phase,
}

impl PttMachine {
    /// Creates the machine. `supports_release = false`: the backend does not report the release,
    /// hold mode falls back to a toggle at every press. An unrecognised key is
    /// refused in hold mode (`unknown key`); the caller then switches to
    /// toggle mode.
    pub fn new(
        accelerator: &Accelerator,
        mode: ShortcutMode,
        supports_release: bool,
    ) -> Result<Self, AcceleratorError> {
        if let Some(k) = &accelerator.key {
            if !k.is_known() {
                return Err(AcceleratorError::KeyNotRecognized(k.name()));
            }
        }
        Ok(Self {
            mods: accelerator.modifiers,
            key: accelerator.key.clone(),
            mode,
            supports_release,
            down: BTreeMap::new(),
            combo_down: false,
            pressed_at: 0,
            hands_free: false,
            spoiled: false,
            portal_down: false,
            phase: Phase::Idle,
        })
    }

    pub fn mode(&self) -> ShortcutMode {
        self.mode
    }

    /// True if the dictation is hands-free (short press, the next press stops it).
    pub fn is_hands_free(&self) -> bool {
        self.hands_free
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Resynchronises the dictation phase when it changes outside the machine.
    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        if phase != Phase::Listening {
            self.hands_free = false;
        }
    }

    /// Forgets the whole key state (shortcut suspended, listening focus lost, resume).
    pub fn reset(&mut self) {
        self.down.clear();
        self.combo_down = false;
        self.hands_free = false;
        self.spoiled = false;
        self.portal_down = false;
    }

    // ---- raw keys -------------------------------------------------------------------

    /// Processes a raw key (evdev/XI2). Automatic repeats (press of an already
    /// pressed key) have no effect.
    pub fn on_raw_key(&mut self, ev: RawKeyEvent) -> Option<PttAction> {
        if ev.down {
            self.key_down(ev.code, ev.ts_ms)
        } else {
            self.key_up(ev.code, ev.ts_ms)
        }
    }

    fn key_down(&mut self, code: u16, now: u64) -> Option<PttAction> {
        if !self.combo_down {
            // "Phantom" presses (missed releases: screen lock…).
            self.down
                .retain(|_, t| now.saturating_sub(*t) <= PTT_GHOST_KEY_MS);
        }
        self.down.entry(code).or_insert(now);
        if !self.combo_down {
            if self.combo_active() {
                self.combo_down = true;
                self.spoiled = false;
                self.pressed_at = now;
                return self.on_combo_down(now);
            }
        } else if !self.combo_active() {
            self.spoiled = true; // une autre touche a rejoint l'accord
        }
        None
    }

    fn key_up(&mut self, code: u16, now: u64) -> Option<PttAction> {
        self.down.remove(&code);
        if !self.combo_down || self.combo_active() {
            return None;
        }
        if self.foreign_key_held() {
            self.spoiled = true; // wait for the release of the foreign key
            return None;
        }
        self.combo_down = false;
        self.on_combo_up(now)
    }

    /// Exact match: same modifiers, the shortcut key, no other key.
    fn combo_active(&self) -> bool {
        let mut mods = Modifiers::default();
        let mut key_down = false;
        let mut others = 0usize;
        for code in self.down.keys() {
            if let Some(m) = Modifier::from_evdev(*code) {
                mods.insert(m);
            } else if self.key.as_ref().is_some_and(|k| k.matches_evdev(*code)) {
                key_down = true;
            } else {
                others += 1;
            }
        }
        if mods != self.mods || others > 0 {
            return false;
        }
        self.key.is_none() || key_down
    }

    fn foreign_key_held(&self) -> bool {
        self.down.keys().any(|code| {
            Modifier::from_evdev(*code).is_none()
                && !self.key.as_ref().is_some_and(|k| k.matches_evdev(*code))
        })
    }

    fn on_combo_down(&mut self, _now: u64) -> Option<PttAction> {
        match self.mode {
            ShortcutMode::Ptt => self.press_ptt(),
            // Combination with a normal key: toggles on press (like the global shortcut).
            // Modifiers alone: toggles on release (see `on_combo_up`).
            ShortcutMode::Toggle => self.key.is_some().then_some(PttAction::Toggle),
        }
    }

    fn on_combo_up(&mut self, now: u64) -> Option<PttAction> {
        let held = now.saturating_sub(self.pressed_at);
        match self.mode {
            ShortcutMode::Ptt => {
                if self.phase != Phase::Listening {
                    return None;
                }
                if self.spoiled && held < PTT_SPOILED_CANCEL_MS {
                    return self.finish(PttAction::Cancel); // e.g. Ctrl+Win+D: not a dictation
                }
                if held < PTT_TAP_MS {
                    self.hands_free = true; // appui court = mains libres
                    return None;
                }
                self.finish(PttAction::Stop)
            }
            ShortcutMode::Toggle => {
                (self.key.is_none() && !self.spoiled).then_some(PttAction::Toggle)
            }
        }
    }

    // ---- portail --------------------------------------------------------------------------

    /// Processes a portal event (`Activated`/`Deactivated`). No `spoiled` detection:
    /// reduced rules (300 ms threshold only).
    pub fn on_hotkey(&mut self, ev: HotkeyEvent) -> Option<PttAction> {
        match ev {
            HotkeyEvent::Activated { ts_ms } => self.portal_activated(ts_ms),
            HotkeyEvent::Deactivated { ts_ms } => self.portal_deactivated(ts_ms),
        }
    }

    fn portal_activated(&mut self, now: u64) -> Option<PttAction> {
        if self.mode == ShortcutMode::Toggle || !self.supports_release {
            return Some(PttAction::Toggle);
        }
        if self.portal_down {
            return None; // activation repeat
        }
        self.portal_down = true;
        self.pressed_at = now;
        self.press_ptt()
    }

    fn portal_deactivated(&mut self, now: u64) -> Option<PttAction> {
        if self.mode == ShortcutMode::Toggle || !self.supports_release || !self.portal_down {
            return None;
        }
        self.portal_down = false;
        if self.phase != Phase::Listening {
            return None;
        }
        if now.saturating_sub(self.pressed_at) < PTT_TAP_MS {
            self.hands_free = true;
            return None;
        }
        self.finish(PttAction::Stop)
    }

    // ---- commun ---------------------------------------------------------------------------

    /// Press in hold mode: stops if hands-free, starts if at rest.
    fn press_ptt(&mut self) -> Option<PttAction> {
        if self.phase == Phase::Listening && self.hands_free {
            self.hands_free = false;
            return self.finish(PttAction::Stop);
        }
        if self.phase == Phase::Idle {
            self.phase = Phase::Listening;
            self.hands_free = false;
            return Some(PttAction::Start);
        }
        None
    }

    /// Ends listening (Stop/Cancel): the phase goes back to rest until `set_phase`.
    fn finish(&mut self, action: PttAction) -> Option<PttAction> {
        self.hands_free = false;
        self.phase = Phase::Idle;
        Some(action)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::accelerator::{evdev::*, parse};

    fn machine(acc: &str, mode: ShortcutMode) -> PttMachine {
        PttMachine::new(&parse(acc).unwrap(), mode, true).unwrap()
    }

    fn dn(m: &mut PttMachine, code: u16, ts: u64) -> Option<PttAction> {
        m.on_raw_key(RawKeyEvent {
            code,
            down: true,
            ts_ms: ts,
        })
    }

    fn up(m: &mut PttMachine, code: u16, ts: u64) -> Option<PttAction> {
        m.on_raw_key(RawKeyEvent {
            code,
            down: false,
            ts_ms: ts,
        })
    }

    fn act(m: &mut PttMachine, ts: u64) -> Option<PttAction> {
        m.on_hotkey(HotkeyEvent::Activated { ts_ms: ts })
    }

    fn deact(m: &mut PttMachine, ts: u64) -> Option<PttAction> {
        m.on_hotkey(HotkeyEvent::Deactivated { ts_ms: ts })
    }

    // ---- push-to-talk, raw keys ----

    #[test]
    fn ptt_hold_2s_stops_on_release() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        assert_eq!(dn(&mut m, KEY_LEFTCTRL, 1000), None);
        assert_eq!(dn(&mut m, KEY_SPACE, 1010), Some(PttAction::Start));
        assert_eq!(m.phase(), Phase::Listening);
        assert_eq!(up(&mut m, KEY_SPACE, 3010), Some(PttAction::Stop));
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 3020), None);
        assert_eq!(m.phase(), Phase::Idle);
        assert!(!m.is_hands_free());
    }

    #[test]
    fn ptt_press_200ms_is_hands_free_then_new_press_stops() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 1000);
        assert_eq!(dn(&mut m, KEY_SPACE, 1000), Some(PttAction::Start));
        assert_eq!(up(&mut m, KEY_SPACE, 1200), None);
        assert!(m.is_hands_free());
        assert_eq!(m.phase(), Phase::Listening);
        up(&mut m, KEY_LEFTCTRL, 1210);
        // Released for 250 ms then pressed again: stops.
        dn(&mut m, KEY_LEFTCTRL, 1450);
        assert_eq!(dn(&mut m, KEY_SPACE, 1460), Some(PttAction::Stop));
        assert!(!m.is_hands_free());
        // The next release does nothing (the dictation is no longer listening).
        assert_eq!(up(&mut m, KEY_SPACE, 1500), None);
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 1510), None);
        // A new press restarts.
        dn(&mut m, KEY_LEFTCTRL, 5000);
        assert_eq!(dn(&mut m, KEY_SPACE, 5001), Some(PttAction::Start));
    }

    #[test]
    fn ptt_seuil_300ms() {
        let mut m = machine("F9", ShortcutMode::Ptt);
        assert_eq!(dn(&mut m, KEY_F9, 0), Some(PttAction::Start));
        assert_eq!(up(&mut m, KEY_F9, 299), None);
        assert!(m.is_hands_free());

        let mut m = machine("F9", ShortcutMode::Ptt);
        assert_eq!(dn(&mut m, KEY_F9, 0), Some(PttAction::Start));
        assert_eq!(up(&mut m, KEY_F9, 300), Some(PttAction::Stop));
    }

    #[test]
    fn ptt_other_key_before_700ms_cancels() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 1000);
        assert_eq!(dn(&mut m, KEY_LEFTMETA, 1010), Some(PttAction::Start));
        // Another key (D) joins the combination.
        assert_eq!(dn(&mut m, KEY_D, 1100), None);
        assert_eq!(up(&mut m, KEY_D, 1200), None); // combination exact again: nothing
        assert_eq!(up(&mut m, KEY_LEFTMETA, 1500), Some(PttAction::Cancel));
        assert_eq!(m.phase(), Phase::Idle);
        assert!(!m.is_hands_free());
    }

    #[test]
    fn ptt_short_spoiled_press_also_cancels_under_300ms() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_LEFTMETA, 0);
        dn(&mut m, KEY_D, 50);
        // D still pressed: wait for its release before concluding.
        assert_eq!(up(&mut m, KEY_LEFTMETA, 100), None);
        assert_eq!(up(&mut m, KEY_D, 120), Some(PttAction::Cancel));
    }

    #[test]
    fn ptt_spoiled_700ms_or_more_stops_normally() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 1000);
        assert_eq!(dn(&mut m, KEY_LEFTMETA, 1000), Some(PttAction::Start));
        dn(&mut m, KEY_D, 1300);
        assert_eq!(up(&mut m, KEY_LEFTMETA, 1650), None);
        assert_eq!(up(&mut m, KEY_D, 1700), Some(PttAction::Stop));

        // 699 ms : annulation.
        let mut m = machine("Ctrl+Super", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_LEFTMETA, 0);
        dn(&mut m, KEY_D, 10);
        assert_eq!(up(&mut m, KEY_LEFTMETA, 600), None);
        assert_eq!(up(&mut m, KEY_D, 699), Some(PttAction::Cancel));
    }

    #[test]
    fn ptt_foreign_key_held_waits() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_SPACE, 0);
        dn(&mut m, KEY_A, 1000); // spoiled
                                 // Ctrl and Space are released but A stays pressed: wait.
        assert_eq!(up(&mut m, KEY_SPACE, 1500), None);
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 1600), None);
        assert_eq!(m.phase(), Phase::Listening);
        // Release of A: end of the combination, duration >= 700 ms -> Stop.
        assert_eq!(up(&mut m, KEY_A, 2000), Some(PttAction::Stop));
    }

    #[test]
    fn ptt_foreign_key_already_down_prevents_start() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_A, 0);
        dn(&mut m, KEY_LEFTCTRL, 10);
        assert_eq!(dn(&mut m, KEY_SPACE, 20), None);
        assert_eq!(m.phase(), Phase::Idle);
        // A released but Ctrl+Space already pressed: the combination becomes exact on a keyup... no,
        // re-evaluation only happens on keydown: nothing starts.
        assert_eq!(up(&mut m, KEY_A, 30), None);
        assert_eq!(m.phase(), Phase::Idle);
        // A new complete press starts.
        up(&mut m, KEY_SPACE, 40);
        up(&mut m, KEY_LEFTCTRL, 41);
        dn(&mut m, KEY_LEFTCTRL, 100);
        assert_eq!(dn(&mut m, KEY_SPACE, 110), Some(PttAction::Start));
    }

    #[test]
    fn ptt_purge_of_phantom_keys_after_8s() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        // A stays "pressed" (missed release).
        dn(&mut m, KEY_A, 0);
        dn(&mut m, KEY_LEFTCTRL, 5000);
        assert_eq!(dn(&mut m, KEY_SPACE, 5010), None); // A still counted
        up(&mut m, KEY_SPACE, 5020);
        up(&mut m, KEY_LEFTCTRL, 5030);
        // 8001 ms after A: purged at the next press.
        dn(&mut m, KEY_LEFTCTRL, 8001);
        assert_eq!(dn(&mut m, KEY_SPACE, 8002), Some(PttAction::Start));
    }

    #[test]
    fn ptt_purge_exact_limit_8000ms_not_purged() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_A, 0);
        // Purged if strictly > 8000 ms: at 8000 the key is kept.
        dn(&mut m, KEY_LEFTCTRL, 8000);
        assert_eq!(dn(&mut m, KEY_SPACE, 8000), None);
    }

    #[test]
    fn ptt_no_purge_during_the_chord() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_SPACE, 0);
        // Held for 20 s: another press purges nothing (combination in progress).
        dn(&mut m, KEY_A, 20_000);
        assert_eq!(up(&mut m, KEY_A, 20_100), None);
        assert_eq!(up(&mut m, KEY_SPACE, 20_200), Some(PttAction::Stop));
    }

    #[test]
    fn ptt_auto_repeats_ignored() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 0);
        assert_eq!(dn(&mut m, KEY_SPACE, 0), Some(PttAction::Start));
        for t in (500..3000).step_by(33) {
            assert_eq!(dn(&mut m, KEY_SPACE, t), None);
            assert_eq!(dn(&mut m, KEY_LEFTCTRL, t), None);
        }
        assert_eq!(up(&mut m, KEY_SPACE, 3000), Some(PttAction::Stop));
    }

    #[test]
    fn ptt_repeat_after_hands_free_does_not_stop() {
        let mut m = machine("F9", ShortcutMode::Ptt);
        dn(&mut m, KEY_F9, 0);
        up(&mut m, KEY_F9, 100); // mains libres
        dn(&mut m, KEY_F9, 1000); // vrai appui : stop
        assert_eq!(dn(&mut m, KEY_F9, 1033), None); // repeat
        assert_eq!(dn(&mut m, KEY_F9, 1066), None);
    }

    #[test]
    fn ptt_left_and_right_modifiers() {
        let mut m = machine("Ctrl+Alt", ShortcutMode::Ptt);
        dn(&mut m, KEY_RIGHTCTRL, 0);
        assert_eq!(dn(&mut m, KEY_RIGHTALT, 0), Some(PttAction::Start));
        assert_eq!(up(&mut m, KEY_RIGHTALT, 1000), Some(PttAction::Stop));
    }

    #[test]
    fn ptt_extra_or_missing_modifiers_do_not_trigger() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_LEFTSHIFT, 0);
        assert_eq!(dn(&mut m, KEY_SPACE, 0), None); // Maj en trop
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        assert_eq!(dn(&mut m, KEY_SPACE, 0), None); // Ctrl manquant
    }

    #[test]
    fn ptt_modifier_only_keys() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Ptt);
        dn(&mut m, KEY_LEFTCTRL, 0);
        assert_eq!(dn(&mut m, KEY_LEFTMETA, 5), Some(PttAction::Start));
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 1005), Some(PttAction::Stop));
        assert_eq!(up(&mut m, KEY_LEFTMETA, 1010), None);
    }

    #[test]
    fn ptt_no_start_when_not_idle() {
        let mut m = machine("F9", ShortcutMode::Ptt);
        m.set_phase(Phase::Transcribing);
        assert_eq!(dn(&mut m, KEY_F9, 0), None);
        assert_eq!(up(&mut m, KEY_F9, 1000), None);
        m.set_phase(Phase::Idle);
        assert_eq!(dn(&mut m, KEY_F9, 2000), Some(PttAction::Start));
    }

    #[test]
    fn ptt_external_end_of_dictation_resets_hands_free() {
        let mut m = machine("F9", ShortcutMode::Ptt);
        dn(&mut m, KEY_F9, 0);
        up(&mut m, KEY_F9, 100);
        assert!(m.is_hands_free());
        m.set_phase(Phase::Idle); // minuterie maxDictationSec, par exemple
        assert!(!m.is_hands_free());
        assert_eq!(dn(&mut m, KEY_F9, 5000), Some(PttAction::Start));
    }

    #[test]
    fn ptt_reset_forgets_the_keys() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        dn(&mut m, KEY_A, 0);
        m.reset();
        dn(&mut m, KEY_LEFTCTRL, 10);
        assert_eq!(dn(&mut m, KEY_SPACE, 11), Some(PttAction::Start));
    }

    #[test]
    fn ptt_unknown_key_refused() {
        let acc = parse("Ctrl+Esc").unwrap();
        assert_eq!(
            PttMachine::new(&acc, ShortcutMode::Ptt, true).unwrap_err(),
            AcceleratorError::KeyNotRecognized("Esc".into())
        );
    }

    // ---- toggle, raw keys ----

    #[test]
    fn toggle_normal_key_acts_on_press() {
        let mut m = machine("Ctrl+Alt+Space", ShortcutMode::Toggle);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_LEFTALT, 0);
        assert_eq!(dn(&mut m, KEY_SPACE, 0), Some(PttAction::Toggle));
        assert_eq!(up(&mut m, KEY_SPACE, 100), None);
        assert_eq!(up(&mut m, KEY_LEFTALT, 110), None);
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 120), None);
    }

    #[test]
    fn toggle_modifier_only_acts_on_release() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Toggle);
        assert_eq!(dn(&mut m, KEY_LEFTCTRL, 0), None);
        assert_eq!(dn(&mut m, KEY_LEFTMETA, 10), None);
        assert_eq!(up(&mut m, KEY_LEFTMETA, 400), Some(PttAction::Toggle));
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 410), None);
        // A second cycle works.
        dn(&mut m, KEY_LEFTCTRL, 1000);
        dn(&mut m, KEY_LEFTMETA, 1001);
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 1500), Some(PttAction::Toggle));
    }

    #[test]
    fn toggle_modifier_only_spoiled_does_not_trigger() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Toggle);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_LEFTMETA, 0);
        dn(&mut m, KEY_D, 100); // Ctrl+Win+D
        up(&mut m, KEY_D, 200);
        assert_eq!(up(&mut m, KEY_LEFTMETA, 300), None);
        assert_eq!(up(&mut m, KEY_LEFTCTRL, 310), None);
        // The next one is clean.
        dn(&mut m, KEY_LEFTCTRL, 1000);
        dn(&mut m, KEY_LEFTMETA, 1000);
        assert_eq!(up(&mut m, KEY_LEFTMETA, 1100), Some(PttAction::Toggle));
    }

    #[test]
    fn toggle_modifier_only_foreign_key_held_after_release() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Toggle);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_LEFTMETA, 0);
        dn(&mut m, KEY_D, 100);
        up(&mut m, KEY_LEFTMETA, 200);
        up(&mut m, KEY_LEFTCTRL, 210);
        assert_eq!(up(&mut m, KEY_D, 300), None); // spoiled : pas de bascule
    }

    #[test]
    fn toggle_modifier_only_ignores_repeats() {
        let mut m = machine("Ctrl+Super", ShortcutMode::Toggle);
        dn(&mut m, KEY_LEFTCTRL, 0);
        dn(&mut m, KEY_LEFTMETA, 0);
        for t in (500..2000).step_by(33) {
            assert_eq!(dn(&mut m, KEY_LEFTMETA, t), None);
        }
        assert_eq!(up(&mut m, KEY_LEFTMETA, 2000), Some(PttAction::Toggle));
    }

    // ---- portail ----

    #[test]
    fn portal_ptt_hold_stops() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        assert_eq!(act(&mut m, 1000), Some(PttAction::Start));
        assert_eq!(deact(&mut m, 3000), Some(PttAction::Stop));
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn portal_ptt_short_press_is_hands_free() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        assert_eq!(act(&mut m, 1000), Some(PttAction::Start));
        assert_eq!(deact(&mut m, 1200), None);
        assert!(m.is_hands_free());
        assert_eq!(act(&mut m, 2000), Some(PttAction::Stop));
        assert_eq!(deact(&mut m, 2100), None);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn portal_ptt_without_cancel_spoiled() {
        // No spoiled detection: 500 ms of hold = Stop, never Cancel.
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        act(&mut m, 0);
        assert_eq!(deact(&mut m, 500), Some(PttAction::Stop));
    }

    #[test]
    fn portal_repeated_activations_ignored() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Ptt);
        assert_eq!(act(&mut m, 0), Some(PttAction::Start));
        assert_eq!(act(&mut m, 30), None);
        assert_eq!(act(&mut m, 60), None);
        assert_eq!(deact(&mut m, 1000), Some(PttAction::Stop));
        assert_eq!(deact(&mut m, 1001), None);
    }

    #[test]
    fn portal_toggle() {
        let mut m = machine("Ctrl+Space", ShortcutMode::Toggle);
        assert_eq!(act(&mut m, 0), Some(PttAction::Toggle));
        assert_eq!(deact(&mut m, 50), None);
        assert_eq!(act(&mut m, 1000), Some(PttAction::Toggle));
    }

    #[test]
    fn toggle_fallback_without_release() {
        let mut m =
            PttMachine::new(&parse("Ctrl+Space").unwrap(), ShortcutMode::Ptt, false).unwrap();
        assert_eq!(act(&mut m, 0), Some(PttAction::Toggle));
        assert_eq!(deact(&mut m, 50), None);
        assert_eq!(act(&mut m, 5000), Some(PttAction::Toggle));
        assert_eq!(deact(&mut m, 9000), None);
    }
}
