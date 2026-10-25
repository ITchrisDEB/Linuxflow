//! Model manager: state, robust download and verification.
//!
//! The models folder is injected. The download writes to `<file>.part`, resumes with
//! `Range`, verifies the SHA-256 before the atomic rename, and never contacts the network in
//! offline mode. Downloaded files are never executed.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use futures_util::StreamExt;
use linuxflow_core::domain::{ModelState, ModelStatus};
use linuxflow_i18n::tr;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use crate::catalog::{Catalog, ModelEntry};
use crate::config::Config;
use crate::error::{ModelError, Result};
use crate::sys;

/// Phase reported in the progress notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPhase {
    Connecting,
    Downloading,
    Paused,
    /// SHA-256 computation (`done` = bytes read).
    Verifying,
    Finished,
}

/// Progress in exact bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressUpdate {
    pub model: String,
    pub done: u64,
    pub total: u64,
    pub phase: ProgressPhase,
}

impl ProgressUpdate {
    /// Displayable percentage, capped at 99 until the verification is finished.
    pub fn percent(&self) -> u8 {
        if self.phase == ProgressPhase::Finished {
            return 100;
        }
        if self.total == 0 {
            return 0;
        }
        (self.done.min(self.total) as u128 * 100 / self.total as u128).min(99) as u8
    }
}

pub type ProgressCallback = Arc<dyn Fn(ProgressUpdate) + Send + Sync>;
/// Free space (bytes) of the file system that contains the path.
pub type FreeSpaceFn = Arc<dyn Fn(&Path) -> io::Result<u64> + Send + Sync>;

/// Result of "Verify integrity".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    Valid,
    Corrupted {
        actual: String,
    },
    /// The catalogue gives no checksum: nothing to compare.
    NoReferenceSum {
        actual: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctl {
    Run,
    Pause,
    Cancel,
}

/// Identity of a file (size, modification date, inode): changes if the file is replaced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileId {
    len: u64,
    mtime_ns: i128,
    ino: u64,
}

fn file_id(path: &Path) -> Option<FileId> {
    let m = fs::metadata(path).ok()?;
    Some(FileId {
        len: m.len(),
        mtime_ns: m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
        ino: m.ino(),
    })
}

/// File lock (inter-process): removed from the disk when released.
struct FileLock {
    file: File,
    path: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Removed while the lock is held; `file` is then closed (and unlocked).
        let _ = fs::remove_file(&self.path);
        let _ = &self.file;
    }
}

struct Job {
    ctl: watch::Sender<Ctl>,
    done: AtomicU64,
    paused: AtomicBool,
}

struct Inner {
    dir: PathBuf,
    catalog: Catalog,
    cfg: Config,
    progress: Option<ProgressCallback>,
    free_space: FreeSpaceFn,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    errors: Mutex<HashMap<String, String>>,
    /// Installed files whose verification failed (identity of the file at the time of the check).
    corrupt: Mutex<HashMap<String, FileId>>,
    in_use: Mutex<Option<String>>,
    recommended: Mutex<Option<String>>,
}

/// Model manager (cloneable, shares its state).
#[derive(Clone)]
pub struct ModelManager {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s: OsString = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// SHA-256 of a file (blocking), with a callback of bytes read.
fn sha256_file(path: &Path, mut on_bytes: impl FnMut(u64)) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
        on_bytes(total);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Failure of a download attempt.
enum AttemptError {
    Cancelled,
    Paused,
    /// Start from zero immediately (resume impossible).
    Restart,
    /// New attempt after a wait (transient network error).
    Retry(ModelError),
    /// This mirror cannot provide the file: move on to the next one.
    Mirror(ModelError),
    /// Local error (disk…): no use trying another mirror.
    Abort(ModelError),
}

/// Progress emitter with a limited rate.
struct Emitter<'a> {
    inner: &'a Inner,
    id: &'a str,
    total: u64,
    last: Option<Instant>,
}

impl Emitter<'_> {
    fn emit(&mut self, done: u64, phase: ProgressPhase, force: bool) {
        let Some(cb) = &self.inner.progress else {
            return;
        };
        let now = Instant::now();
        let due = self
            .last
            .is_none_or(|t| now.duration_since(t) >= self.inner.cfg.progress_interval);
        if force || due {
            self.last = Some(now);
            cb(ProgressUpdate {
                model: self.id.to_string(),
                done,
                total: self.total,
                phase,
            });
        }
    }
}

struct JobGuard {
    inner: Arc<Inner>,
    id: String,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        lock(&self.inner.jobs).remove(&self.id);
    }
}

