//! Models service: download (tokio), progress towards the engine state, required VAD.

use crate::engine::EngineHost;
use linuxflow_core::domain::{DownloadProgress, ModelStatus};
use linuxflow_i18n::tr;
use linuxflow_models::{
    Catalog, Config, Integrity, ModelError, ModelKind, ModelManager, ProgressPhase, ProgressUpdate,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

/// Identifier of the voice-detection model, required for correct operation.
pub const VAD_ID: &str = linuxflow_core::constants::VAD_MODEL_ID;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelsEvent {
    /// The progress or state of a model changed.
    Progress,
    /// A download (VAD included) finished successfully.
    Finished { id: String },
    /// A local file was adopted as model `id` (no download took place).
    Adopted { id: String },
    /// Translated message to display (failure, verification...).
    Notice(String),
}

type Listener = Arc<dyn Fn(ModelsEvent) + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct ModelsService {
    handle: tokio::runtime::Handle,
    catalog: Catalog,
    cfg: Config,
    manager: RwLock<ModelManager>,
    listener: Arc<Mutex<Option<Listener>>>,
    host: Arc<EngineHost>,
    /// New folder requested during a download, applied at the end.
    pending_dir: Mutex<Option<PathBuf>>,
}

/// What to do with an error that occurred during a step of a composite download (VAD then model)?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepAction {
    /// Next step (the VAD is already being downloaded elsewhere: the model must not
    /// be abandoned for that reason).
    Continue,
    /// Stop without a message (voluntary cancellation, or the requested model is already in progress).
    StopQuietly,
    /// Failure to report.
    Fail,
}

fn step_action(error: &ModelError, is_last_step: bool) -> StepAction {
    match error {
        ModelError::Cancelled => StepAction::StopQuietly,
        ModelError::AlreadyDownloading if is_last_step => StepAction::StopQuietly,
        ModelError::AlreadyDownloading => StepAction::Continue,
        _ => StepAction::Fail,
    }
}

impl ModelsService {
    pub fn new(
        handle: tokio::runtime::Handle,
        catalog: Catalog,
        cfg: Config,
        dir: PathBuf,
        host: Arc<EngineHost>,
    ) -> Arc<Self> {
        let listener: Arc<Mutex<Option<Listener>>> = Arc::default();
        let manager = Self::build(&catalog, &cfg, dir, &host, &listener);
        Arc::new(Self {
            handle,
            catalog,
            cfg,
            manager: RwLock::new(manager),
            listener,
            host,
            pending_dir: Mutex::new(None),
        })
    }

    fn build(
        catalog: &Catalog,
        cfg: &Config,
        dir: PathBuf,
        host: &Arc<EngineHost>,
        listener: &Arc<Mutex<Option<Listener>>>,
    ) -> ModelManager {
        let (host, listener, cat) = (host.clone(), listener.clone(), catalog.clone());
        ModelManager::new(dir, catalog.clone(), cfg.clone()).with_progress(Arc::new(
            move |u: ProgressUpdate| {
                let is_stt = cat.get(&u.model).is_some_and(|e| e.kind == ModelKind::Stt);
                if is_stt {
                    host.set_download(match u.phase {
                        ProgressPhase::Finished => None,
                        _ => Some(DownloadProgress {
                            model: u.model.clone(),
                            done: u.done,
                            total: u.total,
                        }),
                    });
                }
                if let Some(l) = lock(&listener).clone() {
                    l(ModelsEvent::Progress);
                }
            },
        ))
    }

    pub fn set_listener(&self, f: impl Fn(ModelsEvent) + Send + Sync + 'static) {
        *lock(&self.listener) = Some(Arc::new(f));
    }

    fn notify(&self, e: ModelsEvent) {
        if let Some(l) = lock(&self.listener).clone() {
            l(e);
        }
    }

