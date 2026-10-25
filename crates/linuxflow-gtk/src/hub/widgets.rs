//! Small widget builders shared by the hub pages.

use gtk::prelude::*;
use gtk4 as gtk;

/// Label with CSS classes, wrapping enabled.
pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

/// Updates the text only if it changes (avoids relayout and loss of selection).
pub fn set_label(l: &gtk::Label, text: &str) {
    if l.text() != text {
        l.set_text(text);
    }
}

/// Text button with CSS classes.
pub fn button(text: &str, classes: &[&str]) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    for c in classes {
        b.add_css_class(c);
    }
    b
}

/// White card with rounded corners.
pub fn card() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("lf-card");
    b
}

/// Styled keyboard key (`kbd`).
pub fn kbd_chip(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("lf-kbd");
    l
}

/// Page header: title + subtitle.
pub fn page_head(title: &str, subtitle: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("lf-page-head");
    let h1 = label(title, &["lf-h1"]);
    let sub = label(subtitle, &["lf-sub"]);
    b.append(&h1);
    if !subtitle.is_empty() {
        b.append(&sub);
    }
    (b, h1, sub)
}

/// Centred scrolling page (max width 980) around `content`.
pub fn scrolled_page(content: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = libadwaita::Clamp::new();
    clamp.set_maximum_size(980);
    clamp.set_tightening_threshold(700);
    clamp.set_child(Some(content));
    let sw = gtk::ScrolledWindow::new();
    sw.set_hscrollbar_policy(gtk::PolicyType::Never);
    sw.set_child(Some(&clamp));
    sw.set_vexpand(true);
    sw
}

/// Vertical page box with standard margins.
pub fn page_box() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 14);
    b.set_margin_top(32);
    b.set_margin_bottom(36);
    b.set_margin_start(36);
    b.set_margin_end(36);
    b
}

/// Gives an accessible name to a widget (icon-only buttons).
pub fn set_accessible_label(w: &impl IsA<gtk::Accessible>, text: &str) {
    w.update_property(&[gtk::accessible::Property::Label(text)]);
}

/// Escapes an ALREADY translated template (`&tr("...")`, so that xgettext extracts it) then replaces the
/// `{}` with valid markup fragments (never sentence concatenation).
pub fn markup_fmt(translated: &str, args_markup: &[&str]) -> String {
    let mut out = gtk::glib::markup_escape_text(translated).to_string();
    for a in args_markup {
        if let Some(i) = out.find("{}") {
            out.replace_range(i..i + 2, a);
        }
    }
    out
}

/// Pango fragment highlighting a shortcut ("kbd" style). The highlight is a translucent tint of the
/// accent colour, so that the text stays readable in the light and in the dark theme.
pub fn kbd_markup(text: &str) -> String {
    format!(
        "<span weight=\"bold\" background=\"#a550a7\" background_alpha=\"22%\">\u{a0}{}\u{a0}</span>",
        gtk::glib::markup_escape_text(text)
    )
}

/// Short text pill (never wrapped).
pub fn badge(text: &str, kind: &str) -> gtk::Label {
    let l = label(text, &["lf-badge", kind]);
    l.set_wrap(false);
    l.set_valign(gtk::Align::Center);
    l
}
