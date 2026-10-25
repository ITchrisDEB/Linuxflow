//! Engine in the current process: background loading, `EngineInfo` states, fallback
//! chain, serialisation lock. The native computation is behind the [`Backend`] trait (whisper.cpp
//! in production, a fake backend in the tests).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use linuxflow_core::domain::{
    DeviceKind, EngineInfo, EnginePhase, TranscribeRequest, Transcription,
};
use linuxflow_core::error::SttError;
use linuxflow_core::traits::SpeechToText;
use linuxflow_i18n::{tr, tr_fmt};

use crate::cpu_guard;
use crate::plan::{
    build_initial_prompt, describe_model, join_segments, normalize_language, Device, LoadPlan,
};
use crate::whisper_backend::WhisperBackend;

/// Use of the Silero VAD before decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum VadMode {
    /// No VAD (silence can produce hallucinations).
    Off,
    /// Splits the speech into segments and glues them back: degrades recognition (measured).
    Segments,
    /// Detects the absence of speech (empty text, decoder not called) and trims the leading silence
    /// and end with margins; internal pauses stay intact.
    #[default]
    Trim,
}

/// Common engine settings.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// Silero VAD model (GGML). Absent or not found: VAD disabled (warning).
    pub vad_model_path: Option<PathBuf>,
    /// Number of compute threads (default: available cores, at most 8).
    pub n_threads: Option<usize>,
    /// Warm-up after loading (1 s of silence). Can be disabled for the tests.
    pub warmup: bool,
    pub vad_mode: VadMode,
}

impl EngineConfig {
    pub fn new() -> Self {
        Self {
            vad_model_path: None,
            n_threads: None,
            warmup: true,
            vad_mode: VadMode::default(),
        }
    }
}

/// Decoding parameters specific to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeParams {
    /// `None` = automatic detection.
    pub language: Option<String>,
    /// Initial prompt (joined hints), already cleaned.
    pub prompt: Option<String>,
}

/// Raw result of a loaded model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawTranscript {
    pub segments: Vec<String>,
    pub language: Option<String>,
}

/// Loading failure of a step of the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadFailure {
    /// Move on to the next step (file missing, insufficient memory, Vulkan unavailable…).
    Recoverable(String),
    /// No use trying the rest (e.g. incompatible processor).
    Fatal(String),
}

/// Loaded model, ready to transcribe. Used under a lock: one call at a time.
pub trait LoadedModel: Send {
    fn transcribe(&mut self, audio: &[f32], params: &DecodeParams)
        -> Result<RawTranscript, String>;
    /// Warm-up (1 s of silence, beam 1, no VAD). A failure is only a warning.
    fn warmup(&mut self) -> Result<(), String>;
}

/// Factory of loaded models.
pub trait Backend: Send + Sync + 'static {
    fn load(&self, plan: &LoadPlan) -> Result<Box<dyn LoadedModel>, LoadFailure>;
}

