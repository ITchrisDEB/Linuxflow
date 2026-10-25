//! `--screenshots[=dir]`: renders the hub pages to PNG on a private GDK Broadway server
//! (`gtk4-broadwayd`), without ever opening a window on the user's screen. The data
//! shown are those of a fresh instance (temporary folders) enriched with examples.
//! The pill is not rendered here (see the `overlay_preview` example of the linuxflow-gtk crate).

use crate::clipboard_gdk::GdkClipboard;
use crate::wiring::{build_core, Options};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_core::paths::{AppPaths, MapEnv};
use linuxflow_core::traits::Controller;
use linuxflow_gtk::hub::{HubView, PageId};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let ctx = glib::MainContext::default();
    while Instant::now() < end {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn start_broadway() -> Result<Option<std::process::Child>, String> {
    if std::env::var("GDK_BACKEND").is_ok_and(|b| b == "broadway") {
        return Ok(None);
    }
    let display = format!(":{}", 30 + std::process::id() % 60);
    let child = std::process::Command::new("gtk4-broadwayd")
        .arg(&display)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            format!("gtk4-broadwayd not found ({e}): offscreen rendering is impossible")
        })?;
    std::env::set_var("GDK_BACKEND", "broadway");
    std::env::set_var("BROADWAY_DISPLAY", &display);
    std::thread::sleep(Duration::from_millis(600));
    Ok(Some(child))
}

fn render(widget: &gtk::Widget, path: &Path) -> Result<(), String> {
    pump(150);
    let (w, h) = (1100, 720);
    widget.allocate(w, h, -1, None);
    let snapshot = gtk::Snapshot::new();
    let parent = widget.parent().ok_or("widget without parent")?;
    parent.snapshot_child(widget, &snapshot);
    let node = snapshot.to_node().ok_or("empty render")?;
    let renderer = gtk::gsk::CairoRenderer::new();
    renderer
        .realize(None::<&gtk::gdk::Surface>)
        .map_err(|e| e.to_string())?;
    let texture = renderer.render_texture(&node, None);
    renderer.unrealize();
    texture.save_to_png(path).map_err(|e| e.to_string())?;
    println!("SCREENSHOT {}", path.display());
    Ok(())
}

/// Returns the exit code.
pub fn run(dir_arg: &str, _paths: &AppPaths) -> i32 {
    let mut broadway = match start_broadway() {
        Ok(b) => b,
        Err(e) => {
            println!("DEV_ERROR {e}");
            return 1;
        }
    };
    let result = capture(dir_arg);
    if let Some(c) = broadway.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    match result {
        Ok(()) => 0,
        Err(e) => {
            println!("DEV_ERROR {e}");
            1
        }
    }
}

fn capture(dir_arg: &str) -> Result<(), String> {
    gtk::init().map_err(|e| format!("GTK unavailable: {e}"))?;
    adw::init().map_err(|e| e.to_string())?;
    let backend = gtk::gdk::Display::default()
        .map(|d| d.type_().name().to_string())
        .unwrap_or_default();
    if !backend.contains("Broadway") {
        return Err(format!(
            "unexpected GDK backend ({backend}): refusing to open windows on a real display"
        ));
    }
    let out: PathBuf = if dir_arg.is_empty() {
        PathBuf::from("target/screenshots")
    } else {
        PathBuf::from(dir_arg)
    };
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;

    // Fresh instance in a temporary folder.
    let tmp = std::env::temp_dir().join(format!("linuxflow-shots-{}", std::process::id()));
    let sub = |n: &str| tmp.join(n).to_string_lossy().into_owned();
    let env = MapEnv::new(&[
        ("LINUXFLOW_CONFIG_DIR", &sub("config")),
        ("LINUXFLOW_DATA_DIR", &sub("data")),
        ("LINUXFLOW_STATE_DIR", &sub("state")),
        ("LINUXFLOW_CACHE_DIR", &sub("cache")),
    ]);
    let paths = AppPaths::resolve(&env).map_err(|e| e.to_string())?;
    paths.ensure_dirs().map_err(|e| e.to_string())?;
    let (events_tx, _events_rx) = async_channel::unbounded();
    let (ui_tx, _ui_rx) = async_channel::unbounded();
    let core = build_core(
        paths,
        Options {
            display_name: Some("Alex".into()),
            no_engine: true,
        },
        events_tx,
        ui_tx,
        Arc::new(GdkClipboard),
    )?;
    let c: Arc<dyn Controller> = core.service.clone();
    let _ = c.dictionary_add("LinuxFlow", "linux flow, linux flo");
    let _ = c.snippet_add("my signature", "Have a great day!");
    let _ = c.set_settings(serde_json::json!({"firstRunDone": true}));

    let view: Rc<HubView> = HubView::new(c.clone());
    let root = view.widget().clone();
    let window = gtk::Window::builder()
        .default_width(1100)
        .default_height(720)
        .child(&root)
        .build();
    window.present();
    pump(300);
    let mut result = Ok(());
    for (name, page) in [
        ("01-home", PageId::Home),
        ("02-history", PageId::History),
        ("03-dictionary", PageId::Dictionary),
        ("04-snippets", PageId::Snippets),
        ("05-models", PageId::Models),
        ("06-settings", PageId::Settings),
        ("07-compat", PageId::Troubleshooting),
    ] {
        view.show_page(page);
        pump(200);
        if let Err(e) = render(&root, &out.join(format!("{name}.png"))) {
            result = Err(e);
            break;
        }
    }
    window.destroy();
    core.engine.host().shutdown();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

use std::rc::Rc;
