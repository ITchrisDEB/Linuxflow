//! Installation of the hub stylesheet and of the light / dark theme.
//!
//! The rules (`style.css`) only use named colours (the `lf_` names); the light and the dark palettes define
//! them. libadwaita decides whether the application is dark: by default it follows the desktop
//! preference (XDG portal / GSettings), and the "Appearance" setting can force light or dark. The
//! palette is reloaded whenever that decision changes, so the whole interface follows the desktop
//! live. (The `prefers-color-scheme` CSS media query needs GTK 4.20, above our baseline.)

use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_core::domain::Theme;
use std::cell::RefCell;

/// Rules of the hub. All of them are limited to `.lf-hub`.
const RULES: &str = include_str!("../../data/style.css");
const PALETTE_LIGHT: &str = include_str!("../../data/palette-light.css");
const PALETTE_DARK: &str = include_str!("../../data/palette-dark.css");

thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// Complete stylesheet for the light or the dark theme: palette first, then the rules.
pub fn css(dark: bool) -> String {
    let palette = if dark { PALETTE_DARK } else { PALETTE_LIGHT };
    format!("{palette}\n{RULES}")
}

/// libadwaita colour scheme for a setting: `System` follows the desktop (light when the desktop
/// has no preference).
pub fn color_scheme(theme: Theme) -> adw::ColorScheme {
    match theme {
        Theme::System => adw::ColorScheme::PreferLight,
        Theme::Light => adw::ColorScheme::ForceLight,
        Theme::Dark => adw::ColorScheme::ForceDark,
    }
}

/// Applies the "Appearance" setting. Without a display, does nothing.
pub fn apply_theme(theme: Theme) {
    if gtk::gdk::Display::default().is_none() {
        return;
    }
    let manager = adw::StyleManager::default();
    let scheme = color_scheme(theme);
    if manager.color_scheme() != scheme {
        manager.set_color_scheme(scheme);
    }
}

/// Loads the stylesheet on the default display (once per process) and keeps its palette in sync
/// with the light / dark decision of libadwaita. Without a display, does nothing.
pub fn install() {
    if PROVIDER.with(|p| p.borrow().is_some()) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let manager = adw::StyleManager::default();
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css(manager.is_dark()));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let live = provider.clone();
    manager.connect_dark_notify(move |m| live.load_from_string(&css(m.is_dark())));
    PROVIDER.with(|p| *p.borrow_mut() = Some(provider));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn defined(palette: &str) -> BTreeSet<String> {
        palette
            .lines()
            .filter_map(|l| l.strip_prefix("@define-color "))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect()
    }

    fn used(rules: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let bytes = rules.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'@' && rules[i..].starts_with("@lf_") {
                let end = rules[i + 1..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map_or(rules.len(), |n| i + 1 + n);
                out.insert(rules[i + 1..end].to_string());
                i = end;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn both_palettes_define_the_same_colours() {
        assert_eq!(defined(PALETTE_LIGHT), defined(PALETTE_DARK));
        assert!(defined(PALETTE_LIGHT).len() >= 18);
    }

    #[test]
    fn every_colour_used_by_the_rules_is_defined_in_both_palettes() {
        let used = used(RULES);
        assert!(!used.is_empty());
        for name in &used {
            assert!(defined(PALETTE_LIGHT).contains(name), "{name} (light)");
            assert!(defined(PALETTE_DARK).contains(name), "{name} (dark)");
        }
    }

    #[test]
    fn the_rules_hard_code_no_colour_and_define_none() {
        assert!(!RULES.contains("@define-color"));
        let hex = RULES
            .match_indices('#')
            .filter(|(i, _)| {
                RULES[i + 1..]
                    .chars()
                    .take(3)
                    .all(|c| c.is_ascii_hexdigit())
            })
            .count();
        assert_eq!(hex, 0, "colours must be named so that both themes work");
    }

    #[test]
    fn the_stylesheet_starts_with_the_palette_of_the_theme() {
        assert!(css(false).contains("lf_bg #fcfcfb"));
        assert!(css(true).contains("lf_bg #161615"));
        assert!(css(true).ends_with(RULES));
    }

    #[test]
    fn the_setting_maps_to_the_libadwaita_scheme() {
        assert_eq!(color_scheme(Theme::System), adw::ColorScheme::PreferLight);
        assert_eq!(color_scheme(Theme::Light), adw::ColorScheme::ForceLight);
        assert_eq!(color_scheme(Theme::Dark), adw::ColorScheme::ForceDark);
    }
}
