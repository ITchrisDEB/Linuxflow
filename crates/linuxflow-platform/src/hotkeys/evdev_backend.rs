//! PASSIVE evdev backend: reads the keyboards without ever grabbing them (no `EVIOCGRAB`), handles
//! hot-plugging and several keyboards, records and logs no keystroke.
//! Functionally close to a key logger: disabled by default (see `HotkeyOptions`).

use super::backend::{BackendError, BackendErrorKind, BackendReport, HotkeyBackend, StartRequest};
use super::clock::now_ms;
use super::normalize::{looks_like_keyboard, KeyNormalizer};
use super::select::BackendKind;
use evdev::{Device, EventType, KeyCode};
use linuxflow_core::traits::HotkeySink;
use linuxflow_i18n::tr;
use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RESCAN: Duration = Duration::from_secs(2);
const INPUT_DIR: &str = "/dev/input";

pub struct EvdevBackend {
    ignore_names: Vec<String>,
    stop: Arc<AtomicBool>,
    suspended: Arc<AtomicBool>,
    wake: Option<UnixStream>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EvdevBackend {
    pub fn new(ignore_names: Vec<String>) -> Self {
        Self {
            ignore_names,
            stop: Arc::new(AtomicBool::new(false)),
            suspended: Arc::new(AtomicBool::new(false)),
            wake: None,
            thread: None,
        }
    }
}

struct Opened {
    id: u64,
    device: Device,
}

#[derive(Default)]
struct ScanResult {
    denied: usize,
}

/// Opens the keyboards not yet followed. Counts the permission refusals.
fn scan(known: &mut HashMap<PathBuf, Opened>, next_id: &mut u64, ignore: &[String]) -> ScanResult {
    let mut res = ScanResult::default();
    let Ok(dir) = std::fs::read_dir(INPUT_DIR) else {
        return res;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event || known.contains_key(&path) {
            continue;
        }
        match Device::open(&path) {
            Ok(dev) => {
                let name = dev.name().unwrap_or("").to_string();
                let is_kb = dev.supported_events().contains(EventType::KEY)
                    && looks_like_keyboard(
                        |c| {
                            dev.supported_keys()
                                .is_some_and(|k| k.contains(KeyCode::new(c)))
                        },
                        &name,
                        ignore,
                    );
                if is_kb && dev.set_nonblocking(true).is_ok() {
                    *next_id += 1;
                    known.insert(
                        path,
                        Opened {
                            id: *next_id,
                            device: dev,
                        },
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => res.denied += 1,
            Err(_) => {}
        }
    }
    res
}

impl HotkeyBackend for EvdevBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Evdev
    }

    fn start(
        &mut self,
        req: &StartRequest,
        sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError> {
        self.stop();
        let mut known = HashMap::new();
        let mut next_id = 0u64;
        let first = scan(&mut known, &mut next_id, &self.ignore_names);
        if known.is_empty() && first.denied > 0 {
            return Err(BackendError::new(
                BackendErrorKind::Denied,
                tr("Keyboard devices are not readable. Add your user to the \"input\" group, then log in again."),
            ));
        }
        let (wake_tx, wake_rx) = UnixStream::pair()
            .map_err(|e| BackendError::new(BackendErrorKind::Failed, e.to_string()))?;
        let _ = wake_rx.set_nonblocking(true);
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let suspended = self.suspended.clone();
        let ignore = self.ignore_names.clone();
        let thread = std::thread::Builder::new()
            .name("lf-evdev-hotkey".into())
            .spawn(move || event_loop(known, next_id, ignore, sink, stop, suspended, wake_rx))
            .map_err(|e| BackendError::new(BackendErrorKind::Failed, e.to_string()))?;
        self.thread = Some(thread);
        self.wake = Some(wake_tx);
        Ok(BackendReport {
            supports_release: true,
            supports_modifier_only: true,
            trigger_description: req.spec.display(),
            provides_hotkey_events: false,
            provides_raw_keys: true,
            note: Some(tr(
                "Reading keyboard devices directly; keystrokes are never recorded.",
            )),
        })
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut w) = self.wake.take() {
            let _ = w.write_all(&[1]);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn set_suspended(&mut self, suspended: bool) {
        self.suspended.store(suspended, Ordering::SeqCst);
    }
}

impl Drop for EvdevBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

fn event_loop(
    mut known: HashMap<PathBuf, Opened>,
    mut next_id: u64,
    ignore: Vec<String>,
    sink: Arc<dyn HotkeySink>,
    stop: Arc<AtomicBool>,
    suspended: Arc<AtomicBool>,
    mut wake: UnixStream,
) {
    use std::io::Read;
    let mut norm = KeyNormalizer::new();
    let mut last_scan = Instant::now();
    let mut was_suspended = false;
    while !stop.load(Ordering::SeqCst) {
        if last_scan.elapsed() >= RESCAN {
            scan(&mut known, &mut next_id, &ignore);
            last_scan = Instant::now();
        }
        let paths: Vec<PathBuf> = known.keys().cloned().collect();
        let mut fds: Vec<libc::pollfd> = paths
            .iter()
            .filter_map(|p| known.get(p))
            .map(|o| libc::pollfd {
                fd: o.device.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        fds.push(libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        // SAFETY: `fds` is a valid buffer of `len` `pollfd`s; the descriptors belong to
        // `known`/`wake`, alive during the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 500) };
        if n > 0 && fds.last().is_some_and(|f| f.revents != 0) {
            let mut buf = [0u8; 16];
            let _ = wake.read(&mut buf);
        }
        let now_susp = suspended.load(Ordering::SeqCst);
        if now_susp != was_suspended {
            was_suspended = now_susp;
            if !now_susp {
                // Resume: the state of the held keys is unknown, start again from zero.
                let _ = norm.release_all(now_ms());
            }
        }
        let mut gone = Vec::new();
        for (i, path) in paths.iter().enumerate() {
            if fds.get(i).is_none_or(|f| f.revents == 0) {
                continue;
            }
            let Some(opened) = known.get_mut(path) else {
                continue;
            };
            match opened.device.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        if ev.event_type() != EventType::KEY || now_susp {
                            continue;
                        }
                        if let Some(raw) = norm.handle(opened.id, ev.code(), ev.value(), now_ms()) {
                            sink.raw_key(raw);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => gone.push(path.clone()),
            }
        }
        for path in gone {
            if let Some(o) = known.remove(&path) {
                for ev in norm.device_removed(o.id, now_ms()) {
                    if !now_susp {
                        sink.raw_key(ev);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::spec::HotkeySpec;
    use linuxflow_core::domain::ShortcutMode;
    use linuxflow_core::traits::{HotkeyEvent, RawKeyEvent};

    struct Null;
    impl HotkeySink for Null {
        fn hotkey(&self, _: HotkeyEvent) {}
        fn raw_key(&self, _: RawKeyEvent) {}
    }

    /// Opens the real keyboards (read-only, without recording): never started automatically.
    #[test]
    #[ignore = "real input devices; set LINUXFLOW_TEST_PORTALS=1"]
    fn real_evdev() {
        if std::env::var("LINUXFLOW_TEST_PORTALS").as_deref() != Ok("1") {
            return;
        }
        let mut b = EvdevBackend::new(vec!["linuxflow".into()]);
        let req = StartRequest {
            spec: HotkeySpec::parse("Ctrl+Alt+Space").unwrap(),
            mode: ShortcutMode::Ptt,
        };
        println!("{:?}", b.start(&req, Arc::new(Null)));
        b.stop();
    }
}
