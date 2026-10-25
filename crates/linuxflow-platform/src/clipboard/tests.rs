#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

fn env(pairs: &[(&str, &str)]) -> SessionEnv {
    SessionEnv::from_map(
        &pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

/// In-memory clipboard: never touches the real clipboard.
#[derive(Default)]
struct Fake {
    name: &'static str,
    fail_set: bool,
    primary: bool,
    served: Option<ServedStatus>,
    store: Mutex<HashMap<&'static str, SelectionData>>,
    sets: Mutex<Vec<(String, Vec<Selection>)>>,
}

fn key(sel: Selection) -> &'static str {
    match sel {
        Selection::Clipboard => "c",
        Selection::Primary => "p",
    }
}

fn text_data(text: &str) -> SelectionData {
    SelectionData {
        items: vec![ClipItem {
            mime: "text/plain".into(),
            data: text.as_bytes().to_vec(),
        }],
        incomplete: false,
    }
}

impl ClipboardBackend for Fake {
    fn name(&self) -> &'static str {
        self.name
    }
    fn supports_primary(&self) -> bool {
        self.primary
    }
    fn get_text(&self, sel: Selection) -> Result<Option<String>, ClipError> {
        let store = self.store.lock().unwrap();
        Ok(store
            .get(key(sel))
            .and_then(|d| d.items.first())
            .map(|i| String::from_utf8_lossy(&i.data).into_owned()))
    }
    fn set_text(&self, text: &str, sels: &[Selection]) -> Result<(), ClipError> {
        if self.fail_set {
            return Err(ClipError::Unavailable("nope".into()));
        }
        self.sets
            .lock()
            .unwrap()
            .push((text.to_string(), sels.to_vec()));
        let mut store = self.store.lock().unwrap();
        for s in sels {
            store.insert(key(*s), text_data(text));
        }
        Ok(())
    }
    fn snapshot_selection(&self, sel: Selection) -> Result<SelectionData, ClipError> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .get(key(sel))
            .cloned()
            .unwrap_or_default())
    }
    fn restore_selection(&self, sel: Selection, data: &SelectionData) -> Result<(), ClipError> {
        let mut store = self.store.lock().unwrap();
        if data.items.is_empty() {
            store.remove(key(sel));
        } else {
            store.insert(key(sel), data.clone());
        }
        Ok(())
    }
    fn wait_served(&self, _t: Duration) -> ServedStatus {
        self.served.unwrap_or(ServedStatus::Unknown)
    }
}

fn fake(name: &'static str) -> Fake {
    Fake {
        name,
        primary: true,
        ..Default::default()
    }
}

#[test]
fn chain_per_session() {
    use ClipboardBackendKind::*;
    let x11 = env(&[("XDG_SESSION_TYPE", "x11"), ("DISPLAY", ":0")]);
    assert_eq!(clipboard_chain(&x11, false, false), vec![X11]);
    assert_eq!(clipboard_chain(&x11, false, true), vec![X11, App]);

    let kde = env(&[
        ("XDG_SESSION_TYPE", "wayland"),
        ("XDG_CURRENT_DESKTOP", "KDE"),
        ("DISPLAY", ":1"),
    ]);
    assert_eq!(
        clipboard_chain(&kde, false, true),
        vec![DataControl, X11, App]
    );

    let sway = env(&[("XDG_SESSION_TYPE", "wayland"), ("SWAYSOCK", "x")]);
    assert_eq!(clipboard_chain(&sway, false, false), vec![DataControl]);

    let gnome = env(&[
        ("XDG_SESSION_TYPE", "wayland"),
        ("XDG_CURRENT_DESKTOP", "GNOME"),
        ("DISPLAY", ":0"),
    ]);
    assert_eq!(clipboard_chain(&gnome, true, true), vec![Portal, App]);
    assert_eq!(clipboard_chain(&gnome, false, true), vec![App]);
}

#[test]
fn set_text_uses_first_working_backend_and_records_it() {
    let a = Arc::new(Fake {
        fail_set: true,
        ..fake("a")
    });
    let b = Arc::new(fake("b"));
    let c = SystemClipboard::new(vec![a.clone(), b.clone()], false);
    c.set_text("hello").unwrap();
    assert_eq!(
        b.sets.lock().unwrap()[0],
        ("hello".to_string(), vec![Selection::Clipboard])
    );
    assert_eq!(
        b.get_text(Selection::Clipboard).unwrap().as_deref(),
        Some("hello")
    );
}

#[test]
fn also_primary_fills_both_only_where_supported() {
    let with = Arc::new(fake("with"));
    let c = SystemClipboard::new(vec![with.clone()], true);
    c.set_text("x").unwrap();
    assert_eq!(
        with.sets.lock().unwrap()[0].1,
        vec![Selection::Clipboard, Selection::Primary]
    );

    let without = Arc::new(Fake {
        primary: false,
        ..fake("without")
    });
    let c = SystemClipboard::new(vec![without.clone()], true);
    c.set_text("x").unwrap();
    assert_eq!(
        without.sets.lock().unwrap()[0].1,
        vec![Selection::Clipboard]
    );
}

#[test]
fn all_backends_failing_is_an_error_not_a_panic() {
    let a = Arc::new(Fake {
        fail_set: true,
        ..fake("a")
    });
    let c = SystemClipboard::new(vec![a], false);
    assert!(c.set_text("x").is_err());
    let empty = SystemClipboard::new(vec![], false);
    assert!(empty.set_text("x").is_err());
    assert!(empty.get_text().is_err());
}

