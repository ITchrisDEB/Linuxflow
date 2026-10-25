//! Engine life cycle: hardware detection, model choice, (re)start, waiting.

use crate::engine::{
    build_plans, choose_model, device_error_text, find_engine_exe, gpu_available, hardware_summary,
    EngineHost, EngineLauncher, EnginePlan,
};
use crate::models::{ModelsService, VAD_ID};
use linuxflow_core::domain::{EnginePhase, Settings};
use linuxflow_core::traits::SpeechToText;
use linuxflow_i18n::tr;
use linuxflow_models::{recommend_in, ModelKind, Usage};
use linuxflow_stt::{cpu_guard, device_setting_from_env, select_device, DeviceChoice, Hardware};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub type HardwareProbe = Arc<dyn Fn() -> Hardware + Send + Sync>;

pub struct EngineController {
    host: Arc<EngineHost>,
    models: Arc<ModelsService>,
    launcher: Arc<dyn EngineLauncher>,
    settings: Arc<dyn Fn() -> Settings + Send + Sync>,
    probe: HardwareProbe,
    hardware: OnceLock<Hardware>,
    no_engine: bool,
    exe_dir: Option<PathBuf>,
    reload_lock: Mutex<()>,
}

impl EngineController {
    pub fn new(
        host: Arc<EngineHost>,
        models: Arc<ModelsService>,
        launcher: Arc<dyn EngineLauncher>,
        settings: Arc<dyn Fn() -> Settings + Send + Sync>,
        probe: HardwareProbe,
        no_engine: bool,
        exe_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            host,
            models,
            launcher,
            settings,
            probe,
            hardware: OnceLock::new(),
            no_engine,
            exe_dir,
            reload_lock: Mutex::new(()),
        }
    }

    pub fn host(&self) -> &Arc<EngineHost> {
        &self.host
    }

    fn hardware(&self) -> &Hardware {
        self.hardware.get_or_init(|| (self.probe)())
    }

    /// Transcription model currently loaded.
    pub fn current_model(&self) -> Option<String> {
        self.host.current_model()
    }

    /// (Re)starts the engine according to the settings. Blocking (includes stopping the old process);
    /// does not say whether loading succeeded: see [`EngineController::wait_settled`].
    pub fn reload(&self) {
        let _guard = self.reload_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.host.set_engine(None, None);
        self.models.manager().set_in_use(None);
        if self.no_engine {
            self.host.set_reason(Some(tr(
                "Interface-only mode: transcription is turned off.",
            )));
            return;
        }
        match self.plan() {
            Ok(plan) => {
                self.host.set_reason(None);
                self.models.manager().set_in_use(Some(&plan.model_id));
                let engine = self.launcher.launch(&plan);
                self.host.set_engine(Some(engine), Some(plan.model_id));
            }
            Err(reason) => self.host.set_reason(Some(reason)),
        }
    }

    fn plan(&self) -> Result<EnginePlan, String> {
        let settings = (self.settings)();
        let hw = self.hardware();
        let setting = device_setting_from_env(settings.device.as_deref())
            .map_err(|e| device_error_text(&e))?;
        let features = &hw.cpu_features;
        let level = cpu_guard::best_supported_level(&|f| features.iter().any(|x| x == f));
        if level.is_none() {
            return Err(tr(
                "This processor is too old for the transcription engine.",
            ));
        }
        let gpu_ok = self.gpu_available(level);
        let choice = select_device(hw, setting, gpu_ok).map_err(|e| device_error_text(&e))?;
        let catalog = self.models.catalog();
        let recommended = recommend_in(catalog, &hardware_summary(hw, gpu_ok)).map(|r| r.model);
        self.models
            .manager()
            .set_recommended(recommended.as_deref());
        let gpu_usable = matches!(choice, DeviceChoice::Gpu { .. });
        let installed = |id: &str| self.models.path_of(id).is_some();
        let model = choose_model(
            catalog,
            &installed,
            settings.selected_model.as_deref(),
            recommended.as_deref(),
            gpu_usable,
        )
        .ok_or_else(|| {
            let any_installed = catalog
                .entries()
                .iter()
                .any(|e| e.kind == ModelKind::Stt && installed(&e.id));
            if any_installed {
                tr("The installed models need a GPU, which is not available. Download a lighter model.")
            } else {
                tr("No speech model is installed yet.")
            }
        })?;
        let path_of = |id: &str| self.models.path_of(id);
        let plans = build_plans(catalog, &path_of, &model, &choice);
        if plans.is_empty() {
            return Err(tr("No installed model can run on this machine."));
        }
        let vad = self.models.path_of(VAD_ID);
        if vad.is_none() {
            self.models.ensure_vad();
        }
        let exe = self.engine_exe(level, gpu_usable);
        Ok(EnginePlan {
            model_id: model,
            plans,
            vad,
            exe,
        })
    }

    fn engine_exe(&self, level: Option<&str>, gpu: bool) -> Option<PathBuf> {
        let dir = self.exe_dir.as_deref()?;
        find_engine_exe(dir, level, gpu, &|p| p.is_file())
    }

    /// Can the GPU be used with this installation (Vulkan engine shipped, v3 processor)?
    fn gpu_available(&self, level: Option<&str>) -> bool {
        gpu_available(self.exe_dir.as_deref(), level, &|p| p.is_file())
    }

    fn gpu_available_now(&self) -> bool {
        let features = &self.hardware().cpu_features;
        let level = cpu_guard::best_supported_level(&|f| features.iter().any(|x| x == f));
        self.gpu_available(level)
    }

    /// Waits until the engine is ready or in error. `Ok` = ready; `Err` = translated message or timeout.
    pub fn wait_settled(&self, timeout: Duration) -> Result<(), String> {
        let end = Instant::now() + timeout;
        loop {
            let info = self.host.info();
            if info.ready {
                return Ok(());
            }
            if info.phase == EnginePhase::Error && !info.loading {
                return Err(info
                    .error
                    .unwrap_or_else(|| tr("The transcription engine could not start.")));
            }
            if Instant::now() >= end {
                return Err(tr("The transcription engine took too long to start."));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Can a model of the given use run here? (`model_use` refused without a GPU)
    pub fn can_use(&self, id: &str) -> Result<(), String> {
        let Some(e) = self.models.catalog().get(id) else {
            return Err(tr("This model is not in the catalog."));
        };
        if e.kind != ModelKind::Stt {
            return Err(tr("This file is not a transcription model."));
        }
        if e.usage == Usage::Gpu {
            let settings = (self.settings)();
            let ok = device_setting_from_env(settings.device.as_deref())
                .ok()
                .and_then(|s| select_device(self.hardware(), s, self.gpu_available_now()).ok())
                .is_some_and(|c| matches!(c, DeviceChoice::Gpu { .. }));
            if !ok {
                return Err(tr(
                    "This model needs a usable GPU, which is not available on this machine.",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use linuxflow_core::domain::{EngineInfo, TranscribeRequest, Transcription};
    use linuxflow_core::error::SttError;
    use linuxflow_models::{Catalog, Config};
    use linuxflow_stt::LoadPlan;

    struct Fake(EngineInfo);
    impl SpeechToText for Fake {
        fn info(&self) -> EngineInfo {
            self.0.clone()
        }
        fn transcribe(&self, _: &TranscribeRequest) -> Result<Transcription, SttError> {
            Err(SttError::NotReady)
        }
    }
    impl Engine for Fake {
        fn shutdown(&self) {}
    }

    #[derive(Default)]
    struct Launcher(Mutex<Vec<EnginePlan>>);
    impl EngineLauncher for Launcher {
        fn launch(&self, plan: &EnginePlan) -> Arc<dyn Engine> {
            self.0.lock().unwrap().push(plan.clone());
            Arc::new(Fake(EngineInfo {
                phase: EnginePhase::Ready,
                ready: true,
                model: Some(plan.model_id.clone()),
                ..EngineInfo::default()
            }))
        }
    }

    fn catalog() -> Catalog {
        Catalog::from_json(
            r#"{"endpoint":"http://127.0.0.1:9","models":[
            {"id":"tiny","name":"Tiny","kind":"stt","file":"tiny.bin","download_bytes":4,
             "urls":["http://127.0.0.1:9/t"],"usage":"light","rank":1},
            {"id":"big","name":"Big","kind":"stt","file":"big.bin","download_bytes":5,
             "urls":["http://127.0.0.1:9/b"],"usage":"gpu","rank":2,"vram_bytes":100},
            {"id":"silero-vad","name":"VAD","kind":"vad","file":"vad.bin","download_bytes":3,
             "urls":["http://127.0.0.1:9/v"],"required":true}]}"#,
        )
        .unwrap()
    }

    struct Rig {
        _rt: tokio::runtime::Runtime,
        dir: tempfile::TempDir,
        ctl: EngineController,
        launcher: Arc<Launcher>,
        settings: Arc<Mutex<Settings>>,
    }

    fn rig(no_engine: bool) -> Rig {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(EngineHost::new());
        let models = ModelsService::new(
            rt.handle().clone(),
            catalog(),
            Config {
                offline: true,
                ..Config::default()
            },
            dir.path().to_path_buf(),
            host.clone(),
        );
        let launcher = Arc::new(Launcher::default());
        let settings = Arc::new(Mutex::new(Settings::default()));
        let s2 = settings.clone();
        let ctl = EngineController::new(
            host,
            models,
            launcher.clone(),
            Arc::new(move || s2.lock().unwrap().clone()),
            Arc::new(|| Hardware {
                gpus: vec![],
                cpu_features: vec![
                    "sse4.2".into(),
                    "avx".into(),
                    "avx2".into(),
                    "bmi2".into(),
                    "fma".into(),
                    "f16c".into(),
                ],
                ram_bytes: Some(8 << 30),
                vulkan_error: None,
            }),
            no_engine,
            None,
        );
        Rig {
            _rt: rt,
            dir,
            ctl,
            launcher,
            settings,
        }
    }

    #[test]
    fn no_model_installed_keeps_engine_down() {
        let r = rig(false);
        r.ctl.reload();
        let i = r.ctl.host().info();
        assert!(!i.ready);
        assert_eq!(i.phase, EnginePhase::Error);
        assert_eq!(
            i.error.as_deref(),
            Some("No speech model is installed yet.")
        );
        assert!(r.launcher.0.lock().unwrap().is_empty());
        assert!(r.ctl.wait_settled(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn installed_model_is_launched_with_vad() {
        let r = rig(false);
        std::fs::write(r.dir.path().join("tiny.bin"), b"abcd").unwrap();
        std::fs::write(r.dir.path().join("vad.bin"), b"abc").unwrap();
        r.ctl.reload();
        assert!(r.ctl.wait_settled(Duration::from_secs(2)).is_ok());
        let plans = r.launcher.0.lock().unwrap().clone();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].model_id, "tiny");
        assert!(plans[0].vad.is_some());
        assert_eq!(plans[0].plans.len(), 1);
        assert!(matches!(plans[0].plans[0], LoadPlan { .. }));
        assert_eq!(r.ctl.current_model().as_deref(), Some("tiny"));
        // A GPU model is never started without a GPU, even if chosen.
        std::fs::write(r.dir.path().join("big.bin"), b"abcde").unwrap();
        r.settings.lock().unwrap().selected_model = Some("big".into());
        r.ctl.reload();
        assert_eq!(r.ctl.current_model().as_deref(), Some("tiny"));
        assert!(r.ctl.can_use("big").is_err());
        assert!(r.ctl.can_use("tiny").is_ok());
        assert!(r.ctl.can_use("silero-vad").is_err());
    }

    #[test]
    fn interface_only_mode_never_launches() {
        let r = rig(true);
        std::fs::write(r.dir.path().join("tiny.bin"), b"abcd").unwrap();
        r.ctl.reload();
        assert!(r.launcher.0.lock().unwrap().is_empty());
        assert!(r.ctl.host().info().error.is_some());
    }

    #[test]
    fn forced_cuda_is_an_explicit_error() {
        let r = rig(false);
        std::fs::write(r.dir.path().join("tiny.bin"), b"abcd").unwrap();
        r.settings.lock().unwrap().device = Some("cuda".into());
        r.ctl.reload();
        assert!(r.launcher.0.lock().unwrap().is_empty());
        assert!(r.ctl.host().info().error.unwrap().contains("CUDA and ROCm"));
    }
}
