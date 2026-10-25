//! `PillWidget`: GTK widget that draws the pill (capsule, bars, check mark, label).
//!
//! The drawing goes through `snapshot()`; the animations are driven by `add_tick_callback` and
//! computed by `PillState` (pure logic). No CSS file: everything is drawn.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::graphene::{Point, Rect, Size};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gsk, pango};
use gtk4 as gtk;
use linuxflow_core::domain::Phase;
use linuxflow_i18n::{tr, tr_fmt};

use crate::overlay::geometry::{
    bar_radius, bars_width, pill_rect, pill_width, BAR_GAP, BAR_W, CHECK_SIZE, GAP, LABEL_PX,
    PAD_LEFT, PILL_H, SURFACE_H, SURFACE_W,
};
use crate::overlay::msg::OverlayMsg;
use crate::overlay::state::{PillState, PillVisual};

const BG: (f32, f32, f32) = (26.0 / 255.0, 26.0 / 255.0, 26.0 / 255.0);
const CREAM: (f32, f32, f32) = (1.0, 1.0, 235.0 / 255.0);
const RED: (f32, f32, f32) = (248.0 / 255.0, 113.0 / 255.0, 113.0 / 255.0);
/// Fonts tried in order (Figtree if installed, otherwise the system font).
const FONT_FAMILIES: &str = "Figtree, Inter, Cantarell, Noto Sans, sans-serif";

fn rgba(c: (f32, f32, f32), a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.0, c.1, c.2, a)
}

