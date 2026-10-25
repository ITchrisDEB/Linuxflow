#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Tests of the worker as a real subprocess. `harness = false`: this same executable serves as a fake
//! worker when launched with `--engine-worker` (as the application binary will).
//! Behaviours simulated through `FAKE_*` environment variables passed to the worker.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use linuxflow_core::domain::{EnginePhase, Recording, TranscribeRequest};
use linuxflow_core::error::SttError;
use linuxflow_core::traits::SpeechToText;
use linuxflow_stt::{
    run_worker, worker_main, Backend, DecodeParams, EngineConfig, LoadFailure, LoadPlan,
    LoadedModel, RawTranscript, WorkerClient, WorkerConfig,
};

// ---- Fake worker --------------------------------------------------------------------------------

struct FakeBackend;
struct FakeModel {
    /// Number of this launch (0 = first), from the state file.
    run: usize,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Backend for FakeBackend {
    fn load(&self, _: &LoadPlan) -> Result<Box<dyn LoadedModel>, LoadFailure> {
        if env("FAKE_LOAD_FAIL").is_some() {
            return Err(LoadFailure::Recoverable("simulated failure".into()));
        }
        let mut run = 0;
        if let Some(state) = env("FAKE_STATE") {
            let previous = std::fs::read_to_string(&state).unwrap_or_default();
            run = previous.lines().count();
            let _ = std::fs::write(&state, format!("{previous}x\n"));
        }
        if let (Some(file), Some(_)) = (env("FAKE_GRANDCHILD_FILE"), env("FAKE_GRANDCHILD")) {
            if let Ok(child) = std::process::Command::new("sleep").arg("120").spawn() {
                let _ = std::fs::write(file, child.id().to_string());
            }
        }
        Ok(Box::new(FakeModel { run }))
    }
}

impl LoadedModel for FakeModel {
    fn transcribe(&mut self, audio: &[f32], p: &DecodeParams) -> Result<RawTranscript, String> {
        match env("FAKE_MODE").as_deref() {
            Some("always_crash") => std::process::exit(7),
            Some("crash_first_run") if self.run == 0 => std::process::exit(7),
            Some("hang") => std::thread::sleep(Duration::from_secs(60)),
            Some("fail") => return Err("simulated transcription failure".into()),
            _ => {}
        }
        // Echo: size received, checksum, language, prompt and a non-allowed variable.
        let sum: f32 = audio.iter().sum();
        Ok(RawTranscript {
            segments: vec![format!(
                "{}|{:.1}|{}|{}|{}",
                audio.len(),
                sum,
                p.language.clone().unwrap_or_default(),
                p.prompt.clone().unwrap_or_default(),
                std::env::var("UNLISTED_SECRET").unwrap_or_default()
            )],
            language: Some("xx".into()),
        })
    }
    fn warmup(&mut self) -> Result<(), String> {
        Ok(())
    }
}

// ---- Test tools -----------------------------------------------------------------------------

type TestResult = Result<(), String>;
type TestFn = fn() -> TestResult;

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

fn config(extra: &[(&str, &str)]) -> WorkerConfig {
    let mut c = WorkerConfig::new(
        vec![LoadPlan::cpu("ggml-fake.bin")],
        EngineConfig {
            warmup: false,
            ..EngineConfig::new()
        },
    );
    c.restart_delay = Duration::from_millis(150);
    c.extra_env = extra
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    c
}

fn request(samples: usize, hints: &[&str]) -> TranscribeRequest {
    TranscribeRequest {
        audio: Recording {
            samples: vec![0.5; samples],
        },
        language: Some("fr".into()),
        hints: hints.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn ready(client: &WorkerClient) -> bool {
    wait_until(Duration::from_secs(10), || client.info().ready)
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn tmp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("linuxflow-stt-test-{}-{name}", std::process::id()))
}

// ---- Tests --------------------------------------------------------------------------------------

fn basic_roundtrip() -> TestResult {
    let client = WorkerClient::start(config(&[]));
    if !ready(&client) {
        return Err(format!("never ready: {:?}", client.info()));
    }
    let out = client
        .transcribe(&request(160_000, &[" Alice ", "", "Bob"]))
        .map_err(|e| e.to_string())?;
    // 160,000 samples at 0.5: sum 80000.0 (binary audio intact), prompt cleaned.
    if out.text != "160000|80000.0|fr|Alice, Bob|" {
        return Err(format!("texte inattendu : {}", out.text));
    }
    if out.audio_ms != 10_000 || out.language.as_deref() != Some("fr") {
        return Err(format!("unexpected metadata: {out:?}"));
    }
    Ok(())
}

/// Pure cost of the pipe: 10 s of audio (640 KiB) round trip with an instant fake engine.
fn pipe_overhead_is_small() -> TestResult {
    let client = WorkerClient::start(config(&[]));
    if !ready(&client) {
        return Err("never ready".into());
    }
    let req = request(160_000, &[]);
    client.transcribe(&req).map_err(|e| e.to_string())?;
    let mut times = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        client.transcribe(&req).map_err(|e| e.to_string())?;
        times.push(t.elapsed().as_micros());
    }
    times.sort_unstable();
    println!(
        "MEASURE pipe: 10 s of audio, median {} us, max {} us (20 calls)",
        times[10], times[19]
    );
    if times[10] > 50_000 {
        return Err(format!("abnormal pipe overhead: {} us", times[10]));
    }
    Ok(())
}

/// Real model in the worker (if LINUXFLOW_TEST_MODEL_DIR and LINUXFLOW_TEST_SAMPLES_DIR):
/// compares the time measured by the engine (`stt_ms`) with the time seen by the caller.
fn real_model_through_worker() -> TestResult {
    let (Some(models), Some(samples)) = (
        std::env::var_os("LINUXFLOW_TEST_MODEL_DIR").map(PathBuf::from),
        std::env::var_os("LINUXFLOW_TEST_SAMPLES_DIR").map(PathBuf::from),
    ) else {
        println!("SKIP : variables LINUXFLOW_TEST_* absentes");
        return Ok(());
    };
    let wav = samples.join("dix.wav");
    let mut reader = hound::WavReader::open(&wav).map_err(|e| e.to_string())?;
    if reader.spec().sample_rate != 16_000 || reader.spec().channels != 1 {
        println!("SKIP: dix.wav must be 16 kHz mono");
        return Ok(());
    }
    let audio: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| f32::from(v) / 32768.0))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut cfg = WorkerConfig::new(
        vec![LoadPlan::cpu(models.join("ggml-small.bin"))],
        EngineConfig {
            vad_model_path: Some(models.join("ggml-silero-v6.2.0.bin")),
            ..EngineConfig::new()
        },
    );
    cfg.extra_env = vec![("FAKE_REAL".into(), "1".into())];
    let t = Instant::now();
    let client = WorkerClient::start(cfg);
    if !wait_until(Duration::from_secs(60), || client.info().ready) {
        return Err(format!("never ready: {:?}", client.info()));
    }
    println!(
        "MEASURE real worker: ready after {} ms (process + loading + warm-up)",
        t.elapsed().as_millis()
    );
    let req = TranscribeRequest {
        audio: Recording { samples: audio },
        language: Some("fr".into()),
        hints: vec![],
    };
    let t = Instant::now();
    let out = client.transcribe(&req).map_err(|e| e.to_string())?;
    let wall = t.elapsed().as_millis() as u64;
    println!(
        "MEASURE real worker: stt_ms={} wall={} ms => overhead={} ms; text={:?}",
        out.stt_ms,
        wall,
        wall.saturating_sub(out.stt_ms),
        out.text
    );
    if out.text.is_empty() {
        return Err("texte vide".into());
    }
    Ok(())
}

