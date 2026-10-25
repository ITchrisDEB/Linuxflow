//! Home page: greeting, statistics, how-to and test area.

use crate::hub::ctx::Ctx;
use crate::hub::labels::{
    format_count, format_wpm, greeting, insert_separator, pretty_accelerator, try_status,
};
use crate::hub::widgets::{
    card, kbd_markup, label, markup_fmt, page_box, page_head, scrolled_page, set_accessible_label,
    set_label,
};
use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_core::domain::{AppState, Phase, ShortcutMode};
use linuxflow_i18n::tr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct HomePage {
    root: gtk::ScrolledWindow,
    ctx: Ctx,
    greeting: gtk::Label,
    today: gtk::Label,
    total: gtk::Label,
    wpm: gtk::Label,
    count: gtk::Label,
    step2: gtk::Label,
    step3: gtk::Label,
    paste_hint: gtk::Label,
    mic: gtk::Button,
    status: gtk::Label,
    view: gtk::TextView,
    placeholder: gtk::Label,
    ring: gtk::DrawingArea,
    pulse: adw::TimedAnimation,
    phase: Cell<Phase>,
    hub_dictation: Cell<bool>,
    last_howto: RefCell<(String, ShortcutMode, bool)>,
}

fn tile(class: &str, title: &str, unit: Option<&str>) -> (gtk::Box, gtk::Label) {
    let t = gtk::Box::new(gtk::Orientation::Vertical, 0);
    t.add_css_class("lf-tile");
    t.add_css_class(class);
    t.set_hexpand(true);
    t.append(&label(title, &["lf-tile-label"]));
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_size_request(-1, 26);
    t.append(&spacer);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.set_valign(gtk::Align::End);
    let value = gtk::Label::new(Some("0"));
    value.add_css_class("lf-tile-value");
    row.append(&value);
    if let Some(u) = unit {
        let u = gtk::Label::new(Some(u));
        u.add_css_class("lf-tile-unit");
        u.set_valign(gtk::Align::End);
        row.append(&u);
    }
    t.append(&row);
    (t, value)
}

fn step(n: u32, text: &gtk::Label) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let num = gtk::Label::new(Some(&n.to_string()));
    num.add_css_class("lf-step-num");
    num.set_valign(gtk::Align::Start);
    row.append(&num);
    text.set_hexpand(true);
    row.append(text);
    row
}

impl HomePage {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let page = page_box();
        let (head, greeting_label, _) = page_head(
            &greeting(12, None),
            &tr("Speak, and LinuxFlow writes. Everything happens on your machine: nothing leaves your computer."),
        );
        page.append(&head);

        let tiles = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        tiles.set_homogeneous(true);
        let (t1, today) = tile("lavender", &tr("Words today"), None);
        let (t2, total) = tile("cream", &tr("Total words"), None);
        let (t3, wpm) = tile("green", &tr("Average speed"), Some(&tr("words/min")));
        let (t4, count) = tile("grey", &tr("Dictations"), None);
        for t in [&t1, &t2, &t3, &t4] {
            tiles.append(t);
        }
        page.append(&tiles);

        let cards = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        cards.set_homogeneous(true);
        cards.set_valign(gtk::Align::Start);

        let how = card();
        how.append(&label(&tr("How to use"), &["lf-h2"]));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        let step1 = label(
            &tr("Put the cursor where you want to write (email, chat, editor…)."),
            &[],
        );
        let step2 = label("", &[]);
        step2.set_use_markup(true);
        let step3 = label("", &[]);
        list.append(&step(1, &step1));
        list.append(&step(2, &step2));
        list.append(&step(3, &step3));
        how.append(&list);
        let paste_hint = label("", &["lf-hint"]);
        how.append(&paste_hint);