/// Label shown for a phase (English source texts, translated through `linuxflow_i18n`).
pub fn label_for(phase: Phase, message: Option<&str>, latency_ms: Option<u64>) -> Option<String> {
    match phase {
        Phase::Idle => None,
        Phase::Listening => Some(tr("Listening")),
        Phase::Transcribing => Some(tr("Transcribing")),
        Phase::Formatting => Some(tr("Formatting")),
        Phase::Done => Some(match latency_ms {
            Some(ms) if ms > 0 => tr_fmt("Done · {} s", &[&format!("{:.1}", ms as f64 / 1000.0)]),
            _ => tr("Done"),
        }),
        Phase::Error => Some(match message {
            Some(m) if !m.trim().is_empty() => m.to_string(),
            _ => tr("Error"),
        }),
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PillWidget {
        pub state: RefCell<PillState>,
        pub label: RefCell<String>,
        pub opaque: Cell<bool>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub last_us: Cell<Option<i64>>,
        pub on_hidden: RefCell<Option<Box<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PillWidget {
        const NAME: &'static str = "LinuxflowPillWidget";
        type Type = super::PillWidget;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PillWidget {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_can_target(false);
            obj.set_can_focus(false);
            obj.set_focusable(false);
            obj.set_overflow(gtk::Overflow::Visible);
        }

        fn dispose(&self) {
            if let Some(id) = self.tick.take() {
                id.remove();
            }
        }
    }

    impl WidgetImpl for PillWidget {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let size = match orientation {
                gtk::Orientation::Horizontal => SURFACE_W,
                _ if self.opaque.get() => PILL_H as i32,
                _ => SURFACE_H,
            };
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            obj.paint(snapshot, f64::from(obj.width()), f64::from(obj.height()));
        }
    }
}

glib::wrapper! {
    pub struct PillWidget(ObjectSubclass<imp::PillWidget>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PillWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl PillWidget {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Mode without a compositor: the pill fills the whole surface, without shadow or transparency.
    pub fn set_opaque(&self, opaque: bool) {
        self.imp().opaque.set(opaque);
        self.queue_resize();
        self.queue_draw();
    }

    /// Called back once when the disappearance animation is finished.
    pub fn connect_hidden(&self, f: impl Fn() + 'static) {
        self.imp().on_hidden.replace(Some(Box::new(f)));
    }

    pub fn label_text(&self) -> String {
        self.imp().label.borrow().clone()
    }

    pub fn visual(&self) -> PillVisual {
        self.imp().state.borrow().visual()
    }

    /// Applies a control message.
    pub fn apply(&self, msg: &OverlayMsg) {
        if let Some(level) = msg.level {
            self.push_level(level);
        }
        match msg.phase {
            Some(phase) => self.set_phase(phase, msg.message.as_deref(), msg.latency_ms),
            None if msg.message.is_some() || msg.latency_ms.is_some() => {
                let phase = self.imp().state.borrow().phase();
                if let Some(text) = label_for(phase, msg.message.as_deref(), msg.latency_ms) {
                    self.imp().label.replace(text);
                    self.queue_draw();
                }
            }
            None => {}
        }
        match msg.visible {
            Some(true) => self.reveal(),
            Some(false) => self.dismiss(),
            None => {}
        }
    }

    pub fn set_phase(&self, phase: Phase, message: Option<&str>, latency_ms: Option<u64>) {
        let imp = self.imp();
        if let Some(text) = label_for(phase, message, latency_ms) {
            imp.label.replace(text);
        }
        imp.state.borrow_mut().set_phase(phase);
        self.ensure_ticking();
        self.queue_draw();
    }

    pub fn push_level(&self, level: f32) {
        self.imp().state.borrow_mut().push_level(level);
        self.ensure_ticking();
    }

    /// Shows the pill (appearance animation).
    pub fn reveal(&self) {
        let imp = self.imp();
        if imp.label.borrow().is_empty() {
            imp.label.replace(tr("Listening"));
        }
        imp.state.borrow_mut().show();
        self.ensure_ticking();
        self.queue_draw();
    }

    /// Hides the pill (disappearance animation, then `connect_hidden`).
    pub fn dismiss(&self) {
        self.imp().state.borrow_mut().hide();
        self.ensure_ticking();
        self.queue_draw();
    }

    /// Advances the animations by hand (offscreen rendering, tests).
    pub fn advance(&self, dt_ms: f64) {
        self.imp().state.borrow_mut().tick(dt_ms);
    }

    fn ensure_ticking(&self) {
        let imp = self.imp();
        if imp.tick.borrow().is_some() || !imp.state.borrow().is_animating() {
            return;
        }
        let id = self.add_tick_callback(|widget, clock| {
            let imp = widget.imp();
            let now = clock.frame_time();
            let dt_ms = imp
                .last_us
                .get()
                .map_or(0.0, |prev| (now - prev) as f64 / 1000.0);
            imp.last_us.set(Some(now));
            let (hidden, animating) = {
                let mut state = imp.state.borrow_mut();
                state.tick(dt_ms);
                (state.take_hidden(), state.is_animating())
            };
            widget.queue_draw();
            if hidden {
                if let Some(cb) = imp.on_hidden.borrow().as_ref() {
                    cb();
                }
            }
            if animating {
                glib::ControlFlow::Continue
            } else {
                imp.tick.replace(None);
                imp.last_us.set(None);
                glib::ControlFlow::Break
            }
        });
        imp.tick.replace(Some(id));
    }

    /// Draws the pill in a `width × height` surface (usable offscreen).
    pub fn paint(&self, snapshot: &gtk::Snapshot, width: f64, height: f64) {
        let imp = self.imp();
        let mut visual = imp.state.borrow().visual();
        let opaque = imp.opaque.get();
        if opaque {
            // Without a compositor: neither slide nor fade, visible as long as the state is.
            visual.slide = 0.0;
            visual.opacity = if visual.opacity > 0.0 { 1.0 } else { 0.0 };
        }
        let text = imp.label.borrow().clone();
        paint_pill(
            self.upcast_ref(),
            snapshot,
            &visual,
            &text,
            width,
            height,
            opaque,
        );
    }
}

fn label_layout(widget: &gtk::Widget, text: &str) -> pango::Layout {
    let layout = widget.create_pango_layout(Some(text));
    let mut font = pango::FontDescription::new();
    font.set_family(FONT_FAMILIES);
    font.set_weight(pango::Weight::Semibold);
    font.set_absolute_size(LABEL_PX * f64::from(pango::SCALE));
    layout.set_font_description(Some(&font));
    // letter-spacing: 0.01em
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrInt::new_letter_spacing(
        (LABEL_PX * 0.01 * f64::from(pango::SCALE)).round() as i32,
    ));
    layout.set_attributes(Some(&attrs));
    layout
}

fn pill_outline(x: f64, y: f64, w: f64, h: f64) -> gsk::RoundedRect {
    let r = Size::new((h / 2.0) as f32, (h / 2.0) as f32);
    gsk::RoundedRect::new(
        Rect::new(x as f32, y as f32, w as f32, h as f32),
        r,
        r,
        r,
        r,
    )
}

/// The drawing proper (separated from the widget to stay readable).
fn paint_pill(
    widget: &gtk::Widget,
    snapshot: &gtk::Snapshot,
    v: &PillVisual,
    text: &str,
    width: f64,
    height: f64,
    opaque: bool,
) {
    if v.opacity <= 0.0 {
        return;
    }
    let layout = label_layout(widget, text);
    let (lw_pango, lh_pango) = layout.size();
    let label_w = f64::from(lw_pango) / f64::from(pango::SCALE);
    let label_h = f64::from(lh_pango) / f64::from(pango::SCALE);
    let lead_w = if v.check.is_some() {
        CHECK_SIZE
    } else {
        bars_width()
    };
    let natural_w = pill_width(lead_w, label_w);
    let (px, py, pw, ph) = if opaque {
        (0.0, 0.0, width, PILL_H)
    } else {
        pill_rect(width, height, natural_w)
    };
    // Origin of the content (the opaque capsule fills the whole width: centred content).
    let content_x = px + (pw - natural_w) / 2.0 + PAD_LEFT;
    let mid_y = py + PILL_H / 2.0;

    snapshot.save();
    snapshot.translate(&Point::new(0.0, v.slide as f32));
    snapshot.push_opacity(v.opacity);

    let outline = pill_outline(px, py, pw, ph);
    if !opaque {
        snapshot.append_outset_shadow(&outline, &rgba((0.0, 0.0, 0.0), 0.35), 0.0, 10.0, 0.0, 30.0);
    }
    snapshot.push_rounded_clip(&outline);
    snapshot.append_color(&rgba(BG, 1.0), outline.bounds());
    snapshot.pop();
    // Inner border: 1 px cream at 6 %; 1.5 px red outline on error.
    let (bw, bc) = if v.error {
        (1.5, rgba(RED, 1.0))
    } else {
        (1.0, rgba(CREAM, 0.06))
    };
    snapshot.append_border(&outline, &[bw; 4], &[bc, bc, bc, bc]);

    // Barres.
    let bar_color = if v.error { RED } else { CREAM };
    for (i, bar) in v.bars.iter().enumerate() {
        let x = content_x + i as f64 * (BAR_W + BAR_GAP);
        let y = mid_y - bar.height / 2.0;
        let rect = Rect::new(x as f32, y as f32, BAR_W as f32, bar.height as f32);
        let r = bar_radius(bar.height) as f32;
        let s = Size::new(r, r);
        let rr = gsk::RoundedRect::new(rect, s, s, s, s);
        snapshot.push_rounded_clip(&rr);
        snapshot.append_color(&rgba(bar_color, bar.opacity as f32), &rect);
        snapshot.pop();
    }

    // Check mark: cream disc, "pop" animation from its centre.
    if let Some(check) = v.check {
        let cx = content_x + CHECK_SIZE / 2.0;
        snapshot.save();
        snapshot.translate(&Point::new(cx as f32, mid_y as f32));
        snapshot.scale(check.scale as f32, check.scale as f32);
        snapshot.translate(&Point::new(
            -(CHECK_SIZE / 2.0) as f32,
            -(CHECK_SIZE / 2.0) as f32,
        ));
        snapshot.push_opacity(check.opacity);
        let disc = Rect::new(0.0, 0.0, CHECK_SIZE as f32, CHECK_SIZE as f32);
        let rs = Size::new((CHECK_SIZE / 2.0) as f32, (CHECK_SIZE / 2.0) as f32);
        snapshot.push_rounded_clip(&gsk::RoundedRect::new(disc, rs, rs, rs, rs));
        snapshot.append_color(&rgba(CREAM, 1.0), &disc);
        snapshot.pop();
        draw_check_mark(snapshot);
        snapshot.pop();
        snapshot.restore();
    }

    // Label.
    let label_x = content_x + lead_w + GAP;
    snapshot.save();
    snapshot.translate(&Point::new(label_x as f32, (mid_y - label_h / 2.0) as f32));
    snapshot.append_layout(&layout, &rgba(if v.error { RED } else { CREAM }, 1.0));
    snapshot.restore();

    snapshot.pop();
    snapshot.restore();
}

/// Check-mark stroke (24×24 SVG shown at 18 px in the centre of a 22 px disc,
/// 2.6-unit stroke with round ends), drawn as two rounded segments.
fn draw_check_mark(snapshot: &gtk::Snapshot) {
    const K: f64 = 0.75;
    let p = |x: f64, y: f64| (2.0 + x * K, 2.0 + y * K);
    let width = 2.6 * K;
    let points = [p(5.0, 12.5), p(9.5, 17.0), p(19.0, 7.5)];
    for pair in points.windows(2) {
        let ((ax, ay), (bx, by)) = (pair[0], pair[1]);
        let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
        let angle = (by - ay).atan2(bx - ax).to_degrees();
        snapshot.save();
        snapshot.translate(&Point::new(ax as f32, ay as f32));
        snapshot.rotate(angle as f32);
        let rect = Rect::new(
            (-width / 2.0) as f32,
            (-width / 2.0) as f32,
            (len + width) as f32,
            width as f32,
        );
        let r = Size::new((width / 2.0) as f32, (width / 2.0) as f32);
        snapshot.push_rounded_clip(&gsk::RoundedRect::new(rect, r, r, r, r));
        snapshot.append_color(&rgba(BG, 1.0), &rect);
        snapshot.pop();
        snapshot.restore();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_phases() {
        assert_eq!(label_for(Phase::Idle, None, None), None);
        assert_eq!(
            label_for(Phase::Listening, None, None).as_deref(),
            Some("Listening")
        );
        assert_eq!(
            label_for(Phase::Transcribing, None, None).as_deref(),
            Some("Transcribing")
        );
        assert_eq!(
            label_for(Phase::Formatting, None, None).as_deref(),
            Some("Formatting")
        );
        assert_eq!(label_for(Phase::Done, None, None).as_deref(), Some("Done"));
        assert_eq!(
            label_for(Phase::Done, None, Some(0)).as_deref(),
            Some("Done")
        );
        assert_eq!(
            label_for(Phase::Done, None, Some(600)).as_deref(),
            Some("Done · 0.6 s")
        );
        assert_eq!(
            label_for(Phase::Done, None, Some(1449)).as_deref(),
            Some("Done · 1.4 s")
        );
        assert_eq!(
            label_for(Phase::Error, None, None).as_deref(),
            Some("Error")
        );
        assert_eq!(
            label_for(Phase::Error, Some("  "), None).as_deref(),
            Some("Error")
        );
        assert_eq!(
            label_for(Phase::Error, Some("No speech"), None).as_deref(),
            Some("No speech")
        );
    }

    /// Single GTK test (GTK accepts only one thread): offscreen rendering, no window.
    #[test]
    fn offscreen_render_smoke() {
        if gtk::init().is_err() {
            eprintln!("no GTK display: test skipped");
            return;
        }
        let pill = PillWidget::new();
        pill.apply(&OverlayMsg::phase(Phase::Listening));
        pill.advance(400.0);
        for _ in 0..22 {
            pill.apply(&OverlayMsg::level(0.8));
            pill.advance(100.0);
        }
        assert_eq!(pill.label_text(), "Listening");
        let snap = gtk::Snapshot::new();
        pill.paint(&snap, 420.0, 120.0);
        assert!(snap.to_node().is_some(), "nothing was drawn");

        pill.apply(&OverlayMsg::done(600));
        assert_eq!(pill.label_text(), "Done · 0.6 s");
        pill.apply(&OverlayMsg::error("Nope"));
        assert_eq!(pill.label_text(), "Nope");
        assert!(pill.visual().error);
        pill.apply(&OverlayMsg::hide());
        pill.advance(250.0);
        pill.advance(250.0);
        let snap = gtk::Snapshot::new();
        pill.paint(&snap, 420.0, 120.0);
        assert!(snap.to_node().is_none(), "a hidden pill draws nothing");
    }
}
