//! Troubleshooting page: folder shortcuts, engine restart and the `Controller::capabilities()` report with hints to fix.

use crate::hub::ctx::{run_blocking, Ctx};
use crate::hub::widgets::{button, label, page_box, page_head, scrolled_page};
use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_core::domain::Capabilities;
use linuxflow_i18n::tr;
use std::rc::Rc;

/// One row of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatRow {
    pub title: String,
    pub detail: String,
    pub status: CompatStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatStatus {
    Available,
    Missing,
    Info(String),
}

/// Builds the rows of the report (pure, testable).
///
/// `Missing` (warning) is reserved for a component whose absence really degrades LinuxFlow on this machine.
/// A component that the desktop does not offer, that is replaced by another mechanism, or that is optional is
/// reported with a neutral label and the reason, so that nothing looks like a fault when it is not.
pub fn rows(c: &Capabilities) -> Vec<CompatRow> {
    let yn = |b: bool| {
        if b {
            CompatStatus::Available
        } else {
            CompatStatus::Missing
        }
    };
    let row = |title: String, detail: String, status| CompatRow {
        title,
        detail,
        status,
    };
    // Neutral row: label + the reason shown as detail.
    let neutral = |title: String, label: String, reason: String| CompatRow {
        title,
        detail: reason,
        status: CompatStatus::Info(label),
    };
    let unknown = |s: &str| {
        if s.is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    let desktop = unknown(&c.desktop);
    let mut out = vec![
        row(
            tr("Session type"),
            tr("Wayland or X11."),
            CompatStatus::Info(unknown(&c.session)),
        ),
        row(
            tr("Desktop"),
            tr("The desktop environment in use."),
            CompatStatus::Info(desktop.clone()),
        ),
    ];

    // Global shortcut.
    let t = tr("Global shortcut portal");
    out.push(if c.portal_global_shortcuts {
        row(
            t,
            tr("Lets the shortcut work while other applications are focused."),
            CompatStatus::Available,
        )
    } else if c.has_native_shortcuts() {
        let how = match c.desktop.as_str() {
            "gnome" => {
                tr("Replaced by the GNOME keyboard shortcuts, where LinuxFlow sets its shortcut.")
            }
            "kde" => tr(
                "Replaced by the KDE global shortcut service, where LinuxFlow sets its shortcut.",
            ),
            _ => tr("Replaced by the compositor's own key bindings, which LinuxFlow sets."),
        };
        neutral(t, tr("Not needed"), how)
    } else if c.is_x11() {
        neutral(
            t,
            tr("Not needed"),
            tr("On X11 the shortcut key is grabbed directly."),
        )
    } else {
        row(
            t,
            tr("Lets the shortcut work while other applications are focused."),
            CompatStatus::Missing,
        )
    });

    // Paste key simulation.
    let t = tr("Remote desktop portal");
    out.push(if c.portal_remote_desktop {
        row(
            t,
            tr("Used to simulate the paste key in the active application."),
            CompatStatus::Available,
        )
    } else if c.is_x11() {
        neutral(
            t,
            tr("Not needed"),
            tr("On X11 the paste key is sent directly."),
        )
    } else {
        row(
            t,
            tr("Used to simulate the paste key in the active application."),
            CompatStatus::Missing,
        )
    });

    // Clipboard.
    let t = tr("Clipboard portal");
    out.push(if c.portal_clipboard {
        row(
            t,
            tr("Used to copy the text and restore your clipboard."),
            CompatStatus::Available,
        )
    } else if c.is_x11() {
        neutral(
            t,
            tr("Not needed"),
            tr("On X11 the regular clipboard is used."),
        )
    } else if c.data_control {
        neutral(
            t,
            tr("Not needed"),
            tr("The compositor's data-control protocol is used instead."),
        )
    } else {
        row(
            t,
            tr("Used to copy the text and restore your clipboard."),
            CompatStatus::Missing,
        )
    });

    // Recording pill.
    let t = tr("Layer shell");
    out.push(if c.layer_shell {
        row(
            t,
            tr("Lets the recording pill float above other windows."),
            CompatStatus::Available,
        )
    } else if c.is_wayland() {
        neutral(
            t,
            tr("Not offered"),
            tr("This desktop does not provide it: the recording pill uses a regular window."),
        )
    } else {
        neutral(t, tr("Not applicable"), tr("Only used on Wayland."))
    });

    // Wayland clipboard access.
    let t = tr("Clipboard data control");
    out.push(if c.data_control {
        row(
            t,
            tr("Alternative clipboard access on Wayland."),
            CompatStatus::Available,
        )
    } else if c.portal_clipboard {
        neutral(
            t,
            tr("Not needed"),
            tr("The clipboard portal is used instead."),
        )
    } else if c.is_wayland() {
        row(
            t,
            tr("Alternative clipboard access on Wayland."),
            CompatStatus::Missing,
        )
    } else {
        neutral(t, tr("Not applicable"), tr("Only used on Wayland."))
    });

    // GPU: running on the CPU is a normal mode, not a fault.
    out.push(if c.vulkan_available {
        row(
            tr("Vulkan GPU"),
            c.vulkan_devices.join(", "),
            CompatStatus::Available,
        )
    } else {
        neutral(
            tr("Vulkan GPU"),
            tr("CPU mode"),
            tr("No usable GPU was found: the lighter model runs on the CPU."),
        )
    });

    out.push(if c.pipewire {
        row(
            tr("PipeWire"),
            tr("Audio server; ALSA is used when it is missing."),
            CompatStatus::Available,
        )
    } else {
        neutral(
            tr("PipeWire"),
            tr("Not used"),
            tr("PipeWire was not found: audio capture uses ALSA."),
        )
    });

    out.push(row(
        tr("Notification area"),
        tr("Tray icon support."),
        yn(c.tray_watcher),
    ));

    out.push(if c.uinput_writable {
        row(
            tr("Direct input access (uinput)"),
            tr("Optional: key simulation without a portal."),
            CompatStatus::Available,
        )
    } else {
        neutral(
            tr("Direct input access (uinput)"),
            tr("Optional"),
            tr("Key simulation without a portal. Not enabled: this is the normal setup."),
        )
    });
    out.push(if c.input_group {
        row(
            tr("Input group membership"),
            tr("Optional: reading keys for hold-to-talk."),
            CompatStatus::Available,
        )
    } else {
        neutral(
            tr("Input group membership"),
            tr("Optional"),
            tr("Reading keys for hold-to-talk. Not enabled: this is the normal setup."),
        )
    });

    out.push(row(
        tr("GTK version"),
        String::new(),
        CompatStatus::Info(unknown(&c.gtk_version)),
    ));
    out.push(row(
        tr("libadwaita version"),
        String::new(),
        CompatStatus::Info(unknown(&c.adw_version)),
    ));
    out
}

pub struct CompatPage {
    root: gtk::ScrolledWindow,
    ctx: Ctx,
    list: gtk::ListBox,
    hints_title: gtk::Label,
    hints: gtk::ListBox,
}

fn clear(list: &gtk::ListBox) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
}

