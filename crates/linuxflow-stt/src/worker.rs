//! Isolation of the engine in a child process of the same binary (`<exe> --engine-worker`).
//!
//! Protocol on the worker's stdin/stdout: each message is `u32 LE JSON length`,
//! `u32 LE binary length`, the JSON, then the raw bytes (`f32` little-endian audio for a
//! transcription). The worker reserves stdout for the protocol (descriptor 1 is redirected to
//! `/dev/null` for stray writes from the native code); its logs go to stderr, read back
//! by the client and republished through `tracing`.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use linuxflow_core::domain::{
    EngineInfo, EnginePhase, Recording, TranscribeRequest, Transcription,
};
use linuxflow_core::error::SttError;
use linuxflow_core::traits::SpeechToText;
use linuxflow_i18n::tr_fmt;
use serde::{Deserialize, Serialize};

use crate::engine::{Backend, EngineConfig, InProcessEngine, VadMode};
use crate::plan::LoadPlan;
use crate::whisper_backend::WhisperBackend;

const MAX_JSON_BYTES: usize = 8 << 20;
const MAX_BIN_BYTES: usize = 1 << 30;

// ---- Messages -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command_ {
    Init {
        plans: Vec<LoadPlan>,
        vad_model_path: Option<PathBuf>,
        n_threads: Option<usize>,
        warmup: bool,
        #[serde(default)]
        vad_mode: VadMode,
    },
    /// The audio follows in binary.
    Transcribe {
        language: Option<String>,
        hints: Vec<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Info {
        info: EngineInfo,
    },
    Result {
        text: String,
        language: Option<String>,
        stt_ms: u64,
        audio_ms: u64,
    },
    Failure {
        kind: FailureKind,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FailureKind {
    NotReady,
    NoModel,
    Failed,
}

fn write_frame<W: Write>(w: &mut W, json: &[u8], bin: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(8 + json.len() + bin.len());
    frame.extend_from_slice(&(json.len() as u32).to_le_bytes());
    frame.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    frame.extend_from_slice(json);
    frame.extend_from_slice(bin);
    w.write_all(&frame)?;
    w.flush()
}

fn send<W: Write, T: Serialize>(w: &mut W, message: &T, bin: &[u8]) -> io::Result<()> {
    let json = serde_json::to_vec(message).map_err(io::Error::other)?;
    write_frame(w, &json, bin)
}

/// Reads a message; `Ok(None)` if the stream is cleanly closed before a new header.
fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut header = [0u8; 8];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let json_len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let bin_len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if json_len > MAX_JSON_BYTES || bin_len > MAX_BIN_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message trop grand",
        ));
    }
    let mut json = vec![0u8; json_len];
    r.read_exact(&mut json)?;
    let mut bin = vec![0u8; bin_len];
    r.read_exact(&mut bin)?;
    Ok(Some((json, bin)))
}

fn samples_to_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn bytes_to_samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ---- Worker side --------------------------------------------------------------------------------

/// `--engine-worker` mode with the whisper.cpp engine. Returns the process exit code.
pub fn worker_main() -> i32 {
    run_worker(|config| Arc::new(WhisperBackend::new(config.clone())))
}

