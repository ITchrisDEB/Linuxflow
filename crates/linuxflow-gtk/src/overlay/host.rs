//! `OverlayHost`: control of the pill by the application, in process or in a child process.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk4 as gtk;
use linuxflow_core::domain::Phase;

use crate::overlay::mode::OverlayMode;
use crate::overlay::msg::OverlayMsg;
use crate::overlay::window::{OverlayWindow, WindowKind};

/// Command-line argument that starts the binary in "pill process" mode.
pub const WORKER_ARG: &str = "--overlay-worker";

/// True if this binary can display the layer-shell pill itself (`layer-shell` feature).
pub const HAS_LAYER_SHELL: bool = cfg!(feature = "layer-shell");
/// Helper program shipped next to the application: layer-shell pill in a separate process.
pub const HELPER_NAME: &str = "linuxflow-overlay";
/// Helper argument: exits at once with 0 if the dynamic loader was able to start it.
pub const PROBE_ARG: &str = "--probe";
const PROBE_DEADLINE: Duration = Duration::from_secs(3);

/// Does the helper start? (It fails at load time if `libgtk4-layer-shell.so.0` is missing.)
pub fn helper_starts(path: &std::path::Path) -> bool {
    let Ok(mut child) = Command::new(path)
        .arg(PROBE_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < PROBE_DEADLINE => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Layer-shell pill entrusted to the helper next to the current executable; `None` if it is missing
/// or does not start (library absent): the caller then falls back to XWayland.
pub fn layer_shell_child() -> Option<ChildOverlay> {
    let helper = std::env::current_exe().ok()?.parent()?.join(HELPER_NAME);
    if !helper.is_file() || !helper_starts(&helper) {
        tracing::info!("pill: layer-shell helper program unavailable, falling back to XWayland");
        return None;
    }
    Some(ChildOverlay::with_command(
        helper,
        Vec::new(),
        vec![
            ("GDK_BACKEND".into(), "wayland".into()),
            ("LANGUAGE".into(), linuxflow_i18n::current_language()),
        ],
    ))
}

/// Control of the pill. The hiding delay (420 ms after rest) is handled by
/// the application: `hide()` hides immediately (with an animation).
pub trait OverlayHost {
    /// Applies a message (from `Event::Overlay` / `Event::Level`).
    fn push(&self, msg: OverlayMsg);
    /// Shows the pill in its current phase.
    fn show(&self) {
        self.push(OverlayMsg {
            visible: Some(true),
            ..OverlayMsg::default()
        });
    }
    /// Hides the pill.
    fn hide(&self) {
        self.push(OverlayMsg::hide());
    }
}

/// Host that does nothing (`Disabled` mode).
pub struct NullOverlay;

impl OverlayHost for NullOverlay {
    fn push(&self, _msg: OverlayMsg) {}
}

/// Creates the host suited to the chosen mode. The in-process modes require the GTK main thread.
pub fn create_host(mode: OverlayMode) -> Box<dyn OverlayHost> {
    match mode {
        OverlayMode::LayerShell if HAS_LAYER_SHELL => Box::new(InProcessOverlay::new()),
        OverlayMode::LayerShell => match layer_shell_child() {
            Some(child) => Box::new(child),
            // Without the helper: the same fallback as GNOME, if an X server (XWayland) exists.
            None if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) => {
                Box::new(ChildOverlay::new())
            }
            None => Box::new(NullOverlay),
        },
        OverlayMode::X11Native => Box::new(InProcessOverlay::new()),
        OverlayMode::ChildXWayland => Box::new(ChildOverlay::new()),
        OverlayMode::Disabled => Box::new(NullOverlay),
    }
}

/// Pill in the current process; the window is created at the first message.
#[derive(Default)]
pub struct InProcessOverlay {
    window: std::cell::RefCell<Option<OverlayWindow>>,
    failed: std::cell::Cell<bool>,
}

impl InProcessOverlay {
    pub fn new() -> Self {
        Self::default()
    }
}

impl OverlayHost for InProcessOverlay {
    fn push(&self, msg: OverlayMsg) {
        if self.failed.get() {
            return;
        }
        let mut slot = self.window.borrow_mut();
        if slot.is_none() {
            // Nothing to create for a simple hide or a level.
            if msg.phase.is_none_or(|p| p == Phase::Idle) && msg.visible != Some(true) {
                return;
            }
            match WindowKind::detect() {
                Some(kind) => *slot = Some(OverlayWindow::new(kind)),
                None => {
                    tracing::warn!("pill: no GDK display, disabled");
                    self.failed.set(true);
                    return;
                }
            }
        }
        if let Some(win) = slot.as_ref() {
            win.apply(&msg);
        }
    }
}

/// Pill in a child process (same binary, `--overlay-worker`, `GDK_BACKEND=x11`).
pub struct ChildOverlay {
    tx: Option<mpsc::Sender<OverlayMsg>>,
    join: Option<JoinHandle<()>>,
}

impl Default for ChildOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl ChildOverlay {
    /// Standard child: `current_exe() --overlay-worker` with `GDK_BACKEND=x11`.
    pub fn new() -> Self {
        match std::env::current_exe() {
            Ok(exe) => Self::with_command(
                exe,
                vec![WORKER_ARG.into()],
                vec![("GDK_BACKEND".into(), "x11".into())],
            ),
            Err(e) => {
                tracing::warn!("pill: current executable not found: {e}");
                Self {
                    tx: None,
                    join: None,
                }
            }
        }
    }

    /// Arbitrary command (tests).
    pub fn with_command(program: PathBuf, args: Vec<String>, envs: Vec<(String, String)>) -> Self {
        let (tx, rx) = mpsc::channel();
        let spawner = Spawner {
            program,
            args,
            envs,
        };
        let join = std::thread::Builder::new()
            .name("overlay-child".into())
            .spawn(move || supervise(spawner, rx))
            .ok();
        Self {
            tx: join.is_some().then_some(tx),
            join,
        }
    }
}

impl OverlayHost for ChildOverlay {
    fn push(&self, msg: OverlayMsg) {
        if let Some(tx) = &self.tx {
            // Failure = thread stopped: give up silently.
            let _ = tx.send(msg);
        }
    }
}

impl Drop for ChildOverlay {
    fn drop(&mut self) {
        // Closing the channel stops the thread, which closes the child's pipe (clean stop).
        self.tx.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Spawner {
    program: PathBuf,
    args: Vec<String>,
    envs: Vec<(String, String)>,
}

impl Spawner {
    fn spawn(&self) -> std::io::Result<(Child, ChildStdin)> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for (k, v) in &self.envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        match child.stdin.take() {
            Some(stdin) => Ok((child, stdin)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err(std::io::Error::other("stdin de l'enfant indisponible"))
            }
        }
    }
}

const MAX_FAST_FAILURES: u32 = 5;
const HEALTHY_AFTER: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_millis(1500);

struct Live {
    child: Child,
    stdin: ChildStdin,
    started: Instant,
}

fn supervise(spawner: Spawner, rx: mpsc::Receiver<OverlayMsg>) {
    let mut live: Option<Live> = None;
    let mut failures = 0u32;
    let mut last_spawn: Option<Instant> = None;
    // Last "displayed" state, replayed after a restart.
    let mut last_state: Option<OverlayMsg> = None;

    while let Ok(msg) = rx.recv() {
        match msg.phase {
            Some(Phase::Idle) => last_state = None,
            Some(_) => {
                last_state = Some(OverlayMsg {
                    level: None,
                    visible: None,
                    ..msg.clone()
                })
            }
            None => {}
        }
        let needs_child = !matches!(msg.phase, None | Some(Phase::Idle))
            || msg.visible == Some(true)
            || last_state.is_some();
        // Enfant mort ?
        if let Some(l) = live.as_mut() {
            if !matches!(l.child.try_wait(), Ok(None)) {
                failures = account_death(failures, l.started);
                live = None;
            }
        }
        for _ in 0..2 {
            if live.is_none() {
                if !needs_child {
                    break; // masquage ou niveau sans pastille active : inutile de lancer un enfant
                }
                if failures >= MAX_FAST_FAILURES {
                    break; // too many failures: pill abandoned, the application continues
                }
                if last_spawn.is_some_and(|t| t.elapsed() < Duration::from_millis(300) * failures) {
                    break;
                }
                last_spawn = Some(Instant::now());
                match spawner.spawn() {
                    Ok((child, mut stdin)) => {
                        // After a restart, replay the current state (unless this message carries one).
                        if msg.phase.is_none() {
                            if let Some(state) = last_state.as_ref() {
                                let _ = write_msg(&mut stdin, state);
                            }
                        }
                        live = Some(Live {
                            child,
                            stdin,
                            started: Instant::now(),
                        });
                    }
                    Err(e) => {
                        tracing::warn!("pastille : lancement de l'enfant impossible : {e}");
                        failures += 1;
                        break;
                    }
                }
            }
            let Some(l) = live.as_mut() else { break };
            match write_msg(&mut l.stdin, &msg) {
                Ok(()) => break,
                Err(_) => {
                    failures = account_death(failures, l.started);
                    if let Some(mut dead) = live.take() {
                        let _ = dead.child.kill();
                        let _ = dead.child.wait();
                    }
                }
            }
        }
    }
    if let Some(l) = live {
        stop_child(l);
    }
}

fn account_death(failures: u32, started: Instant) -> u32 {
    if started.elapsed() > HEALTHY_AFTER {
        0
    } else {
        failures + 1
    }
}

fn write_msg(stdin: &mut ChildStdin, msg: &OverlayMsg) -> std::io::Result<()> {
    let mut line = msg.to_line().map_err(std::io::Error::other)?;
    line.push('\n');
    stdin.write_all(line.as_bytes())?;
    stdin.flush()
}

/// Closes the pipe (the child leaves its loop), waits a little, then forces the stop.
fn stop_child(live: Live) {
    let Live {
        mut child, stdin, ..
    } = live;
    drop(stdin);
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Entry point of the child process (`--overlay-worker`): reads the JSON messages on stdin,
/// drives the pill, exits when the pipe is closed.
pub fn worker_main() -> ExitCode {
    // The child process does not inherit the application's catalogue: it initialises it itself.
    // The language comes from the environment (`LANGUAGE`, set by the application at launch).
    linuxflow_i18n::init(linuxflow_i18n::DEFAULT_DOMAIN, None);
    if gtk::init().is_err() {
        return ExitCode::from(2);
    }
    let Some(kind) = WindowKind::detect() else {
        return ExitCode::from(2);
    };
    let window = OverlayWindow::new(kind);
    let (tx, rx) = async_channel::unbounded::<Option<OverlayMsg>>();
    let reader = std::thread::Builder::new()
        .name("overlay-stdin".into())
        .spawn(move || {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match OverlayMsg::from_line(&line) {
                    Ok(msg) => {
                        if tx.send_blocking(Some(msg)).is_err() {
                            return;
                        }
                    }
                    Err(e) => tracing::debug!("pill: line ignored ({e})"),
                }
            }
            let _ = tx.send_blocking(None);
        });
    if reader.is_err() {
        return ExitCode::from(2);
    }
    let main_loop = glib::MainLoop::new(None, false);
    let quit = main_loop.clone();
    glib::spawn_future_local(async move {
        while let Ok(Some(msg)) = rx.recv().await {
            window.apply(&msg);
        }
        quit.quit();
    });
    main_loop.run();
    ExitCode::SUCCESS
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    fn script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn the_helper_probe_accepts_only_a_clean_exit() {
        let d = tempfile::tempdir().unwrap();
        // successful load: exits 0 at once
        // a concurrent fork from another test can briefly keep the script open for writing (ETXTBSY)
        let ok = script(d.path(), "ok", "exit 0");
        assert!((0..20).any(|_| {
            let started = helper_starts(&ok);
            if !started {
                std::thread::sleep(Duration::from_millis(50));
            }
            started
        }));
        // missing library: the dynamic loader exits with 127
        assert!(!helper_starts(&script(d.path(), "missing-lib", "exit 127")));
        // programme absent
        assert!(!helper_starts(&d.path().join("absent")));
    }

    #[test]
    fn a_helper_that_hangs_is_killed_and_refused() {
        let d = tempfile::tempdir().unwrap();
        let started = Instant::now();
        assert!(!helper_starts(&script(d.path(), "hang", "sleep 30")));
        assert!(started.elapsed() < Duration::from_secs(6));
    }

    use super::*;

    fn sh(script: &str) -> ChildOverlay {
        ChildOverlay::with_command(
            PathBuf::from("sh"),
            vec!["-c".into(), script.into()],
            vec![],
        )
    }

    fn lines(path: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn messages_reach_the_child_as_json_lines_and_pipe_closes_on_drop() {
        let dir = tempfile::tempdir().expect("tmp");
        let out = dir.path().join("out.txt");
        let host = sh(&format!("cat >> '{}'", out.display()));
        host.push(OverlayMsg::phase(Phase::Listening));
        host.push(OverlayMsg::level(0.5));
        host.push(OverlayMsg::done(600));
        drop(host); // closes the pipe, waits for the child to end
        assert_eq!(
            lines(&out),
            vec![
                r#"{"phase":"listening"}"#,
                r#"{"level":0.5}"#,
                r#"{"phase":"done","latency_ms":600}"#
            ]
        );
    }

    #[test]
    fn dead_child_is_restarted_and_state_replayed() {
        let dir = tempfile::tempdir().expect("tmp");
        let out = dir.path().join("out.txt");
        // Each child reads only one line then dies.
        let host = sh(&format!("head -n1 >> '{}'", out.display()));
        host.push(OverlayMsg::phase(Phase::Transcribing));
        std::thread::sleep(Duration::from_millis(400));
        host.push(OverlayMsg::level(0.3));
        std::thread::sleep(Duration::from_millis(600));
        drop(host);
        // 2nd child: the "transcribing" state is replayed before the level.
        assert_eq!(
            lines(&out),
            vec![r#"{"phase":"transcribing"}"#, r#"{"phase":"transcribing"}"#]
        );
    }

    #[test]
    fn no_replay_after_hide() {
        let dir = tempfile::tempdir().expect("tmp");
        let out = dir.path().join("out.txt");
        let host = sh(&format!("head -n1 >> '{}'", out.display()));
        host.push(OverlayMsg::phase(Phase::Listening));
        std::thread::sleep(Duration::from_millis(300));
        host.push(OverlayMsg::hide());
        std::thread::sleep(Duration::from_millis(400));
        host.push(OverlayMsg::level(0.3));
        std::thread::sleep(Duration::from_millis(400));
        drop(host);
        // Neither a restart to hide, nor a replay of the old state.
        assert_eq!(lines(&out), vec![r#"{"phase":"listening"}"#]);
    }

    #[test]
    fn missing_program_gives_up_without_panic() {
        let host = ChildOverlay::with_command(
            PathBuf::from("/nonexistent/linuxflow-test-binary"),
            vec![],
            vec![],
        );
        for _ in 0..10 {
            host.push(OverlayMsg::phase(Phase::Listening));
        }
        drop(host);
    }

    #[test]
    fn null_overlay_and_create_host_disabled_are_inert() {
        create_host(OverlayMode::Disabled).push(OverlayMsg::phase(Phase::Listening));
    }
}
