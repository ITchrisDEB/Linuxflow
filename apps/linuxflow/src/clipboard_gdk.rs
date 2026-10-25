//! GDK clipboard (`gdk::Clipboard`): last resort of the chain (GNOME without a portal) and
//! the interface clipboard (the "Copy" button).
//!
//! `gdk::Clipboard` can only be used on the GTK main thread; calls coming from a
//! worker thread are sent there (`MainContext::invoke`) and awaited for 2 s at most.

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use linuxflow_core::error::{Error, Result};
use linuxflow_core::traits::Clipboard;
use linuxflow_platform::clipboard::{ClipError, ClipboardBackend, Selection};
use std::sync::mpsc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct GdkClipboard;

fn select(display: &gdk::Display, selection: Selection) -> gdk::Clipboard {
    match selection {
        Selection::Clipboard => display.clipboard(),
        Selection::Primary => display.primary_clipboard(),
    }
}

fn on_main_thread() -> bool {
    glib::MainContext::default().is_owner()
}

fn read(selection: Selection) -> std::result::Result<Option<String>, ClipError> {
    if on_main_thread() {
        return Err(ClipError::Unavailable(
            "the GDK clipboard cannot be read synchronously on the main thread".into(),
        ));
    }
    let (tx, rx) = mpsc::channel();
    glib::MainContext::default().invoke(move || {
        let Some(display) = gdk::Display::default() else {
            let _ = tx.send(Err(ClipError::Unavailable("no display".into())));
            return;
        };
        select(&display, selection).read_text_async(gio::Cancellable::NONE, move |res| {
            let _ = tx.send(
                res.map(|o| o.map(|g| g.to_string()))
                    .map_err(|e| ClipError::Failed(e.to_string())),
            );
        });
    });
    rx.recv_timeout(WAIT)
        .map_err(|_| ClipError::Failed("clipboard read timed out".into()))?
}

fn write(text: &str, selections: &[Selection]) -> std::result::Result<(), ClipError> {
    let selections = selections.to_vec();
    let text = text.to_string();
    let apply = move || -> std::result::Result<(), ClipError> {
        let display =
            gdk::Display::default().ok_or_else(|| ClipError::Unavailable("no display".into()))?;
        for s in &selections {
            select(&display, *s).set_text(&text);
        }
        Ok(())
    };
    if on_main_thread() {
        return apply();
    }
    let (tx, rx) = mpsc::channel();
    glib::MainContext::default().invoke(move || {
        let _ = tx.send(apply());
    });
    rx.recv_timeout(WAIT)
        .map_err(|_| ClipError::Failed("clipboard write timed out".into()))?
}

impl ClipboardBackend for GdkClipboard {
    fn name(&self) -> &'static str {
        "gdk"
    }

    fn get_text(&self, selection: Selection) -> std::result::Result<Option<String>, ClipError> {
        read(selection)
    }

    fn set_text(&self, text: &str, selections: &[Selection]) -> std::result::Result<(), ClipError> {
        write(text, selections)
    }

    fn supports_primary(&self) -> bool {
        true
    }
}

impl Clipboard for GdkClipboard {
    fn get_text(&self) -> Result<Option<String>> {
        read(Selection::Clipboard).map_err(|e| Error::msg(e.to_string()))
    }

    fn set_text(&self, text: &str) -> Result<()> {
        write(text, &[Selection::Clipboard]).map_err(|e| Error::msg(e.to_string()))
    }
}
