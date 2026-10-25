//! Session detection and choice of the backend chain (pure, testable with a simulated environment).

use std::collections::HashMap;

/// Desktop/compositor family, to order the backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Gnome,
    Kde,
    Sway,
    Hyprland,
    OtherWlroots,
    Other,
}

/// Snapshot of the useful environment variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionEnv {
    pub session_type: Option<String>,
    pub wayland_display: bool,
    pub x_display: bool,
    pub desktop: Vec<String>,
    pub sway: bool,
    pub hyprland: bool,
}

impl SessionEnv {
    pub fn from_env() -> Self {
        let vars: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&vars)
    }

    pub fn from_map(vars: &HashMap<String, String>) -> Self {
        let non_empty = |k: &str| vars.get(k).is_some_and(|v| !v.is_empty());
        Self {
            session_type: vars
                .get("XDG_SESSION_TYPE")
                .map(|v| v.to_lowercase())
                .filter(|v| !v.is_empty()),
            wayland_display: non_empty("WAYLAND_DISPLAY"),
            x_display: non_empty("DISPLAY"),
            desktop: vars
                .get("XDG_CURRENT_DESKTOP")
                .map(|v| {
                    v.split(':')
                        .map(|s| s.trim().to_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            sway: non_empty("SWAYSOCK"),
            hyprland: non_empty("HYPRLAND_INSTANCE_SIGNATURE"),
        }
    }

    /// Wayland session (session variable or socket); XWayland does not count as native X11.
    pub fn is_wayland(&self) -> bool {
        match self.session_type.as_deref() {
            Some("wayland") => true,
            Some("x11") => false,
            _ => self.wayland_display,
        }
    }

    pub fn is_x11(&self) -> bool {
        !self.is_wayland() && self.x_display
    }

    pub fn compositor(&self) -> Compositor {
        let has = |name: &str| self.desktop.iter().any(|d| d == name);
        if self.hyprland || has("hyprland") {
            Compositor::Hyprland
        } else if self.sway || has("sway") {
            Compositor::Sway
        } else if has("gnome") || has("ubuntu") || has("unity") {
            Compositor::Gnome
        } else if has("kde") {
            Compositor::Kde
        } else if has("river") || has("wayfire") || has("labwc") || has("niri") || has("cosmic") {
            Compositor::OtherWlroots
        } else {
            Compositor::Other
        }
    }
}

/// Known injection backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InjectBackendKind {
    XTest,
    Portal,
    VirtualKeyboard,
    HyprlandShortcut,
    Uinput,
}

/// Order of attempt of the backends for a session. `uinput` only appears if explicitly enabled.
pub fn inject_chain(env: &SessionEnv, enable_uinput: bool) -> Vec<InjectBackendKind> {
    use InjectBackendKind::*;
    let mut chain = if env.is_wayland() {
        match env.compositor() {
            Compositor::Hyprland => vec![HyprlandShortcut, VirtualKeyboard, Portal],
            Compositor::Sway | Compositor::OtherWlroots => vec![VirtualKeyboard, Portal],
            Compositor::Gnome | Compositor::Kde => vec![Portal],
            Compositor::Other => vec![VirtualKeyboard, Portal],
        }
    } else if env.is_x11() {
        vec![XTest]
    } else {
        // Unknown session: everything is tried, from the least to the most intrusive.
        vec![XTest, VirtualKeyboard, Portal]
    };
    if enable_uinput {
        chain.push(Uinput);
    }
    chain
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use InjectBackendKind::*;

    fn env(pairs: &[(&str, &str)]) -> SessionEnv {
        SessionEnv::from_map(
            &pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn x11_session_uses_xtest_only() {
        let e = env(&[("XDG_SESSION_TYPE", "x11"), ("DISPLAY", ":0")]);
        assert!(e.is_x11() && !e.is_wayland());
        assert_eq!(inject_chain(&e, false), vec![XTest]);
    }

    #[test]
    fn xwayland_is_not_x11() {
        let e = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":1"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ]);
        assert!(e.is_wayland() && !e.is_x11());
        assert_eq!(inject_chain(&e, false), vec![Portal]);
    }

    #[test]
    fn gnome_and_kde_use_portal() {
        for d in ["GNOME", "ubuntu:GNOME", "KDE"] {
            let e = env(&[("XDG_SESSION_TYPE", "wayland"), ("XDG_CURRENT_DESKTOP", d)]);
            assert_eq!(inject_chain(&e, false), vec![Portal], "{d}");
        }
    }

    #[test]
    fn sway_prefers_virtual_keyboard() {
        let e = env(&[("WAYLAND_DISPLAY", "wayland-1"), ("SWAYSOCK", "/run/x")]);
        assert_eq!(e.compositor(), Compositor::Sway);
        assert_eq!(inject_chain(&e, false), vec![VirtualKeyboard, Portal]);
    }

    #[test]
    fn hyprland_tries_sendshortcut_first() {
        let e = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
        ]);
        assert_eq!(
            inject_chain(&e, false),
            vec![HyprlandShortcut, VirtualKeyboard, Portal]
        );
    }

    #[test]
    fn uinput_only_when_explicitly_enabled_and_last() {
        let e = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
        ]);
        assert!(!inject_chain(&e, false).contains(&Uinput));
        assert_eq!(inject_chain(&e, true), vec![Portal, Uinput]);
    }

    #[test]
    fn unknown_session_tries_everything_but_uinput() {
        let e = env(&[]);
        assert_eq!(
            inject_chain(&e, false),
            vec![XTest, VirtualKeyboard, Portal]
        );
    }

    #[test]
    fn session_type_wins_over_display_variables() {
        let e = env(&[
            ("XDG_SESSION_TYPE", "X11"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
        ]);
        assert!(e.is_x11());
    }
}
