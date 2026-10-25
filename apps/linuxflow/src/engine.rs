//! Application-side transcription engine: model choice, fallback chain, launch of the
//! engine process, aggregated state (`EngineInfo` with download progress).

use linuxflow_core::domain::{
    DeviceKind, DownloadProgress, EngineInfo, EnginePhase, TranscribeRequest, Transcription,
};
use linuxflow_core::error::SttError;
use linuxflow_core::traits::SpeechToText;
use linuxflow_i18n::tr;
use linuxflow_models::{Catalog, ModelEntry, ModelKind, Usage};
use linuxflow_stt::{
    build_fallback_chain, DeviceChoice, DeviceError, EngineConfig, Hardware, LoadPlan,
    WorkerClient, WorkerConfig,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Engine launched (child process or test fake).
pub trait Engine: SpeechToText {
    fn shutdown(&self);
}

impl Engine for WorkerClient {
    fn shutdown(&self) {
        WorkerClient::shutdown(self);
    }
}

/// Everything needed to launch an engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnginePlan {
    pub model_id: String,
    pub plans: Vec<LoadPlan>,
    pub vad: Option<PathBuf>,
    pub exe: Option<PathBuf>,
}

pub trait EngineLauncher: Send + Sync {
    fn launch(&self, plan: &EnginePlan) -> Arc<dyn Engine>;
}

/// Production launcher: `WorkerClient` with the interface language in the environment.
pub struct WorkerLauncher;

impl EngineLauncher for WorkerLauncher {
    fn launch(&self, plan: &EnginePlan) -> Arc<dyn Engine> {
        let mut engine = EngineConfig::new();
        engine.vad_model_path = plan.vad.clone();
        let mut cfg = WorkerConfig::new(plan.plans.clone(), engine);
        cfg.exe = plan.exe.clone();
        cfg.extra_env
            .push(("LANGUAGE".into(), linuxflow_i18n::current_language()));
        Arc::new(WorkerClient::start(cfg))
    }
}

/// Aggregated engine state, used as `SpeechToText` by the pipeline.
#[derive(Default)]
pub struct EngineHost {
    current: Mutex<Option<Arc<dyn Engine>>>,
    download: Mutex<Option<DownloadProgress>>,
    reason: Mutex<Option<String>>,
    model: Mutex<Option<String>>,
}

impl EngineHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the engine; the old one is stopped (blocking, 800 ms at most).
    pub fn set_engine(&self, engine: Option<Arc<dyn Engine>>, model: Option<String>) {
        let old = std::mem::replace(&mut *lock(&self.current), engine);
        *lock(&self.model) = model;
        if let Some(old) = old {
            old.shutdown();
        }
    }

    /// Reason why there is no engine (no model, interface-only mode, CPU too old...).
    pub fn set_reason(&self, reason: Option<String>) {
        *lock(&self.reason) = reason;
    }

    pub fn set_download(&self, d: Option<DownloadProgress>) {
        *lock(&self.download) = d;
    }

    pub fn download(&self) -> Option<DownloadProgress> {
        lock(&self.download).clone()
    }

    pub fn current_model(&self) -> Option<String> {
        lock(&self.model).clone()
    }

    pub fn shutdown(&self) {
        self.set_engine(None, None);
    }
}

impl SpeechToText for EngineHost {
    fn info(&self) -> EngineInfo {
        let download = self.download();
        let engine = lock(&self.current).clone();
        if let Some(e) = engine {
            let mut info = e.info();
            if info.model.is_none() {
                info.model = self.current_model();
            }
            if !info.ready && info.error.is_none() {
                if let Some(d) = download {
                    info.phase = EnginePhase::Downloading;
                    info.loading = true;
                    info.download = Some(d);
                }
            }
            return info;
        }
        if let Some(d) = download {
            return EngineInfo {
                phase: EnginePhase::Downloading,
                loading: true,
                download: Some(d),
                ..EngineInfo::default()
            };
        }
        EngineInfo {
            phase: EnginePhase::Error,
            error: Some(
                lock(&self.reason)
                    .clone()
                    .unwrap_or_else(|| tr("No speech model is installed yet.")),
            ),
            device: DeviceKind::Cpu,
            ..EngineInfo::default()
        }
    }