fn env_is_filtered() -> TestResult {
    // Variable outside the allow-list set in the parent: must not reach the worker.
    std::env::set_var("UNLISTED_SECRET", "fuite");
    let client = WorkerClient::start(config(&[]));
    if !ready(&client) {
        return Err("never ready".into());
    }
    let out = client
        .transcribe(&request(16_000, &[]))
        .map_err(|e| e.to_string())?;
    std::env::remove_var("UNLISTED_SECRET");
    if out.text.contains("fuite") {
        return Err(format!("variable transmise au worker : {}", out.text));
    }
    Ok(())
}

fn load_failure_is_reported() -> TestResult {
    let client = WorkerClient::start(config(&[("FAKE_LOAD_FAIL", "1")]));
    if !wait_until(Duration::from_secs(10), || {
        client.info().phase == EnginePhase::Error
    }) {
        return Err("Error state never reached".into());
    }
    match client.transcribe(&request(16_000, &[])) {
        Err(SttError::Failed(_)) => Ok(()),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

fn transcription_error_keeps_worker_alive() -> TestResult {
    let client = WorkerClient::start(config(&[("FAKE_MODE", "fail")]));
    ready(&client);
    match client.transcribe(&request(16_000, &[])) {
        Err(SttError::Failed(m)) if m.contains("simulated") => {}
        other => return Err(format!("unexpected answer: {other:?}")),
    }
    if client.restarts() != 0 || !client.info().ready {
        return Err("the worker must not be restarted".into());
    }
    Ok(())
}

fn crash_then_automatic_restart() -> TestResult {
    let state = tmp_file("state-restart");
    let _ = std::fs::remove_file(&state);
    let state_s = state.to_string_lossy().into_owned();
    let client = WorkerClient::start(config(&[
        ("FAKE_MODE", "crash_first_run"),
        ("FAKE_STATE", &state_s),
    ]));
    if !ready(&client) {
        return Err("never ready".into());
    }
    let first = client.transcribe(&request(16_000, &[]));
    match &first {
        Err(SttError::Failed(m)) if m.contains("(code 7)") => {}
        other => {
            return Err(format!(
                "expected \"Engine stopped (code 7)\", got {other:?}"
            ))
        }
    }
    // The error is exposed in EngineInfo (before the restart, at least briefly).
    let seen_error = wait_until(Duration::from_millis(100), || {
        client.info().error.is_some_and(|e| e.contains("(code 7)"))
    });
    if !seen_error {
        return Err(format!("EngineInfo.error absent : {:?}", client.info()));
    }
    let restarted = wait_until(Duration::from_secs(10), || {
        client.info().ready && client.restarts() == 1
    });
    if !restarted {
        return Err(format!("no restart: {:?}", client.info()));
    }
    client
        .transcribe(&request(16_000, &[]))
        .map_err(|e| format!("after restart: {e}"))?;
    // A success resets the counter to zero.
    if client.restarts() != 0 {
        return Err("restart counter not reset to zero".into());
    }
    let _ = std::fs::remove_file(&state);
    Ok(())
}

fn restarts_are_capped_at_two() -> TestResult {
    let client = WorkerClient::start(config(&[("FAKE_MODE", "always_crash")]));
    let mut crashes = 0;
    for _ in 0..6 {
        if !wait_until(Duration::from_secs(5), || client.info().ready) {
            break;
        }
        if client.transcribe(&request(16_000, &[])).is_err() {
            crashes += 1;
        }
    }
    std::thread::sleep(Duration::from_millis(600));
    if crashes != 3 || client.restarts() != 2 || client.info().ready {
        return Err(format!(
            "crashes={crashes}, relances={}, info={:?}",
            client.restarts(),
            client.info()
        ));
    }
    if client.worker_pid().is_some() {
        return Err("a worker is still running after giving up".into());
    }
    match client.info().error {
        Some(e) if e.contains("(code 7)") => Ok(()),
        other => Err(format!("erreur finale inattendue : {other:?}")),
    }
}

fn no_restart_if_never_ready() -> TestResult {
    let client = WorkerClient::start(config(&[("FAKE_DIE_ON_START", "1")]));
    if !wait_until(Duration::from_secs(5), || {
        client.info().error.is_some_and(|e| e.contains("(code 9)"))
    }) {
        return Err(format!("erreur « code 9 » absente : {:?}", client.info()));
    }
    std::thread::sleep(Duration::from_millis(500));
    if client.restarts() != 0 {
        return Err("restart although the engine had never been ready".into());
    }
    Ok(())
}

fn timeout_kills_and_restarts() -> TestResult {
    let mut c = config(&[("FAKE_MODE", "hang")]);
    c.transcribe_timeout = Duration::from_millis(400);
    let client = WorkerClient::start(c);
    ready(&client);
    let pid = client.worker_pid().ok_or("no pid")?;
    match client.transcribe(&request(16_000, &[])) {
        Err(SttError::Failed(m)) if m.contains("timeout") => {}
        other => return Err(format!("unexpected answer: {other:?}")),
    }
    if !wait_until(Duration::from_secs(3), || !alive(pid)) {
        return Err("blocked worker not killed".into());
    }
    if !wait_until(Duration::from_secs(10), || client.restarts() >= 1) {
        return Err("no restart after the timeout".into());
    }
    Ok(())
}

fn shutdown_leaves_no_zombie_nor_orphan() -> TestResult {
    let file = tmp_file("grandchild");
    let _ = std::fs::remove_file(&file);
    let file_s = file.to_string_lossy().into_owned();
    let client = WorkerClient::start(config(&[
        ("FAKE_GRANDCHILD", "1"),
        ("FAKE_GRANDCHILD_FILE", &file_s),
    ]));
    if !ready(&client) {
        return Err("never ready".into());
    }
    let pid = client.worker_pid().ok_or("no pid")?;
    if !wait_until(Duration::from_secs(2), || file.exists()) {
        return Err("grandchild not created".into());
    }
    let grandchild: u32 = std::fs::read_to_string(&file)
        .map_err(|e| e.to_string())?
        .trim()
        .parse()
        .map_err(|_| "pid illisible")?;
    if !alive(grandchild) {
        return Err("grandchild already dead".into());
    }
    let started = Instant::now();
    drop(client);
    if started.elapsed() > Duration::from_secs(3) {
        return Err(format!("stop too slow: {:?}", started.elapsed()));
    }
    // A zombie would stay visible in /proc: the worker must be reaped.
    if alive(pid) {
        return Err("worker still present (zombie?)".into());
    }
    if !wait_until(Duration::from_secs(3), || !alive(grandchild)) {
        return Err("the grandchild survived (tree not killed)".into());
    }
    let _ = std::fs::remove_file(&file);
    Ok(())
}

fn worker_dies_when_parent_pipe_closes() -> TestResult {
    // Without a client, a worker started by hand stops when stdin is closed (parent gone).
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(exe)
        .arg("--engine-worker")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    drop(child.stdin.take());
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("the orphan worker did not stop".into())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--engine-worker") {
        if env("FAKE_DIE_ON_START").is_some() {
            std::process::exit(9);
        }
        if env("FAKE_REAL").is_some() {
            std::process::exit(worker_main());
        }
        std::process::exit(run_worker(|_| Arc::new(FakeBackend)));
    }
    let tests: &[(&str, TestFn)] = &[
        ("basic_roundtrip", basic_roundtrip),
        ("pipe_overhead_is_small", pipe_overhead_is_small),
        ("real_model_through_worker", real_model_through_worker),
        ("env_is_filtered", env_is_filtered),
        ("load_failure_is_reported", load_failure_is_reported),
        (
            "transcription_error_keeps_worker_alive",
            transcription_error_keeps_worker_alive,
        ),
        ("crash_then_automatic_restart", crash_then_automatic_restart),
        ("restarts_are_capped_at_two", restarts_are_capped_at_two),
        ("no_restart_if_never_ready", no_restart_if_never_ready),
        ("timeout_kills_and_restarts", timeout_kills_and_restarts),
        (
            "shutdown_leaves_no_zombie_nor_orphan",
            shutdown_leaves_no_zombie_nor_orphan,
        ),
        (
            "worker_dies_when_parent_pipe_closes",
            worker_dies_when_parent_pipe_closes,
        ),
    ];
    let mut failed = 0;
    println!("running {} tests", tests.len());
    for (name, test) in tests {
        match test() {
            Ok(()) => println!("test {name} ... ok"),
            Err(e) => {
                failed += 1;
                println!("test {name} ... FAILED: {e}");
            }
        }
    }
    println!(
        "test result: {}. {} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" },
        tests.len() - failed
    );
    std::process::exit(i32::from(failed > 0));
}
