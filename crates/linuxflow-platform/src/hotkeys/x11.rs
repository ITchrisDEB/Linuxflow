//! X11 backend: `XGrabKey` on all the lock combinations, detectable auto-repeat,
//! `BadAccess` handled; raw XInput2 (passive) for modifiers alone.

use super::backend::{BackendError, BackendErrorKind, BackendReport, HotkeyBackend, StartRequest};
use super::clock::now_ms;
use super::normalize::{from_xi2, KeyNormalizer};
use super::select::BackendKind;
use super::x11_plan::{lock_masks_from_mapping, plan_grabs, GrabPlan};
use linuxflow_core::traits::{HotkeyEvent, HotkeySink};
use linuxflow_i18n::tr;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

pub struct X11Backend {
    stop: Arc<AtomicBool>,
    suspended: Arc<AtomicBool>,
    wake: Option<UnixStream>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl X11Backend {
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            suspended: Arc::new(AtomicBool::new(false)),
            wake: None,
            thread: None,
        }
    }
}

impl Default for X11Backend {
    fn default() -> Self {
        Self::new()
    }
}

fn unavailable(msg: String) -> BackendError {
    BackendError::new(BackendErrorKind::Unavailable, msg)
}

fn grab_all(conn: &RustConnection, root: u32, plans: &[GrabPlan]) -> Result<(), BackendError> {
    for p in plans {
        let cookie = conn
            .grab_key(
                false,
                root,
                ModMask::from(p.modifiers),
                p.keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|e| BackendError::new(BackendErrorKind::Failed, e.to_string()))?;
        if let Err(e) = cookie.check() {
            ungrab_all(conn, root, plans);
            return Err(match e {
                ReplyError::X11Error(x) if x.error_kind == x11rb::protocol::ErrorKind::Access => {
                    BackendError::new(
                        BackendErrorKind::Conflict,
                        tr("This key combination is already used by another application."),
                    )
                }
                other => BackendError::new(BackendErrorKind::Failed, other.to_string()),
            });
        }
    }
    Ok(())
}

fn ungrab_all(conn: &RustConnection, root: u32, plans: &[GrabPlan]) {
    for p in plans {
        let _ = conn.ungrab_key(p.keycode, root, ModMask::from(p.modifiers));
    }
    let _ = conn.flush();
}

impl HotkeyBackend for X11Backend {
    fn kind(&self) -> BackendKind {
        BackendKind::X11
    }

