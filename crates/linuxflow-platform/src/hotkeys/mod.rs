//! Global shortcut: backend chain chosen at run time.
//!
//! 1. GlobalShortcuts portal (ashpd): Activated/Deactivated;
//! 2. X11: `XGrabKey` (+ raw XInput2 for modifiers alone);
//! 3. passive evdev, without `EVIOCGRAB`, DISABLED by default (explicit option);
//! 4. no backend: control through the CLI/D-Bus.
//!
//! Pure logic (selection, grab planning, key translation, normalisation):
//! `select`, `x11_plan`, `keymap`, `normalize`, `spec`. E/S : `portal`, `x11`, `evdev_backend`.
//!
//! Emission rule: a shortcut with a main key goes through `HotkeyEvent` (portal, X11);
//! evdev and modifiers alone under X11 only emit `RawKeyEvent`s, and the core PTT
//! machine reconstructs the shortcut (`HotkeyStatus::provides_*`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod backend;
pub mod clock;
pub mod compositor;
pub mod evdev_backend;
pub mod gnome;
pub mod keymap;
pub mod kglobalaccel;
pub mod manager;
pub mod native;
pub mod normalize;
pub mod portal;
pub mod select;
pub mod spec;
pub mod x11;
pub mod x11_plan;

pub use backend::{
    BackendError, BackendErrorKind, BackendFactory, BackendReport, HotkeyBackend, StartRequest,
};
pub use manager::{HotkeyManager, HotkeyStatus};
pub use select::{plan_chain, BackendKind, HotkeyOptions};
pub use spec::{AcceleratorSpec, HotkeySpec, Modifiers};