struct Shared {
    info: Mutex<EngineInfo>,
    /// The loaded model: the lock serialises the transcriptions.
    model: Mutex<Option<Box<dyn LoadedModel>>>,
    config_warmup: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Engine run in the current process (tests, or inside the worker).
pub struct InProcessEngine {
    shared: Arc<Shared>,
    loader: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl InProcessEngine {
    /// Starts the loading in the background. Returns at once (`Starting`/`Loading` phase).
    pub fn start(backend: Arc<dyn Backend>, warmup: bool, plans: Vec<LoadPlan>) -> Self {
        let shared = Arc::new(Shared {
            info: Mutex::new(EngineInfo {
                phase: EnginePhase::Starting,
                ..EngineInfo::default()
            }),
            model: Mutex::new(None),
            config_warmup: warmup,
        });
        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("linuxflow-stt-load".into())
            .spawn(move || load_chain(&thread_shared, backend.as_ref(), &plans))
            .map_err(|e| tracing::error!("thread de chargement impossible : {e}"))
            .ok();
        if handle.is_none() {
            set_error(&shared, tr("The speech engine could not start."));
        }
        Self {
            shared,
            loader: Mutex::new(handle),
        }
    }

    /// Waits for the end of the loading (tests, tools). Returns the final state.
    pub fn wait_loaded(&self) -> EngineInfo {
        if let Some(handle) = lock(&self.loader).take() {
            let _ = handle.join();
        }
        self.info()
    }
}

fn update(shared: &Shared, f: impl FnOnce(&mut EngineInfo)) {
    f(&mut lock(&shared.info));
}

fn set_error(shared: &Shared, message: String) {
    update(shared, |i| {
        i.phase = EnginePhase::Error;
        i.ready = false;
        i.loading = false;
        i.error = Some(message);
    });
}

fn load_chain(shared: &Shared, backend: &dyn Backend, plans: &[LoadPlan]) {
    update(shared, |i| {
        i.phase = EnginePhase::Loading;
        i.loading = true;
    });
    // CPU guard before any native call: a SIGILL cannot be caught.
    if let Err(err) = cpu_guard::ensure_cpu_supported() {
        tracing::error!("{err}");
        set_error(
            shared,
            tr_fmt(
                "This processor lacks instructions required by the speech engine: {}",
                &[&err.missing.join(", ")],
            ),
        );
        return;
    }
    if plans.is_empty() {
        set_error(shared, tr("No speech model is installed."));
        return;
    }
    let mut last_error = String::new();
    for plan in plans {
        let (name, precision) = describe_model(&plan.model_path);
        update(shared, |i| {
            i.model = Some(name.clone());
            i.precision = precision.clone();
            i.device = device_kind(plan.device);
            i.device_name = plan.device_name.clone();
            i.error = None;
        });
        let started = Instant::now();
        let loaded = catch_unwind(AssertUnwindSafe(|| backend.load(plan)))
            .unwrap_or_else(|_| Err(LoadFailure::Recoverable("panic during loading".into())));
        match loaded {
            Ok(mut model) => {
                if shared.config_warmup {
                    if let Err(e) = catch_unwind(AssertUnwindSafe(|| model.warmup()))
                        .unwrap_or_else(|_| Err("panic during warm-up".into()))
                    {
                        tracing::warn!("warm-up failed: {e}");
                    }
                }
                let load_ms = started.elapsed().as_millis() as u64;
                *lock(&shared.model) = Some(model);
                update(shared, |i| {
                    i.phase = EnginePhase::Ready;
                    i.ready = true;
                    i.loading = false;
                    i.error = None;
                    i.load_ms = Some(load_ms);
                });
                tracing::info!(?plan, load_ms, "engine ready");
                return;
            }
            Err(LoadFailure::Recoverable(msg)) => {
                tracing::warn!(?plan, "loading impossible, next step: {msg}");
                last_error = msg;
            }
            Err(LoadFailure::Fatal(msg)) => {
                tracing::error!(?plan, "chargement impossible : {msg}");
                set_error(shared, msg);
                return;
            }
        }
    }
    set_error(
        shared,
        tr_fmt("The speech model could not be loaded: {}", &[&last_error]),
    );
}

fn device_kind(device: Device) -> DeviceKind {
    match device {
        Device::Cpu => DeviceKind::Cpu,
        Device::Gpu(_) => DeviceKind::Gpu,
    }
}

impl SpeechToText for InProcessEngine {
    fn info(&self) -> EngineInfo {
        lock(&self.shared.info).clone()
    }

    fn transcribe(&self, request: &TranscribeRequest) -> Result<Transcription, SttError> {
        {
            let info = lock(&self.shared.info);
            if !info.ready {
                return Err(match info.phase {
                    EnginePhase::Error if info.model.is_none() => SttError::NoModel,
                    EnginePhase::Error => SttError::Failed(
                        info.error
                            .clone()
                            .unwrap_or_else(|| "moteur en erreur".into()),
                    ),
                    _ => SttError::NotReady,
                });
            }
        }
        let samples = &request.audio.samples;
        let audio_ms = request.audio.duration_ms();
        let language = normalize_language(request.language.as_deref());
        if samples.is_empty() {
            return Ok(Transcription {
                text: String::new(),
                language,
                stt_ms: 0,
                audio_ms,
            });
        }
        let params = DecodeParams {
            language: language.clone(),
            prompt: build_initial_prompt(&request.hints),
        };
        // Lock: one transcription at a time.
        let mut guard = lock(&self.shared.model);
        let model = guard.as_mut().ok_or(SttError::NotReady)?;
        let started = Instant::now();
        let raw = catch_unwind(AssertUnwindSafe(|| model.transcribe(samples, &params)))
            .map_err(|_| SttError::Failed("panic during transcription".into()))?
            .map_err(SttError::Failed)?;
        let stt_ms = started.elapsed().as_millis() as u64;
        Ok(Transcription {
            text: join_segments(&raw.segments),
            language: language.or(raw.language),
            stt_ms,
            audio_ms,
        })
    }
}

/// whisper.cpp engine in the current process.
pub struct WhisperEngine {
    inner: InProcessEngine,
}

impl WhisperEngine {
    /// Starts the loading in the background; `plans` is the fallback chain (see
    /// [`crate::plan::build_fallback_chain`]).
    pub fn start(config: EngineConfig, plans: Vec<LoadPlan>) -> Self {
        let warmup = config.warmup;
        let backend = Arc::new(WhisperBackend::new(config));
        Self {
            inner: InProcessEngine::start(backend, warmup, plans),
        }
    }