/// Parses `Content-Range: bytes a-b/total`; returns `(start, total)`.
fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((start.trim().parse().ok()?, total))
}

impl ModelManager {
    /// `dir`: models folder (injected, never hard-coded).
    pub fn new(dir: impl Into<PathBuf>, catalog: Catalog, cfg: Config) -> ModelManager {
        ModelManager {
            inner: Arc::new(Inner {
                dir: dir.into(),
                catalog,
                cfg,
                progress: None,
                free_space: Arc::new(sys::free_space),
                jobs: Mutex::new(HashMap::new()),
                errors: Mutex::new(HashMap::new()),
                corrupt: Mutex::new(HashMap::new()),
                in_use: Mutex::new(None),
                recommended: Mutex::new(None),
            }),
        }
    }

    /// Installs the progress callback (to call before cloning the manager).
    pub fn with_progress(mut self, cb: ProgressCallback) -> Self {
        if let Some(i) = Arc::get_mut(&mut self.inner) {
            i.progress = Some(cb);
        }
        self
    }

    /// Replaces the free-space measurement (tests, or particular platforms).
    pub fn with_free_space_fn(mut self, f: FreeSpaceFn) -> Self {
        if let Some(i) = Arc::get_mut(&mut self.inner) {
            i.free_space = f;
        }
        self
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    pub fn catalog(&self) -> &Catalog {
        &self.inner.catalog
    }

    pub fn is_offline(&self) -> bool {
        self.inner.cfg.offline
    }

    /// Remembers the model used by the engine (shown in `statuses`).
    pub fn set_in_use(&self, id: Option<&str>) {
        *lock(&self.inner.in_use) = id.map(str::to_string);
    }

    /// Remembers the model recommended for the machine (shown in `statuses`).
    pub fn set_recommended(&self, id: Option<&str>) {
        *lock(&self.inner.recommended) = id.map(str::to_string);
    }

    fn entry(&self, id: &str) -> Result<&ModelEntry> {
        self.inner
            .catalog
            .get(id)
            .ok_or_else(|| ModelError::UnknownModel(id.to_string()))
    }

    fn dest(&self, e: &ModelEntry) -> PathBuf {
        self.inner.dir.join(&e.file)
    }

    fn part(&self, e: &ModelEntry) -> PathBuf {
        suffixed(&self.dest(e), ".part")
    }

    fn etag_file(&self, e: &ModelEntry) -> PathBuf {
        suffixed(&self.dest(e), ".part.etag")
    }

    fn lock_file(&self, e: &ModelEntry) -> PathBuf {
        suffixed(&self.dest(e), ".lock")
    }

    fn set_error(&self, id: &str, msg: Option<String>) {
        let mut m = lock(&self.inner.errors);
        match msg {
            Some(m_) => {
                m.insert(id.to_string(), m_);
            }
            None => {
                m.remove(id);
            }
        }
    }

    fn job(&self, id: &str) -> Option<Arc<Job>> {
        lock(&self.inner.jobs).get(id).cloned()
    }

    fn installed_len(&self, e: &ModelEntry) -> Option<u64> {
        fs::metadata(self.dest(e))
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
    }

    /// Exclusive lock on the `<model>.lock` file; `AlreadyDownloading` if it is taken.
    fn acquire_lock(&self, e: &ModelEntry) -> Result<FileLock> {
        fs::create_dir_all(&self.inner.dir)?;
        let path = self.lock_file(e);
        loop {
            let file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => return Err(ModelError::AlreadyDownloading),
                Err(fs::TryLockError::Error(err)) => return Err(err.into()),
            }
            // The lock file may have been removed and recreated between opening and locking.
            let same = match (file.metadata(), fs::metadata(&path)) {
                (Ok(a), Ok(b)) => a.ino() == b.ino(),
                _ => false,
            };
            if same {
                return Ok(FileLock { file, path });
            }
        }
    }

    fn flag_corrupt(&self, e: &ModelEntry, id: Option<FileId>) {
        let mut m = lock(&self.inner.corrupt);
        match id {
            Some(i) => {
                m.insert(e.id.clone(), i);
            }
            None => {
                m.remove(&e.id);
            }
        }
    }

    /// The installed file is the one that `verify` declared corrupt.
    fn is_flagged_corrupt(&self, e: &ModelEntry) -> bool {
        let flagged = lock(&self.inner.corrupt).get(&e.id).copied();
        flagged.is_some_and(|f| file_id(&self.dest(e)) == Some(f))
    }