impl CompatPage {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let page = page_box();
        let (head, _, _) = page_head(
            &tr("Troubleshooting"),
            &tr("Open the folders LinuxFlow uses, restart the speech engine, and check each system component it relies on."),
        );
        page.append(&head);
        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        tools.set_margin_bottom(6);
        let logs_b = button(&tr("Open logs folder"), &["lf-btn"]);
        let data_b = button(&tr("Open data folder"), &["lf-btn"]);
        let models_b = button(&tr("Open models folder"), &["lf-btn"]);
        let restart_b = button(&tr("Restart speech engine"), &["lf-btn"]);
        for b in [&logs_b, &data_b, &models_b, &restart_b] {
            tools.append(b);
        }
        page.append(&tools);
        let report_head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let report_title = label(&tr("Compatibility report"), &["lf-h2"]);
        report_title.set_hexpand(true);
        report_title.set_halign(gtk::Align::Start);
        let refresh = button(&tr("Refresh"), &["lf-btn"]);
        report_head.append(&report_title);
        report_head.append(&refresh);
        page.append(&report_head);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        page.append(&list);
        let hints_title = label(&tr("How to fix"), &["lf-h2"]);
        hints_title.set_margin_top(10);
        let hints = gtk::ListBox::new();
        hints.set_selection_mode(gtk::SelectionMode::None);
        hints.add_css_class("boxed-list");
        page.append(&hints_title);
        page.append(&hints);
        let this = Rc::new(Self {
            root: scrolled_page(&page),
            ctx: ctx.clone(),
            list,
            hints_title,
            hints,
        });
        let c = this.ctx.controller.clone();
        logs_b.connect_clicked(move |_| c.open_logs_dir());
        let c = this.ctx.controller.clone();
        data_b.connect_clicked(move |_| c.open_data_dir());
        let c = this.ctx.controller.clone();
        models_b.connect_clicked(move |_| c.open_models_dir());
        let c = this.ctx.controller.clone();
        restart_b.connect_clicked(move |_| c.restart_engine());
        let weak = Rc::downgrade(&this);
        refresh.connect_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                t.refresh();
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn refresh(self: &Rc<Self>) {
        let controller = self.ctx.controller.clone();
        let weak = Rc::downgrade(self);
        run_blocking(
            move || controller.capabilities(),
            move |caps| {
                if let Some(t) = weak.upgrade() {
                    t.set_capabilities(&caps);
                }
            },
        );
    }

    pub fn row_count(&self) -> usize {
        let mut n = 0;
        let mut c = self.list.first_child();
        while let Some(w) = c {
            n += 1;
            c = w.next_sibling();
        }
        n
    }

