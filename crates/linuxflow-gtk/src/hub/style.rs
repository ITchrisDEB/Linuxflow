//! Installation of the hub stylesheet (light theme).

use gtk4 as gtk;
use libadwaita as adw;
use std::cell::Cell;

/// Hub stylesheet. All the rules are limited to `.lf-hub`.
pub const CSS: &str = include_str!("../../data/style.css");

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Loads the CSS on the default display (once per process) and forces the light theme.
/// Without a display, does nothing.
pub fn install() {
    if INSTALLED.with(Cell::get) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
    INSTALLED.with(|i| i.set(true));
}
