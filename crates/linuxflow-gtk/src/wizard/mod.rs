//! First-launch wizard: hardware, model (with size and disk space), shortcut.

pub mod logic;

use crate::hub::ctx::{run_blocking, Ctx};
use crate::hub::widgets::{badge, button, card, label, set_label};
use crate::shortcut_recorder::ShortcutRecorderWidget;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use linuxflow_core::constants::{APP_NAME, VAD_MODEL_ID};
use linuxflow_core::domain::{AppState, Capabilities, ModelStatus};
use linuxflow_i18n::{tr, tr_fmt};
use logic::{disk_text, hardware_text, recommended, sorted, tag_for, Choices, Step};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Free space of the user data volume (XDG), if known.
pub fn free_disk_space() -> Option<u64> {
    let file = gio::File::for_path(glib::user_data_dir());
    let info = file
        .query_filesystem_info("filesystem::free", None::<&gio::Cancellable>)
        .ok()?;
    Some(info.attribute_uint64("filesystem::free"))
}

pub struct Wizard {
    root: gtk::Box,
    ctx: Ctx,
    stack: gtk::Stack,
    progress: gtk::Label,
    back: gtk::Button,
    next: gtk::Button,
    finish: gtk::Button,
    hardware: gtk::Label,
    models_box: gtk::Box,
    disk: gtk::Label,
    skip: gtk::CheckButton,
    summary_model: gtk::Label,
    summary_shortcut: gtk::Label,
    recorder: Rc<ShortcutRecorderWidget>,
    choices: RefCell<Choices>,
    models: RefCell<Vec<ModelStatus>>,
    shortcut: RefCell<String>,
    /// True as soon as the user has chosen a model: the recommendation no longer changes it.
    user_chose: Cell<bool>,
    /// Interface language selector (first step).
    language: gtk::DropDown,
    lang_updating: Cell<bool>,
    request_rebuild: Rc<dyn Fn()>,
    on_finished: Rc<dyn Fn()>,
}

fn page(title: &str, sub: &str) -> (gtk::Box, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 12);
    outer.append(&label(title, &["lf-h1"]));
    outer.append(&label(sub, &["lf-sub"]));
    let c = card();
    c.set_margin_top(8);
    outer.append(&c);
    (outer, c)
}

impl Wizard {
    pub fn new(ctx: &Ctx, on_finished: Rc<dyn Fn()>, request_rebuild: Rc<dyn Fn()>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        root.add_css_class("lf-wizard");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_width_request(620);
        let progress = label("", &["lf-hint"]);
        root.append(&progress);
        let stack = gtk::Stack::new();
        stack.set_vhomogeneous(false);

        // 1. Welcome + hardware
        let (p1, c1) = page(
            &tr_fmt("Welcome to {}", &[APP_NAME]),
            &tr(
                "Voice dictation that runs entirely on this computer. Let's set it up in a minute.",
            ),
        );
        c1.append(&label(&tr("Your computer"), &["lf-h2"]));
        let hardware = label(&tr("Detecting your hardware…"), &[]);
        c1.append(&hardware);
        // Interface language: chosen from the first step (before, only the system language
        // applied, with no way to change it here).
        c1.append(&label(&tr("Language"), &["lf-h2"]));
        let automatic = tr("Automatic");
        let language = gtk::DropDown::from_strings(&[automatic.as_str(), "English", "Français"]);
        language.set_halign(gtk::Align::Start);
        c1.append(&language);
        stack.add_named(&p1, Some(Step::Welcome.name()));

        // 2. Model
        let (p2, c2) = page(
            &tr("Choose a speech model"),
            &tr("The model is downloaded once and stays on your computer."),
        );
        let models_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        c2.append(&models_box);
        let disk = label("", &["lf-hint"]);
        c2.append(&disk);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_margin_top(12);
        let local = button(&tr("Use a file I already have…"), &["lf-btn", "ghost"]);
        row.append(&local);
        c2.append(&row);
        let skip = gtk::CheckButton::with_label(&tr(
            "Skip for now (dictation stays unavailable until a model is installed)",
        ));
        skip.set_margin_top(8);
        c2.append(&skip);
        stack.add_named(&p2, Some(Step::Model.name()));

        // 3. Raccourci
        let (p3, c3) = page(
            &tr("Choose your shortcut"),
            &tr("The key combination that starts and stops dictation, in any application."),
        );
        let recorder = ShortcutRecorderWidget::new(ctx);
        c3.append(recorder.widget());
        stack.add_named(&p3, Some(Step::Shortcut.name()));

        // 4. Summary
        let (p4, c4) = page(
            &tr("All set"),
            &tr("Here is what will happen when you finish."),
        );
        let summary_model = label("", &[]);
        let summary_shortcut = label("", &[]);
        c4.append(&summary_model);
        summary_shortcut.set_margin_top(6);
        c4.append(&summary_shortcut);
        stack.add_named(&p4, Some(Step::Summary.name()));
        root.append(&stack);

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let back = button(&tr("Back"), &["lf-btn", "ghost"]);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let next = button(&tr("Next"), &["lf-btn", "primary"]);
        let finish = button(&tr("Finish"), &["lf-btn", "primary"]);
        for w in [
            back.upcast_ref::<gtk::Widget>(),
            spacer.upcast_ref(),
            next.upcast_ref(),
            finish.upcast_ref(),
        ] {
            footer.append(w);
        }
        root.append(&footer);

        let this = Rc::new(Self {
            root,
            ctx: ctx.clone(),
            stack,
            progress,
            back,
            next,
            finish,
            hardware,
            models_box,
            disk,
            skip,
            summary_model,
            summary_shortcut,
            recorder,
            choices: RefCell::new(Choices::default()),
            models: Default::default(),
            shortcut: RefCell::new(String::new()),
            user_chose: Cell::new(false),
            language,
            lang_updating: Cell::new(false),
            request_rebuild,
            on_finished,
        });
        this.wire(&local);
        this.render();
        this
    }

