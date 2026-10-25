//! Dictionary page: add/edit form, live preview, list of words.

use crate::hub::ctx::Ctx;
use crate::hub::keyed::KeyedBox;
use crate::hub::labels::word_count;
use crate::hub::widgets::{button, card, label, page_box, page_head, scrolled_page, set_label};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use linuxflow_core::domain::{AppState, DictionaryEntry};
use linuxflow_i18n::tr;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// Grouping delay of the typing before the preview.
pub const PREVIEW_DEBOUNCE_MS: u64 = 120;

/// Form mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormMode {
    Add,
    /// Edit: original word.
    Edit(String),
}

impl FormMode {
    pub fn submit_label(&self) -> String {
        match self {
            FormMode::Add => tr("Add"),
            FormMode::Edit(_) => tr("Save"),
        }
    }
}

/// Validates the word of the form: non-empty after `trim`.
pub fn validate_word(word: &str) -> Option<String> {
    let w = word.trim();
    (!w.is_empty()).then(|| w.to_string())
}

/// Content of the form and of the test field (kept when the interface is rebuilt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormValues {
    pub word: String,
    pub sounds: String,
    pub test: String,
    pub mode: FormMode,
}

impl Default for FormValues {
    fn default() -> Self {
        Self {
            word: String::new(),
            sounds: String::new(),
            test: String::new(),
            mode: FormMode::Add,
        }
    }
}

pub struct DictionaryPage {
    root: gtk::ScrolledWindow,
    ctx: Ctx,
    word: gtk::Entry,
    sounds: gtk::Entry,
    submit: gtk::Button,
    cancel: gtk::Button,
    test: gtk::Entry,
    test_out: gtk::Label,
    count: gtk::Label,
    list: KeyedBox,
    empty: gtk::Label,
    mode: RefCell<FormMode>,
    timer: RefCell<Option<glib::SourceId>>,
    entries: RefCell<Vec<DictionaryEntry>>,
}

impl DictionaryPage {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let page = page_box();
        let (head, _, _) = page_head(
            &tr("Dictionary"),
            &tr("The correct word, and what the model hears instead: “cooper nettys” becomes “Kubernetes”. Capitals, accents, hyphens and “eight” / “8” are handled automatically."),
        );
        page.append(&head);

        let form = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let word = gtk::Entry::new();
        word.set_placeholder_text(Some(&tr("Correct word (e.g. Kubernetes)")));
        word.set_width_chars(26);
        let sounds = gtk::Entry::new();
        sounds.set_placeholder_text(Some(&tr(
            "What the model hears, comma-separated (e.g. cooper nettys, kuber neties)",
        )));
        sounds.set_hexpand(true);
        let submit = button(&FormMode::Add.submit_label(), &["lf-btn", "primary"]);
        let cancel = button(&tr("Cancel"), &["lf-btn", "ghost"]);
        cancel.set_visible(false);
        for w in [
            word.upcast_ref::<gtk::Widget>(),
            sounds.upcast_ref(),
            submit.upcast_ref(),
            cancel.upcast_ref(),
        ] {
            form.append(w);
        }
        page.append(&form);

        let test_card = card();
        test_card.append(&label(&tr("Test"), &["lf-field-label"]));
        let test = gtk::Entry::new();
        test.set_placeholder_text(Some(&tr(
            "Type a sentence the way the model would hear it: I deploy with cooper nettys",
        )));
        test_card.append(&test);
        let test_out = label(
            &tr("The corrected result appears here."),
            &["lf-test-out", "muted"],
        );
        test_out.set_margin_top(10);
        test_card.append(&test_out);
        page.append(&test_card);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let count = label("", &["lf-hint"]);
        count.set_hexpand(true);
        let restore = button(&tr("Restore default words"), &["lf-btn", "small", "ghost"]);
        bar.append(&count);
        bar.append(&restore);
        page.append(&bar);
        let list = KeyedBox::new(10);
        page.append(list.widget());
        let empty = label(
            &tr("No words yet. Add the names the model gets wrong."),
            &["lf-empty"],
        );
        page.append(&empty);

