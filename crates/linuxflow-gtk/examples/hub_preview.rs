//! Renders the hub pages to PNG without showing anything on the user's screen.
//!
//! Rendering needs "mapped" widgets: the example therefore starts a GDK Broadway server
//! (`gtk4-broadwayd`, a purely software display that nobody watches) and presents the
//! window there. It refuses to run on any other GDK backend.
//!
//! Language: `--lang fr` (or `LINUXFLOW_PREVIEW_LANG`); the `.mo` catalogues are looked up through
//! `LINUXFLOW_LOCALE_DIR` (see `linuxflow-i18n`).
//!
//! Usage: `cargo run -p linuxflow-gtk --example hub_preview [folder]`
//! (default folder: `target/hub-preview`, or `LINUXFLOW_PREVIEW_DIR`).

#[path = "support/fake_controller.rs"]
mod fake_controller;

use fake_controller::{real_models, FakeController};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_core::domain::Phase;
use linuxflow_core::events::Event;
use linuxflow_core::traits::Controller;
use linuxflow_gtk::hub::{HubView, PageId};
use linuxflow_gtk::shortcut_recorder::{Modifier, Press};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Runs the GLib loop for a moment (background tasks, timers).
fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let ctx = glib::MainContext::default();
    while Instant::now() < end {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Starts a private Broadway server (unless the user supplies one through the environment).
fn start_broadway() -> Result<Option<std::process::Child>, String> {
    if std::env::var("GDK_BACKEND").is_ok_and(|b| b == "broadway") {
        return Ok(None);
    }
    let n = 30 + std::process::id() % 60;
    let display = format!(":{n}");
    let child = std::process::Command::new("gtk4-broadwayd")
        .arg(&display)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("gtk4-broadwayd not found ({e}): offscreen rendering impossible"))?;
    std::env::set_var("GDK_BACKEND", "broadway");
    std::env::set_var("BROADWAY_DISPLAY", &display);
    std::thread::sleep(Duration::from_millis(600));
    Ok(Some(child))
}

fn render(widget: &gtk::Widget, path: &Path) -> Result<(), String> {
    render_sized(widget, 720, path)
}

fn render_sized(widget: &gtk::Widget, height: i32, path: &Path) -> Result<(), String> {
    pump(120);
    // No frame clock without a Broadway client: the layout is forced by hand.
    let (w, h) = (1100, height);
    widget.allocate(w, h, -1, None);
    let snapshot = gtk::Snapshot::new();
    let parent = widget.parent().ok_or("widget without a parent")?;
    parent.snapshot_child(widget, &snapshot);
    let node = snapshot.to_node().ok_or("rendu vide")?;
    let renderer = gtk::gsk::CairoRenderer::new();
    renderer
        .realize(None::<&gtk::gdk::Surface>)
        .map_err(|e| e.to_string())?;
    let texture = renderer.render_texture(&node, None);
    renderer.unrealize();
    texture.save_to_png(path).map_err(|e| e.to_string())?;
    println!("PNG {} ({w}x{h})", path.display());
    Ok(())
}

fn main() -> Result<(), String> {
    let mut broadway = start_broadway()?;
    let result = run();
    if let Some(c) = broadway.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    result
}

