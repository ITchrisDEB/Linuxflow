//! Offscreen rendering of the pill to PNG (no window is shown).
//!
//! Usage: `cargo run -p linuxflow-gtk --example overlay_preview -- <output folder>`
//!
//! Trial in a real window (once, ~4 s, without focus): set `LINUXFLOW_PREVIEW_LIVE=1`.

use std::path::{Path, PathBuf};

use gtk::prelude::*;
use gtk::{gdk, glib, graphene};
use gtk4 as gtk;
use linuxflow_core::domain::Phase;
use linuxflow_gtk::overlay::{OverlayMsg, OverlayWindow, PillWidget, WindowKind};

const SCALE: f32 = 2.0;
const W: f32 = 420.0;
const H: f32 = 120.0;

fn levels(pill: &PillWidget, values: &[f32]) {
    for v in values {
        pill.push_level(*v);
        pill.advance(100.0);
    }
    pill.advance(300.0);
}

fn settled(pill: &PillWidget) {
    pill.advance(400.0);
}

fn render(nodes: &[(&PillWidget, f32)], bg: bool) -> Option<gdk::Texture> {
    let snap = gtk::Snapshot::new();
    let total_h = H * nodes.len() as f32;
    snap.scale(SCALE, SCALE);
    if bg {
        snap.append_color(
            &gdk::RGBA::new(0.80, 0.82, 0.86, 1.0),
            &graphene::Rect::new(0.0, 0.0, W, total_h),
        );
    }
    for (i, (pill, _)) in nodes.iter().enumerate() {
        snap.save();
        snap.translate(&graphene::Point::new(0.0, H * i as f32));
        pill.paint(&snap, f64::from(W), f64::from(H));
        snap.restore();
    }
    let node = snap.to_node()?;
    let renderer = gtk::gsk::CairoRenderer::new();
    renderer.realize(None::<&gdk::Surface>).ok()?;
    let rect = graphene::Rect::new(0.0, 0.0, W * SCALE, total_h * SCALE);
    let tex = renderer.render_texture(&node, Some(&rect));
    renderer.unrealize();
    Some(tex)
}

fn save(tex: &gdk::Texture, path: &Path) {
    match tex.save_to_png(path) {
        Ok(()) => println!("PNG {}", path.display()),
        Err(e) => eprintln!("write impossible ({}): {e}", path.display()),
    }
}

fn live() {
    let Some(kind) = WindowKind::detect() else {
        println!("live: no display");
        return;
    };
    println!("live : kind={kind:?}");
    let win = OverlayWindow::new(kind);
    win.apply(&OverlayMsg::phase(Phase::Listening));
    let main_loop = glib::MainLoop::new(None, false);
    let win = std::rc::Rc::new(win);
    for step in 0..20u32 {
        let w = win.clone();
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(u64::from(step) * 150),
            move || {
                w.apply(&OverlayMsg::level(
                    0.2 + 0.7 * ((step as f32) * 0.7).sin().abs(),
                ));
            },
        );
    }
    {
        let w = win.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1800), move || {
            w.apply(&OverlayMsg::phase(Phase::Transcribing));
        });
    }
    {
        let w = win.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(2800), move || {
            w.apply(&OverlayMsg::done(600));
        });
    }
    {
        let w = win.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(3300), move || {
            println!(
                "live : mapped={} focusable={} focus_widget={}",
                w.window().is_mapped(),
                w.window().is_focusable(),
                w.window().is_active()
            );
            w.apply(&OverlayMsg::hide());
        });
    }
    let quit = main_loop.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(4000), move || quit.quit());
    main_loop.run();
}

fn main() {
    if let Err(e) = gtk::init() {
        eprintln!("GTK indisponible : {e}");
        return;
    }
    if std::env::var_os("LINUXFLOW_PREVIEW_LIVE").is_some() {
        live();
        return;
    }
    let out: PathBuf = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("dossier de sortie : {e}");
        return;
    }

    let make = |phase: Phase, msg: Option<&str>, lat: Option<u64>| {
        let p = PillWidget::new();
        p.set_phase(phase, msg, lat);
        p
    };

    let silent = make(Phase::Listening, None, None);
    settled(&silent);
    let low = make(Phase::Listening, None, None);
    settled(&low);
    levels(
        &low,
        &[
            0.1, 0.2, 0.15, 0.3, 0.25, 0.2, 0.1, 0.18, 0.22, 0.3, 0.2, 0.15, 0.1, 0.2, 0.25, 0.3,
            0.2, 0.1, 0.15, 0.2, 0.25, 0.3,
        ],
    );
    let loud = make(Phase::Listening, None, None);
    settled(&loud);
    let wave: Vec<f32> = (0..22)
        .map(|i| 0.5 + 0.5 * ((i as f32) * 0.55).sin() * (1.0 - i as f32 / 40.0))
        .collect();
    levels(&loud, &wave);
    let transcribing = make(Phase::Transcribing, None, None);
    settled(&transcribing);
    transcribing.advance(137.0);
    let formatting = make(Phase::Formatting, None, None);
    settled(&formatting);
    formatting.advance(137.0);
    let done = make(Phase::Done, None, Some(600));
    settled(&done);
    let error = make(Phase::Error, Some("Nothing heard"), None);
    settled(&error);
    let long_error = make(Phase::Error, Some("Model downloading · 42 %"), None);
    settled(&long_error);
    // Appearance in progress (middle of the animation) and end of the disappearance.
    let appearing = make(Phase::Listening, None, None);
    appearing.advance(110.0);
    let pop = make(Phase::Done, None, Some(600));
    pop.advance(80.0);

    let named: Vec<(&str, &PillWidget)> = vec![
        ("listening-silent", &silent),
        ("listening-low", &low),
        ("listening-loud", &loud),
        ("transcribing", &transcribing),
        ("formatting", &formatting),
        ("done", &done),
        ("error", &error),
        ("error-long", &long_error),
        ("appearing", &appearing),
        ("done-pop", &pop),
    ];
    for (name, pill) in &named {
        if let Some(tex) = render(&[(pill, 0.0)], true) {
            save(&tex, &out.join(format!("pill-{name}.png")));
        }
    }
    let all: Vec<(&PillWidget, f32)> = named.iter().map(|(_, p)| (*p, 0.0)).collect();
    if let Some(tex) = render(&all, true) {
        save(&tex, &out.join("sheet.png"));
    }
}