        let this = Rc::new(Self {
            root: scrolled_page(&page),
            ctx: ctx.clone(),
            word,
            sounds,
            submit,
            cancel,
            test,
            test_out,
            count,
            list,
            empty,
            mode: RefCell::new(FormMode::Add),
            timer: RefCell::new(None),
            entries: Default::default(),
        });
        this.wire(&restore);
        this
    }

    fn wire(self: &Rc<Self>, restore: &gtk::Button) {
        let weak = Rc::downgrade(self);
        let submit = {
            let weak = weak.clone();
            move || {
                if let Some(t) = weak.upgrade() {
                    t.submit();
                }
            }
        };
        self.submit.connect_clicked({
            let s = submit.clone();
            move |_| s()
        });
        self.word.connect_activate({
            let s = submit.clone();
            move |_| s()
        });
        self.sounds.connect_activate(move |_| submit());
        self.cancel.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(t) = weak.upgrade() {
                    t.reset_form();
                }
            }
        });
        restore.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(t) = weak.upgrade() {
                    match t.ctx.controller.dictionary_restore_defaults() {
                        Ok(()) => t.ctx.toast.show(&tr("Default words added")),
                        Err(e) => t.ctx.toast.show(&e.to_string()),
                    }
                }
            }
        });
        self.test.connect_changed(move |_| {
            if let Some(t) = weak.upgrade() {
                t.schedule_preview();
            }
        });
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

    pub fn mode(&self) -> FormMode {
        self.mode.borrow().clone()
    }

    pub fn update(self: &Rc<Self>, state: &AppState) {
        *self.entries.borrow_mut() = state.dictionary.clone();
        let wanted: Vec<(String, String)> = state
            .dictionary
            .iter()
            .map(|d| {
                (
                    d.word.to_lowercase(),
                    format!("{}|{}", d.word, d.sounds.join("\u{1f}")),
                )
            })
            .collect();
        let entries = state.dictionary.clone();
        self.list
            .sync(&wanted, &mut |i| self.build_row(&entries[i]));
        self.empty.set_visible(entries.is_empty());
        set_label(&self.count, &word_count(entries.len() as u64));
        // The rules may have changed: the preview is recomputed.
        if !self.test.text().is_empty() {
            self.schedule_preview();
        }
    }

    fn build_row(self: &Rc<Self>, d: &DictionaryEntry) -> gtk::Widget {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("lf-item");
        let left = gtk::Box::new(gtk::Orientation::Vertical, 8);
        left.set_hexpand(true);
        left.append(&label(&d.word, &["lf-word"]));
        let chips = gtk::FlowBox::new();
        chips.set_selection_mode(gtk::SelectionMode::None);
        chips.set_max_children_per_line(30);
        chips.set_column_spacing(6);
        chips.set_row_spacing(6);
        chips.set_homogeneous(false);
        chips.set_halign(gtk::Align::Start);
        if d.sounds.is_empty() {
            chips.append(&label(&tr("case only"), &["lf-sound", "none"]));
        }
        for s in &d.sounds {
            chips.append(&label(s, &["lf-sound"]));
        }
        left.append(&chips);
        row.append(&left);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_valign(gtk::Align::Start);
        let edit = button(&tr("Edit"), &["lf-btn", "small"]);
        let delete = button(&tr("Delete"), &["lf-btn", "small", "danger"]);
        let (weak, entry) = (Rc::downgrade(self), d.clone());
        edit.connect_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                t.start_edit(&entry);
            }
        });
        let (ctx, word) = (self.ctx.clone(), d.word.clone());
        delete.connect_clicked(move |_| {
            if let Err(e) = ctx.controller.dictionary_remove(&word) {
                ctx.toast.show(&e.to_string());
            }
        });
        actions.append(&edit);
        actions.append(&delete);
        row.append(&actions);
        row.upcast()
    }

    pub fn form_values(&self) -> FormValues {
        FormValues {
            word: self.word.text().to_string(),
            sounds: self.sounds.text().to_string(),
            test: self.test.text().to_string(),
            mode: self.mode(),
        }
    }

    pub fn restore_form(&self, v: &FormValues) {
        self.word.set_text(&v.word);
        self.sounds.set_text(&v.sounds);
        self.test.set_text(&v.test);
        *self.mode.borrow_mut() = v.mode.clone();
        self.submit.set_label(&v.mode.submit_label());
        self.cancel.set_visible(matches!(v.mode, FormMode::Edit(_)));
    }

    pub fn start_edit(&self, d: &DictionaryEntry) {
        *self.mode.borrow_mut() = FormMode::Edit(d.word.clone());
        self.word.set_text(&d.word);
        self.sounds.set_text(&d.sounds.join(", "));
        self.submit.set_label(&self.mode().submit_label());
        self.cancel.set_visible(true);
        self.sounds.grab_focus();
    }

    pub fn reset_form(&self) {
        *self.mode.borrow_mut() = FormMode::Add;
        self.word.set_text("");
        self.sounds.set_text("");
        self.submit.set_label(&FormMode::Add.submit_label());
        self.cancel.set_visible(false);
    }

    /// Submits the form (addition or update).
    pub fn submit(&self) {
        let Some(word) = validate_word(&self.word.text()) else {
            self.word.grab_focus();
            return;
        };
        let sounds = self.sounds.text().to_string();
        let mode = self.mode();
        let result = match &mode {
            FormMode::Add => self.ctx.controller.dictionary_add(&word, &sounds),
            FormMode::Edit(previous) => self
                .ctx
                .controller
                .dictionary_update(previous, &word, &sounds),
        };
        match result {
            Ok(()) => {
                self.ctx.toast.show(&if mode == FormMode::Add {
                    tr("Word added")
                } else {
                    tr("Word updated")
                });
                self.reset_form();
            }
            Err(e) => self.ctx.toast.show(&e.to_string()),
        }
    }

    fn schedule_preview(self: &Rc<Self>) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id =
            glib::timeout_add_local_once(Duration::from_millis(PREVIEW_DEBOUNCE_MS), move || {
                if let Some(t) = weak.upgrade() {
                    t.timer.borrow_mut().take();
                    t.refresh_preview();
                }
            });
        *self.timer.borrow_mut() = Some(id);
    }

    /// Computes the preview right away (the delay is handled by `schedule_preview`).
    pub fn refresh_preview(&self) {
        let text = self.test.text().to_string();
        if text.trim().is_empty() {
            set_label(&self.test_out, &tr("The corrected result appears here."));
            self.test_out.add_css_class("muted");
            return;
        }
        self.test_out.remove_css_class("muted");
        set_label(&self.test_out, &self.ctx.controller.format_preview(&text));
    }

    pub fn set_test_text(&self, text: &str) {
        self.test.set_text(text);
    }

    pub fn preview_text(&self) -> String {
        self.test_out.text().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_validation() {
        assert_eq!(validate_word("  Docker "), Some("Docker".to_string()));
        assert_eq!(validate_word("   "), None);
        assert_eq!(validate_word(""), None);
    }

    #[test]
    fn form_mode_labels() {
        assert_eq!(FormMode::Add.submit_label(), "Add");
        assert_eq!(FormMode::Edit("x".into()).submit_label(), "Save");
    }
}
