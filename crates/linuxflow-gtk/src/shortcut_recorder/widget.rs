//! Shortcut recorder widget: key area, Configure/Confirm/Cancel buttons and help.

use super::logic::{
    press_from_key_name, qwerty_from_evdev, Modifier, Press, PressOutcome, Recorder, RecorderView,
};
use crate::hub::ctx::Ctx;
use crate::hub::labels::pretty_accelerator;
use crate::hub::widgets::{button, kbd_chip, label, set_label};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use linuxflow_i18n::{tr, tr_fmt};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

pub struct ShortcutRecorderWidget {
    root: gtk::Box,
    keys: gtk::Box,
    record: gtk::Button,
    rec_dot: gtk::Label,
    rec_text: gtk::Label,
    confirm: gtk::Button,
    cancel: gtk::Button,
    hint: gtk::Label,
    ctx: Ctx,
    recorder: RefCell<Recorder>,
    current: RefCell<String>,
    ok: Cell<bool>,
    last_view: RefCell<Option<RecorderView>>,
    attached: Cell<bool>,
}

fn mods_from_gdk(state: gdk::ModifierType) -> BTreeSet<Modifier> {
    let mut set = BTreeSet::new();
    for (mask, m) in [
        (gdk::ModifierType::CONTROL_MASK, Modifier::Ctrl),
        (gdk::ModifierType::ALT_MASK, Modifier::Alt),
        (gdk::ModifierType::SHIFT_MASK, Modifier::Shift),
        (gdk::ModifierType::SUPER_MASK, Modifier::Super),
    ] {
        if state.contains(mask) {
            set.insert(m);
        }
    }
    set
}