    /// Path of the model if it is present with the expected size and not declared corrupt.
    pub fn model_path(&self, id: &str) -> Option<PathBuf> {
        let e = self.inner.catalog.get(id)?;
        (self.installed_len(e) == Some(e.download_bytes) && !self.is_flagged_corrupt(e))
            .then(|| self.dest(e))
    }

    fn remove_partial(&self, e: &ModelEntry) {
        let _ = fs::remove_file(self.part(e));
        let _ = fs::remove_file(self.etag_file(e));
    }

    fn state_of(&self, e: &ModelEntry) -> ModelState {
        let total = e.download_bytes;
        if let Some(job) = self.job(&e.id) {
            let done = job.done.load(Ordering::Relaxed);
            return if job.paused.load(Ordering::Relaxed) {
                ModelState::Paused { done, total }
            } else {
                ModelState::Downloading { done, total }
            };
        }
        if let Some(len) = self.installed_len(e) {
            return if self.is_flagged_corrupt(e) {
                ModelState::Error(
                    ModelError::ChecksumMismatch {
                        expected: String::new(),
                        actual: String::new(),
                    }
                    .user_message(),
                )
            } else if len != total {
                ModelState::Error(tr(
                    "The file has an unexpected size. Verify it or delete it.",
                ))
            } else if e.is_verified() {
                ModelState::Installed
            } else {
                ModelState::Unverified
            };
        }
        if let Some(msg) = lock(&self.inner.errors).get(&e.id) {
            return ModelState::Error(msg.clone());
        }
        match fs::metadata(self.part(e)) {
            Ok(m) if m.is_file() && m.len() > 0 => ModelState::Paused {
                done: m.len().min(total),
                total,
            },
            _ => ModelState::NotInstalled,
        }
    }

    /// State of all the models of the catalogue.
    pub fn statuses(&self) -> Vec<ModelStatus> {
        let rec = lock(&self.inner.recommended).clone();
        let used = lock(&self.inner.in_use).clone();
        self.inner
            .catalog
            .entries()
            .iter()
            .map(|e| ModelStatus {
                id: e.id.clone(),
                name: e.name.clone(),
                download_bytes: e.download_bytes,
                disk_bytes: e.disk_bytes,
                memory_bytes: e.memory_bytes(),
                state: self.state_of(e),
                recommended: rec.as_deref() == Some(e.id.as_str()),
                in_use: used.as_deref() == Some(e.id.as_str()),
            })
            .collect()
    }

