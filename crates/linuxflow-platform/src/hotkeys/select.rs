//! Pure selection of the backend chain.

use super::spec::HotkeySpec;
use linuxflow_core::domain::Capabilities;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// KDE Plasma: `kglobalacceld` service (key imposed by the application).
    KGlobalAccel,
    /// GNOME: GNOME custom shortcut created by the application.
    GnomeKeys,
    /// Sway: binding set live through the i3-ipc socket.
    SwayKeys,
    /// Hyprland: binding set live through the command socket.
    HyprlandKeys,
    Portal,
    X11,
    Evdev,
    None,
}

/// User options. Direct reading of the keyboards is DISABLED by default: it is
/// functionally equivalent to a key logger and requires explicit consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyOptions {
    /// Desktop-specific shortcut managers (KDE, GNOME): the application sets its own key there.
    pub allow_native: bool,
    pub allow_portal: bool,
    pub allow_x11: bool,
    pub allow_evdev: bool,
    /// Sub-strings (lower case) of device names to ignore (our own uinput).
    pub ignore_device_names: Vec<String>,
}

impl Default for HotkeyOptions {
    fn default() -> Self {
        Self {
            allow_native: true,
            allow_portal: true,
            allow_x11: true,
            allow_evdev: false,
            ignore_device_names: vec!["linuxflow".into()],
        }
    }
}

/// Backends to try, in order. Always ends with `BackendKind::None`.
pub fn plan_chain(
    caps: &Capabilities,
    opts: &HotkeyOptions,
    spec: &HotkeySpec,
) -> Vec<BackendKind> {
    let mut chain = Vec::new();
    let modifier_only = spec.is_modifier_only();
    match caps.session.as_str() {
        "wayland" => {
            // Desktop manager: the application chooses the key itself and learns about conflicts.
            if opts.allow_native && !modifier_only {
                match caps.desktop.as_str() {
                    "kde" => chain.push(BackendKind::KGlobalAccel),
                    "gnome" => chain.push(BackendKind::GnomeKeys),
                    "sway" => chain.push(BackendKind::SwayKeys),
                    "hyprland" => chain.push(BackendKind::HyprlandKeys),
                    _ => {}
                }
            }
            // The portal does not make modifiers alone reliable: evdev only.
            if opts.allow_portal && caps.portal_global_shortcuts && !modifier_only {
                chain.push(BackendKind::Portal);
            }
            if opts.allow_evdev {
                chain.push(BackendKind::Evdev);
            }
        }
        "x11" => {
            if opts.allow_x11 {
                chain.push(BackendKind::X11);
            }
            if opts.allow_evdev {
                chain.push(BackendKind::Evdev);
            }
        }
        _ => {
            if opts.allow_evdev {
                chain.push(BackendKind::Evdev);
            }
        }
    }
    chain.push(BackendKind::None);
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{detect, mock::MockEnv};

    fn spec(t: &str) -> HotkeySpec {
        HotkeySpec::parse(t).unwrap()
    }

    #[test]
    fn kde_native_service_then_portal() {
        let c = detect(&MockEnv::kde());
        let o = HotkeyOptions::default();
        assert_eq!(
            plan_chain(&c, &o, &spec("Ctrl+Alt+Space")),
            vec![
                BackendKind::KGlobalAccel,
                BackendKind::Portal,
                BackendKind::None
            ]
        );
        // without a native manager: the portal alone, as before
        let sans = HotkeyOptions {
            allow_native: false,
            ..Default::default()
        };
        assert_eq!(
            plan_chain(&c, &sans, &spec("Ctrl+Alt+Space")),
            vec![BackendKind::Portal, BackendKind::None]
        );
    }

    #[test]
    fn gnome_wayland_custom_shortcut_then_portal() {
        let c = detect(&MockEnv::gnome());
        assert_eq!(
            plan_chain(&c, &HotkeyOptions::default(), &spec("Ctrl+Alt+Space")),
            vec![
                BackendKind::GnomeKeys,
                BackendKind::Portal,
                BackendKind::None
            ]
        );
    }

    #[test]
    fn evdev_off_by_default_and_opt_in() {
        let c = detect(&MockEnv::kde());
        let o = HotkeyOptions {
            allow_evdev: true,
            ..Default::default()
        };
        assert_eq!(
            plan_chain(&c, &o, &spec("Ctrl+Alt+Space")),
            vec![
                BackendKind::KGlobalAccel,
                BackendKind::Portal,
                BackendKind::Evdev,
                BackendKind::None
            ]
        );
        assert!(!HotkeyOptions::default().allow_evdev);
    }

    #[test]
    fn modifier_only_on_wayland() {
        let c = detect(&MockEnv::gnome());
        assert_eq!(
            plan_chain(&c, &HotkeyOptions::default(), &spec("Ctrl+Super")),
            vec![BackendKind::None]
        );
        let o = HotkeyOptions {
            allow_evdev: true,
            ..Default::default()
        };
        assert_eq!(
            plan_chain(&c, &o, &spec("Ctrl+Super")),
            vec![BackendKind::Evdev, BackendKind::None]
        );
    }

    #[test]
    fn x11_and_sway_without_portal() {
        let o = HotkeyOptions::default();
        assert_eq!(
            plan_chain(&detect(&MockEnv::x11()), &o, &spec("F9")),
            vec![BackendKind::X11, BackendKind::None]
        );
        // Sway has no shortcuts portal: the application sets its own binding
        assert_eq!(
            plan_chain(&detect(&MockEnv::sway()), &o, &spec("F9")),
            vec![BackendKind::SwayKeys, BackendKind::None]
        );
        let sans = HotkeyOptions {
            allow_native: false,
            ..Default::default()
        };
        assert_eq!(
            plan_chain(&detect(&MockEnv::sway()), &sans, &spec("F9")),
            vec![BackendKind::None]
        );
    }

    #[test]
    fn nothing_at_all() {
        let c = detect(&MockEnv::empty());
        assert_eq!(
            plan_chain(&c, &HotkeyOptions::default(), &spec("F9")),
            vec![BackendKind::None]
        );
        let o = HotkeyOptions {
            allow_evdev: true,
            ..Default::default()
        };
        assert_eq!(
            plan_chain(&c, &o, &spec("F9")),
            vec![BackendKind::Evdev, BackendKind::None]
        );
    }
}
