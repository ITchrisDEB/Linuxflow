//! Foreground window (`ActiveWindow`).
//!
//! Detection chain, chosen at run time from the environment:
//! - Sway: i3 IPC on `SWAYSOCK` (`get_tree`, `focused` node);
//! - Hyprland: `.socket.sock` socket (`j/activewindow`);
//! - plain X11: EWMH (`_NET_ACTIVE_WINDOW`, `WM_CLASS`, `_NET_WM_PID`, `_NET_WM_NAME`);
//! - KDE Plasma and GNOME (Wayland): **accepted limit**, `None` (no stable API without a script
//!   KWin script or GNOME Shell extension, which LinuxFlow does not install). Pasting then falls back to the
//!   default mode (Ctrl+V) and the manual "terminal" setting.
//!
//! No error is ever raised or causes a panic: a missing socket gives `None`.
//! Under Wayland, X11 is deliberately not queried (it would only see the XWayland windows
//! and return a stale answer).

mod hyprland;
mod sway;
mod x11;

use linuxflow_core::domain::WindowInfo;
use linuxflow_core::traits::ActiveWindow;
use std::path::PathBuf;

/// Detection engine chosen for the current session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusBackend {
    Sway(PathBuf),
    Hyprland(PathBuf),
    X11,
    /// KDE, GNOME, unknown session: no detection possible.
    Unsupported,
}

/// Chooses the engine from environment variables (`get` function injected for the tests).
pub fn detect_backend(get: &dyn Fn(&str) -> Option<String>) -> FocusBackend {
    let non_empty = |k: &str| get(k).filter(|v| !v.is_empty());
    if let Some(sock) = non_empty("SWAYSOCK") {
        return FocusBackend::Sway(PathBuf::from(sock));
    }
    if let (Some(sig), Some(runtime)) = (
        non_empty("HYPRLAND_INSTANCE_SIGNATURE"),
        non_empty("XDG_RUNTIME_DIR"),
    ) {
        return FocusBackend::Hyprland(hyprland::socket_path(&runtime, &sig));
    }
    let wayland = non_empty("WAYLAND_DISPLAY").is_some()
        || non_empty("XDG_SESSION_TYPE").is_some_and(|t| t.eq_ignore_ascii_case("wayland"));
    if !wayland && non_empty("DISPLAY").is_some() {
        return FocusBackend::X11;
    }
    FocusBackend::Unsupported
}

/// System implementation of `ActiveWindow`.
#[derive(Debug, Clone)]
pub struct SystemActiveWindow {
    backend: FocusBackend,
}

impl SystemActiveWindow {
    /// Detects the engine from the process environment.
    pub fn detect() -> Self {
        Self {
            backend: detect_backend(&|k| std::env::var(k).ok()),
        }
    }

    pub fn with_backend(backend: FocusBackend) -> Self {
        Self { backend }
    }

    pub fn backend(&self) -> &FocusBackend {
        &self.backend
    }
}

impl ActiveWindow for SystemActiveWindow {
    fn current(&self) -> Option<WindowInfo> {
        let info = match &self.backend {
            FocusBackend::Sway(path) => sway::active_window(path),
            FocusBackend::Hyprland(path) => hyprland::active_window(path),
            FocusBackend::X11 => x11::active_window(),
            FocusBackend::Unsupported => None,
        }?;
        Some(normalize(info))
    }
}

/// `app_id` in lower case, without edge spaces; empty becomes `None`.
fn normalize(mut info: WindowInfo) -> WindowInfo {
    info.app_id = info
        .app_id
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty());
    info.title = info.title.filter(|t| !t.is_empty());
    info
}

/// Maximum delay for an answer from a compositor socket.
pub(crate) const IPC_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn sway_has_priority() {
        let get = env(&[("SWAYSOCK", "/run/x/sway.sock"), ("DISPLAY", ":0")]);
        assert_eq!(
            detect_backend(&get),
            FocusBackend::Sway(PathBuf::from("/run/x/sway.sock"))
        );
    }

    #[test]
    fn hyprland_socket_path() {
        let get = env(&[
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
            ("XDG_RUNTIME_DIR", "/run/u"),
            ("WAYLAND_DISPLAY", "wayland-1"),
        ]);
        assert_eq!(
            detect_backend(&get),
            FocusBackend::Hyprland(PathBuf::from("/run/u/hypr/abc/.socket.sock"))
        );
    }

    #[test]
    fn hyprland_without_runtime_dir_is_unsupported() {
        let get = env(&[
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
            ("WAYLAND_DISPLAY", "w"),
        ]);
        assert_eq!(detect_backend(&get), FocusBackend::Unsupported);
    }

    #[test]
    fn pure_x11() {
        assert_eq!(
            detect_backend(&env(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "x11")])),
            FocusBackend::X11
        );
        assert_eq!(
            detect_backend(&env(&[("DISPLAY", ":1")])),
            FocusBackend::X11
        );
    }

    #[test]
    fn kde_gnome_wayland_are_unsupported_even_with_xwayland() {
        let get = env(&[
            ("DISPLAY", ":0"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ]);
        assert_eq!(detect_backend(&get), FocusBackend::Unsupported);
        let get = env(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "wayland")]);
        assert_eq!(detect_backend(&get), FocusBackend::Unsupported);
    }

    #[test]
    fn empty_env_is_unsupported() {
        assert_eq!(detect_backend(&env(&[])), FocusBackend::Unsupported);
        assert_eq!(
            detect_backend(&env(&[("SWAYSOCK", "")])),
            FocusBackend::Unsupported
        );
    }

    #[test]
    fn missing_sockets_never_panic() {
        let dir = std::env::temp_dir().join("linuxflow-absent-socket-xyz");
        for b in [
            FocusBackend::Sway(dir.join("sway.sock")),
            FocusBackend::Hyprland(dir.join(".socket.sock")),
            FocusBackend::Unsupported,
        ] {
            assert_eq!(SystemActiveWindow::with_backend(b).current(), None);
        }
    }

    #[test]
    fn app_id_is_lowercased() {
        let w = normalize(WindowInfo {
            app_id: Some(" Org.Gnome.Terminal ".into()),
            pid: Some(1),
            title: Some(String::new()),
        });
        assert_eq!(w.app_id.as_deref(), Some("org.gnome.terminal"));
        assert_eq!(w.title, None);
    }
}