#[test]
fn snapshot_set_restore_round_trip_when_content_is_still_ours() {
    let b = Arc::new(fake("b"));
    b.store.lock().unwrap().insert("c", text_data("previous"));
    b.store.lock().unwrap().insert("p", text_data("selected"));
    let c = SystemClipboard::new(vec![b.clone()], true);

    let snap = c.snapshot();
    assert_eq!(snap.clipboard, Some(text_data("previous")));
    assert_eq!(snap.primary, Some(text_data("selected")));

    c.set_text("dictated").unwrap();
    assert_eq!(
        c.restore_if_ours(&snap, "dictated"),
        RestoreOutcome::Restored
    );
    assert_eq!(
        b.store.lock().unwrap().get("c"),
        Some(&text_data("previous"))
    );
    assert_eq!(
        b.store.lock().unwrap().get("p"),
        Some(&text_data("selected"))
    );
}

#[test]
fn restore_is_skipped_when_user_copied_something_else() {
    let b = Arc::new(fake("b"));
    b.store.lock().unwrap().insert("c", text_data("previous"));
    let c = SystemClipboard::new(vec![b.clone()], false);
    let snap = c.snapshot();
    c.set_text("dictated").unwrap();
    // The user copies something else in the meantime.
    b.store
        .lock()
        .unwrap()
        .insert("c", text_data("user copied"));
    assert_eq!(
        c.restore_if_ours(&snap, "dictated"),
        RestoreOutcome::SkippedChanged
    );
    assert_eq!(
        b.store.lock().unwrap().get("c"),
        Some(&text_data("user copied"))
    );
}

#[test]
fn empty_clipboard_is_restored_as_empty() {
    let b = Arc::new(fake("b"));
    let c = SystemClipboard::new(vec![b.clone()], false);
    let snap = c.snapshot();
    assert_eq!(snap.clipboard, Some(SelectionData::default()));
    c.set_text("dictated").unwrap();
    assert_eq!(
        c.restore_if_ours(&snap, "dictated"),
        RestoreOutcome::Restored
    );
    assert!(b.store.lock().unwrap().get("c").is_none());
}

#[test]
fn backend_without_snapshot_support_leaves_nothing_to_restore() {
    struct NoSnap;
    impl ClipboardBackend for NoSnap {
        fn name(&self) -> &'static str {
            "nosnap"
        }
        fn get_text(&self, _: Selection) -> Result<Option<String>, ClipError> {
            Ok(None)
        }
        fn set_text(&self, _: &str, _: &[Selection]) -> Result<(), ClipError> {
            Ok(())
        }
    }
    let c = SystemClipboard::new(vec![Arc::new(NoSnap)], false);
    let snap = c.snapshot();
    assert!(snap.is_empty());
    assert_eq!(
        c.restore_if_ours(&snap, "x"),
        RestoreOutcome::NothingToRestore
    );
    assert!(c.restore(&snap));
}

#[test]
fn wait_served_delegates_to_last_writer_and_defaults_to_unknown() {
    let b = Arc::new(Fake {
        served: Some(ServedStatus::Served),
        ..fake("b")
    });
    let c = SystemClipboard::new(vec![b], false);
    assert_eq!(
        c.wait_served(Duration::from_millis(1)),
        ServedStatus::Unknown
    );
    c.set_text("x").unwrap();
    assert_eq!(
        c.wait_served(Duration::from_millis(1)),
        ServedStatus::Served
    );
}

/// Unix stream already filled then closed on the write side: equivalent to a finished selection pipe.
fn filled_stream(bytes: &[u8]) -> std::os::unix::net::UnixStream {
    use std::io::Write;
    let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    writer.write_all(bytes).unwrap();
    drop(writer);
    reader
}

#[test]
fn read_limited_truncates_and_reports() {
    let (data, truncated) =
        read_limited(filled_stream(&[7u8; 100]), 10, Duration::from_secs(2)).unwrap();
    assert_eq!(data.len(), 10);
    assert!(truncated);
    let (data, truncated) =
        read_limited(filled_stream(&[7u8; 5]), 10, Duration::from_secs(2)).unwrap();
    assert_eq!((data.len(), truncated), (5, false));
}

/// Reader that reports its destruction: proves that `read_limited` does not keep it in the background.
struct Tracked {
    inner: std::os::unix::net::UnixStream,
    dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl std::io::Read for Tracked {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}
impl std::os::fd::AsRawFd for Tracked {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.inner.as_raw_fd()
    }
}
impl Drop for Tracked {
    fn drop(&mut self) {
        self.dropped
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn read_limited_on_a_silent_source_does_not_keep_the_reader_alive() {
    // Regression: 10 expired reads on a mute source left 10 extra threads and 20
    // descriptors, held until the source was closed. Here the source stays open
    // and mute during the whole test: the reader must nevertheless be released on return.
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut writers = Vec::new();
    for i in 1..=10 {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        writers.push(writer);
        let reader = Tracked {
            inner: reader,
            dropped: dropped.clone(),
        };
        let started = std::time::Instant::now();
        let err = read_limited(reader, 1024, Duration::from_millis(30)).unwrap_err();
        assert!(
            matches!(err, ClipError::Failed(ref m) if m.contains("timed out")),
            "{err:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            i,
            "reader #{i} is still held after the return"
        );
    }
    drop(writers);
}