    /// Bytes used in the models folder (finished files and `.part`).
    pub fn disk_usage(&self) -> u64 {
        let Ok(rd) = fs::read_dir(&self.inner.dir) else {
            return 0;
        };
        rd.filter_map(|d| d.ok())
            .filter_map(|d| d.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum()
    }

    // ---- Control of a running download ----

    /// Pauses (the `.part` is kept). `false` if no active download.
    pub fn pause(&self, id: &str) -> bool {
        self.job(id).is_some_and(|j| j.ctl.send(Ctl::Pause).is_ok())
    }

    /// Resumes a download paused in this instance.
    /// After a restart, calling `download` again resumes the `.part`.
    pub fn resume(&self, id: &str) -> bool {
        self.job(id).is_some_and(|j| j.ctl.send(Ctl::Run).is_ok())
    }

    /// Cancels and deletes the `.part`. Without an active download, deletes an orphan `.part`.
    pub fn cancel(&self, id: &str) -> bool {
        if let Some(j) = self.job(id) {
            return j.ctl.send(Ctl::Cancel).is_ok();
        }
        let Ok(e) = self.entry(id) else { return false };
        let existed = self.part(e).exists();
        if !existed {
            self.set_error(id, None);
            return false;
        }
        // A `.part` held by another process must not be deleted.
        let Ok(_lock) = self.acquire_lock(e) else {
            return false;
        };
        self.remove_partial(e);
        self.set_error(id, None);
        existed
    }

    // ---- Download ----

    /// Downloads the model `id` (resumes an existing `.part`). The future ends at the end of the
    /// download, on error or on cancellation; `pause`/`resume`/`cancel` drive it.
    pub async fn download(&self, id: &str) -> Result<PathBuf> {
        let entry = self.entry(id)?.clone();
        if self.installed_len(&entry) == Some(entry.download_bytes) {
            return Ok(self.dest(&entry));
        }
        if self.inner.cfg.offline {
            return Err(ModelError::Offline);
        }
        let (tx, mut rx) = watch::channel(Ctl::Run);
        let job = Arc::new(Job {
            ctl: tx,
            done: AtomicU64::new(0),
            paused: AtomicBool::new(false),
        });
        {
            let mut jobs = lock(&self.inner.jobs);
            if jobs.contains_key(id) {
                return Err(ModelError::AlreadyDownloading);
            }
            jobs.insert(id.to_string(), job.clone());
        }
        let _guard = JobGuard {
            inner: self.inner.clone(),
            id: id.to_string(),
        };
        let res = self.run(&entry, &job, &mut rx).await;
        match &res {
            Ok(_) | Err(ModelError::Cancelled) => self.set_error(id, None),
            Err(ModelError::AlreadyDownloading) => {}
            Err(e) => {
                tracing::warn!(model = id, error = %e, "model download failed");
                self.set_error(id, Some(e.user_message()));
            }
        }
        res
    }

    /// Effective URLs of a model: the official origin of the catalogue can be replaced.
    fn urls(&self, e: &ModelEntry) -> Vec<String> {
        let base = self.inner.catalog.endpoint();
        match &self.inner.cfg.endpoint {
            Some(over) if !base.is_empty() => e
                .urls
                .iter()
                .map(|u| match u.strip_prefix(base) {
                    Some(rest) if rest.starts_with('/') => format!("{over}{rest}"),
                    _ => u.clone(),
                })
                .collect(),
            _ => e.urls.clone(),
        }
    }

    fn build_client(&self) -> Result<reqwest::Client> {
        use reqwest::redirect::Policy;
        let cfg = &self.inner.cfg;
        let policy = Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if attempt
                .previous()
                .last()
                .is_some_and(|p| p.scheme() == "https")
                && attempt.url().scheme() != "https"
            {
                attempt.error("redirect from https to http refused")
            } else {
                attempt.follow()
            }
        });
        let mut b = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .connect_timeout(cfg.connect_timeout)
            .redirect(policy);
        if !cfg.system_proxy {
            b = b.no_proxy();
        }
        b.build()
            .map_err(|e| ModelError::Network(e.without_url().to_string()))
    }

    async fn run(
        &self,
        e: &ModelEntry,
        job: &Arc<Job>,
        rx: &mut watch::Receiver<Ctl>,
    ) -> Result<PathBuf> {
        // Inter-process lock: only one download of this file at a time.
        let _lock = self.acquire_lock(e)?;
        // Another process may have finished in the meantime.
        if self.installed_len(e) == Some(e.download_bytes) {
            return Ok(self.dest(e));
        }
        let part_len = fs::metadata(self.part(e))
            .map(|m| m.len())
            .unwrap_or(0)
            .min(e.download_bytes);
        let needed = e.download_bytes - part_len + self.inner.cfg.disk_margin_bytes;
        match (self.inner.free_space)(&self.inner.dir) {
            Ok(available) if available < needed => {
                return Err(ModelError::InsufficientSpace { needed, available });
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "free disk space unknown, continuing"),
        }
        job.done.store(part_len, Ordering::Relaxed);
        let client = self.build_client()?;
        let mut last: Option<ModelError> = None;
        for url in self.urls(e) {
            match self.try_mirror(&client, e, &url, job, rx).await {
                Ok(()) => return Ok(self.dest(e)),
                Err(ModelError::Cancelled) => {
                    self.remove_partial(e);
                    return Err(ModelError::Cancelled);
                }
                Err(err @ (ModelError::Io(_) | ModelError::InsufficientSpace { .. })) => {
                    return Err(err)
                }
                Err(err) => {
                    tracing::warn!(model = %e.id, error = %err, "mirror failed, trying next");
                    last = Some(err);
                }
            }
        }
        Err(last.unwrap_or_else(|| ModelError::Network("no download source".into())))
    }

    async fn try_mirror(
        &self,
        client: &reqwest::Client,
        e: &ModelEntry,
        url: &str,
        job: &Arc<Job>,
        rx: &mut watch::Receiver<Ctl>,
    ) -> Result<()> {
        let cfg = &self.inner.cfg;
        let mut attempts = 0u32;
        let mut restarts = 0u32;
        loop {
            // State requested before each attempt.
            let state = *rx.borrow_and_update();
            match state {
                Ctl::Cancel => return Err(ModelError::Cancelled),
                Ctl::Pause => {
                    self.wait_resume(e, job, rx).await?;
                    continue;
                }
                Ctl::Run => {}
            }
            match self.attempt(client, e, url, job, rx).await {
                Ok(()) => return Ok(()),
                Err(AttemptError::Cancelled) => return Err(ModelError::Cancelled),
                Err(AttemptError::Paused) => {
                    self.wait_resume(e, job, rx).await?;
                }
                Err(AttemptError::Restart) => {
                    restarts += 1;
                    if restarts > 2 {
                        return Err(ModelError::Protocol(
                            "the file keeps changing on the server".into(),
                        ));
                    }
                    self.remove_partial(e);
                    job.done.store(0, Ordering::Relaxed);
                }
                Err(AttemptError::Abort(err)) | Err(AttemptError::Mirror(err)) => return Err(err),
                Err(AttemptError::Retry(err)) => {
                    attempts += 1;
                    if attempts >= cfg.max_attempts {
                        return Err(err);
                    }
                    let wait = cfg
                        .retry_base
                        .saturating_mul(1 << (attempts - 1).min(16))
                        .min(cfg.retry_max);
                    tracing::info!(model = %e.id, attempt = attempts, ?wait, "retrying after error: {err}");
                    // The wait is interrupted by a pause or a cancellation.
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = rx.changed() => {}
                    }
                }
            }
        }
    }

    /// Waits for the resume (or the cancellation) after a pause.
    async fn wait_resume(
        &self,
        e: &ModelEntry,
        job: &Arc<Job>,
        rx: &mut watch::Receiver<Ctl>,
    ) -> Result<()> {
        job.paused.store(true, Ordering::Relaxed);
        let done = job.done.load(Ordering::Relaxed);
        Emitter {
            inner: &self.inner,
            id: &e.id,
            total: e.download_bytes,
            last: None,
        }
        .emit(done, ProgressPhase::Paused, true);
        let out = loop {
            let state = *rx.borrow_and_update();
            match state {
                Ctl::Run => break Ok(()),
                Ctl::Cancel => break Err(ModelError::Cancelled),
                Ctl::Pause => {
                    if rx.changed().await.is_err() {
                        break Err(ModelError::Cancelled);
                    }
                }
            }
        };
        job.paused.store(false, Ordering::Relaxed);
        out
    }

    async fn attempt(
        &self,
        client: &reqwest::Client,
        e: &ModelEntry,
        url: &str,
        job: &Arc<Job>,
        rx: &mut watch::Receiver<Ctl>,
    ) -> std::result::Result<(), AttemptError> {
        let cfg = &self.inner.cfg;
        let total = e.download_bytes;
        let part = self.part(e);
        let etag_path = self.etag_file(e);
        let mut em = Emitter {
            inner: &self.inner,
            id: &e.id,
            total,
            last: None,
        };

        let mut offset = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset > total {
            self.remove_partial(e);
            offset = 0;
        }
        job.done.store(offset, Ordering::Relaxed);
        em.emit(offset, ProgressPhase::Connecting, true);

        if offset < total {
            let stored_etag = fs::read_to_string(&etag_path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let mut req = client.get(url);
            if offset > 0 {
                req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
                if let Some(t) = stored_etag.as_deref().filter(|t| !t.starts_with("W/")) {
                    req = req.header(reqwest::header::IF_RANGE, t);
                }
            }
            // Headers: response expected within the delay, interruptible.
            let resp = tokio::select! {
                biased;
                _ = rx.changed() => {
                    let state = *rx.borrow_and_update();
                    return Err(match state {
                        Ctl::Cancel => AttemptError::Cancelled,
                        Ctl::Pause => AttemptError::Paused,
                        Ctl::Run => AttemptError::Retry(ModelError::Network("interrupted".into())),
                    });
                }
                r = tokio::time::timeout(cfg.stall_timeout, req.send()) => match r {
                    Err(_) => return Err(AttemptError::Retry(ModelError::Network("timeout waiting for the server".into()))),
                    Ok(Err(err)) => return Err(AttemptError::Retry(ModelError::Network(err.without_url().to_string()))),
                    Ok(Ok(r)) => r,
                },
            };
            let status = resp.status().as_u16();
            let new_etag = resp
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string());
            let start = match status {
                206 => {
                    let cr = resp
                        .headers()
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_content_range);
                    let ok = offset > 0
                        && cr.is_some_and(|(s, t)| s == offset && t.is_none_or(|t| t == total))
                        && !matches!((&stored_etag, &new_etag), (Some(a), Some(b)) if a != b);
                    if !ok {
                        return Err(AttemptError::Restart);
                    }
                    offset
                }
                200 => {
                    if let Some(len) = resp.content_length() {
                        if len != total {
                            return Err(AttemptError::Mirror(ModelError::Protocol(format!(
                                "unexpected size {len}, expected {total}"
                            ))));
                        }
                    }
                    // Range ignored or file modified: start again from zero with this response.
                    0
                }
                416 => {
                    return Err(AttemptError::Restart);
                }
                408 | 429 | 500..=599 => return Err(AttemptError::Retry(ModelError::Http(status))),
                _ => return Err(AttemptError::Mirror(ModelError::Http(status))),
            };
            if status == 200 {
                match &new_etag {
                    Some(t) => {
                        fs::write(&etag_path, t).map_err(|er| AttemptError::Abort(er.into()))?;
                    }
                    None => {
                        let _ = fs::remove_file(&etag_path);
                    }
                }
            }
            let mut opts = tokio::fs::OpenOptions::new();
            opts.create(true);
            if start == 0 {
                opts.write(true).truncate(true);
            } else {
                opts.append(true);
            }
            let mut file = opts
                .open(&part)
                .await
                .map_err(|er| AttemptError::Abort(er.into()))?;
            job.done.store(start, Ordering::Relaxed);
            let body = self
                .stream_body(resp, &mut file, start, job, rx, &mut em)
                .await;
            // Write always finished before returning: the `.part` reflects the bytes received.
            let flushed = file.flush().await;
            drop(file);
            body?;
            flushed.map_err(|er| AttemptError::Abort(er.into()))?;
        }
        self.finalize(e, job, &mut em).await
    }

    async fn stream_body(
        &self,
        resp: reqwest::Response,
        file: &mut tokio::fs::File,
        start: u64,
        job: &Arc<Job>,
        rx: &mut watch::Receiver<Ctl>,
        em: &mut Emitter<'_>,
    ) -> std::result::Result<(), AttemptError> {
        let total = em.total;
        let stall = self.inner.cfg.stall_timeout;
        let mut stream = Box::pin(resp.bytes_stream());
        let mut written = start;
        em.emit(written, ProgressPhase::Downloading, true);
        loop {
            tokio::select! {
                biased;
                changed = rx.changed() => {
                    let state = if changed.is_err() { Ctl::Cancel } else { *rx.borrow_and_update() };
                    match state {
                        Ctl::Run => {}
                        Ctl::Pause => return Err(AttemptError::Paused),
                        Ctl::Cancel => return Err(AttemptError::Cancelled),
                    }
                }
                next = tokio::time::timeout(stall, stream.next()) => match next {
                    Err(_) => return Err(AttemptError::Retry(ModelError::Network("connection stalled".into()))),
                    Ok(None) => break,
                    Ok(Some(Err(err))) => {
                        return Err(AttemptError::Retry(ModelError::Network(err.without_url().to_string())));
                    }
                    Ok(Some(Ok(chunk))) => {
                        if written + chunk.len() as u64 > total {
                            return Err(AttemptError::Mirror(ModelError::Protocol("more data than expected".into())));
                        }
                        file.write_all(&chunk).await.map_err(|er| AttemptError::Abort(er.into()))?;
                        written += chunk.len() as u64;
                        job.done.store(written, Ordering::Relaxed);
                        em.emit(written, ProgressPhase::Downloading, false);
                    }
                },
            }
        }
        if written < total {
            return Err(AttemptError::Retry(ModelError::Protocol(format!(
                "response truncated at {written}/{total}"
            ))));
        }
        Ok(())
    }

    /// Verifies the SHA-256 of the complete `.part` then renames it atomically.
    async fn finalize(
        &self,
        e: &ModelEntry,
        job: &Arc<Job>,
        em: &mut Emitter<'_>,
    ) -> std::result::Result<(), AttemptError> {
        let part = self.part(e);
        let dest = self.dest(e);
        job.done.store(e.download_bytes, Ordering::Relaxed);
        if fs::metadata(&part).map(|m| m.len()).unwrap_or(0) != e.download_bytes {
            self.remove_partial(e);
            return Err(AttemptError::Restart);
        }
        if let Some(expected) = &e.sha256 {
            em.emit(0, ProgressPhase::Verifying, true);
            let actual = self
                .hash_blocking(&part, e, ProgressPhase::Verifying)
                .await
                .map_err(AttemptError::Abort)?;
            if &actual != expected {
                self.remove_partial(e);
                return Err(AttemptError::Mirror(ModelError::ChecksumMismatch {
                    expected: expected.clone(),
                    actual,
                }));
            }
        }
        let part2 = part.clone();
        let dir = self.inner.dir.clone();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            File::open(&part2)?.sync_all()?;
            fs::set_permissions(&part2, fs::Permissions::from_mode(0o644))?;
            fs::rename(&part2, &dest)?;
            if let Ok(d) = File::open(&dir) {
                let _ = d.sync_all();
            }
            Ok(())
        })
        .await
        .map_err(|er| AttemptError::Abort(ModelError::Io(io::Error::other(er))))?
        .map_err(|er| AttemptError::Abort(er.into()))?;
        let _ = fs::remove_file(self.etag_file(e));
        em.emit(e.download_bytes, ProgressPhase::Finished, true);
        Ok(())
    }

    async fn hash_blocking(
        &self,
        path: &Path,
        e: &ModelEntry,
        phase: ProgressPhase,
    ) -> Result<String> {
        let path = path.to_path_buf();
        let inner = self.inner.clone();
        let id = e.id.clone();
        let total = e.download_bytes;
        let res = tokio::task::spawn_blocking(move || {
            let mut em = Emitter {
                inner: &inner,
                id: &id,
                total,
                last: None,
            };
            sha256_file(&path, |n| em.emit(n, phase, false))
        })
        .await
        .map_err(|er| ModelError::Io(io::Error::other(er)))?;
        Ok(res?)
    }

    // ---- Integrity, deletion, local file ----

    /// Recomputes the SHA-256 of the installed file.
    pub async fn verify(&self, id: &str) -> Result<Integrity> {
        let e = self.entry(id)?.clone();
        if self.job(id).is_some() {
            return Err(ModelError::Busy);
        }
        if self.installed_len(&e).is_none() {
            return Err(ModelError::NotInstalled);
        }
        let before = file_id(&self.dest(&e));
        let actual = self
            .hash_blocking(&self.dest(&e), &e, ProgressPhase::Verifying)
            .await?;
        Ok(match &e.sha256 {
            Some(exp) if *exp == actual => {
                self.set_error(id, None);
                self.flag_corrupt(&e, None);
                Integrity::Valid
            }
            Some(_) => {
                self.flag_corrupt(&e, before);
                Integrity::Corrupted { actual }
            }
            None => Integrity::NoReferenceSum { actual },
        })
    }

    /// Deletes the installed file and the download leftovers.
    pub fn delete(&self, id: &str) -> Result<()> {
        let e = self.entry(id)?;
        if self.job(id).is_some() || lock(&self.inner.in_use).as_deref() == Some(id) {
            return Err(ModelError::Busy);
        }
        if !self.inner.dir.exists() {
            return Err(ModelError::NotInstalled);
        }
        let _lock = match self.acquire_lock(e) {
            Ok(l) => l,
            Err(ModelError::AlreadyDownloading) => return Err(ModelError::Busy),
            Err(err) => return Err(err),
        };
        let mut removed = false;
        for p in [self.dest(e), self.part(e), self.etag_file(e)] {
            match fs::remove_file(&p) {
                Ok(()) => removed = true,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        self.set_error(id, None);
        self.flag_corrupt(e, None);
        if removed {
            Ok(())
        } else {
            Err(ModelError::NotInstalled)
        }
    }

    /// Adopts an already downloaded local file: check of presence, size and checksum, then
    /// hard link (or copy) into the models folder. Symbolic links are resolved before
    /// any operation; the file is never executed.
    pub async fn use_local_file(&self, id: &str, path: &Path) -> Result<PathBuf> {
        let e = self.entry(id)?.clone();
        if self.job(id).is_some() {
            return Err(ModelError::Busy);
        }
        // Canonical path: a symbolic link is never linked, only its target.
        let src =
            fs::canonicalize(path).map_err(|_| ModelError::InvalidFile("file not found".into()))?;
        let md =
            fs::metadata(&src).map_err(|_| ModelError::InvalidFile("file not found".into()))?;
        if !md.is_file() {
            return Err(ModelError::InvalidFile("not a regular file".into()));
        }
        if md.len() != e.download_bytes {
            return Err(ModelError::InvalidFile(format!(
                "size {} differs from expected {}",
                md.len(),
                e.download_bytes
            )));
        }
        let _lock = match self.acquire_lock(&e) {
            Ok(l) => l,
            Err(ModelError::AlreadyDownloading) => return Err(ModelError::Busy),
            Err(err) => return Err(err),
        };
        if let Some(exp) = &e.sha256 {
            let actual = self
                .hash_blocking(&src, &e, ProgressPhase::Verifying)
                .await?;
            if &actual != exp {
                return Err(ModelError::ChecksumMismatch {
                    expected: exp.clone(),
                    actual,
                });
            }
        }
        let dest = self.dest(&e);
        let tmp = suffixed(&dest, ".import");
        let (dest2, len) = (dest.clone(), e.download_bytes);
        let free = self.inner.free_space.clone();
        let margin = self.inner.cfg.disk_margin_bytes;
        tokio::task::spawn_blocking(move || {
            import_file(&src, &dest2, &tmp, len, margin, &free, false)
        })
        .await
        .map_err(|er| ModelError::Io(io::Error::other(er)))??;
        self.remove_partial(&e);
        self.set_error(id, None);
        self.flag_corrupt(&e, None);
        Ok(dest)
    }
}

/// Places `src` (canonical path of a regular file) at `dest`: hard link, otherwise copy after
/// checking the free space. The temporary file is deleted on failure; the result
/// must be a regular file of the right size.
fn import_file(
    src: &Path,
    dest: &Path,
    tmp: &Path,
    len: u64,
    margin: u64,
    free: &FreeSpaceFn,
    force_copy: bool,
) -> Result<()> {
    if let (Ok(a), Ok(b)) = (fs::canonicalize(src), fs::canonicalize(dest)) {
        if a == b {
            return Ok(());
        }
    }
    let _ = fs::remove_file(tmp);
    let linked = !force_copy && fs::hard_link(src, tmp).is_ok();
    if !linked {
        let dir = dest.parent().unwrap_or(Path::new("."));
        if let Ok(available) = free(dir) {
            let needed = len + margin;
            if available < needed {
                return Err(ModelError::InsufficientSpace { needed, available });
            }
        }
        let copied = fs::copy(src, tmp)
            .and_then(|_| fs::set_permissions(tmp, fs::Permissions::from_mode(0o644)));
        if let Err(err) = copied {
            let _ = fs::remove_file(tmp);
            return Err(err.into());
        }
    }
    let regular = fs::symlink_metadata(tmp).is_ok_and(|m| m.is_file() && m.len() == len);
    if !regular {
        let _ = fs::remove_file(tmp);
        return Err(ModelError::InvalidFile(
            "imported file is not a regular file".into(),
        ));
    }
    if let Err(err) = fs::rename(tmp, dest) {
        let _ = fs::remove_file(tmp);
        return Err(err.into());
    }
    let ok = fs::metadata(dest).is_ok_and(|m| m.is_file() && m.len() == len);
    if ok {
        Ok(())
    } else {
        Err(ModelError::InvalidFile(
            "imported file is not readable".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_parsing() {
        assert_eq!(
            parse_content_range("bytes 100-999/1000"),
            Some((100, Some(1000)))
        );
        assert_eq!(parse_content_range("bytes 0-9/*"), Some((0, None)));
        assert_eq!(parse_content_range("bytes */1000"), None);
        assert_eq!(parse_content_range("items 1-2/3"), None);
        assert_eq!(parse_content_range("bytes x-2/3"), None);
    }

    #[test]
    fn percent_is_capped_until_finished() {
        let mut p = ProgressUpdate {
            model: "m".into(),
            done: 100,
            total: 100,
            phase: ProgressPhase::Verifying,
        };
        assert_eq!(p.percent(), 99);
        p.phase = ProgressPhase::Finished;
        assert_eq!(p.percent(), 100);
        p.phase = ProgressPhase::Downloading;
        p.done = 42;
        p.total = 100;
        assert_eq!(p.percent(), 42);
        p.total = 0;
        assert_eq!(p.percent(), 0);
    }

    #[test]
    fn import_copy_checks_space_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let (src, dest, tmp) = (
            d.path().join("s"),
            d.path().join("m.bin"),
            d.path().join("m.bin.import"),
        );
        fs::write(&src, vec![1u8; 100]).unwrap();
        let low: FreeSpaceFn = Arc::new(|_| Ok(10));
        let err = import_file(&src, &dest, &tmp, 100, 0, &low, true).unwrap_err();
        assert!(matches!(err, ModelError::InsufficientSpace { .. }));
        assert!(!tmp.exists() && !dest.exists());
        // Copy that fails (source gone): no leftover.
        let ok: FreeSpaceFn = Arc::new(|_| Ok(u64::MAX));
        assert!(import_file(&d.path().join("absent"), &dest, &tmp, 100, 0, &ok, true).is_err());
        assert!(!tmp.exists() && !dest.exists());
        import_file(&src, &dest, &tmp, 100, 0, &ok, true).unwrap();
        assert!(fs::symlink_metadata(&dest).unwrap().is_file());
        assert_ne!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&dest).unwrap().ino(),
            "copie"
        );
    }

    #[test]
    fn suffix_keeps_directory() {
        assert_eq!(
            suffixed(Path::new("/a/b.bin"), ".part"),
            PathBuf::from("/a/b.bin.part")
        );
    }
}
