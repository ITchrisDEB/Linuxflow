//! Opening of known folders only (a fixed allow-list instead of any
//! path).
//!
//! Allow-list: logs, data, configuration, models. Any other path is refused:
//! relative path, `..` component, symbolic link leaving the allowed folders,
//! element that is not a folder. Opening through the OpenURI portal (`ashpd`), fallback on
//! `xdg-open`.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Folders that the application agrees to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownDir {
    Logs,
    Data,
    Config,
    Models,
}

/// Concrete locations of the known folders (filled in by the application from `AppPaths`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownDirs {
    pub logs: PathBuf,
    pub data: PathBuf,
    pub config: PathBuf,
    pub models: PathBuf,
}

impl KnownDirs {
    pub fn get(&self, kind: KnownDir) -> &Path {
        match kind {
            KnownDir::Logs => &self.logs,
            KnownDir::Data => &self.data,
            KnownDir::Config => &self.config,
            KnownDir::Models => &self.models,
        }
    }

    fn all(&self) -> [&Path; 4] {
        [&self.logs, &self.data, &self.config, &self.models]
    }
}

#[derive(Debug)]
pub enum OpenError {
    /// Path not allowed (relative, `..`, outside the allow-list, outgoing link, not a folder).
    Refused(String),
    /// The folder does not exist (yet).
    Missing(PathBuf),
    /// Neither the portal nor `xdg-open` could open it.
    Launch(String),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Missing(_) => write!(f, "folder does not exist"),
            Self::Launch(why) => write!(f, "could not open folder: {why}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Opens a known folder in the file manager.
pub fn open_dir(kind: KnownDir, dirs: &KnownDirs) -> Result<(), OpenError> {
    open_path(dirs.get(kind), dirs)
}

/// Opens `path` if it is (or is inside) an allowed folder.
pub fn open_path(path: &Path, dirs: &KnownDirs) -> Result<(), OpenError> {
    open_path_with(path, dirs, &launch)
}

/// Variant with an injected launcher (tests): it receives the validated canonical path.
pub fn open_path_with(
    path: &Path,
    dirs: &KnownDirs,
    launcher: &dyn Fn(&Path) -> Result<(), OpenError>,
) -> Result<(), OpenError> {
    let canonical = validate(path, dirs)?;
    launcher(&canonical)
}

/// Validates `path` and returns its canonical form.
pub fn validate(path: &Path, dirs: &KnownDirs) -> Result<PathBuf, OpenError> {
    if !path.is_absolute() {
        return Err(OpenError::Refused("relative path".into()));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(OpenError::Refused("parent directory component".into()));
    }
    let canonical = match path.canonicalize() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(OpenError::Missing(path.to_path_buf()));
        }
        Err(e) => return Err(OpenError::Refused(format!("unreadable path: {e}"))),
    };
    // The roots are canonicalised too: the user may have put a symbolic link
    // on their own data folder; it is a link *inside* that is refused.
    let allowed = dirs
        .all()
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| canonical.starts_with(&root));
    if !allowed {
        return Err(OpenError::Refused("outside of the allowed folders".into()));
    }
    if !canonical.is_dir() {
        return Err(OpenError::Refused("not a folder".into()));
    }
    Ok(canonical)
}

/// OpenURI portal, then `xdg-open`.
fn launch(dir: &Path) -> Result<(), OpenError> {
    match portal_open(dir) {
        Ok(()) => Ok(()),
        Err(portal_err) => {
            tracing::debug!("OpenURI portal unavailable: {portal_err}");
            xdg_open(dir).map_err(|e| OpenError::Launch(format!("{portal_err}; xdg-open: {e}")))
        }
    }
}

/// Runs the portal request on a dedicated thread (the calling thread may already be inside a
/// tokio runtime, where `block_on` is forbidden).
fn portal_open(dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(dir).map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let request = ashpd::desktop::open_uri::OpenFileRequest::default()
                .ask(false)
                .send_file(&file);
            tokio::time::timeout(std::time::Duration::from_secs(5), request)
                .await
                .map_err(|_| "portal timeout".to_string())?
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    })
    .join()
    .map_err(|_| "portal thread panicked".to_string())?
}