fn run() -> Result<(), String> {
    gtk::init().map_err(|e| format!("GTK indisponible : {e}"))?;
    adw::init().map_err(|e| e.to_string())?;
    let backend = gtk::gdk::Display::default()
        .map(|d| d.type_().name().to_string())
        .unwrap_or_default();
    if !backend.contains("Broadway") {
        return Err(format!(
            "unexpected GDK backend ({backend}): refusing to open a window on a real display"
        ));
    }
    let mut args = std::env::args().skip(1);
    let mut dir_arg: Option<String> = None;
    let mut lang: Option<String> = std::env::var("LINUXFLOW_PREVIEW_LANG")
        .ok()
        .filter(|l| !l.is_empty());
    while let Some(a) = args.next() {
        if a == "--lang" {
            lang = args.next();
        } else {
            dir_arg = Some(a);
        }
    }
    linuxflow_i18n::init("linuxflow", None);
    linuxflow_i18n::set_language(lang.as_deref().or(Some("en")));
    println!("langue : {}", linuxflow_i18n::current_language());
    let dir: PathBuf = dir_arg
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LINUXFLOW_PREVIEW_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("target/hub-preview"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let controller = FakeController::new();
    // `LINUXFLOW_PREVIEW_THEME=dark|light|system` renders the pages with that appearance.
    if let Ok(theme) = std::env::var("LINUXFLOW_PREVIEW_THEME") {
        let theme = match theme.as_str() {
            "dark" => linuxflow_core::domain::Theme::Dark,
            "light" => linuxflow_core::domain::Theme::Light,
            _ => linuxflow_core::domain::Theme::System,
        };
        controller.with_state(|s| s.settings.theme = theme);
    }
    let view: Rc<HubView> = HubView::new(controller.clone());
    let root = view.widget().clone();
    let window = gtk::Window::builder()
        .default_width(1100)
        .default_height(720)
        .child(&root)
        .build();
    window.present();
    pump(300);
    let shot = |name: &str| render(&root, &dir.join(format!("{name}.png")));
    pump(150);

    // Home, at rest then listening with dictated text.
    view.show_page(PageId::Home);
    pump(100);
    shot("01-home")?;
    view.handle_event(Event::InsertText("Hello, this is a test dictation.".into()));
    view.handle_event(Event::InsertText(
        "It adds a space between segments.".into(),
    ));
    view.handle_event(Event::Phase {
        phase: Phase::Transcribing,
        message: None,
    });
    pump(50);
    shot("02-home-dictating")?;
    view.handle_event(Event::Phase {
        phase: Phase::Idle,
        message: None,
    });

    view.show_page(PageId::History);
    pump(100);
    shot("03-history")?;

    view.show_page(PageId::Dictionary);
    view.dictionary()
        .set_test_text("I deploy with cooper nettys");
    pump(300);
    shot("04-dictionary")?;
    if let Some(entry) = controller.state().dictionary.first().cloned() {
        view.dictionary().start_edit(&entry);
    }
    pump(50);
    shot("05-dictionary-edit")?;

    view.show_page(PageId::Snippets);
    pump(50);
    shot("06-snippets")?;

    view.show_page(PageId::Settings);
    pump(300);
    shot("07-settings")?;
    // Full page (Interface and Local engine cards, below the fold).
    render_sized(&root, 1250, &dir.join("07b-settings-full.png"))?;
    // Shortcut recorder while listening, then a pending combination.
    let rec = view.settings().recorder().clone();
    rec.start_recording();
    rec.key_pressed(37, Press::Modifier(Modifier::Ctrl), &BTreeSet::new());
    pump(30);
    shot("08-settings-recording")?;
    rec.key_pressed(
        40,
        Press::Key("K".into()),
        &BTreeSet::from([Modifier::Ctrl]),
    );
    rec.stop_recording();
    pump(30);
    shot("09-settings-pending")?;
    rec.cancel();

    *controller.models.lock().unwrap_or_else(|e| e.into_inner()) = real_models(false);
    view.show_page(PageId::Models);
    pump(300);
    shot("10-models")?;

    view.show_page(PageId::Troubleshooting);
    pump(300);
    shot("11-compat")?;

    // Engine loading / error / listening (pill).
    let mut state = controller.state();
    state.engine.ready = false;
    state.engine.loading = true;
    state.engine.phase = linuxflow_core::domain::EnginePhase::Downloading;
    state.engine.download = Some(linuxflow_core::domain::DownloadProgress {
        model: "small".into(),
        done: 1_300_000_000,
        total: 3_100_000_000,
    });
    view.show_page(PageId::Home);
    view.handle_event(Event::State(Box::new(state)));
    pump(50);
    shot("12-home-engine-downloading")?;

    // First-launch wizard: nothing installed yet, the real catalogue.
    *controller.models.lock().unwrap_or_else(|e| e.into_inner()) = real_models(true);
    view.wizard().load();
    pump(300);
    let mut state = controller.state();
    state.settings.first_run_done = false;
    controller.with_state(|s| s.settings.first_run_done = false);
    view.handle_event(Event::State(Box::new(state)));
    pump(300);
    shot("13-wizard-1")?;
    view.wizard().next_step();
    pump(100);
    shot("14-wizard-2-model")?;
    view.wizard().next_step();
    pump(100);
    shot("15-wizard-3-shortcut")?;
    view.wizard().next_step();
    pump(100);
    shot("16-wizard-4-summary")?;

    window.destroy();
    Ok(())
}
