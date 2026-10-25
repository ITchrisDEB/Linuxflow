//! Choice of the way to realise the pill according to the detected capabilities.

use linuxflow_core::domain::Capabilities;

/// Way of displaying the pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    /// Wayland with `wlr-layer-shell` (KDE, Sway, Hyprland, Cosmic…), in the process.
    LayerShell,
    /// X11 session: frameless window, EWMH hints, in the process.
    X11Native,
    /// Wayland without layer-shell (GNOME/Mutter): child process forced onto XWayland.
    ChildXWayland,
    /// No way of displaying the pill: the application works without it.
    Disabled,
}

/// Pure choice, with the presence of an X server (`DISPLAY`) given explicitly.
pub fn choose_mode_with(caps: &Capabilities, has_x_display: bool) -> OverlayMode {
    match caps.session.as_str() {
        "wayland" => {
            if caps.layer_shell {
                OverlayMode::LayerShell
            } else if has_x_display {
                OverlayMode::ChildXWayland
            } else {
                OverlayMode::Disabled
            }
        }
        "x11" => OverlayMode::X11Native,
        _ => {
            if caps.layer_shell {
                OverlayMode::LayerShell
            } else if has_x_display {
                OverlayMode::X11Native
            } else {
                OverlayMode::Disabled
            }
        }
    }
}

/// Chooses the mode; `DISPLAY` is read from the process environment.
pub fn choose_mode(caps: &Capabilities) -> OverlayMode {
    let has_x = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    choose_mode_with(caps, has_x)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn caps(session: &str, layer_shell: bool) -> Capabilities {
        Capabilities {
            session: session.into(),
            layer_shell,
            ..Capabilities::default()
        }
    }

    #[test]
    fn wayland_with_layer_shell() {
        for x in [true, false] {
            assert_eq!(
                choose_mode_with(&caps("wayland", true), x),
                OverlayMode::LayerShell
            );
        }
    }

    #[test]
    fn wayland_without_layer_shell_uses_xwayland_child() {
        assert_eq!(
            choose_mode_with(&caps("wayland", false), true),
            OverlayMode::ChildXWayland
        );
        assert_eq!(
            choose_mode_with(&caps("wayland", false), false),
            OverlayMode::Disabled
        );
    }

    #[test]
    fn x11_session_is_native_even_if_layer_shell_flag_is_set() {
        assert_eq!(
            choose_mode_with(&caps("x11", false), true),
            OverlayMode::X11Native
        );
        assert_eq!(
            choose_mode_with(&caps("x11", true), true),
            OverlayMode::X11Native
        );
    }

    #[test]
    fn unknown_session_falls_back() {
        assert_eq!(
            choose_mode_with(&caps("unknown", true), false),
            OverlayMode::LayerShell
        );
        assert_eq!(
            choose_mode_with(&caps("unknown", false), true),
            OverlayMode::X11Native
        );
        assert_eq!(
            choose_mode_with(&caps("", false), false),
            OverlayMode::Disabled
        );
    }
}