        let try_card = card();
        try_card.append(&label(&tr("Try it here"), &["lf-h2"]));
        try_card.append(&label(
            &tr("Click the microphone, then speak. The text is inserted in the box below, without touching other windows."),
            &["lf-hint"],
        ));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        let ring = gtk::DrawingArea::new();
        ring.set_content_width(72);
        ring.set_content_height(72);
        ring.set_can_target(false);
        let mic = gtk::Button::from_icon_name("audio-input-microphone-symbolic");
        mic.add_css_class("lf-mic");
        mic.set_halign(gtk::Align::Center);
        mic.set_valign(gtk::Align::Center);
        mic.set_tooltip_text(Some(&tr("Dictate in this box")));
        set_accessible_label(&mic, &tr("Dictate in this box"));
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&ring));
        overlay.add_overlay(&mic);
        row.append(&overlay);
        let status = label(&try_status(Phase::Idle, false), &["lf-try-status"]);
        status.set_valign(gtk::Align::Center);
        status.set_wrap(false);
        row.append(&status);
        try_card.append(&row);

        let view = gtk::TextView::new();
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.add_css_class("lf-try-area");
        view.set_top_margin(12);
        view.set_bottom_margin(12);
        view.set_left_margin(14);
        view.set_right_margin(14);
        let placeholder = label(&tr("Dictated text will appear here…"), &["lf-placeholder"]);
        placeholder.set_can_target(false);
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_valign(gtk::Align::Start);
        placeholder.set_margin_top(12);
        placeholder.set_margin_start(14);
        let ov = gtk::Overlay::new();
        ov.set_child(Some(&view));
        ov.add_overlay(&placeholder);
        let frame = gtk::ScrolledWindow::new();
        frame.set_min_content_height(150);
        frame.set_child(Some(&ov));
        let outline = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outline.add_css_class("lf-try-frame");
        outline.set_overflow(gtk::Overflow::Hidden);
        outline.append(&frame);
        try_card.append(&outline);
        view.buffer().connect_changed({
            let placeholder = placeholder.clone();
            move |b| placeholder.set_visible(b.char_count() == 0)
        });

        cards.append(&how);
        cards.append(&try_card);
        page.append(&cards);

        // Pulsing ring while listening (Rust animation: animated CSS is not reliable).
        let progress = Rc::new(Cell::new(-1.0_f64));
        ring.set_draw_func({
            let progress = progress.clone();
            move |_, cr, w, h| {
                let p = progress.get();
                if p < 0.0 {
                    return;
                }
                let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
                let radius = 24.0 + 12.0 * p;
                cr.set_source_rgba(165.0 / 255.0, 80.0 / 255.0, 167.0 / 255.0, 0.45 * (1.0 - p));
                cr.set_line_width(4.0);
                cr.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
            }
        });
        let target = adw::CallbackAnimationTarget::new({
            let progress = progress.clone();
            let ring = ring.clone();
            move |v| {
                progress.set(v);
                ring.queue_draw();
            }
        });
        let pulse = adw::TimedAnimation::builder()
            .widget(&ring)
            .value_from(0.0)
            .value_to(1.0)
            .duration(1200)
            .repeat_count(0)
            .target(&target)
            .build();

        let this = Rc::new(Self {
            root: scrolled_page(&page),
            ctx: ctx.clone(),
            greeting: greeting_label,
            today,
            total,
            wpm,
            count,
            step2,
            step3,
            paste_hint,
            mic: mic.clone(),
            status,
            view,
            placeholder,
            ring,
            pulse,
            phase: Cell::new(Phase::Idle),
            hub_dictation: Cell::new(false),
            last_howto: RefCell::new((String::new(), ShortcutMode::Toggle, true)),
        });
        let weak = Rc::downgrade(&this);
        mic.connect_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                t.mic_clicked();
            }
        });
        this.render_howto("Ctrl+Alt+Space", None, ShortcutMode::Toggle, true);
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    fn mic_clicked(&self) {
        if self.phase.get() == Phase::Idle {
            self.hub_dictation.set(true);
            self.view.grab_focus();
        }
        let phase = self.ctx.controller.dictation_toggle(true);
        self.set_phase(phase);
    }

    /// Current dictation phase (updates the microphone button and the status).
    pub fn set_phase(&self, phase: Phase) {
        self.phase.set(phase);
        if phase == Phase::Idle {
            self.hub_dictation.set(false);
        }
        let mine = self.hub_dictation.get();
        let listening = phase == Phase::Listening && mine;
        let busy = matches!(phase, Phase::Transcribing | Phase::Formatting) && mine;
        for (class, on) in [("listening", listening), ("busy", busy)] {
            if on {
                self.mic.add_css_class(class);
            } else {
                self.mic.remove_css_class(class);
            }
        }
        if listening {
            self.pulse.play();
        } else {
            self.pulse.reset();
            self.ring.queue_draw();
        }
        set_label(&self.status, &try_status(phase, mine));
    }

    pub fn phase(&self) -> Phase {
        self.phase.get()
    }

    pub fn update(&self, state: &AppState) {
        let hour = glib::DateTime::now_local()
            .map(|d| d.hour() as u32)
            .unwrap_or(12);
        set_label(&self.greeting, &greeting(hour, state.user.as_deref()));
        set_label(
            &self.today,
            &format_count(u64::from(state.stats.words_today)),
        );
        set_label(
            &self.total,
            &format_count(u64::from(state.stats.total_words)),
        );
        set_label(&self.wpm, &format_wpm(state.stats.avg_wpm));
        set_label(
            &self.count,
            &format_count(u64::from(state.stats.dictations)),
        );
        self.render_howto(
            &state.settings.shortcut,
            state.shortcut_desktop_trigger.as_deref(),
            state.settings.mode,
            state.settings.auto_paste,
        );
        self.set_phase(state.phase);
    }

    /// `desktop`: key actually assigned by the desktop (portal), shown in place of the
    /// setting when it exists.
    fn render_howto(
        &self,
        shortcut: &str,
        desktop: Option<&str>,
        mode: ShortcutMode,
        auto_paste: bool,
    ) {
        let shown = desktop.map_or_else(|| pretty_accelerator(shortcut), str::to_string);
        let key = (shown.clone(), mode, auto_paste);
        if *self.last_howto.borrow() == key {
            return;
        }
        let kbd = kbd_markup(&shown);
        let (s2, s3) = match mode {
            ShortcutMode::Toggle => (
                markup_fmt(&tr("Press {}: the pill appears at the bottom of the screen and starts listening."), &[&kbd]),
                tr("Press it again to stop: the text is transcribed, formatted and pasted."),
            ),
            ShortcutMode::Ptt => (
                markup_fmt(&tr("Press and hold {}: the pill appears at the bottom of the screen and starts listening."), &[&kbd]),
                tr("Release the key to stop: the text is transcribed, formatted and pasted."),
            ),
        };
        self.step2.set_markup(&s2);
        set_label(&self.step3, &s3);
        set_label(
            &self.paste_hint,
            &if auto_paste {
                tr("Automatic pasting is on. The clipboard is restored right after.")
            } else {
                tr("Automatic pasting is off: the text is copied to the clipboard, paste it with Ctrl+V.")
            },
        );
        *self.last_howto.borrow_mut() = key;
    }

    /// Inserts a dictated text at the cursor position (a space is added if needed).
    pub fn insert_text(&self, text: &str) {
        let buffer = self.view.buffer();
        buffer.delete_selection(true, true);
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let before = buffer.text(&buffer.start_iter(), &cursor, false);
        let sep = insert_separator(&before);
        buffer.insert_at_cursor(&format!("{sep}{text}"));
        self.view.grab_focus();
    }

    pub fn try_text(&self) -> String {
        let b = self.view.buffer();
        b.text(&b.start_iter(), &b.end_iter(), false).to_string()
    }

    pub fn set_try_text(&self, text: &str) {
        self.view.buffer().set_text(text);
    }

    pub fn placeholder_visible(&self) -> bool {
        self.placeholder.is_visible()
    }

    pub fn mic_button(&self) -> &gtk::Button {
        &self.mic
    }
}