    pub fn manager(&self) -> ModelManager {
        self.manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Changes the models folder (`modelDir` setting). During a download, the change
    /// is DEFERRED: otherwise pause/resume/cancel would target the new manager (without a task)
    /// and the cancellation would delete a `.part` of the wrong folder.
    pub fn set_dir(&self, dir: PathBuf) {
        if self.host.download().is_some() {
            *lock(&self.pending_dir) = Some(dir);
            self.notify(ModelsEvent::Notice(tr(
                "The models folder will change when the current download ends.",
            )));
            return;
        }
        *lock(&self.pending_dir) = None;
        self.rebuild_for(dir);
    }

    fn rebuild_for(&self, dir: PathBuf) {
        if self.manager().dir() == dir {
            return;
        }
        let m = Self::build(&self.catalog, &self.cfg, dir, &self.host, &self.listener);
        *self.manager.write().unwrap_or_else(|e| e.into_inner()) = m;
    }

    /// Applies a pending folder change (end of a download).
    pub(crate) fn apply_pending_dir(&self) {
        let pending = lock(&self.pending_dir).take();
        if let Some(dir) = pending {
            self.rebuild_for(dir);
            self.notify(ModelsEvent::Progress);
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.manager().dir().to_path_buf()
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn path_of(&self, id: &str) -> Option<PathBuf> {
        self.manager().model_path(id)
    }

    pub fn statuses(&self) -> Vec<ModelStatus> {
        self.manager().statuses()
    }

    pub fn is_offline(&self) -> bool {
        self.manager().is_offline()
    }

    /// Downloads `id` (and the missing VAD first). Returns immediately.
    pub fn download(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let entry = self
            .catalog
            .get(id)
            .ok_or_else(|| ModelError::UnknownModel(id.into()).user_message())?;
        let mut ids = Vec::new();
        if entry.kind == ModelKind::Stt && self.path_of(VAD_ID).is_none() {
            if let Some(v) = self.catalog.get(VAD_ID) {
                ids.push(v.id.clone());
            }
        }
        ids.push(entry.id.clone());
        let this = self.clone();
        let wanted = id.to_string();
        self.handle.spawn(async move {
            let last = ids.len().saturating_sub(1);
            for (index, step) in ids.into_iter().enumerate() {
                let manager = this.manager();
                match manager.download(&step).await {
                    Ok(_) => {}
                    Err(e) => match step_action(&e, index == last) {
                        StepAction::Continue => continue,
                        StepAction::StopQuietly => {
                            this.host.set_download(None);
                            this.notify(ModelsEvent::Progress);
                            this.apply_pending_dir();
                            return;
                        }
                        StepAction::Fail => {
                            tracing::warn!("model download failed: {e}");
                            this.host.set_download(None);
                            this.notify(ModelsEvent::Notice(e.user_message()));
                            this.notify(ModelsEvent::Progress);
                            this.apply_pending_dir();
                            return;
                        }
                    },
                }
            }
            this.host.set_download(None);
            this.apply_pending_dir();
            this.notify(ModelsEvent::Finished { id: wanted });
        });
        Ok(())
    }

    /// Downloads the VAD if it is missing (first launch with a model already present).
    pub fn ensure_vad(self: &Arc<Self>) {
        if self.path_of(VAD_ID).is_none()
            && self.catalog.get(VAD_ID).is_some()
            && !self.is_offline()
        {
            let _ = self.download(VAD_ID);
        }
    }

    pub fn pause(&self, id: &str) -> Result<(), String> {
        if self.manager().pause(id) {
            self.notify(ModelsEvent::Progress);
            Ok(())
        } else {
            Err(tr("There is no running download for this model."))
        }
    }

    pub fn resume(self: &Arc<Self>, id: &str) -> Result<(), String> {
        if self.manager().resume(id) {
            self.notify(ModelsEvent::Progress);
            Ok(())
        } else {
            // After a restart: starting again resumes the partial file.
            self.download(id)
        }
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        self.manager().cancel(id);
        if self.host.download().is_some_and(|d| d.model == id) {
            self.host.set_download(None);
        }
        self.notify(ModelsEvent::Progress);
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let r = self.manager().delete(id).map_err(|e| e.user_message());
        self.notify(ModelsEvent::Progress);
        r
    }

    pub fn verify(self: &Arc<Self>, id: &str) -> Result<(), String> {
        if self.catalog.get(id).is_none() {
            return Err(ModelError::UnknownModel(id.into()).user_message());
        }
        let this = self.clone();
        let id = id.to_string();
        self.handle.spawn(async move {
            let msg = match this.manager().verify(&id).await {
                Ok(Integrity::Valid) => tr("The model file is intact."),
                Ok(Integrity::Corrupted { .. }) => {
                    tr("The model file is corrupted. Delete it and download it again.")
                }
                Ok(Integrity::NoReferenceSum { .. }) => {
                    tr("This model has no reference checksum, so it cannot be verified.")
                }
                Err(e) => e.user_message(),
            };
            this.notify(ModelsEvent::Notice(msg));
            this.notify(ModelsEvent::Progress);
        });
        Ok(())
    }

    /// Adopts a local file: the model is recognised by its exact size.
    pub fn use_local_file(self: &Arc<Self>, path: &Path) -> Result<(), String> {
        let size = std::fs::metadata(path)
            .map_err(|_| ModelError::InvalidFile("file not found".into()).user_message())?
            .len();
        let id = self
            .catalog
            .entries()
            .iter()
            .find(|e| e.download_bytes == size)
            .map(|e| e.id.clone())
            .ok_or_else(|| ModelError::InvalidFile("unknown size".into()).user_message())?;
        let this = self.clone();
        let path = path.to_path_buf();
        self.handle.spawn(async move {
            match this.manager().use_local_file(&id, &path).await {
                Ok(_) => this.notify(ModelsEvent::Adopted { id }),
                Err(e) => this.notify(ModelsEvent::Notice(e.user_message())),
            }
        });
        Ok(())
    }

    pub fn disk_usage(&self) -> u64 {
        self.manager().disk_usage()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxflow_core::domain::ModelState;
    use std::sync::mpsc;
    use std::time::Duration;

    fn catalog() -> Catalog {
        Catalog::from_json(
            r#"{"endpoint":"http://127.0.0.1:9","models":[
            {"id":"tiny","name":"Tiny","kind":"stt","file":"tiny.bin","download_bytes":4,
             "urls":["http://127.0.0.1:9/tiny.bin"],
             "sha256":"88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589",
             "usage":"light","rank":1},
            {"id":"silero-vad","name":"VAD","kind":"vad","file":"vad.bin","download_bytes":3,
             "urls":["http://127.0.0.1:9/vad.bin"],"required":true}]}"#,
        )
        .unwrap()
    }

    fn svc(
        dir: &Path,
        offline: bool,
    ) -> (tokio::runtime::Runtime, Arc<ModelsService>, Arc<EngineHost>) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let cfg = Config {
            offline,
            ..Config::default()
        };
        let host = Arc::new(EngineHost::new());
        let s = ModelsService::new(
            rt.handle().clone(),
            catalog(),
            cfg,
            dir.to_path_buf(),
            host.clone(),
        );
        (rt, s, host)
    }

    #[test]
    fn statuses_delete_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tiny.bin"), b"abcd").unwrap();
        let (_rt, s, _h) = svc(dir.path(), true);
        let st = s.statuses();
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].state, ModelState::Installed);
        assert!(s.path_of("tiny").is_some());
        assert!(s.delete("tiny").is_ok());
        assert!(s.path_of("tiny").is_none());
        assert!(s.delete("tiny").is_err());
        assert!(s.download("nope").is_err());
    }

    #[test]
    fn offline_download_reports_notice_and_no_progress() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, s, h) = svc(dir.path(), true);
        let (tx, rx) = mpsc::channel();
        s.set_listener(move |e| {
            if let ModelsEvent::Notice(m) = e {
                let _ = tx.send(m);
            }
        });
        s.download("tiny").unwrap();
        let msg = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(msg.contains("Offline mode"));
        assert!(h.download().is_none());
    }

    #[test]
    fn local_file_is_recognized_by_size_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir(&models).unwrap();
        let src = dir.path().join("mine.bin");
        std::fs::write(&src, b"abcd").unwrap();
        let (_rt, s, _h) = svc(&models, true);
        let (tx, rx) = mpsc::channel();
        s.set_listener(move |e| {
            let _ = tx.send(e);
        });
        // Unknown size: immediate refusal.
        let bad = dir.path().join("bad.bin");
        std::fs::write(&bad, b"x").unwrap();
        assert!(s.use_local_file(&bad).is_err());
        s.use_local_file(&src).unwrap();
        let ev = loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                ModelsEvent::Progress => continue,
                other => break other,
            }
        };
        assert_eq!(ev, ModelsEvent::Adopted { id: "tiny".into() });
        assert!(s.path_of("tiny").is_some());
    }

    #[test]
    fn set_dir_during_a_download_is_deferred_until_it_ends() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (_rt, s, host) = svc(a.path(), true);
        host.set_download(Some(DownloadProgress {
            model: "tiny".into(),
            done: 1,
            total: 10,
        }));
        s.set_dir(b.path().to_path_buf());
        assert_eq!(s.dir(), a.path(), "no change during the download");
        host.set_download(None);
        s.apply_pending_dir();
        assert_eq!(s.dir(), b.path());
    }

    #[test]
    fn a_vad_already_downloading_does_not_abandon_the_main_model() {
        // Regression: `AlreadyDownloading` of the VAD did a `return`: the model was never downloaded.
        assert_eq!(
            step_action(&ModelError::AlreadyDownloading, false),
            StepAction::Continue
        );
        assert_eq!(
            step_action(&ModelError::AlreadyDownloading, true),
            StepAction::StopQuietly
        );
        assert_eq!(
            step_action(&ModelError::Cancelled, false),
            StepAction::StopQuietly
        );
        assert_eq!(
            step_action(&ModelError::UnknownModel("x".into()), true),
            StepAction::Fail
        );
    }

    #[test]
    fn set_dir_rebuilds_manager() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (_rt, s, _h) = svc(a.path(), true);
        assert_eq!(s.dir(), a.path());
        s.set_dir(b.path().to_path_buf());
        assert_eq!(s.dir(), b.path());
    }
}
