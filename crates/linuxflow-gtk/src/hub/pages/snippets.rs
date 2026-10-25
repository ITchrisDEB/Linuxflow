//! Snippets page: trigger → inserted text.

use crate::hub::ctx::Ctx;
use crate::hub::keyed::KeyedBox;
use crate::hub::widgets::{button, label, page_box, page_head, scrolled_page};
use gtk::prelude::*;
use gtk4 as gtk;
use linuxflow_core::domain::{AppState, Snippet};
use linuxflow_i18n::tr;
use std::rc::Rc;

pub struct SnippetsPage {
    root: gtk::ScrolledWindow,
    ctx: Ctx,
    trigger: gtk::Entry,
    expansion: gtk::TextView,
    list: KeyedBox,
    empty: gtk::Label,
}

impl SnippetsPage {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let page = page_box();
        let (head, _, _) = page_head(
            &tr("Snippets"),
            &tr("Say a trigger phrase, LinuxFlow writes the full text."),
        );
        page.append(&head);

        let form = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        form.set_valign(gtk::Align::Start);
        let trigger = gtk::Entry::new();
        trigger.set_placeholder_text(Some(&tr("Trigger (e.g. “my signature”)")));
        trigger.set_width_chars(22);
        trigger.set_valign(gtk::Align::Start);
        let expansion = gtk::TextView::new();
        expansion.set_wrap_mode(gtk::WrapMode::WordChar);
        expansion.add_css_class("lf-snippet-text");
        for set in [
            gtk::TextView::set_top_margin,
            gtk::TextView::set_bottom_margin,
        ] {
            set(&expansion, 8);
        }
        expansion.set_left_margin(10);
        expansion.set_right_margin(10);
        let frame = gtk::ScrolledWindow::new();
        frame.set_min_content_height(56);
        frame.set_hexpand(true);
        frame.set_child(Some(&expansion));
        let outline = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outline.add_css_class("lf-try-frame");
        outline.set_overflow(gtk::Overflow::Hidden);
        outline.set_hexpand(true);
        outline.append(&frame);
        let placeholder = label(
            &tr("Text inserted instead (e.g. “Have a great day and talk soon!”)"),
            &["lf-placeholder"],
        );
        placeholder.set_can_target(false);
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_valign(gtk::Align::Start);
        placeholder.set_margin_top(8);
        placeholder.set_margin_start(10);
        let ov = gtk::Overlay::new();
        ov.set_child(Some(&outline));
        ov.add_overlay(&placeholder);
        ov.set_hexpand(true);
        expansion.buffer().connect_changed({
            let p = placeholder.clone();
            move |b| p.set_visible(b.char_count() == 0)
        });
        let add = button(&tr("Add snippet"), &["lf-btn", "primary"]);
        add.set_valign(gtk::Align::Start);
        form.append(&trigger);
        form.append(&ov);
        form.append(&add);
        page.append(&form);
        let list = KeyedBox::new(10);
        page.append(list.widget());
        let empty = label(
            &tr("No snippets yet. Example: “my signature” → “Have a great day and talk soon!”."),
            &["lf-empty"],
        );
        page.append(&empty);

        let this = Rc::new(Self {
            root: scrolled_page(&page),
            ctx: ctx.clone(),
            trigger,
            expansion,
            list,
            empty,
        });
        let weak = Rc::downgrade(&this);
        add.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(t) = weak.upgrade() {
                    t.submit();
                }
            }
        });
        this.trigger.connect_activate(move |_| {
            if let Some(t) = weak.upgrade() {
                t.submit();
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn update(&self, state: &AppState) {
        let wanted: Vec<(String, String)> = state
            .snippets
            .iter()
            .map(|s| (s.id.clone(), format!("{}|{}", s.trigger, s.expansion)))
            .collect();
        let snippets = state.snippets.clone();
        self.list
            .sync(&wanted, &mut |i| self.build_row(&snippets[i]));
        self.empty.set_visible(snippets.is_empty());
    }

    fn build_row(&self, s: &Snippet) -> gtk::Widget {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("lf-item");
        let left = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        left.set_hexpand(true);
        left.set_valign(gtk::Align::Start);
        left.append(&label(&s.trigger, &["lf-trigger"]));
        left.append(&label("→", &["lf-meta"]));
        let exp = label(&s.expansion, &["lf-item-text"]);
        exp.set_hexpand(true);
        left.append(&exp);
        row.append(&left);
        let delete = button(&tr("Delete"), &["lf-btn", "small", "danger"]);
        delete.set_valign(gtk::Align::Start);
        let (ctx, id) = (self.ctx.clone(), s.id.clone());
        delete.connect_clicked(move |_| {
            if let Err(e) = ctx.controller.snippet_remove(&id) {
                ctx.toast.show(&e.to_string());
            }
        });
        row.append(&delete);
        row.upcast()
    }

    /// (trigger, text) being typed.
    pub fn form_values(&self) -> (String, String) {
        let b = self.expansion.buffer();
        (
            self.trigger.text().to_string(),
            b.text(&b.start_iter(), &b.end_iter(), false).to_string(),
        )
    }

    pub fn restore_form(&self, v: &(String, String)) {
        self.trigger.set_text(&v.0);
        self.expansion.buffer().set_text(&v.1);
    }

    /// Adds the typed snippet (non-empty trigger).
    pub fn submit(&self) {
        let trigger = self.trigger.text().trim().to_string();
        if trigger.is_empty() {
            self.trigger.grab_focus();
            return;
        }
        let buf = self.expansion.buffer();
        let expansion = buf
            .text(&buf.start_iter(), &buf.end_iter(), false)
            .to_string();
        match self.ctx.controller.snippet_add(&trigger, &expansion) {
            Ok(()) => {
                self.trigger.set_text("");
                buf.set_text("");
                self.ctx.toast.show(&tr("Snippet added"));
            }
            Err(e) => self.ctx.toast.show(&e.to_string()),
        }
    }
}
