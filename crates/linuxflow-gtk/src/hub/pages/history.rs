//! History page: list of dictations, Copy / Delete, "Clear all" with confirmation.

use crate::hub::ctx::Ctx;
use crate::hub::dialog;
use crate::hub::keyed::KeyedBox;
use crate::hub::labels::{format_latency_ms, relative_iso, word_count};
use crate::hub::widgets::{button, label, page_box, page_head, scrolled_page};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use linuxflow_core::domain::{AppState, HistoryEntry};
use linuxflow_i18n::{tr, tr_fmt};
use std::cell::RefCell;
use std::rc::Rc;

/// Refresh period of the relative times.
const TICK_SECS: u32 = 30;

pub struct HistoryPage {
    root: gtk::ScrolledWindow,
    ctx: Ctx,
    list: KeyedBox,
    empty: gtk::Label,
    clear: gtk::Button,
    entries: RefCell<Vec<HistoryEntry>>,
}

fn relative(entry: &HistoryEntry) -> String {
    relative_iso(&entry.ts)
}

impl HistoryPage {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let page = page_box();
        let (head, _, _) = page_head(
            &tr("History"),
            &tr("Your latest dictations, most recent first."),
        );
        head.set_hexpand(true);
        let clear = button(&tr("Clear all"), &["lf-btn", "ghost"]);
        clear.set_valign(gtk::Align::Start);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        top.append(&head);
        top.append(&clear);
        page.append(&top);
        let list = KeyedBox::new(10);
        page.append(list.widget());
        let empty = label(
            &tr("No dictation yet. Press the shortcut and speak."),
            &["lf-empty"],
        );
        page.append(&empty);

        let this = Rc::new(Self {
            root: scrolled_page(&page),
            ctx: ctx.clone(),
            list,
            empty,
            clear: clear.clone(),
            entries: Default::default(),
        });
        let weak = Rc::downgrade(&this);
        clear.connect_clicked(move |b| {
            if let Some(t) = weak.upgrade() {
                t.confirm_clear(b.upcast_ref());
            }
        });
        let weak = Rc::downgrade(&this);
        glib::timeout_add_seconds_local(TICK_SECS, move || match weak.upgrade() {
            Some(t) => {
                t.sync();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        this.sync();
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
        *self.entries.borrow_mut() = state.history.clone();
        self.sync();
    }

    fn sync(&self) {
        let entries = self.entries.borrow().clone();
        let wanted: Vec<(String, String)> = entries
            .iter()
            .map(|e| (e.id.clone(), format!("{}|{}", e.text, relative(e))))
            .collect();
        self.list
            .sync(&wanted, &mut |i| self.build_row(&entries[i]));
        self.empty.set_visible(entries.is_empty());
        self.clear.set_sensitive(!entries.is_empty());
    }

    fn build_row(&self, e: &HistoryEntry) -> gtk::Widget {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("lf-item");
        let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
        left.set_hexpand(true);
        let text = label(&e.text, &["lf-item-text"]);
        text.set_selectable(true);
        left.append(&text);
        let meta = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let mut items = vec![relative(e), word_count(u64::from(e.words))];
        if e.latency_ms > 0 {
            items.push(format_latency_ms(e.latency_ms as f64));
        }
        if let Some(app) = e.app.as_deref().filter(|a| !a.is_empty()) {
            items.push(tr_fmt("→ {}", &[app]));
        }
        if let Some(lang) = e.language.as_deref().filter(|l| !l.is_empty()) {
            items.push(lang.to_string());
        }
        for m in items.into_iter().filter(|m| !m.is_empty()) {
            meta.append(&label(&m, &["lf-meta"]));
        }
        left.append(&meta);
        row.append(&left);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_valign(gtk::Align::Start);
        let copy = button(&tr("Copy"), &["lf-btn", "small"]);
        let delete = button(&tr("Delete"), &["lf-btn", "small", "danger"]);
        let (ctx, text_value) = (self.ctx.clone(), e.text.clone());
        copy.connect_clicked(move |_| match ctx.controller.copy_text(&text_value) {
            Ok(()) => ctx.toast.show(&tr("Copied")),
            Err(err) => ctx.toast.show(&err.to_string()),
        });
        let (ctx, id) = (self.ctx.clone(), e.id.clone());
        delete.connect_clicked(move |_| {
            if let Err(err) = ctx.controller.history_delete(&id) {
                ctx.toast.show(&err.to_string());
            }
        });
        actions.append(&copy);
        actions.append(&delete);
        row.append(&actions);
        row.upcast()
    }

    fn confirm_clear(&self, source: &gtk::Widget) {
        if self.entries.borrow().is_empty() {
            return;
        }
        let ctx = self.ctx.clone();
        dialog::confirm(
            source,
            &tr("Clear the whole history?"),
            &tr("All saved dictations will be deleted. This cannot be undone."),
            &tr("Clear all"),
            move || {
                if let Err(err) = ctx.controller.history_clear() {
                    ctx.toast.show(&err.to_string());
                }
            },
        );
    }
}