    fn wire(self: &Rc<Self>, local: &gtk::Button) {
        let weak = Rc::downgrade(self);
        macro_rules! click {
            ($b:expr, $t:ident => $body:expr) => {{
                let weak = weak.clone();
                $b.connect_clicked(move |_| {
                    if let Some($t) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        click!(self.back, t => { t.choices.borrow_mut().go_back(); t.render(); });
        click!(self.next, t => { t.choices.borrow_mut().go_next(); t.render(); });
        click!(self.finish, t => t.finish());
        let weak_lang = weak.clone();
        self.language.connect_selected_notify(move |d| {
            let Some(t) = weak_lang.upgrade() else { return };
            if t.lang_updating.get() {
                return;
            }
            let setting = crate::hub::pages::settings::ui_language_setting(d.selected());
            if let Err(e) = t
                .ctx
                .controller
                .set_settings(serde_json::json!({"uiLanguage": setting}))
            {
                t.ctx.toast.show(&e.to_string());
                return;
            }
            linuxflow_i18n::set_language(if setting == "auto" {
                None
            } else {
                Some(setting)
            });
            (t.request_rebuild)();
        });
        let weak2 = weak.clone();
        self.skip.connect_toggled(move |c| {
            if let Some(t) = weak2.upgrade() {
                t.choices.borrow_mut().skip_model = c.is_active();
                t.render();
            }
        });
        local.connect_clicked(move |b| {
            if let Some(t) = weak.upgrade() {
                t.choose_local(b.upcast_ref());
            }
        });
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Moves to the next step (like the "Next" button).
    pub fn next_step(&self) {
        self.choices.borrow_mut().go_next();
        self.render();
    }

    pub fn choices(&self) -> Choices {
        self.choices.borrow().clone()
    }

    pub fn recorder(&self) -> &Rc<ShortcutRecorderWidget> {
        &self.recorder
    }

    /// Detects the hardware and loads the catalogue (off the GTK thread).
    pub fn load(self: &Rc<Self>) {
        let c = self.ctx.controller.clone();
        let weak = Rc::downgrade(self);
        run_blocking(
            move || (c.capabilities(), c.models()),
            move |(caps, models)| {
                if let Some(t) = weak.upgrade() {
                    t.set_data(&caps, models);
                }
            },
        );
    }

    pub fn set_data(self: &Rc<Self>, caps: &Capabilities, models: Vec<ModelStatus>) {
        set_label(&self.hardware, &hardware_text(caps));
        // Only transcription models can be chosen: the speech detector (885 KB) is a
        // helper downloaded with them.
        let models: Vec<ModelStatus> = models
            .into_iter()
            .filter(|m| m.id != VAD_MODEL_ID)
            .collect();
        {
            let mut ch = self.choices.borrow_mut();
            if !self.user_chose.get() {
                ch.model = recommended(&models).map(|m| m.id.clone());
            }
        }
        *self.models.borrow_mut() = models;
        self.rebuild_models();
        self.render();
    }

    pub fn update(&self, state: &AppState) {
        let wanted =
            crate::hub::pages::settings::ui_language_index(state.settings.ui_language.as_deref());
        if self.language.selected() != wanted {
            self.lang_updating.set(true);
            self.language.set_selected(wanted);
            self.lang_updating.set(false);
        }
        *self.shortcut.borrow_mut() = state.settings.shortcut.clone();
        self.recorder
            .set_desktop_trigger(state.shortcut_desktop_trigger.as_deref());
        self.recorder.set_failure(state.shortcut_message.as_deref());
        self.recorder
            .set_current(&state.settings.shortcut, state.shortcut_ok);
        self.render_summary();
    }

    fn rebuild_models(self: &Rc<Self>) {
        while let Some(c) = self.models_box.first_child() {
            self.models_box.remove(&c);
        }
        let models = self.models.borrow().clone();
        let rec = recommended(&models).cloned();
        let selected = self.choices.borrow().model.clone();
        let mut group: Option<gtk::CheckButton> = None;
        for m in sorted(&models) {
            let radio = gtk::CheckButton::new();
            if let Some(g) = &group {
                radio.set_group(Some(g));
            } else {
                group = Some(radio.clone());
            }
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
            text.append(&label(
                &crate::models_page::logic::display_name(&m),
                &["lf-radio-title"],
            ));
            text.append(&label(
                &crate::models_page::logic::sizes_text(&m),
                &["lf-radio-sub"],
            ));
            row.append(&text);
            if let Some(tag) = tag_for(&m, rec.as_ref()) {
                row.append(&badge(
                    &tag.label(),
                    if tag == logic::Tag::Recommended {
                        "recommended"
                    } else {
                        "plain"
                    },
                ));
            }
            radio.set_child(Some(&row));
            radio.set_active(selected.as_deref() == Some(m.id.as_str()));
            let (weak, id) = (Rc::downgrade(self), m.id.clone());
            radio.connect_toggled(move |r| {
                if let (true, Some(t)) = (r.is_active(), weak.upgrade()) {
                    t.user_chose.set(true);
                    t.choices.borrow_mut().model = Some(id.clone());
                    t.render();
                }
            });
            self.models_box.append(&radio);
        }
    }

    fn render(&self) {
        let ch = self.choices.borrow().clone();
        self.stack.set_visible_child_name(ch.step.name());
        set_label(
            &self.progress,
            &tr_fmt(
                "Step {} of {}",
                &[&(ch.step.index() + 1).to_string(), &Step::COUNT.to_string()],
            ),
        );
        self.back.set_visible(ch.step != Step::Welcome);
        self.next.set_visible(ch.step != Step::Summary);
        self.next.set_sensitive(ch.can_next());
        self.finish.set_visible(ch.step == Step::Summary);
        if let Some(m) = ch
            .model
            .as_ref()
            .and_then(|id| self.models.borrow().iter().find(|m| &m.id == id).cloned())
        {
            let (text, ok) = disk_text(m.disk_bytes, free_disk_space());
            set_label(&self.disk, &text);
            if ok {
                self.disk.remove_css_class("bad");
            } else {
                self.disk.add_css_class("bad");
            }
        }
        self.render_summary();
    }

    fn render_summary(&self) {
        let ch = self.choices.borrow().clone();
        let text = if ch.local_file {
            tr("The model file you selected will be used.")
        } else if let Some(id) = ch.model_for_finish() {
            let name = self
                .models
                .borrow()
                .iter()
                .find(|m| m.id == id)
                .map(crate::models_page::logic::display_name)
                .unwrap_or_else(|| id.to_string());
            tr_fmt("The model “{}” will be downloaded.", &[&name])
        } else {
            tr("No model will be downloaded now. You can install one later from the Models page.")
        };
        set_label(&self.summary_model, &text);
        let sc = crate::hub::labels::pretty_accelerator(&self.shortcut.borrow());
        set_label(&self.summary_shortcut, &tr_fmt("Your shortcut: {}", &[&sc]));
    }

    fn choose_local(self: &Rc<Self>, source: &gtk::Widget) {
        let window = source.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let dialog = gtk::FileDialog::builder()
            .title(tr("Choose a model file"))
            .build();
        let weak = Rc::downgrade(self);
        dialog.open(window.as_ref(), None::<&gio::Cancellable>, move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else {
                return;
            };
            let Some(t) = weak.upgrade() else { return };
            let controller = t.ctx.controller.clone();
            let weak = Rc::downgrade(&t);
            run_blocking(
                move || controller.model_use_local_file(&path),
                move |r| {
                    let Some(t) = weak.upgrade() else { return };
                    match r {
                        Ok(()) => {
                            t.choices.borrow_mut().local_file = true;
                            t.ctx.toast.show(&tr("Model file selected"));
                        }
                        Err(e) => t.ctx.toast.show(&e.to_string()),
                    }
                    t.render();
                },
            );
        });
    }

    fn finish(&self) {
        let ch = self.choices.borrow().clone();
        let shortcut = self.shortcut.borrow().clone();
        match self
            .ctx
            .controller
            .complete_first_run(&shortcut, ch.model_for_finish())
        {
            Ok(()) => (self.on_finished)(),
            Err(e) => self.ctx.toast.show(&e.to_string()),
        }
    }
}
