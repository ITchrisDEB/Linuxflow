//! Access to the environment, injectable: everything `detect` reads goes through the `Env` trait,
//! which makes it possible to simulate X11, GNOME, KDE, Sway, Hyprland or "nothing at all" in the tests.

use std::path::Path;

/// Interface exposed by the Wayland registry (name + version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaylandGlobal {
    pub interface: String,
    pub version: u32,
}

/// Reading of the environment. No method ever panics: absence is expressed by `None`/`false`.
pub trait Env {
    /// Environment variable (empty = absent).
    fn var(&self, key: &str) -> Option<String>;
    fn path_exists(&self, path: &Path) -> bool;
    /// True if the current user can write to this file (without opening it).
    fn path_writable(&self, path: &Path) -> bool;
    /// Globals of the Wayland registry; `None` if the compositor cannot be reached.
    fn wayland_globals(&self) -> Option<Vec<WaylandGlobal>>;
    /// `Some(true)` if the name has an owner on the session bus; `None` if there is no bus.
    fn dbus_name_has_owner(&self, name: &str) -> Option<bool>;
    /// Version of a portal interface (`GlobalShortcuts`, `RemoteDesktop`, `Clipboard`,
    /// `Background`); `None` if absent or the portal is unreachable.
    fn portal_interface_version(&self, interface: &str) -> Option<u32>;
    /// Names of the user's groups (primary + supplementary).
    fn user_groups(&self) -> Vec<String>;
    /// Names of the Vulkan GPUs found, or `Err(reason)` if the Vulkan loader is absent/unusable.
    fn vulkan_devices(&self) -> Result<Vec<String>, String>;
}
