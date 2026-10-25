//! Detection of the machine capabilities at run time.
//!
//! `detect` never panics: every missing component produces a fallback and a help text.
//! All the reads go through the `Env` trait (variables, files, Wayland registry, D-Bus,
//! groups, Vulkan) so that any environment can be simulated.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod env;
mod groups;
pub mod mock;
mod system;

pub use env::{Env, WaylandGlobal};
pub use groups::{group_names_for, parse_group_file, parse_status_gids};
pub use system::SystemEnv;

use linuxflow_core::domain::Capabilities;
use linuxflow_i18n::tr;
use std::path::PathBuf;

/// Versions of the portal interfaces found (0 = absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PortalVersions {
    pub global_shortcuts: Option<u32>,
    pub remote_desktop: Option<u32>,
    pub clipboard: Option<u32>,
    pub background: Option<u32>,
}

/// Complete report: `Capabilities` (shared contract) + details not covered by the contract.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Detection {
    pub capabilities: Capabilities,
    pub portals: PortalVersions,
    /// Reachable session D-Bus name? (`None` = no session bus).
    pub session_bus: Option<bool>,
}

/// Detects the capabilities (simple version, conforming to the contract).
pub fn detect(env: &dyn Env) -> Capabilities {
    detect_detailed(env).capabilities
}

/// Recognised canonical desktop identifiers.
const DESKTOPS: &[(&str, &str)] = &[
    ("gnome", "gnome"),
    ("ubuntu", "gnome"),
    ("kde", "kde"),
    ("plasma", "kde"),
    ("sway", "sway"),
    ("hyprland", "hyprland"),
    ("niri", "niri"),
    ("cosmic", "cosmic"),
    ("xfce", "xfce"),
    ("x-cinnamon", "cinnamon"),
    ("cinnamon", "cinnamon"),
    ("mate", "mate"),
    ("lxqt", "lxqt"),
    ("budgie", "budgie"),
    ("pantheon", "pantheon"),
    ("i3", "i3"),
];

/// Session type: `wayland`, `x11` or `unknown`.
pub fn detect_session(env: &dyn Env) -> String {
    let declared = env.var("XDG_SESSION_TYPE").map(|s| s.to_lowercase());
    match declared.as_deref() {
        Some("wayland") => return "wayland".into(),
        Some("x11") => return "x11".into(),
        _ => {}
    }
    // `tty`, `unspecified` or absent: deduced from the display sockets.
    if env.var("WAYLAND_DISPLAY").is_some() {
        "wayland".into()
    } else if env.var("DISPLAY").is_some() {
        "x11".into()
    } else {
        "unknown".into()
    }
}

/// Canonical lower-case desktop (`gnome`, `kde`, `sway`, `hyprland`...) or `unknown`.
pub fn detect_desktop(env: &dyn Env) -> String {
    if let Some(raw) = env.var("XDG_CURRENT_DESKTOP") {
        for part in raw.split(':') {
            let p = part.trim().to_lowercase();
            if let Some((_, id)) = DESKTOPS.iter().find(|(k, _)| *k == p) {
                return (*id).into();
            }
        }
        // Unknown but non-empty value: it is kept, cleaned.
        let first = raw.split(':').next().unwrap_or("").trim().to_lowercase();
        if !first.is_empty() {
            return first;
        }
    }
    if env.var("SWAYSOCK").is_some() {
        "sway".into()
    } else if env.var("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        "hyprland".into()
    } else if env.var("NIRI_SOCKET").is_some() {
        "niri".into()
    } else {
        "unknown".into()
    }
}

fn hint(caps: &mut Capabilities, brick: &str, message: String) {
    caps.hints.push((brick.to_string(), message));
}

