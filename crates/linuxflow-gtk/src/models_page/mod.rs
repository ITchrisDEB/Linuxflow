//! "Models" page: list of the catalogue models with state, real progress and actions
//! (download, pause, resume, cancel, delete, verify, use, local file).

pub mod logic;

use crate::hub::ctx::{run_blocking, Ctx};
use crate::hub::dialog;
use crate::hub::keyed::KeyedBox;
use crate::hub::labels::format_size;
use crate::hub::widgets::{
    badge, button, card, label, page_box, page_head, scrolled_page, set_label,
};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use linuxflow_core::domain::{ModelState, ModelStatus};
use linuxflow_i18n::{tr, tr_fmt};
use logic::{
    actions_for, disk_used, is_in_progress, progress_fraction, progress_text, sizes_text,
    state_text, ModelAction, SpeedMeter,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Minimum interval between two catalogue queries triggered by state events.
const REFRESH_MIN_MS: i64 = 400;

struct RowHandle {
    bar: gtk::ProgressBar,
    text: gtk::Label,
    state: gtk::Label,
}

pub struct ModelsPage {
    root: gtk::ScrolledWindow,
    ctx: Ctx,
    list: KeyedBox,
    summary: gtk::Label,
    empty: gtk::Label,
    handles: RefCell<HashMap<String, RowHandle>>,
    meters: RefCell<HashMap<String, SpeedMeter>>,
    models: RefCell<Vec<ModelStatus>>,
    last_refresh_us: Cell<i64>,
    visible: Cell<bool>,
}

impl ModelsPage {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let page = page_box();
        let (head, _, _) = page_head(
            &tr("Models"),
            &tr("Speech recognition models run on this computer. Nothing is sent anywhere except the download you ask for."),
        );
        let local = button(&tr("Choose a local file…"), &["lf-btn"]);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        head.set_hexpand(true);
        top.append(&head);
        local.set_valign(gtk::Align::Start);
        top.append(&local);
        page.append(&top);
        let summary = label("", &["lf-hint"]);
        page.append(&summary);
        let list = KeyedBox::new(10);
        page.append(list.widget());
        let empty = label(&tr("No model in the catalog."), &["lf-empty"]);
        page.append(&empty);

        let this = Rc::new(Self {
            root: scrolled_page(&page),
            ctx: ctx.clone(),
            list,
            summary,
            empty,
            handles: Default::default(),
            meters: Default::default(),
            models: Default::default(),
            last_refresh_us: Cell::new(0),
            visible: Cell::new(false),
        });
        let weak = Rc::downgrade(&this);
        local.connect_clicked(move |b| {
            if let Some(t) = weak.upgrade() {
                t.choose_local_file(b.upcast_ref());
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn set_visible_page(self: &Rc<Self>, visible: bool) {
        self.visible.set(visible);
        if visible {
            self.refresh();
        }
    }

    /// Queries the catalogue again (off the GTK thread).
    pub fn refresh(self: &Rc<Self>) {
        self.last_refresh_us.set(glib::monotonic_time());
        let controller = self.ctx.controller.clone();
        let weak = Rc::downgrade(self);
        run_blocking(
            move || controller.models(),
            move |models| {
                if let Some(t) = weak.upgrade() {
                    t.set_models(models);
                }
            },
        );
    }

    /// Called at every `Event::State`: refreshes if the page is visible, at most every 400 ms.
    pub fn on_state(self: &Rc<Self>) {
        if self.visible.get()
            && glib::monotonic_time() - self.last_refresh_us.get() >= REFRESH_MIN_MS * 1000
        {
            self.refresh();
        }
    }

    pub fn models(&self) -> Vec<ModelStatus> {
        self.models.borrow().clone()
    }

    /// Applies a list of models (differences only).
    pub fn set_models(self: &Rc<Self>, models: Vec<ModelStatus>) {
        let now_ms = (glib::monotonic_time() / 1000) as u64;
        {
            let mut meters = self.meters.borrow_mut();
            meters.retain(|id, _| models.iter().any(|m| &m.id == id));
            for m in &models {
                let meter = meters.entry(m.id.clone()).or_default();
                match &m.state {
                    ModelState::Downloading { done, .. } => meter.sample(now_ms, *done),
                    _ => meter.reset(),
                }
            }
        }
        let wanted: Vec<(String, String)> = models
            .iter()
            .map(|m| (m.id.clone(), signature(m)))
            .collect();
        self.handles
            .borrow_mut()
            .retain(|id, _| models.iter().any(|m| &m.id == id));
        self.list.sync(&wanted, &mut |i| self.build_row(&models[i]));
        // Updates the progress bars without rebuilding the rows.
        let handles = self.handles.borrow();
        let meters = self.meters.borrow();
        for m in &models {
            if let (Some(h), Some(meter)) = (handles.get(&m.id), meters.get(&m.id)) {
                update_progress(h, m, meter);
            }
        }
        self.empty.set_visible(models.is_empty());
        set_label(
            &self.summary,
            &tr_fmt(
                "Disk space used by models: {}",
                &[&format_size(disk_used(&models))],
            ),
        );
        *self.models.borrow_mut() = models;
    }

    fn build_row(self: &Rc<Self>, m: &ModelStatus) -> gtk::Widget {
        let c = card();
        c.add_css_class("lf-model");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.append(&label(&logic::display_name(m), &["lf-model-name"]));
        if m.recommended {
            top.append(&badge(&tr("Recommended for this computer"), "recommended"));
        }
        if m.in_use {
            top.append(&badge(&tr("In use"), "in-use"));
        }
        c.append(&top);
        c.append(&label(&sizes_text(m), &["lf-hint"]));
        let state = label(&state_text(&m.state), &["lf-model-state"]);
        if matches!(m.state, ModelState::Error(_)) {
            state.add_css_class("bad");
        }
        c.append(&state);
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("lf-progress");
        let text = label("", &["lf-hint"]);
        let show_progress = is_in_progress(&m.state);
        bar.set_visible(show_progress);
        text.set_visible(show_progress);
        c.append(&bar);
        c.append(&text);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_margin_top(10);
        for a in actions_for(m) {
            let b = button(
                &a.label(),
                &[
                    "lf-btn",
                    "small",
                    if a == ModelAction::Use {
                        "primary"
                    } else {
                        "ghost"
                    },
                ],
            );
            let weak = Rc::downgrade(self);
            let id = m.id.clone();
            let name = m.name.clone();
            b.connect_clicked(move |btn| {
                if let Some(t) = weak.upgrade() {
                    t.run_action(btn.upcast_ref(), a, &id, &name);
                }
            });
            actions.append(&b);
        }
        c.append(&actions);
        self.handles
            .borrow_mut()
            .insert(m.id.clone(), RowHandle { bar, text, state });
        c.upcast()
    }

    fn run_action(
        self: &Rc<Self>,
        source: &gtk::Widget,
        action: ModelAction,
        id: &str,
        name: &str,
    ) {
        if action == ModelAction::Delete {
            let weak = Rc::downgrade(self);
            let id = id.to_string();
            dialog::confirm(
                source,
                &tr_fmt("Delete “{}”?", &[name]),
                &tr("The downloaded file is removed from this computer. You can download it again later."),
                &tr("Delete"),
                move || {
                    if let Some(t) = weak.upgrade() {
                        t.dispatch(ModelAction::Delete, &id);
                    }
                },
            );
            return;
        }
        self.dispatch(action, id);
    }

    fn dispatch(self: &Rc<Self>, action: ModelAction, id: &str) {
        let controller = self.ctx.controller.clone();
        let id = id.to_string();
        let weak = Rc::downgrade(self);
        run_blocking(
            move || match action {
                ModelAction::Download => controller.model_download(&id),
                ModelAction::Pause => controller.model_pause(&id),
                ModelAction::Resume => controller.model_resume(&id),
                ModelAction::Cancel => controller.model_cancel(&id),
                ModelAction::Delete => controller.model_delete(&id),
                ModelAction::Verify => controller.model_verify(&id),
                ModelAction::Use => controller.model_use(&id),
            },
            move |result| {
                let Some(t) = weak.upgrade() else { return };
                match result {
                    Ok(()) if action == ModelAction::Verify => {
                        t.ctx.toast.show(&tr("Integrity check started"))
                    }
                    Ok(()) if action == ModelAction::Use => t.ctx.toast.show(&tr("Model selected")),
                    Ok(()) => {}
                    Err(e) => t.ctx.toast.show(&e.to_string()),
                }
                t.refresh();
            },
        );
    }

    fn choose_local_file(self: &Rc<Self>, source: &gtk::Widget) {
        let window = source.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let dialog = gtk::FileDialog::builder()
            .title(tr("Choose a model file"))
            .build();
        let weak = Rc::downgrade(self);
        dialog.open(
            window.as_ref(),
            None::<&gtk::gio::Cancellable>,
            move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else { return };
                let Some(t) = weak.upgrade() else { return };
                let controller = t.ctx.controller.clone();
                let weak = Rc::downgrade(&t);
                run_blocking(
                    move || controller.model_use_local_file(&path),
                    move |r| {
                        let Some(t) = weak.upgrade() else { return };
                        match r {
                            Ok(()) => t.ctx.toast.show(&tr("Model file selected")),
                            Err(e) => t.ctx.toast.show(&e.to_string()),
                        }
                        t.refresh();
                    },
                );
            },
        );
    }
}

/// Structural signature: changes when the buttons/badges must change (not at every byte received).
fn signature(m: &ModelStatus) -> String {
    let kind = match &m.state {
        ModelState::NotInstalled => "n".to_string(),
        ModelState::Downloading { total, .. } => format!("d{}", *total > 0),
        ModelState::Paused { total, .. } => format!("p{}", *total > 0),
        ModelState::Installed => "i".to_string(),
        ModelState::Unverified => "u".to_string(),
        ModelState::Error(e) => format!("e{e}"),
    };
    format!(
        "{kind}|{}|{}|{}|{}|{}|{}",
        m.name, m.recommended, m.in_use, m.download_bytes, m.disk_bytes, m.memory_bytes
    )
}

fn update_progress(h: &RowHandle, m: &ModelStatus, meter: &SpeedMeter) {
    match &m.state {
        ModelState::Downloading { done, total } | ModelState::Paused { done, total } => {
            match progress_fraction(&m.state) {
                Some(f) => h.bar.set_fraction(f),
                None => h.bar.pulse(),
            }
            set_label(&h.text, &progress_text(*done, *total, meter));
            set_label(&h.state, &state_text(&m.state));
        }
        _ => {}
    }
}
