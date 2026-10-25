//! `OverlayWindow`: window of the pill (Wayland layer-shell or frameless X11 window).

use gdk4 as gdk;
use gdk4_x11::X11Surface;
use gtk::prelude::*;
use gtk4 as gtk;
#[cfg(feature = "layer-shell")]
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use linuxflow_core::domain::Phase;

#[cfg(feature = "layer-shell")]
use crate::overlay::geometry::SCREEN_MARGIN;
use crate::overlay::geometry::{x11_position, SURFACE_H, SURFACE_W};
use crate::overlay::msg::OverlayMsg;
use crate::overlay::pill::PillWidget;
use crate::overlay::x11;

/// Realisation of the window, determined at run time from the GDK display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// Only available with the `layer-shell` feature (`linuxflow-overlay` program).
    LayerShell,
    /// `composited`: a compositor handles transparency.
    X11 { composited: bool },
    /// Wayland without layer-shell, or unknown display: plain window (not very satisfying).
    Plain,
}

impl WindowKind {
    /// To call after `gtk::init()`.
    pub fn detect() -> Option<Self> {
        let display = gdk::Display::default()?;
        if display.downcast_ref::<gdk4_x11::X11Display>().is_some() {
            return Some(Self::X11 {
                composited: display.is_composited(),
            });
        }
        // `is_supported` requires a Wayland display: on any other backend (Broadway, …) it emits
        // a critical GDK warning. The display type is therefore tested first.
        #[cfg(feature = "layer-shell")]
        {
            let is_wayland = display.type_().name() == "GdkWaylandDisplay";
            if is_wayland && gtk4_layer_shell::is_supported() {
                return Some(Self::LayerShell);
            }
        }
        Some(Self::Plain)
    }
}

const CSS: &str = "window.linuxflow-overlay, window.linuxflow-overlay.background { \
    background: transparent; box-shadow: none; border: none; }";

pub struct OverlayWindow {
    window: gtk::Window,
    pill: PillWidget,
    kind: WindowKind,
}

impl OverlayWindow {
    /// Creates the window (not shown). GTK main thread only.
    pub fn new(kind: WindowKind) -> Self {
        let opaque = matches!(kind, WindowKind::X11 { composited: false });
        let window = gtk::Window::builder()
            .title("LinuxFlow overlay")
            .decorated(false)
            .resizable(false)
            .focusable(false)
            .can_focus(false)
            .default_width(SURFACE_W)
            .default_height(if opaque { 46 } else { SURFACE_H })
            .build();
        window.add_css_class("linuxflow-overlay");
        window.set_focus_on_click(false);
        install_css();

        let pill = PillWidget::new();
        pill.set_opaque(opaque);
        window.set_child(Some(&pill));

        #[cfg(feature = "layer-shell")]
        if kind == WindowKind::LayerShell {
            // init_layer_shell must precede the realisation of the window.
            window.init_layer_shell();
            window.set_layer(Layer::Overlay);
            window.set_anchor(Edge::Bottom, true);
            window.set_margin(Edge::Bottom, SCREEN_MARGIN);
            window.set_keyboard_mode(KeyboardMode::None);
            window.set_exclusive_zone(0);
            window.set_namespace(Some("linuxflow-overlay"));
        }

        // Empty input: the mouse goes through the pill.
        window.connect_realize(move |w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
                if let WindowKind::X11 { .. } = kind {
                    if let Some(xs) = surface.downcast_ref::<X11Surface>() {
                        if let Err(e) = x11::prepare_window(xs.xid() as u32) {
                            tracing::warn!("X11 pill: hints not applied: {e}");
                        }
                    }
                }
            }
        });
        window.connect_map(move |w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
                if let WindowKind::X11 { .. } = kind {
                    if let Some(xs) = surface.downcast_ref::<X11Surface>() {
                        let xid = xs.xid() as u32;
                        if let Err(e) = x11::apply_window_state(xid, position_for(w)) {
                            tracing::warn!("X11 pill: state not applied: {e}");
                        }
                    }
                }
            }
        });

        let weak = window.downgrade();
        pill.connect_hidden(move || {
            if let Some(w) = weak.upgrade() {
                w.set_visible(false);
            }
        });

        Self { window, pill, kind }
    }

    pub fn kind(&self) -> WindowKind {
        self.kind
    }

    pub fn pill(&self) -> &PillWidget {
        &self.pill
    }

    pub fn window(&self) -> &gtk::Window {
        &self.window
    }

    /// Applies a message (phase, level, visibility); an active phase shows the window.
    pub fn apply(&self, msg: &OverlayMsg) {
        let wants_visible = match (msg.phase, msg.visible) {
            (_, Some(v)) => v,
            (Some(Phase::Idle), _) => false,
            (Some(_), _) => true,
            _ => false,
        };
        if wants_visible {
            self.map_window();
        }
        self.pill.apply(msg);
    }

    fn map_window(&self) {
        if !self.window.is_visible() {
            // set_visible rather than present: no activation request (focus).
            self.window.set_visible(true);
        } else if let WindowKind::X11 { .. } = self.kind {
            self.reposition();
        }
    }

    fn reposition(&self) {
        if let Some(xs) = self
            .window
            .surface()
            .and_then(|s| s.downcast::<X11Surface>().ok())
        {
            if let Some((x, y)) = position_for(&self.window) {
                if let Err(e) = x11::move_window(xs.xid() as u32, x, y) {
                    tracing::warn!("X11 pill: move impossible: {e}");
                }
            }
        }
    }
}

/// X11 position (bottom centre of the first monitor), `None` if there is no monitor.
fn position_for(window: &gtk::Window) -> Option<(i32, i32)> {
    let display = gtk::prelude::WidgetExt::display(window);
    let monitors = display.monitors();
    let monitor = monitors.item(0)?.downcast::<gdk::Monitor>().ok()?;
    let g = monitor.geometry();
    Some(x11_position(g.x(), g.y(), g.width(), g.height()))
}

fn install_css() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    if let Some(display) = gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