pub fn detect_detailed(env: &dyn Env) -> Detection {
    let mut caps = Capabilities {
        session: detect_session(env),
        desktop: detect_desktop(env),
        ..Default::default()
    };
    let wayland = caps.session == "wayland";

    if caps.session == "unknown" {
        hint(
            &mut caps,
            "session",
            tr("No graphical session was detected. Control LinuxFlow from the command line instead."),
        );
    }

    // --- Portals (session D-Bus) ---
    let mut portals = PortalVersions::default();
    let bus = env.dbus_name_has_owner("org.freedesktop.DBus");
    let session_bus = bus;
    if bus.is_some() {
        portals.global_shortcuts = env.portal_interface_version("GlobalShortcuts");
        portals.remote_desktop = env.portal_interface_version("RemoteDesktop");
        portals.clipboard = env.portal_interface_version("Clipboard");
        portals.background = env.portal_interface_version("Background");
    } else {
        hint(
            &mut caps,
            "dbus",
            tr("The session D-Bus is not reachable: portals and the tray icon are unavailable."),
        );
    }
    caps.portal_global_shortcuts = portals.global_shortcuts.is_some();
    caps.portal_remote_desktop = portals.remote_desktop.is_some();
    caps.portal_clipboard = portals.clipboard.is_some();
    if bus.is_some() {
        if !caps.portal_global_shortcuts {
            hint(
                &mut caps,
                "portal.global_shortcuts",
                tr("The GlobalShortcuts portal is missing. Bind a compositor shortcut to the LinuxFlow command line, or enable the optional keyboard-device mode."),
            );
        }
        if !caps.portal_remote_desktop && wayland {
            hint(
                &mut caps,
                "portal.remote_desktop",
                tr("The RemoteDesktop portal is missing: automatic pasting may be unavailable. The text stays on the clipboard."),
            );
        }
        if !caps.portal_clipboard && wayland {
            hint(
                &mut caps,
                "portal.clipboard",
                tr("The Clipboard portal is missing: copying while another window has focus may fail."),
            );
        }
    }

    // --- Wayland registry (layer-shell, data-control), without GTK ---
    if wayland {
        match env.wayland_globals() {
            Some(globals) => {
                caps.layer_shell = globals.iter().any(|g| g.interface == "zwlr_layer_shell_v1");
                caps.data_control = globals.iter().any(|g| {
                    g.interface == "zwlr_data_control_manager_v1" || g.interface == "ext_data_control_manager_v1"
                });
            }
            None => hint(
                &mut caps,
                "wayland",
                tr("The Wayland compositor could not be queried; overlay and clipboard capabilities are unknown."),
            ),
        }
        if !caps.layer_shell {
            hint(
                &mut caps,
                "layer_shell",
                tr("This compositor does not offer layer-shell: the recording indicator uses a fallback window."),
            );
        }
        if !caps.data_control {
            hint(
                &mut caps,
                "data_control",
                tr("This compositor does not offer data-control: clipboard access relies on the portal or on window focus."),
            );
        }
    }

    // --- Vulkan (dynamic loading) ---
    match env.vulkan_devices() {
        Ok(devices) if !devices.is_empty() => {
            caps.vulkan_available = true;
            caps.vulkan_devices = devices;
        }
        Ok(_) => hint(&mut caps, "vulkan", tr("Vulkan is installed but reports no GPU: transcription runs on the CPU.")),
        Err(_) => hint(
            &mut caps,
            "vulkan",
            tr("Vulkan is not available: transcription runs on the CPU. Install the Vulkan loader and your GPU driver to use the GPU."),
        ),
    }

    // --- PipeWire ---
    if let Some(runtime) = env.var("XDG_RUNTIME_DIR") {
        let remote = env
            .var("PIPEWIRE_REMOTE")
            .unwrap_or_else(|| "pipewire-0".into());
        let socket = if remote.starts_with('/') {
            PathBuf::from(remote)
        } else {
            PathBuf::from(runtime).join(remote)
        };
        caps.pipewire = env.path_exists(&socket);
    }
    if !caps.pipewire {
        hint(
            &mut caps,
            "pipewire",
            tr("PipeWire was not found: audio capture falls back to ALSA."),
        );
    }

    // --- Notification icon ---
    if bus.is_some() {
        caps.tray_watcher = env
            .dbus_name_has_owner("org.kde.StatusNotifierWatcher")
            .unwrap_or(false);
        if !caps.tray_watcher {
            hint(
                &mut caps,
                "tray",
                tr("No notification-area host was found: there will be no tray icon. Use the main window instead."),
            );
        }
    }

    // --- Input devices ---
    caps.uinput_writable = env.path_writable(std::path::Path::new("/dev/uinput"));
    if !caps.uinput_writable {
        hint(
            &mut caps,
            "uinput",
            tr("/dev/uinput is not writable: the keystroke-injection fallback is disabled. A udev rule can grant access."),
        );
    }
    caps.input_group = env.user_groups().iter().any(|g| g == "input");
    if !caps.input_group {
        hint(
            &mut caps,
            "input_group",
            tr("You are not in the \"input\" group: reading keyboard devices directly is disabled."),
        );
    }

    Detection {
        capabilities: caps,
        portals,
        session_bus,
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockEnv;
    use super::*;

    fn has_hint(c: &Capabilities, brick: &str) -> bool {
        c.hints.iter().any(|(b, m)| b == brick && !m.is_empty())
    }

    #[test]
    fn x11_pur() {
        let c = detect(&MockEnv::x11());
        assert_eq!(c.session, "x11");
        assert!(!c.layer_shell && !c.data_control);
        assert!(!c.portal_global_shortcuts);
        assert!(c.pipewire && c.vulkan_available);
        assert!(
            !has_hint(&c, "layer_shell"),
            "no layer-shell hint outside Wayland"
        );
        assert!(has_hint(&c, "portal.global_shortcuts"));
    }

    #[test]
    fn wayland_gnome() {
        let c = detect(&MockEnv::gnome());
        assert_eq!(
            (c.session.as_str(), c.desktop.as_str()),
            ("wayland", "gnome")
        );
        assert!(c.portal_global_shortcuts && c.portal_remote_desktop && c.portal_clipboard);
        assert!(!c.layer_shell && !c.data_control);
        assert!(has_hint(&c, "layer_shell") && has_hint(&c, "data_control"));
        assert!(!c.tray_watcher && has_hint(&c, "tray"));
    }

    #[test]
    fn wayland_kde() {
        let c = detect(&MockEnv::kde());
        assert_eq!(c.desktop, "kde");
        assert!(c.layer_shell && c.data_control && c.tray_watcher);
        assert!(!has_hint(&c, "tray"));
    }

    #[test]
    fn sway_et_hyprland() {
        let c = detect(&MockEnv::sway());
        assert_eq!(c.desktop, "sway");
        assert!(c.layer_shell && c.data_control && !c.portal_global_shortcuts);
        let h = detect(&MockEnv::hyprland());
        assert_eq!(h.desktop, "hyprland");
        assert!(h.layer_shell);
    }

    #[test]
    fn rien_du_tout() {
        let d = detect_detailed(&MockEnv::empty());
        let c = d.capabilities;
        assert_eq!(
            (c.session.as_str(), c.desktop.as_str()),
            ("unknown", "unknown")
        );
        assert!(!c.portal_global_shortcuts && !c.portal_remote_desktop && !c.portal_clipboard);
        assert!(!c.vulkan_available && !c.pipewire && !c.tray_watcher);
        assert!(!c.uinput_writable && !c.input_group);
        assert_eq!(d.session_bus, None);
        for brick in [
            "session",
            "dbus",
            "vulkan",
            "pipewire",
            "uinput",
            "input_group",
        ] {
            assert!(has_hint(&c, brick), "hint manquant : {brick}");
        }
    }

    #[test]
    fn session_deduite_si_type_absent_ou_tty() {
        let mut e = MockEnv::empty();
        e.set("XDG_SESSION_TYPE", "tty");
        e.set("WAYLAND_DISPLAY", "wayland-1");
        assert_eq!(detect_session(&e), "wayland");
        let mut e = MockEnv::empty();
        e.set("DISPLAY", ":0");
        assert_eq!(detect_session(&e), "x11");
    }

    #[test]
    fn bureau_composite_et_variables_compositeur() {
        let mut e = MockEnv::empty();
        e.set("XDG_CURRENT_DESKTOP", "ubuntu:GNOME");
        assert_eq!(detect_desktop(&e), "gnome");
        e.set("XDG_CURRENT_DESKTOP", "X-Cinnamon");
        assert_eq!(detect_desktop(&e), "cinnamon");
        e.set("XDG_CURRENT_DESKTOP", "Foo");
        assert_eq!(detect_desktop(&e), "foo");
        let mut e = MockEnv::empty();
        e.set("NIRI_SOCKET", "x");
        assert_eq!(detect_desktop(&e), "niri");
    }

    #[test]
    fn pipewire_remote_personnalise() {
        let mut e = MockEnv::empty();
        e.set("XDG_RUNTIME_DIR", "/run/user/x");
        e.set("PIPEWIRE_REMOTE", "pw-test");
        e.add_path("/run/user/x/pw-test");
        assert!(detect(&e).pipewire);
    }

    #[test]
    fn versions_de_portail_rapportees() {
        let d = detect_detailed(&MockEnv::kde());
        assert_eq!(d.portals.global_shortcuts, Some(2));
        assert_eq!(d.portals.clipboard, Some(1));
    }

    #[test]
    fn data_control_ext_accepte() {
        let mut e = MockEnv::sway();
        e.set_wayland_globals(&[("ext_data_control_manager_v1", 1)]);
        let c = detect(&e);
        assert!(c.data_control && !c.layer_shell);
    }

    #[test]
    fn compositeur_injoignable() {
        let mut e = MockEnv::gnome();
        e.wayland = None;
        let c = detect(&e);
        assert!(has_hint(&c, "wayland"));
    }

    #[test]
    #[ignore = "real probes (D-Bus, Wayland, Vulkan); set LINUXFLOW_TEST_PORTALS=1"]
    fn detection_reelle() {
        if std::env::var("LINUXFLOW_TEST_PORTALS").as_deref() != Ok("1") {
            return;
        }
        let c = detect(&SystemEnv);
        assert!(!c.session.is_empty());
    }
}