/// Worker loop with a backend of choice (the tests' fake worker passes its own).
pub fn run_worker(make_backend: impl FnOnce(&EngineConfig) -> Arc<dyn Backend>) -> i32 {
    die_with_parent();
    // Logs on stderr only; level set by RUST_LOG (default: info).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(io::stderr)
        .with_ansi(false)
        .try_init();

    // Protocol channel = copy of descriptor 1; descriptor 1 itself goes to /dev/null.
    let Ok(owned) = io::stdout().as_fd().try_clone_to_owned() else {
        return 2;
    };
    let out = Arc::new(Mutex::new(std::fs::File::from(owned)));
    redirect_stdout_to_null();

    let mut input = BufReader::new(io::stdin().lock());
    let init = match read_frame(&mut input) {
        Ok(Some((json, _))) => serde_json::from_slice::<Command_>(&json),
        _ => return 3,
    };
    let Ok(Command_::Init {
        plans,
        vad_model_path,
        n_threads,
        warmup,
        vad_mode,
    }) = init
    else {
        return 3;
    };
    let config = EngineConfig {
        vad_model_path,
        n_threads,
        warmup,
        vad_mode,
    };
    let engine = Arc::new(InProcessEngine::start(make_backend(&config), warmup, plans));

    // Pushes every state change to the client.
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (engine, out, done) = (Arc::clone(&engine), Arc::clone(&out), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut last: Option<EngineInfo> = None;
            while !done.load(Ordering::SeqCst) {
                let info = engine.info();
                if last.as_ref() != Some(&info) {
                    let _ = send(&mut *lock(&out), &Event::Info { info: info.clone() }, &[]);
                    last = Some(info);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        })
    };

    let mut code = 0;
    loop {
        let frame = match read_frame(&mut input) {
            Ok(Some(frame)) => frame,
            Ok(None) => break, // the parent closed the pipe: it is gone, we stop
            Err(e) => {
                tracing::error!("protocol read: {e}");
                code = 4;
                break;
            }
        };
        match serde_json::from_slice::<Command_>(&frame.0) {
            Ok(Command_::Shutdown) => break,
            Ok(Command_::Transcribe { language, hints }) => {
                let request = TranscribeRequest {
                    audio: Recording {
                        samples: bytes_to_samples(&frame.1),
                    },
                    language,
                    hints,
                };
                let event = match engine.transcribe(&request) {
                    Ok(t) => Event::Result {
                        text: t.text,
                        language: t.language,
                        stt_ms: t.stt_ms,
                        audio_ms: t.audio_ms,
                    },
                    Err(SttError::NotReady) => Event::Failure {
                        kind: FailureKind::NotReady,
                        message: String::new(),
                    },
                    Err(SttError::NoModel) => Event::Failure {
                        kind: FailureKind::NoModel,
                        message: String::new(),
                    },
                    Err(SttError::Failed(m)) => Event::Failure {
                        kind: FailureKind::Failed,
                        message: m,
                    },
                };
                if send(&mut *lock(&out), &event, &[]).is_err() {
                    break;
                }
            }
            Ok(Command_::Init { .. }) | Err(_) => tracing::warn!("unexpected command ignored"),
        }
    }
    done.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    code
}

/// Sends the writes on descriptor 1 to `/dev/null`.
fn redirect_stdout_to_null() {
    use std::os::fd::AsRawFd;
    if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
        // SAFETY: `dup2` on two valid descriptors (ours and 1); no shared memory.
        unsafe {
            libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO);
        }
    }
}

/// To call at the start of the host binary's `main`: runs `--engine-worker` or `--hardware-probe`
/// if that is the requested mode and then returns the exit code to use.
/// The worker dies with the application: if the parent is killed (SIGKILL), the kernel sends SIGKILL to the
/// worker instead of letting it finish an orphan transcription.
fn die_with_parent() {
    // SAFETY: `prctl(PR_SET_PDEATHSIG)` touches no memory; failure ignored (best effort).
    unsafe {
        libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGKILL as libc::c_ulong,
            0,
            0,
            0,
        );
    }
}

pub fn dispatch_subprocess_mode() -> Option<i32> {
    match std::env::args().nth(1).as_deref() {
        Some("--engine-worker") => Some(worker_main()),
        Some("--hardware-probe") => Some(crate::hardware::probe_main()),
        _ => None,
    }
}

// ---- Client side --------------------------------------------------------------------------------