fn xdg_open(dir: &Path) -> Result<(), String> {
    // Detached: the end of the file manager is not waited for.
    let mut child = std::process::Command::new("xdg-open")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fixture {
        _root: tempfile::TempDir,
        base: PathBuf,
        dirs: KnownDirs,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("tmp");
        let base = root.path().canonicalize().expect("canon");
        let mk = |n: &str| {
            let p = base.join(n);
            std::fs::create_dir_all(&p).expect("mkdir");
            p
        };
        let dirs = KnownDirs {
            logs: mk("logs"),
            data: mk("data"),
            config: mk("config"),
            models: mk("data/models"),
        };
        Fixture {
            _root: root,
            base,
            dirs,
        }
    }

    fn run(f: &Fixture, path: &Path) -> (Result<(), OpenError>, Vec<PathBuf>) {
        let calls = RefCell::new(Vec::new());
        let r = open_path_with(path, &f.dirs, &|p| {
            calls.borrow_mut().push(p.to_path_buf());
            Ok(())
        });
        (r, calls.into_inner())
    }

    #[test]
    fn known_dirs_are_opened() {
        let f = fixture();
        for kind in [
            KnownDir::Logs,
            KnownDir::Data,
            KnownDir::Config,
            KnownDir::Models,
        ] {
            let calls = RefCell::new(Vec::new());
            open_path_with(f.dirs.get(kind), &f.dirs, &|p| {
                calls.borrow_mut().push(p.to_path_buf());
                Ok(())
            })
            .expect("allowed");
            assert_eq!(calls.into_inner(), vec![f.dirs.get(kind).to_path_buf()]);
        }
    }

    #[test]
    fn subfolder_of_known_dir_is_allowed() {
        let f = fixture();
        let sub = f.dirs.logs.join("2026");
        std::fs::create_dir(&sub).expect("mkdir");
        assert!(run(&f, &sub).0.is_ok());
    }

    #[test]
    fn relative_path_is_refused() {
        let f = fixture();
        let (r, calls) = run(&f, Path::new("logs"));
        assert!(matches!(r, Err(OpenError::Refused(_))));
        assert!(calls.is_empty());
    }

    #[test]
    fn parent_dir_is_refused_even_if_it_resolves_inside() {
        let f = fixture();
        let sneaky = f.dirs.logs.join("..").join("data");
        let (r, calls) = run(&f, &sneaky);
        assert!(matches!(r, Err(OpenError::Refused(_))));
        assert!(calls.is_empty());
    }

    #[test]
    fn arbitrary_path_is_refused() {
        let f = fixture();
        let other = f.base.join("elsewhere");
        std::fs::create_dir(&other).expect("mkdir");
        let (r, calls) = run(&f, &other);
        assert!(matches!(r, Err(OpenError::Refused(_))));
        assert!(calls.is_empty());
        // The common parent of the allowed folders is not allowed either.
        assert!(matches!(run(&f, &f.base).0, Err(OpenError::Refused(_))));
        assert!(matches!(
            run(&f, Path::new("/")).0,
            Err(OpenError::Refused(_))
        ));
    }

    #[test]
    fn symlink_escaping_the_whitelist_is_refused() {
        let f = fixture();
        let outside = f.base.join("outside");
        std::fs::create_dir(&outside).expect("mkdir");
        let link = f.dirs.logs.join("link");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        let (r, calls) = run(&f, &link);
        assert!(matches!(r, Err(OpenError::Refused(_))));
        assert!(calls.is_empty());
    }

    #[test]
    fn symlink_staying_inside_is_resolved() {
        let f = fixture();
        let link = f.dirs.logs.join("to-data");
        std::os::unix::fs::symlink(&f.dirs.data, &link).expect("symlink");
        let (r, calls) = run(&f, &link);
        assert!(r.is_ok());
        assert_eq!(calls, vec![f.dirs.data.clone()]);
    }

    #[test]
    fn whitelisted_root_may_itself_be_a_symlink() {
        let f = fixture();
        let real = f.base.join("real-logs");
        std::fs::create_dir(&real).expect("mkdir");
        let link_root = f.base.join("logs-link");
        std::os::unix::fs::symlink(&real, &link_root).expect("symlink");
        let dirs = KnownDirs {
            logs: link_root,
            ..f.dirs.clone()
        };
        assert_eq!(validate(&dirs.logs, &dirs).expect("ok"), real);
    }

    #[test]
    fn files_are_refused() {
        let f = fixture();
        let file = f.dirs.config.join("settings.json");
        std::fs::write(&file, "{}").expect("write");
        assert!(matches!(run(&f, &file).0, Err(OpenError::Refused(_))));
    }

    #[test]
    fn missing_folder_is_reported() {
        let f = fixture();
        let missing = f.dirs.logs.join("nope");
        assert!(matches!(run(&f, &missing).0, Err(OpenError::Missing(_))));
    }

    /// Really opens the logs folder: `LINUXFLOW_TEST_DESKTOP=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_open_logs() {
        if std::env::var("LINUXFLOW_TEST_DESKTOP").as_deref() != Ok("1") {
            return;
        }
        let f = fixture();
        open_dir(KnownDir::Logs, &f.dirs).expect("ouverture");
    }
}
