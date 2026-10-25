//! Shortcut recorder of the hub: pure state machine (`logic`) and widget (`widget`).

pub mod logic;
pub mod widget;

pub use logic::{
    check_accelerator, press_from_key_name, Modifier, Press, Recorder, RecorderView, ViewStyle,
};
pub use widget::ShortcutRecorderWidget;