impl ShortcutRecorderWidget {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        let keys = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        keys.add_css_class("lf-shortcut-keys");
        let rec_dot = gtk::Label::new(Some("●"));
        rec_dot.add_css_class("lf-rec-dot");
        let rec_text = gtk::Label::new(Some(&tr("Configure")));
        let rec_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        rec_box.append(&rec_dot);
        rec_box.append(&rec_text);
        let record = gtk::Button::new();
        record.set_child(Some(&rec_box));
        record.add_css_class("lf-btn");
        let confirm = button(&tr("Confirm"), &["lf-btn", "primary"]);
        let cancel = button(&tr("Cancel"), &["lf-btn", "ghost"]);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&record);
        row.append(&confirm);
        row.append(&cancel);
        let hint = label("", &["lf-hint"]);
        root.append(&keys);
        root.append(&row);
        root.append(&hint);

        let this = Rc::new(Self {
            root,
            keys,
            record,
            rec_dot,
            rec_text,
            confirm,
            cancel,
            hint,
            ctx: ctx.clone(),
            recorder: RefCell::new(Recorder::new()),
            current: RefCell::new(String::new()),
            ok: Cell::new(true),
            last_view: RefCell::new(None),
            attached: Cell::new(false),
        });
        this.wire();
        this.refresh();
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    fn wire(self: &Rc<Self>) {
        let me = Rc::downgrade(self);
        self.record.connect_clicked({
            let me = me.clone();
            move |_| {
                if let Some(t) = me.upgrade() {
                    let recording = t.recorder.borrow().is_recording();
                    if recording {
                        t.stop_recording()
                    } else {
                        t.start_recording()
                    }
                }
            }
        });
        self.cancel.connect_clicked({
            let me = me.clone();
            move |_| {
                if let Some(t) = me.upgrade() {
                    t.cancel();
                }
            }
        });
        self.confirm.connect_clicked({
            let me = me.clone();
            move |_| {
                if let Some(t) = me.upgrade() {
                    t.confirm();
                }
            }
        });
        // Key capture at the window level, before any button or field.
        self.root.connect_root_notify({
            let me = me.clone();
            move |w| {
                let Some(t) = me.upgrade() else { return };
                let Some(root) = w.root() else { return };
                if t.attached.replace(true) {
                    return;
                }
                let ctrl = gtk::EventControllerKey::new();
                ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
                ctrl.connect_key_pressed({
                    let me = me.clone();
                    move |c, keyval, keycode, state| {
                        let Some(t) = me.upgrade() else {
                            return glib::Propagation::Proceed;
                        };
                        if !t.recorder.borrow().is_recording() {
                            return glib::Propagation::Proceed;
                        }
                        let display = c.widget().map(|w| w.display());
                        let base = display
                            .and_then(|d| d.translate_key(keycode, gdk::ModifierType::empty(), 0))
                            .map(|k| k.0)
                            .unwrap_or(keyval);
                        let press = Self::classify(base, keycode);
                        t.key_pressed(keycode, press, &mods_from_gdk(state));
                        glib::Propagation::Stop
                    }
                });
                ctrl.connect_key_released({
                    let me = me.clone();
                    move |_, _, keycode, _| {
                        if let Some(t) = me.upgrade() {
                            t.key_released(keycode);
                        }
                    }
                });
                root.upcast_ref::<gtk::Widget>().add_controller(ctrl);
                if let Some(win) = root.dynamic_cast_ref::<gtk::Window>() {
                    let me = me.clone();
                    win.connect_is_active_notify(move |w| {
                        if !w.is_active() {
                            if let Some(t) = me.upgrade() {
                                t.recorder.borrow_mut().blur();
                            }
                        }
                    });
                }
            }
        });
    }

    /// Classifies the key: falls back on the physical position if the layout is not Latin.
    fn classify(base: gdk::Key, keycode: u32) -> Press {
        let name = base.name().map(|n| n.to_string()).unwrap_or_default();
        let press = press_from_key_name(&name, base.to_unicode());
        if let Press::Unsupported(_) = press {
            if base.to_unicode().is_some_and(|c| c.is_alphabetic()) {
                if let Some(c) = qwerty_from_evdev(keycode.saturating_sub(8)) {
                    return Press::Key(c.to_string());
                }
            }
        }
        press
    }

    /// Processes a pressed key (called by the keyboard controller; public for the tests).
    pub fn key_pressed(&self, id: u32, press: Press, active: &BTreeSet<Modifier>) -> bool {
        let outcome = self.recorder.borrow_mut().press(id, press, active);
        if outcome == PressOutcome::Updated {
            self.refresh();
        }
        outcome == PressOutcome::Updated
    }

    pub fn key_released(&self, id: u32) {
        self.recorder.borrow_mut().release(id);
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.borrow().is_recording()
    }

    pub fn pending(&self) -> Option<String> {
        self.recorder.borrow().pending().map(str::to_string)
    }

    pub fn view(&self) -> RecorderView {
        self.recorder
            .borrow()
            .view(&self.current.borrow(), self.ok.get())
    }

    pub fn start_recording(&self) {
        self.recorder.borrow_mut().start();
        self.ctx.controller.shortcut_suspend(true);
        self.refresh();
    }

    pub fn stop_recording(&self) {
        let was = self.recorder.borrow().is_recording();
        self.recorder.borrow_mut().stop();
        if was {
            self.ctx.controller.shortcut_suspend(false);
        }
        self.refresh();
    }

    /// Cancels listening and the pending shortcut (also called when the hub is hidden).
    pub fn cancel(&self) {
        let was = self.recorder.borrow().is_recording();
        self.recorder.borrow_mut().discard();
        if was {
            self.ctx.controller.shortcut_suspend(false);
        }
        self.refresh();
    }

    pub fn confirm(&self) {
        if self.is_recording() {
            self.stop_recording();
        }
        let Some(acc) = self.pending() else { return };
        if let Err(msg) = self.ctx.controller.shortcut_test(&acc) {
            let msg = if msg.trim().is_empty() {
                tr_fmt("“{}” is not available.", &[&pretty_accelerator(&acc)])
            } else {
                msg
            };
            self.recorder.borrow_mut().set_error(Some(msg));
            self.refresh();
            return;
        }
        match self
            .ctx
            .controller
            .set_settings(serde_json::json!({ "shortcut": acc }))
        {
            Ok(()) => {
                self.recorder.borrow_mut().committed();
                self.ctx
                    .toast
                    .show(&tr_fmt("Shortcut: {}", &[&pretty_accelerator(&acc)]));
            }
            Err(e) => self.recorder.borrow_mut().set_error(Some(e.to_string())),
        }
        self.refresh();
    }

    /// Precise reason for the shortcut failure (named conflict).
    pub fn set_failure(&self, message: Option<&str>) {
        let message = message.map(str::to_string);
        if self.recorder.borrow().failure() != message.as_deref() {
            self.recorder.borrow_mut().set_failure(message);
            self.refresh();
        }
    }

    /// Key assigned by the desktop when it chooses itself (shortcuts portal).
    pub fn set_desktop_trigger(&self, trigger: Option<&str>) {
        let trigger = trigger.map(str::to_string);
        if self.recorder.borrow().desktop_trigger() != trigger.as_deref() {
            self.recorder.borrow_mut().set_desktop_trigger(trigger);
            self.refresh();
        }
    }

    /// Updates the set shortcut and its state (called at every `Event::State`).
    pub fn set_current(&self, shortcut: &str, ok: bool) {
        let changed = *self.current.borrow() != shortcut || self.ok.get() != ok;
        *self.current.borrow_mut() = shortcut.to_string();
        self.ok.set(ok);
        if changed {
            self.refresh();
        }
    }

    fn refresh(&self) {
        let view = self.view();
        if self.last_view.borrow().as_ref() == Some(&view) {
            return;
        }
        while let Some(c) = self.keys.first_child() {
            self.keys.remove(&c);
        }
        for s in ["recording", "pending", "bad"] {
            self.keys.remove_css_class(s);
        }
        if let Some(c) = view.style.css_class() {
            self.keys.add_css_class(c);
        }
        if view.keys.is_empty() {
            self.keys
                .append(&label(&view.placeholder, &["lf-placeholder"]));
        }
        for (i, k) in view.keys.iter().enumerate() {
            if i > 0 {
                self.keys.append(&label("+", &["lf-plus"]));
            }
            self.keys.append(&kbd_chip(k));
        }
        set_label(&self.hint, &view.hint);
        if view.hint_bad {
            self.hint.add_css_class("bad");
        } else {
            self.hint.remove_css_class("bad");
        }
        self.rec_dot.set_visible(view.recording);
        set_label(
            &self.rec_text,
            &if view.recording {
                tr("Stop")
            } else {
                tr("Configure")
            },
        );
        if view.recording {
            self.record.add_css_class("recording");
        } else {
            self.record.remove_css_class("recording");
        }
        self.confirm.set_sensitive(view.confirm_enabled);
        self.cancel.set_visible(view.cancel_visible);
        *self.last_view.borrow_mut() = Some(view);
    }
}
