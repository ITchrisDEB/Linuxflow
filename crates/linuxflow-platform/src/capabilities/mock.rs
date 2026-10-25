//! Simulated environments for the tests (X11, GNOME, KDE, Sway, Hyprland, empty).

use super::env::{Env, WaylandGlobal};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct MockEnv {
    pub vars: HashMap<String, String>,
    pub paths: HashSet<PathBuf>,
    pub writable: HashSet<PathBuf>,
    pub wayland: Option<Vec<WaylandGlobal>>,
    /// `None` = no session bus.
    pub dbus_names: Option<HashSet<String>>,
    pub portals: HashMap<String, u32>,
    pub groups: Vec<String>,
    pub vulkan: Option<Vec<String>>,
}

impl MockEnv {
    /// Machine with nothing: no variable, no bus, no GPU.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn set(&mut self, k: &str, v: &str) {
        self.vars.insert(k.into(), v.into());
    }

    pub fn add_path(&mut self, p: &str) {
        self.paths.insert(PathBuf::from(p));
    }

    pub fn set_wayland_globals(&mut self, list: &[(&str, u32)]) {
        self.wayland = Some(
            list.iter()
                .map(|(i, v)| WaylandGlobal {
                    interface: (*i).into(),
                    version: *v,
                })
                .collect(),
        );
    }

    fn base(session: &str, desktop: &str) -> Self {
        let mut e = Self::empty();
        e.set("XDG_SESSION_TYPE", session);
        e.set("XDG_CURRENT_DESKTOP", desktop);
        e.set("XDG_RUNTIME_DIR", "/run/user/test");
        e.add_path("/run/user/test/pipewire-0");
        e.dbus_names = Some(["org.freedesktop.DBus".to_string()].into_iter().collect());
        e.vulkan = Some(vec!["Test GPU".into()]);
        e
    }

    pub fn x11() -> Self {
        let mut e = Self::base("x11", "XFCE");
        e.set("DISPLAY", ":0");
        e
    }

    pub fn gnome() -> Self {
        let mut e = Self::base("wayland", "GNOME");
        e.set("WAYLAND_DISPLAY", "wayland-0");
        e.set_wayland_globals(&[("wl_compositor", 6), ("wl_seat", 8)]);
        for (i, v) in [
            ("GlobalShortcuts", 1),
            ("RemoteDesktop", 2),
            ("Clipboard", 1),
            ("Background", 2),
        ] {
            e.portals.insert(i.into(), v);
        }
        e
    }

    pub fn kde() -> Self {
        let mut e = Self::base("wayland", "KDE");
        e.set("WAYLAND_DISPLAY", "wayland-0");
        e.set_wayland_globals(&[
            ("wl_compositor", 6),
            ("zwlr_layer_shell_v1", 4),
            ("zwlr_data_control_manager_v1", 2),
        ]);
        for (i, v) in [
            ("GlobalShortcuts", 2),
            ("RemoteDesktop", 2),
            ("Clipboard", 1),
            ("Background", 2),
        ] {
            e.portals.insert(i.into(), v);
        }
        if let Some(n) = e.dbus_names.as_mut() {
            n.insert("org.kde.StatusNotifierWatcher".into());
        }
        e
    }

    pub fn sway() -> Self {
        let mut e = Self::base("wayland", "sway");
        e.set("WAYLAND_DISPLAY", "wayland-1");
        e.set("SWAYSOCK", "/run/user/test/sway-ipc.sock");
        e.set_wayland_globals(&[
            ("zwlr_layer_shell_v1", 4),
            ("zwlr_data_control_manager_v1", 2),
        ]);
        e
    }

    pub fn hyprland() -> Self {
        let mut e = Self::base("wayland", "Hyprland");
        e.set("WAYLAND_DISPLAY", "wayland-1");
        e.set("HYPRLAND_INSTANCE_SIGNATURE", "sig");
        e.set_wayland_globals(&[
            ("zwlr_layer_shell_v1", 4),
            ("zwlr_data_control_manager_v1", 2),
        ]);
        e.portals.insert("GlobalShortcuts".into(), 1);
        e
    }
}

impl Env for MockEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned().filter(|v| !v.is_empty())
    }
    fn path_exists(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }
    fn path_writable(&self, path: &Path) -> bool {
        self.writable.contains(path)
    }
    fn wayland_globals(&self) -> Option<Vec<WaylandGlobal>> {
        self.wayland.clone()
    }
    fn dbus_name_has_owner(&self, name: &str) -> Option<bool> {
        self.dbus_names.as_ref().map(|n| n.contains(name))
    }
    fn portal_interface_version(&self, interface: &str) -> Option<u32> {
        self.dbus_names.as_ref()?;
        self.portals.get(interface).copied()
    }
    fn user_groups(&self) -> Vec<String> {
        self.groups.clone()
    }
    fn vulkan_devices(&self) -> Result<Vec<String>, String> {
        self.vulkan.clone().ok_or_else(|| "no loader".to_string())
    }
}