    fn transcribe(&self, request: &TranscribeRequest) -> Result<Transcription, SttError> {
        let engine = lock(&self.current).clone();
        match engine {
            Some(e) => e.transcribe(request),
            None => Err(SttError::NoModel),
        }
    }
}

// ---- Model choice and loading plan (pure functions) ------------------------------------------

/// Model to use: the user's choice, otherwise the recommended one, otherwise the most accurate of
/// the usable installed models. A "GPU" model is never picked without a usable GPU.
pub fn choose_model(
    catalog: &Catalog,
    installed: &dyn Fn(&str) -> bool,
    selected: Option<&str>,
    recommended: Option<&str>,
    gpu_usable: bool,
) -> Option<String> {
    let allowed = |e: &ModelEntry| {
        e.kind == ModelKind::Stt && installed(&e.id) && (e.usage != Usage::Gpu || gpu_usable)
    };
    for id in [selected, recommended].into_iter().flatten() {
        if catalog.get(id).is_some_and(allowed) {
            return Some(id.to_string());
        }
    }
    catalog
        .stt_by_rank()
        .into_iter()
        .rev()
        .find(|e| allowed(e))
        .map(|e| e.id.clone())
}

/// Fallback chain of the chosen model: GPU models (the chosen one, then the lighter installed ones),
/// then a CPU model. Never a "GPU" model on the CPU.
pub fn build_plans(
    catalog: &Catalog,
    path_of: &dyn Fn(&str) -> Option<PathBuf>,
    model_id: &str,
    choice: &DeviceChoice,
) -> Vec<LoadPlan> {
    let Some(chosen) = catalog.get(model_id) else {
        return Vec::new();
    };
    let ranked = catalog.stt_by_rank();
    let mut gpu_models: Vec<PathBuf> = Vec::new();
    if matches!(choice, DeviceChoice::Gpu { .. }) {
        gpu_models.extend(path_of(&chosen.id));
        for e in ranked.iter().rev().filter(|e| e.rank < chosen.rank) {
            gpu_models.extend(path_of(&e.id));
        }
    }
    let cpu_entry = if chosen.usage != Usage::Gpu {
        Some(chosen)
    } else {
        ranked
            .iter()
            .rev()
            .find(|e| e.usage != Usage::Gpu && path_of(&e.id).is_some())
            .copied()
    };
    let cpu_model = cpu_entry.and_then(|e| path_of(&e.id));
    build_fallback_chain(choice, &gpu_models, cpu_model.as_deref(), &|p: &Path| {
        p.exists()
    })
}

/// Engine built with Vulkan. It is hard-linked to `libvulkan.so.1`: it can therefore only start
/// where the Vulkan loader is installed. It is shipped NEXT TO the CPU engine (which starts
/// everywhere) and is only chosen when hardware detection actually found a GPU.
pub const VULKAN_ENGINE: &str = "linuxflow-engine-vulkan";

/// Is the GPU usable with this installation? Yes if this binary itself is built with
/// Vulkan (development) or if the Vulkan engine is shipped next to it and the processor is
/// v3 level (the "v2" fallback engine only exists for the CPU).
pub fn gpu_available(
    exe_dir: Option<&Path>,
    level: Option<&str>,
    exists: &dyn Fn(&Path) -> bool,
) -> bool {
    linuxflow_stt::GPU_COMPILED
        || (level == Some("v3") && exe_dir.is_some_and(|d| exists(&d.join(VULKAN_ENGINE))))
}

/// Like [`gpu_available`], for the current installation (folder of the current binary) without
/// knowing the processor level: used for displays (recommendation, report).
pub fn gpu_available_here() -> bool {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    gpu_available(dir.as_deref(), Some("v3"), &|p| p.is_file())
}

/// Engine executable: `linuxflow-engine-vulkan` (v3, when the GPU is chosen), `linuxflow-engine`
/// (v3) or `linuxflow-engine-v2` next to the current binary; `None` = use the current binary
/// with `--engine-worker` (development).
pub fn find_engine_exe(
    exe_dir: &Path,
    level: Option<&str>,
    gpu: bool,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let names: &[&str] = match (level, gpu) {
        (Some("v3"), true) => &[VULKAN_ENGINE, "linuxflow-engine"],
        (Some("v3"), false) => &["linuxflow-engine"],
        (Some(_), _) => &["linuxflow-engine-v2"],
        (None, _) => &[],
    };
    names.iter().map(|n| exe_dir.join(n)).find(|p| exists(p))
}