    pub fn set_capabilities(&self, caps: &Capabilities) {
        clear(&self.list);
        for r in rows(caps) {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&r.title));
            if !r.detail.is_empty() {
                row.set_subtitle(&gtk::glib::markup_escape_text(&r.detail));
            }
            let (icon, text, class) = match &r.status {
                CompatStatus::Available => ("emblem-ok-symbolic", tr("Available"), "ok"),
                CompatStatus::Missing => ("dialog-warning-symbolic", tr("Not available"), "warn"),
                CompatStatus::Info(s) => ("", s.clone(), "info"),
            };
            let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            suffix.set_valign(gtk::Align::Center);
            if !icon.is_empty() {
                let img = gtk::Image::from_icon_name(icon);
                img.add_css_class(class);
                suffix.append(&img);
            }
            let l = label(&text, &["lf-compat-status", class]);
            l.set_wrap(false);
            suffix.append(&l);
            row.add_suffix(&suffix);
            self.list.append(&row);
        }
        clear(&self.hints);
        let any = !caps.hints.is_empty();
        self.hints_title.set_visible(any);
        self.hints.set_visible(any);
        for (brick, msg) in &caps.hints {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(brick));
            row.set_subtitle(&gtk::glib::markup_escape_text(msg));
            row.set_subtitle_lines(0);
            self.hints.append(&row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status<'a>(r: &'a [CompatRow], title: &str) -> &'a CompatStatus {
        &r.iter()
            .find(|r| r.title == title)
            .unwrap_or_else(|| panic!("row {title}"))
            .status
    }

    fn is_info(s: &CompatStatus) -> bool {
        matches!(s, CompatStatus::Info(_))
    }

    #[test]
    fn nothing_detected_flags_only_what_really_matters() {
        let r = rows(&Capabilities::default());
        // Without any session, the shortcut, paste, clipboard and tray have no way to work.
        for t in [
            "Global shortcut portal",
            "Remote desktop portal",
            "Clipboard portal",
            "Notification area",
        ] {
            assert_eq!(status(&r, t), &CompatStatus::Missing, "{t}");
        }
        // Optional or replaced components are never reported as faults.
        for t in [
            "Vulkan GPU",
            "PipeWire",
            "Direct input access (uinput)",
            "Input group membership",
            "Layer shell",
            "Clipboard data control",
        ] {
            assert!(is_info(status(&r, t)), "{t} must be neutral");
        }
        let vulkan = r.iter().find(|r| r.title == "Vulkan GPU");
        assert!(
            vulkan.is_some_and(|v| v.detail.contains("CPU")),
            "CPU mode announced"
        );
    }

    #[test]
    fn gnome_wayland_without_extras_has_no_false_alarm() {
        // GNOME 46 / 47: no GlobalShortcuts portal, no layer-shell, no data-control, no GPU in a VM.
        let caps = Capabilities {
            session: "wayland".into(),
            desktop: "gnome".into(),
            portal_remote_desktop: true,
            portal_clipboard: true,
            pipewire: true,
            tray_watcher: true,
            ..Capabilities::default()
        };
        let r = rows(&caps);
        let missing: Vec<_> = r
            .iter()
            .filter(|r| r.status == CompatStatus::Missing)
            .map(|r| r.title.clone())
            .collect();
        assert!(missing.is_empty(), "false alarms: {missing:?}");
        assert_eq!(
            status(&r, "Global shortcut portal"),
            &CompatStatus::Info("Not needed".into())
        );
        assert_eq!(
            status(&r, "Layer shell"),
            &CompatStatus::Info("Not offered".into())
        );
    }

    #[test]
    fn missing_tray_is_a_real_limitation() {
        let caps = Capabilities {
            session: "wayland".into(),
            desktop: "gnome".into(),
            portal_remote_desktop: true,
            portal_clipboard: true,
            ..Capabilities::default()
        };
        assert_eq!(
            status(&rows(&caps), "Notification area"),
            &CompatStatus::Missing
        );
    }

    #[test]
    fn unsupported_wayland_desktop_flags_the_shortcut_and_paste() {
        let caps = Capabilities {
            session: "wayland".into(),
            desktop: "weston".into(),
            ..Capabilities::default()
        };
        let r = rows(&caps);
        for t in [
            "Global shortcut portal",
            "Remote desktop portal",
            "Clipboard portal",
            "Clipboard data control",
        ] {
            assert_eq!(status(&r, t), &CompatStatus::Missing, "{t}");
        }
    }

    #[test]
    fn x11_needs_none_of_the_wayland_portals() {
        let caps = Capabilities {
            session: "x11".into(),
            desktop: "xfce".into(),
            tray_watcher: true,
            ..Capabilities::default()
        };
        let r = rows(&caps);
        assert!(r.iter().all(|r| r.status != CompatStatus::Missing));
    }

    #[test]
    fn rows_reflect_gpu_and_versions() {
        let caps = Capabilities {
            session: "wayland".into(),
            vulkan_available: true,
            vulkan_devices: vec!["A".into(), "B".into()],
            ..Capabilities::default()
        };
        let r = rows(&caps);
        assert_eq!(r[0].status, CompatStatus::Info("wayland".into()));
        assert_eq!(
            r[1].status,
            CompatStatus::Info("—".into()),
            "unknown desktop"
        );
        let v = r.iter().find(|r| r.title == "Vulkan GPU");
        assert!(v.is_some_and(|v| v.status == CompatStatus::Available && v.detail == "A, B"));
    }
}
