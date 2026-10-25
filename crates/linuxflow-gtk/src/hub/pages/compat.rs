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
    let vulkan_detail = if c.vulkan_devices.is_empty() {
        tr("Without a usable GPU, the lighter model runs on the CPU.")
    } else {
        c.vulkan_devices.join(", ")
    };
    let unknown = |s: &str| {
        if s.is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    vec![
        row(
            tr("Session type"),
            tr("Wayland or X11."),
            CompatStatus::Info(unknown(&c.session)),
        ),
        row(
            tr("Desktop"),
            tr("The desktop environment in use."),
            CompatStatus::Info(unknown(&c.desktop)),
        ),
        row(
            tr("Global shortcut portal"),
            tr("Lets the shortcut work while other applications are focused."),
            yn(c.portal_global_shortcuts),
        ),
        row(
            tr("Remote desktop portal"),
            tr("Used to simulate the paste key in the active application."),
            yn(c.portal_remote_desktop),
        ),
        row(
            tr("Clipboard portal"),
            tr("Used to copy the text and restore your clipboard."),
            yn(c.portal_clipboard),
        ),
        row(
            tr("Layer shell"),
            tr("Lets the recording pill float above other windows."),
            yn(c.layer_shell),
        ),
        row(
            tr("Clipboard data control"),
            tr("Alternative clipboard access on Wayland."),
            yn(c.data_control),
        ),
        row(tr("Vulkan GPU"), vulkan_detail, yn(c.vulkan_available)),
        row(
            tr("PipeWire"),
            tr("Audio server; ALSA is used when it is missing."),
            yn(c.pipewire),
        ),
        row(
            tr("Notification area"),
            tr("Tray icon support."),
            yn(c.tray_watcher),
        ),
        row(
            tr("Direct input access (uinput)"),
            tr("Optional: key simulation without a portal."),
            yn(c.uinput_writable),
        ),
        row(
            tr("Input group membership"),
            tr("Optional: reading keys for hold-to-talk."),
            yn(c.input_group),
        ),
        row(
            tr("GTK version"),
            String::new(),
            CompatStatus::Info(unknown(&c.gtk_version)),
        ),
        row(
            tr("libadwaita version"),
            String::new(),
            CompatStatus::Info(unknown(&c.adw_version)),
        ),
    ]
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

    #[test]
    fn rows_reflect_capabilities() {
        let none = rows(&Capabilities::default());
        assert!(
            none.iter()
                .filter(|r| r.status == CompatStatus::Missing)
                .count()
                >= 9
        );
        let vulkan = none
            .iter()
            .find(|r| r.title == "Vulkan GPU")
            .map(|r| r.detail.clone());
        assert!(
            vulkan.is_some_and(|d| d.contains("CPU")),
            "CPU fallback announced"
        );
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
            "bureau inconnu"
        );
        let v = r.iter().find(|r| r.title == "Vulkan GPU");
        assert!(v.is_some_and(|v| v.status == CompatStatus::Available && v.detail == "A, B"));
    }
}
