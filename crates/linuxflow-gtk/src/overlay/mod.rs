//! Pill (overlay): drawing widget, layer-shell / X11 window, in-process or child host.
//!
//! Use by the application:
//! 1. `choose_mode(&caps)` (or `choose_mode_auto()` after `gtk::init`);
//! 2. `create_host(mode)` ;
//! 3. for each `Event::Overlay` / `Event::Level`: `host.push(OverlayMsg::from_event(&e))`;
//! 4. `main()` must call `overlay::worker_main()` if `--overlay-worker` is the first argument.

pub mod anim;
pub mod geometry;
pub mod host;
pub mod mode;
pub mod msg;
pub mod pill;
pub mod state;
pub mod window;
pub mod x11;

pub use host::{
    create_host, helper_starts, layer_shell_child, worker_main, ChildOverlay, InProcessOverlay,
    NullOverlay, OverlayHost, HAS_LAYER_SHELL, HELPER_NAME, PROBE_ARG, WORKER_ARG,
};
pub use mode::{choose_mode, choose_mode_with, OverlayMode};
pub use msg::OverlayMsg;
pub use pill::PillWidget;
pub use window::{OverlayWindow, WindowKind};

/// Detects the display capabilities (after `gtk::init`) then chooses the mode.
pub fn choose_mode_auto() -> OverlayMode {
    choose_mode_auto_with(&|| false)
}

/// Like [`choose_mode_auto`]. `compositor_has_layer_shell` says whether the Wayland compositor offers
/// `zwlr_layer_shell_v1`: needed when this binary does not link `libgtk4-layer-shell` (it cannot
/// therefore ask GTK for it) and entrusts the pill to `linuxflow-overlay`.
pub fn choose_mode_auto_with(compositor_has_layer_shell: &dyn Fn() -> bool) -> OverlayMode {
    use gtk4 as gtk;
    use gtk4::prelude::ObjectExt;
    use linuxflow_core::domain::Capabilities;
    let Some(display) = gtk::gdk::Display::default() else {
        return OverlayMode::Disabled;
    };
    let kind = WindowKind::detect();
    let session = match kind {
        Some(WindowKind::X11 { .. }) => "x11",
        Some(WindowKind::LayerShell | WindowKind::Plain) => "wayland",
        None => "unknown",
    };
    let layer_shell = if HAS_LAYER_SHELL {
        kind == Some(WindowKind::LayerShell)
    } else {
        session == "wayland"
            && display.type_().name() == "GdkWaylandDisplay"
            && compositor_has_layer_shell()
    };
    let caps = Capabilities {
        session: session.into(),
        layer_shell,
        ..Capabilities::default()
    };
    choose_mode(&caps)
}