/// Translated message for a device choice error.
pub fn device_error_text(e: &DeviceError) -> String {
    match e {
        DeviceError::Unsupported(_) => {
            tr("CUDA and ROCm are not supported. Use \"gpu\" (Vulkan) or \"cpu\".")
        }
        DeviceError::Invalid(_) => {
            tr("Invalid device setting. Expected auto, cpu, gpu, gpu:N or a number.")
        }
        DeviceError::NoGpu => tr("No usable Vulkan GPU was found (software drivers are ignored)."),
        DeviceError::GpuNotFound(_) => tr("The requested GPU does not exist."),
        DeviceError::NotCompiled => tr("This build has no Vulkan support."),
    }
}

/// Hardware summary for the model recommendation.
pub fn hardware_summary(hw: &Hardware, gpu_available: bool) -> linuxflow_models::HardwareSummary {
    linuxflow_models::HardwareSummary {
        gpu_vulkan: gpu_available && hw.has_real_gpu(),
        vram_bytes: hw.best_vram_bytes(),
        ram_bytes: hw.ram_bytes.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxflow_core::domain::Recording;
    use std::collections::HashSet;

    struct FakeEngine {
        info: Mutex<EngineInfo>,
        shut: Mutex<bool>,
    }
    impl SpeechToText for FakeEngine {
        fn info(&self) -> EngineInfo {
            lock(&self.info).clone()
        }
        fn transcribe(&self, _r: &TranscribeRequest) -> Result<Transcription, SttError> {
            Ok(Transcription {
                text: "ok".into(),
                language: None,
                stt_ms: 1,
                audio_ms: 1,
            })
        }
    }
    impl Engine for FakeEngine {
        fn shutdown(&self) {
            *lock(&self.shut) = true;
        }
    }

    fn req() -> TranscribeRequest {
        TranscribeRequest {
            audio: Recording::default(),
            language: None,
            hints: vec![],
        }
    }

    #[test]
    fn host_without_engine_reports_no_model() {
        let h = EngineHost::new();
        let i = h.info();
        assert_eq!(i.phase, EnginePhase::Error);
        assert!(!i.ready);
        assert_eq!(h.transcribe(&req()), Err(SttError::NoModel));
        h.set_reason(Some("interface only".into()));
        assert_eq!(h.info().error.as_deref(), Some("interface only"));
    }

    #[test]
    fn host_merges_download_progress_until_ready() {
        let h = EngineHost::new();
        h.set_download(Some(DownloadProgress {
            model: "small".into(),
            done: 1,
            total: 2,
        }));
        assert_eq!(h.info().phase, EnginePhase::Downloading);
        let e = Arc::new(FakeEngine {
            info: Mutex::new(EngineInfo {
                phase: EnginePhase::Loading,
                loading: true,
                ..EngineInfo::default()
            }),
            shut: Mutex::new(false),
        });
        h.set_engine(Some(e.clone()), Some("small".into()));
        let i = h.info();
        assert_eq!(i.phase, EnginePhase::Downloading);
        assert_eq!(i.model.as_deref(), Some("small"));
        *lock(&e.info) = EngineInfo {
            phase: EnginePhase::Ready,
            ready: true,
            ..EngineInfo::default()
        };
        assert_eq!(h.info().phase, EnginePhase::Ready);
        assert!(h.transcribe(&req()).is_ok());
        // Replacement: the old one is stopped.
        h.set_engine(None, None);
        assert!(*lock(&e.shut));
    }

    fn installed_set(ids: &[&str]) -> impl Fn(&str) -> bool {
        let s: HashSet<String> = ids.iter().map(|s| s.to_string()).collect();
        move |id| s.contains(id)
    }

    #[test]
    fn choose_model_rules() {
        let cat = Catalog::embedded();
        // Explicit choice installed.
        assert_eq!(
            choose_model(&cat, &installed_set(&["small"]), Some("small"), None, false),
            Some("small".into())
        );
        // Recommended model absent: fall back to an installed model.
        assert_eq!(
            choose_model(
                &cat,
                &installed_set(&["small"]),
                None,
                Some("large-v3-q5_0"),
                true
            ),
            Some("small".into())
        );
        // Never a large model without a GPU, even if chosen explicitly.
        assert_eq!(
            choose_model(
                &cat,
                &installed_set(&["large-v3-q5_0"]),
                Some("large-v3-q5_0"),
                None,
                false
            ),
            None
        );
        // With a GPU: the most accurate installed model.
        assert_eq!(
            choose_model(
                &cat,
                &installed_set(&["small", "large-v3-q5_0"]),
                None,
                None,
                true
            ),
            Some("large-v3-q5_0".into())
        );
        // The VAD is never a transcription model.
        assert_eq!(
            choose_model(
                &cat,
                &installed_set(&["silero-vad"]),
                Some("silero-vad"),
                None,
                true
            ),
            None
        );
    }

    #[test]
    fn plans_cpu_and_gpu() {
        let cat = Catalog::embedded();
        let path_of = |id: &str| Some(PathBuf::from(format!("/m/{id}.bin")));
        // The files do not exist: build_fallback_chain discards them, so the
        // selection is tested through a real directory.
        let dir = tempfile::tempdir().unwrap();
        for id in ["small", "large-v3-q5_0"] {
            std::fs::write(dir.path().join(id), b"x").unwrap();
        }
        let real = |id: &str| {
            let p = dir.path().join(id);
            p.exists().then_some(p)
        };
        let gpu = DeviceChoice::Gpu {
            ggml_index: 0,
            name: "GPU".into(),
            vram_bytes: Some(8 << 30),
        };
        let chain = build_plans(&cat, &real, "large-v3-q5_0", &gpu);
        assert_eq!(chain.len(), 3);
        assert!(matches!(chain[0].device, linuxflow_stt::Device::Gpu(0)));
        assert!(chain[0].model_path.ends_with("large-v3-q5_0"));
        assert!(chain[1].model_path.ends_with("small"));
        assert!(matches!(chain[2].device, linuxflow_stt::Device::Cpu));
        assert!(chain[2].model_path.ends_with("small"));
        // CPU only with a large model chosen: only the light model is used.
        let chain = build_plans(&cat, &real, "large-v3-q5_0", &DeviceChoice::Cpu);
        assert_eq!(chain.len(), 1);
        assert!(chain[0].model_path.ends_with("small"));
        // No file: empty chain.
        let none = |_: &str| None;
        assert!(build_plans(&cat, &none, "small", &DeviceChoice::Cpu).is_empty());
        let _ = path_of("x");
    }

    #[test]
    fn engine_exe_lookup() {
        let dir = Path::new("/opt/lf");
        let all = |_: &Path| true;
        let none = |_: &Path| false;
        assert_eq!(
            find_engine_exe(dir, Some("v3"), false, &all),
            Some(PathBuf::from("/opt/lf/linuxflow-engine"))
        );
        assert_eq!(
            find_engine_exe(dir, Some("v2"), false, &all),
            Some(PathBuf::from("/opt/lf/linuxflow-engine-v2"))
        );
        assert_eq!(find_engine_exe(dir, Some("v3"), false, &none), None);
        assert_eq!(find_engine_exe(dir, None, false, &all), None);
    }

    #[test]
    fn the_vulkan_engine_is_chosen_only_for_the_gpu_and_only_when_shipped() {
        let dir = Path::new("/opt/lf");
        let all = |_: &Path| true;
        let cpu_only = |p: &Path| !p.ends_with(VULKAN_ENGINE);
        assert_eq!(
            find_engine_exe(dir, Some("v3"), true, &all),
            Some(PathBuf::from("/opt/lf/linuxflow-engine-vulkan"))
        );
        // no Vulkan engine in the archive: the CPU engine takes over
        assert_eq!(
            find_engine_exe(dir, Some("v3"), true, &cpu_only),
            Some(PathBuf::from("/opt/lf/linuxflow-engine"))
        );
        // processor without AVX2: never a Vulkan engine
        assert_eq!(
            find_engine_exe(dir, Some("v2"), true, &all),
            Some(PathBuf::from("/opt/lf/linuxflow-engine-v2"))
        );
        assert!(!gpu_available(Some(dir), Some("v2"), &all));
        assert!(!gpu_available(Some(dir), Some("v3"), &cpu_only));
        assert!(!gpu_available(None, Some("v3"), &all));
    }
}
