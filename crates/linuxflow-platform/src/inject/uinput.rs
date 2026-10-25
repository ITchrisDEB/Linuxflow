//! Injection through `/dev/uinput` (evdev). Disabled by default: explicit option of the caller.
//! The virtual device is created ONLY ONCE (the compositor takes tens to
//! hundreds of ms to see it) then reused. The codes are physical: the letter V follows the
//! QWERTY layout, unless the caller provides `v_code`.

use std::sync::Mutex;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use linuxflow_core::domain::PasteMode;

use super::backend::{Delivery, InjectError, KeyInjector};
use super::plan::{Key, KeyEvent};

/// Wait after the device creation, so that the compositor adopts it.
const DEVICE_SETTLE: Duration = Duration::from_millis(300);
const DEVICE_NAME: &str = "linuxflow-virtual-keyboard";

/// evdev code of a key, with a possible override for the letter V.
pub fn code_for(key: Key, v_code: Option<u16>) -> u16 {
    match (key, v_code) {
        (Key::V, Some(code)) => code,
        _ => key.evdev_code(),
    }
}

/// Raw evdev events (type KEY, 1 = press, 0 = release) for a sequence.
pub fn raw_events(events: &[KeyEvent], v_code: Option<u16>) -> Vec<(u16, i32)> {
    events
        .iter()
        .map(|e| (code_for(e.key, v_code), i32::from(e.down)))
        .collect()
}

pub struct UinputInjector {
    v_code: Option<u16>,
    device: Mutex<Option<VirtualDevice>>,
}

impl UinputInjector {
    pub fn new(v_code: Option<u16>) -> Self {
        Self {
            v_code,
            device: Mutex::new(None),
        }
    }

    fn build(&self) -> Result<VirtualDevice, InjectError> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for key in Key::RELEASABLE.iter().chain([Key::Insert, Key::V].iter()) {
            keys.insert(KeyCode::new(code_for(*key, self.v_code)));
        }
        let device = VirtualDevice::builder()
            .and_then(|b| b.name(DEVICE_NAME).with_keys(&keys))
            .and_then(|b| b.build())
            .map_err(|e| InjectError::Unavailable(format!("uinput : {e}")))?;
        std::thread::sleep(DEVICE_SETTLE);
        Ok(device)
    }

    fn emit(&self, events: &[KeyEvent]) -> Result<(), InjectError> {
        let mut guard = self
            .device
            .lock()
            .map_err(|_| InjectError::Failed("poisoned uinput lock".into()))?;
        if guard.is_none() {
            *guard = Some(self.build()?);
        }
        let Some(device) = guard.as_mut() else {
            return Err(InjectError::Unavailable("uinput: no device".into()));
        };
        for (code, value) in raw_events(events, self.v_code) {
            let ev = InputEvent::new(EventType::KEY.0, code, value);
            device
                .emit(&[ev])
                .map_err(|e| InjectError::Failed(format!("uinput : {e}")))?;
        }
        Ok(())
    }
}

impl KeyInjector for UinputInjector {
    fn name(&self) -> &'static str {
        "uinput"
    }

    fn probe(&self) -> Result<(), InjectError> {
        // The device is created at first use; here only the access is checked.
        let writable = std::fs::OpenOptions::new().write(true).open("/dev/uinput");
        match writable {
            Ok(_) => Ok(()),
            Err(e) => Err(InjectError::Unavailable(format!("/dev/uinput : {e}"))),
        }
    }

    fn send_chord(&self, _mode: PasteMode, events: &[KeyEvent]) -> Result<Delivery, InjectError> {
        self.emit(events)?;
        Ok(Delivery::Sent)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::inject::plan::chord_events;

    #[test]
    fn raw_events_follow_the_plan() {
        let raw = raw_events(&chord_events(PasteMode::CtrlV), None);
        assert_eq!(raw, vec![(29, 1), (47, 1), (47, 0), (29, 0)]);
    }

    #[test]
    fn v_code_can_be_overridden_for_other_layouts() {
        let raw = raw_events(&chord_events(PasteMode::CtrlV), Some(52));
        assert_eq!(raw[1], (52, 1));
        assert_eq!(raw[0], (29, 1));
        assert_eq!(code_for(Key::Insert, Some(52)), 110);
    }
}