/// Environment variables passed to the worker (allow-list, no other transfer).
pub fn env_allowed(name: &str) -> bool {
    const EXACT: [&str; 12] = [
        "PATH",
        "HOME",
        "LANG",
        "LD_LIBRARY_PATH",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "TMPDIR",
        "RUST_LOG",
        "RUST_BACKTRACE",
        "DRI_PRIME",
        "XDG_RUNTIME_DIR",
        "LANGUAGE",
    ];
    const PREFIXES: [&str; 8] = [
        "LC_",
        "XDG_",
        "LINUXFLOW_",
        "VK_",
        "GGML_",
        "MESA_",
        "__GLX",
        "__NV",
    ];
    EXACT.contains(&name) || PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Worker client settings.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Worker executable (default: the current executable). A second engine binary (variant
    /// CPU v2, for example) is chosen here.
    pub exe: Option<PathBuf>,
    /// Worker arguments (default: `--engine-worker`).
    pub args: Vec<String>,
    /// Environment variables added to the inherited allow-list.
    pub extra_env: Vec<(String, String)>,
    pub plans: Vec<LoadPlan>,
    pub engine: EngineConfig,
    /// Consecutive automatic restarts at most (2), if the engine had been ready.
    pub max_restarts: u32,
    pub restart_delay: Duration,
    /// Maximum duration of a transcription (120 s).
    pub transcribe_timeout: Duration,
    /// Wait for the voluntary stop before killing the process group (800 ms).
    pub shutdown_grace: Duration,
}

impl WorkerConfig {
    pub fn new(plans: Vec<LoadPlan>, engine: EngineConfig) -> Self {
        Self {
            exe: None,
            args: vec!["--engine-worker".to_string()],
            extra_env: Vec::new(),
            plans,
            engine,
            max_restarts: 2,
            restart_delay: Duration::from_secs(1),
            transcribe_timeout: Duration::from_secs(120),
            shutdown_grace: Duration::from_millis(800),
        }
    }
}

enum Reply {
    Event(Event),
    Died(String),
}

struct Conn {
    stdin: Mutex<ChildStdin>,
    pending: Mutex<Option<Sender<Reply>>>,
    pid: u32,
    /// True as long as the worker has not been reaped (`wait`). The signal is only sent under this
    /// lock: once the PID is reaped it may be reused, and it must never be targeted again.
    alive: Mutex<bool>,
}

struct ClientShared {
    info: Mutex<EngineInfo>,
    conn: Mutex<Option<Arc<Conn>>>,
    stop: AtomicBool,
    restarts: AtomicU32,
}

/// `SpeechToText` whose engine runs in a supervised child process.
pub struct WorkerClient {
    shared: Arc<ClientShared>,
    supervisor: Mutex<Option<std::thread::JoinHandle<()>>>,
    call_lock: Mutex<()>,
    config: WorkerConfig,
}

impl WorkerClient {
    /// Starts the worker and the supervisor; returns at once.
    pub fn start(config: WorkerConfig) -> Self {
        let shared = Arc::new(ClientShared {
            info: Mutex::new(EngineInfo {
                phase: EnginePhase::Starting,
                ..EngineInfo::default()
            }),
            conn: Mutex::new(None),
            stop: AtomicBool::new(false),
            restarts: AtomicU32::new(0),
        });
        let (s, c) = (Arc::clone(&shared), config.clone());
        let handle = std::thread::Builder::new()
            .name("linuxflow-stt-supervisor".into())
            .spawn(move || supervise(&s, &c))
            .ok();
        Self {
            shared,
            supervisor: Mutex::new(handle),
            call_lock: Mutex::new(()),
            config,
        }
    }

    /// Identifier of the current worker process, if it is running.
    pub fn worker_pid(&self) -> Option<u32> {
        lock(&self.shared.conn).as_ref().map(|c| c.pid)
    }

    /// Number of consecutive restarts already done.
    pub fn restarts(&self) -> u32 {
        self.shared.restarts.load(Ordering::SeqCst)
    }

    /// Clean stop: asks the worker to stop, then kills its process group and
    /// reaps it (no zombie). Idempotent.
    pub fn shutdown(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let conn = lock(&self.shared.conn).clone();
        if let Some(conn) = &conn {
            let _ = send(&mut *lock(&conn.stdin), &Command_::Shutdown, &[]);
        }
        let Some(handle) = lock(&self.supervisor).take() else {
            return;
        };
        let deadline = std::time::Instant::now() + self.config.shutdown_grace;
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !handle.is_finished() {
            if let Some(conn) = lock(&self.shared.conn).clone() {
                kill_conn(&conn);
            }
        }
        let _ = handle.join();
    }
}

impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Kills the worker's process group (the worker is its leader: `process_group(0)`).
fn kill_conn(conn: &Conn) {
    let alive = lock(&conn.alive);
    if *alive {
        kill_group(conn.pid);
    }
}

fn kill_group(pid: u32) {
    // SAFETY: `kill` with a negative pid targets the group created by us; no memory involved.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

fn spawn_worker(config: &WorkerConfig) -> io::Result<Child> {
    let exe = match &config.exe {
        Some(exe) => exe.clone(),
        None => std::env::current_exe()?,
    };
    let mut cmd = Command::new(exe);
    cmd.args(&config.args)
        .env_clear()
        .envs(std::env::vars().filter(|(k, _)| env_allowed(k)))
        .envs(config.extra_env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // New process group: the whole tree can be killed at once.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn()
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(-1)
}

fn set_info(shared: &ClientShared, f: impl FnOnce(&mut EngineInfo)) {
    f(&mut lock(&shared.info));
}

fn fail_engine(shared: &ClientShared, message: String) {
    set_info(shared, |i| {
        i.phase = EnginePhase::Error;
        i.ready = false;
        i.loading = false;
        i.error = Some(message);
    });
}

fn supervise(shared: &Arc<ClientShared>, config: &WorkerConfig) {
    loop {
        let mut child = match spawn_worker(config) {
            Ok(child) => child,
            Err(e) => {
                tracing::error!("could not start the worker: {e}");
                fail_engine(
                    shared,
                    tr_fmt(
                        "The speech engine could not be started: {}",
                        &[&e.to_string()],
                    ),
                );
                return;
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            kill_group(child.id());
            let _ = child.wait();
            fail_engine(shared, "worker pipes unavailable".to_string());
            return;
        };
        let conn = Arc::new(Conn {
            stdin: Mutex::new(stdin),
            pending: Mutex::new(None),
            pid: child.id(),
            alive: Mutex::new(true),
        });
        let stderr_thread = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                tracing::debug!(target: "linuxflow_stt::worker", "{line}");
            }
        });
        let init = Command_::Init {
            plans: config.plans.clone(),
            vad_model_path: config.engine.vad_model_path.clone(),
            n_threads: config.engine.n_threads,
            warmup: config.engine.warmup,
            vad_mode: config.engine.vad_mode,
        };
        let sent = send(&mut *lock(&conn.stdin), &init, &[]);
        *lock(&shared.conn) = Some(Arc::clone(&conn));
        set_info(shared, |i| {
            *i = EngineInfo {
                phase: EnginePhase::Loading,
                loading: true,
                ..EngineInfo::default()
            }
        });
        if let Err(e) = sent {
            tracing::error!("envoi de l'initialisation : {e}");
            kill_conn(&conn);
        }

        // Reading the events until the worker dies.
        let mut was_ready = false;
        let mut reader = BufReader::new(stdout);
        while let Ok(Some((json, _))) = read_frame(&mut reader) {
            match serde_json::from_slice::<Event>(&json) {
                Ok(Event::Info { info }) => {
                    was_ready |= info.ready;
                    *lock(&shared.info) = info;
                }
                Ok(event) => {
                    if let Some(tx) = lock(&conn.pending).take() {
                        let _ = tx.send(Reply::Event(event));
                    }
                }
                Err(e) => tracing::warn!("unreadable worker message: {e}"),
            }
        }
        // Stream closed: the worker is dead (or stopped). Any grandchildren are killed.
        kill_conn(&conn);
        // No signal must target this PID any more after the reaping.
        *lock(&conn.alive) = false;
        let code = child.wait().map(exit_code).unwrap_or(-1);
        // The stderr reading thread stops by itself when the pipe is closed: it is not
        // waited for (a grandchild that escaped the group could keep the pipe open).
        drop(stderr_thread);
        *lock(&shared.conn) = None;
        let message = tr_fmt("Engine stopped (code {})", &[&code.to_string()]);
        if let Some(tx) = lock(&conn.pending).take() {
            let _ = tx.send(Reply::Died(message.clone()));
        }
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        tracing::error!("{message}");
        fail_engine(shared, message);
        let restarts = shared.restarts.load(Ordering::SeqCst);
        if !was_ready || restarts >= config.max_restarts {
            return;
        }
        shared.restarts.fetch_add(1, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + config.restart_delay;
        while std::time::Instant::now() < deadline {
            if shared.stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl SpeechToText for WorkerClient {
    fn info(&self) -> EngineInfo {
        lock(&self.shared.info).clone()
    }

    fn transcribe(&self, request: &TranscribeRequest) -> Result<Transcription, SttError> {
        let _serial = lock(&self.call_lock);
        let info = self.info();
        let conn = lock(&self.shared.conn).clone();
        let conn = match conn {
            Some(conn) if info.ready => conn,
            _ => {
                return Err(match info.phase {
                    EnginePhase::Error => {
                        SttError::Failed(info.error.unwrap_or_else(|| "engine stopped".to_string()))
                    }
                    _ => SttError::NotReady,
                })
            }
        };
        let (tx, rx) = mpsc::channel();
        *lock(&conn.pending) = Some(tx);
        let command = Command_::Transcribe {
            language: request.language.clone(),
            hints: request.hints.clone(),
        };
        let audio = samples_to_bytes(&request.audio.samples);
        if let Err(e) = send(&mut *lock(&conn.stdin), &command, &audio) {
            *lock(&conn.pending) = None;
            return Err(SttError::Failed(format!(
                "envoi au worker impossible : {e}"
            )));
        }
        match rx.recv_timeout(self.config.transcribe_timeout) {
            Ok(Reply::Event(Event::Result {
                text,
                language,
                stt_ms,
                audio_ms,
            })) => {
                self.shared.restarts.store(0, Ordering::SeqCst);
                Ok(Transcription {
                    text,
                    language,
                    stt_ms,
                    audio_ms,
                })
            }
            Ok(Reply::Event(Event::Failure { kind, message })) => Err(match kind {
                FailureKind::NotReady => SttError::NotReady,
                FailureKind::NoModel => SttError::NoModel,
                FailureKind::Failed => SttError::Failed(message),
            }),
            Ok(Reply::Event(Event::Info { .. })) => {
                Err(SttError::Failed("unexpected worker answer".to_string()))
            }
            Ok(Reply::Died(message)) => Err(SttError::Failed(message)),
            Err(_) => {
                *lock(&conn.pending) = None;
                // Blocked worker: it is killed, the supervisor will restart it.
                kill_conn(&conn);
                Err(SttError::Failed("transcription timeout".to_string()))
            }
        }
    }
}

/// Utility table for the tests: filtered environment variables.
pub fn filtered_env(vars: impl IntoIterator<Item = (String, String)>) -> HashMap<String, String> {
    vars.into_iter().filter(|(k, _)| env_allowed(k)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        let audio = samples_to_bytes(&[0.5, -1.0, 0.25]);
        send(
            &mut buf,
            &Command_::Transcribe {
                language: Some("fr".into()),
                hints: vec!["a".into()],
            },
            &audio,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let mut cursor = io::Cursor::new(buf);
        let (json, bin) = read_frame(&mut cursor).unwrap_or(None).unwrap_or_default();
        assert!(matches!(
            serde_json::from_slice::<Command_>(&json),
            Ok(Command_::Transcribe { .. })
        ));
        assert_eq!(bytes_to_samples(&bin), vec![0.5, -1.0, 0.25]);
        assert!(read_frame(&mut cursor)
            .unwrap_or(Some(Default::default()))
            .is_none());
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut header = Vec::new();
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        assert!(read_frame(&mut io::Cursor::new(header)).is_err());
    }

    #[test]
    fn truncated_frame_is_an_error_not_a_hang() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{}", &[1, 2, 3, 4]).unwrap_or_else(|e| panic!("{e}"));
        buf.truncate(buf.len() - 2);
        assert!(read_frame(&mut io::Cursor::new(buf)).is_err());
    }

    #[test]
    fn environment_whitelist() {
        assert!(env_allowed("PATH"));
        assert!(env_allowed("LINUXFLOW_DEVICE"));
        assert!(env_allowed("VK_ICD_FILENAMES"));
        assert!(!env_allowed("AWS_SECRET_ACCESS_KEY"));
        assert!(!env_allowed("GITHUB_TOKEN"));
        let kept = filtered_env([("HOME".into(), "/h".into()), ("SECRET".into(), "x".into())]);
        assert_eq!(kept.len(), 1);
    }
}