    /// Waits for the end of the loading and returns the final state.
    pub fn wait_loaded(&self) -> EngineInfo {
        self.inner.wait_loaded()
    }
}

impl SpeechToText for WhisperEngine {
    fn info(&self) -> EngineInfo {
        self.inner.info()
    }

    fn transcribe(&self, request: &TranscribeRequest) -> Result<Transcription, SttError> {
        self.inner.transcribe(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxflow_core::domain::Recording;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    /// Fake backend: paths containing `bad` fail (recoverable), `fatal` (fatal).
    struct Fake {
        loads: AtomicUsize,
        seen: Arc<StdMutex<Vec<DecodeParams>>>,
        delay_ms: u64,
    }

    struct FakeModel {
        seen: Arc<StdMutex<Vec<DecodeParams>>>,
        delay_ms: u64,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl LoadedModel for FakeModel {
        fn transcribe(&mut self, _: &[f32], p: &DecodeParams) -> Result<RawTranscript, String> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.seen.lock().map(|mut s| s.push(p.clone())).ok();
            Ok(RawTranscript {
                segments: vec![" Bonjour ".into(), "".into(), "monde ".into()],
                language: Some("fr".into()),
            })
        }
        fn warmup(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    impl Backend for Fake {
        fn load(&self, plan: &LoadPlan) -> Result<Box<dyn LoadedModel>, LoadFailure> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            let path = plan.model_path.to_string_lossy();
            if path.contains("fatal") {
                return Err(LoadFailure::Fatal("fatal".into()));
            }
            if path.contains("bad") {
                return Err(LoadFailure::Recoverable("insufficient memory".into()));
            }
            Ok(Box::new(FakeModel {
                seen: Arc::clone(&self.seen),
                delay_ms: self.delay_ms,
                active: Arc::new(AtomicUsize::new(0)),
                max_active: Arc::new(AtomicUsize::new(0)),
            }))
        }
    }

    fn fake(delay_ms: u64) -> (Arc<Fake>, Arc<StdMutex<Vec<DecodeParams>>>) {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        (
            Arc::new(Fake {
                loads: AtomicUsize::new(0),
                seen: Arc::clone(&seen),
                delay_ms,
            }),
            seen,
        )
    }

    fn request(hints: &[&str], language: Option<&str>) -> TranscribeRequest {
        TranscribeRequest {
            audio: Recording {
                samples: vec![0.0; 16_000],
            },
            language: language.map(str::to_string),
            hints: hints.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn first_working_plan_wins_and_fallback_is_tried_in_order() {
        let (backend, _) = fake(0);
        let engine = InProcessEngine::start(
            backend.clone(),
            true,
            vec![
                LoadPlan::gpu("bad-large-f16.bin", 0).with_device_name("GPU test"),
                LoadPlan::gpu("bad-large-q5_0.bin", 0),
                LoadPlan::cpu("ggml-small.bin"),
                LoadPlan::cpu("never.bin"),
            ],
        );
        let info = engine.wait_loaded();
        assert!(info.ready && !info.loading && info.error.is_none());
        assert_eq!(info.phase, EnginePhase::Ready);
        assert_eq!(info.device, DeviceKind::Cpu);
        assert_eq!(info.model.as_deref(), Some("small"));
        assert_eq!(info.device_name, None);
        assert_eq!(backend.loads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn all_plans_failing_gives_error_state() {
        let (backend, _) = fake(0);
        let engine = InProcessEngine::start(
            backend,
            false,
            vec![LoadPlan::cpu("bad-a.bin"), LoadPlan::cpu("bad-b.bin")],
        );
        let info = engine.wait_loaded();
        assert_eq!(info.phase, EnginePhase::Error);
        assert!(!info.ready && !info.loading);
        assert!(info
            .error
            .unwrap_or_default()
            .contains("insufficient memory"));
        assert!(matches!(
            engine.transcribe(&request(&[], None)),
            Err(SttError::Failed(_))
        ));
    }

    #[test]
    fn fatal_failure_stops_the_chain() {
        let (backend, _) = fake(0);
        let engine = InProcessEngine::start(
            backend.clone(),
            false,
            vec![LoadPlan::cpu("fatal.bin"), LoadPlan::cpu("ok.bin")],
        );
        assert_eq!(engine.wait_loaded().phase, EnginePhase::Error);
        assert_eq!(backend.loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn no_plan_means_no_model() {
        let (backend, _) = fake(0);
        let engine = InProcessEngine::start(backend, false, Vec::new());
        let info = engine.wait_loaded();
        assert_eq!(info.phase, EnginePhase::Error);
        assert_eq!(
            engine.transcribe(&request(&[], None)),
            Err(SttError::NoModel)
        );
    }

    #[test]
    fn not_ready_while_loading() {
        struct Slow;
        impl Backend for Slow {
            fn load(&self, _: &LoadPlan) -> Result<Box<dyn LoadedModel>, LoadFailure> {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Err(LoadFailure::Recoverable("x".into()))
            }
        }
        let engine = InProcessEngine::start(Arc::new(Slow), false, vec![LoadPlan::cpu("a.bin")]);
        // `start` returns before the loading is finished.
        let early = engine.info();
        assert!(!early.ready);
        assert_eq!(
            engine.transcribe(&request(&[], None)),
            Err(SttError::NotReady)
        );
        engine.wait_loaded();
    }

    #[test]
    fn transcription_formats_text_prompt_and_language() {
        let (backend, seen) = fake(0);
        let engine = InProcessEngine::start(backend, true, vec![LoadPlan::cpu("ggml-small.bin")]);
        engine.wait_loaded();
        let out = engine
            .transcribe(&request(&["  Alice ", "", "Bob"], None))
            .map_err(|e| e.to_string());
        let out = out.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.text, "Bonjour monde");
        assert_eq!(out.language.as_deref(), Some("fr"));
        assert_eq!(out.audio_ms, 1000);
        let params = seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert_eq!(params[0].prompt.as_deref(), Some("Alice, Bob"));
        assert_eq!(params[0].language, None);
        // Explicit language: passed on and returned as is.
        let out = engine
            .transcribe(&request(&[], Some("EN")))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.language.as_deref(), Some("en"));
    }

    #[test]
    fn stt_ms_covers_transcription_only() {
        let (backend, _) = fake(120);
        let engine = InProcessEngine::start(backend, false, vec![LoadPlan::cpu("ok.bin")]);
        engine.wait_loaded();
        let out = engine
            .transcribe(&request(&[], None))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!((100..1000).contains(&out.stt_ms), "stt_ms = {}", out.stt_ms);
    }

    #[test]
    fn empty_audio_returns_empty_text() {
        let (backend, _) = fake(0);
        let engine = InProcessEngine::start(backend, false, vec![LoadPlan::cpu("ok.bin")]);
        engine.wait_loaded();
        let req = TranscribeRequest {
            audio: Recording {
                samples: Vec::new(),
            },
            language: None,
            hints: Vec::new(),
        };
        assert_eq!(engine.transcribe(&req).map(|t| t.text), Ok(String::new()));
    }

    #[test]
    fn transcriptions_are_serialized() {
        let (backend, _) = fake(50);
        let engine = Arc::new(InProcessEngine::start(
            backend,
            false,
            vec![LoadPlan::cpu("ok.bin")],
        ));
        engine.wait_loaded();
        let started = Instant::now();
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let engine = Arc::clone(&engine);
                std::thread::spawn(move || engine.transcribe(&request(&[], None)))
            })
            .collect();
        for h in handles {
            assert!(h.join().map(|r| r.is_ok()).unwrap_or(false));
        }
        // 3 calls of 50 ms in series: at least 150 ms.
        assert!(started.elapsed().as_millis() >= 150);
    }
}