    fn start(
        &mut self,
        req: &StartRequest,
        sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError> {
        self.stop();
        let spec = req.spec;
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| unavailable(format!("{} ({e})", tr("Cannot connect to the X server."))))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .map(|s| s.root)
            .ok_or_else(|| unavailable(tr("No X11 screen.")))?;

        // Detectable auto-repeat: no false releases while the key is held.
        if conn
            .xkb_use_extension(1, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some()
        {
            let flag = xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT;
            let none = xkb::BoolCtrl::from(0u8);
            let _ = conn
                .xkb_per_client_flags(xkb::ID::USE_CORE_KBD.into(), flag, flag, none, none, none)
                .ok()
                .and_then(|c| c.reply().ok());
        }

        let modifier_only = spec.is_modifier_only();
        let plans = if modifier_only {
            Vec::new()
        } else {
            let mapping = conn
                .get_modifier_mapping()
                .ok()
                .and_then(|c| c.reply().ok());
            let locks = mapping
                .map(|m| {
                    lock_masks_from_mapping(usize::from(m.keycodes_per_modifier()), &m.keycodes)
                })
                .unwrap_or_default();
            plan_grabs(&spec, locks).map_err(|m| BackendError::new(BackendErrorKind::Failed, m))?
        };
        grab_all(&conn, root, &plans)?;

        // Raw XInput2: modifiers alone (and reliable modifier state).
        let mut raw = false;
        if modifier_only {
            let ok = conn
                .xinput_xi_query_version(2, 0)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some();
            if ok {
                let mask =
                    xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE;
                let ev = xinput::EventMask {
                    deviceid: 1,
                    mask: vec![mask],
                };
                raw = conn
                    .xinput_xi_select_events(root, &[ev])
                    .ok()
                    .map(|c| c.check().is_ok())
                    .unwrap_or(false);
            }
            if !raw {
                return Err(unavailable(tr(
                    "XInput2 is not available: modifier-only shortcuts cannot work.",
                )));
            }
        }
        let _ = conn.flush();

        let (wake_tx, wake_rx) = UnixStream::pair()
            .map_err(|e| BackendError::new(BackendErrorKind::Failed, e.to_string()))?;
        let _ = wake_rx.set_nonblocking(true);
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let suspended = self.suspended.clone();
        let keycode = plans.first().map(|p| p.keycode);
        let thread = std::thread::Builder::new()
            .name("lf-x11-hotkey".into())
            .spawn(move || {
                event_loop(
                    conn, root, plans, keycode, raw, sink, stop, suspended, wake_rx,
                )
            })
            .map_err(|e| BackendError::new(BackendErrorKind::Failed, e.to_string()))?;
        self.thread = Some(thread);
        self.wake = Some(wake_tx);
        Ok(BackendReport {
            supports_release: true,
            supports_modifier_only: true,
            trigger_description: spec.display(),
            provides_hotkey_events: !modifier_only,
            provides_raw_keys: modifier_only,
            note: None,
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
        if let Some(w) = self.wake.as_mut() {
            let _ = w.write_all(&[1]);
        }
    }
}

impl Drop for X11Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    conn: RustConnection,
    root: u32,
    plans: Vec<GrabPlan>,
    keycode: Option<u8>,
    raw: bool,
    sink: Arc<dyn HotkeySink>,
    stop: Arc<AtomicBool>,
    suspended: Arc<AtomicBool>,
    mut wake: UnixStream,
) {
    use std::io::Read;
    let mut grabbed = true;
    let mut norm = KeyNormalizer::new();
    let fd = conn.stream().as_raw_fd();
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // Suspension: the grabs are released so that the combination serves elsewhere.
        let want_grab = !suspended.load(Ordering::SeqCst);
        if want_grab != grabbed && !plans.is_empty() {
            if want_grab {
                grabbed = grab_all(&conn, root, &plans).is_ok();
            } else {
                ungrab_all(&conn, root, &plans);
                grabbed = false;
            }
        }
        // Drains the internal buffer first, then waits on the socket or the wake-up.
        loop {
            match conn.poll_for_event() {
                Ok(Some(ev)) => handle_event(ev, keycode, raw, &sink, &mut norm, &suspended),
                Ok(None) => break,
                Err(_) => {
                    // Connection lost: stop (the manager does not crash again).
                    return;
                }
            }
        }
        let mut fds = [
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a valid array of 2 `pollfd`s; the descriptors stay open during the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, 1000) };
        if n > 0 && fds[1].revents != 0 {
            let mut buf = [0u8; 16];
            let _ = wake.read(&mut buf);
        }
        if n > 0 && fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return;
        }
    }
    if !plans.is_empty() {
        ungrab_all(&conn, root, &plans);
    }
}

fn handle_event(
    ev: Event,
    keycode: Option<u8>,
    raw: bool,
    sink: &Arc<dyn HotkeySink>,
    norm: &mut KeyNormalizer,
    suspended: &AtomicBool,
) {
    match ev {
        Event::KeyPress(e) if Some(e.detail) == keycode => {
            sink.hotkey(HotkeyEvent::Activated { ts_ms: now_ms() })
        }
        Event::KeyRelease(e) if Some(e.detail) == keycode => {
            sink.hotkey(HotkeyEvent::Deactivated { ts_ms: now_ms() })
        }
        Event::XinputRawKeyPress(e) if raw && !suspended.load(Ordering::SeqCst) => {
            let repeat = u32::from(e.flags) & u32::from(xinput::KeyEventFlags::KEY_REPEAT) != 0;
            forward(from_xi2(e.detail, true, repeat), norm, sink);
        }
        Event::XinputRawKeyRelease(e) if raw && !suspended.load(Ordering::SeqCst) => {
            forward(from_xi2(e.detail, false, false), norm, sink);
        }
        _ => {}
    }
}

fn forward(item: Option<(u16, i32)>, norm: &mut KeyNormalizer, sink: &Arc<dyn HotkeySink>) {
    if let Some((code, value)) = item {
        if let Some(ev) = norm.handle(0, code, value, now_ms()) {
            sink.raw_key(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::spec::HotkeySpec;
    use linuxflow_core::domain::ShortcutMode;

    struct Null;
    impl HotkeySink for Null {
        fn hotkey(&self, _: HotkeyEvent) {}
        fn raw_key(&self, _: linuxflow_core::traits::RawKeyEvent) {}
    }

    /// Sets real grabs on the X server: never started automatically.
    #[test]
    #[ignore = "real X server; set LINUXFLOW_TEST_PORTALS=1"]
    fn real_grab() {
        if std::env::var("LINUXFLOW_TEST_PORTALS").as_deref() != Ok("1") {
            return;
        }
        let mut b = X11Backend::new();
        let req = StartRequest {
            spec: HotkeySpec::parse("Ctrl+Alt+F12").unwrap(),
            mode: ShortcutMode::Toggle,
        };
        let r = b.start(&req, Arc::new(Null));
        println!("{r:?}");
        b.stop();
    }
}
