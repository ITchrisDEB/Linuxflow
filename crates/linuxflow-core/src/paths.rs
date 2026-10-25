//! Application paths: XDG through the `directories` crate, `LINUXFLOW_*` overrides.
//! No hard-coded path; the environment goes through the `Env` trait to stay testable.

use crate::constants::APP_DIR_NAME;
use crate::error::{Error, Result};
use crate::store::create_private_dir;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const ENV_CONFIG_DIR: &str = "LINUXFLOW_CONFIG_DIR";
pub const ENV_DATA_DIR: &str = "LINUXFLOW_DATA_DIR";
pub const ENV_STATE_DIR: &str = "LINUXFLOW_STATE_DIR";
pub const ENV_CACHE_DIR: &str = "LINUXFLOW_CACHE_DIR";
pub const ENV_MODEL_DIR: &str = "LINUXFLOW_MODEL_DIR";

/// XDG base folders of the application (already suffixed with `APP_DIR_NAME`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdgKind {
    Config,
    Data,
    State,
    Cache,
    Runtime,
}

/// Access to the environment, injectable.
pub trait Env {
    fn var(&self, key: &str) -> Option<OsString>;

    /// XDG folder of the application. Default implementation: absolute `XDG_*` then `$HOME`
    /// (same rules as the XDG specification). `SystemEnv` replaces it with the `directories` crate.
    fn xdg_dir(&self, kind: XdgKind) -> Option<PathBuf> {
        let (var, home_rel): (&str, Option<&str>) = match kind {
            XdgKind::Config => ("XDG_CONFIG_HOME", Some(".config")),
            XdgKind::Data => ("XDG_DATA_HOME", Some(".local/share")),
            XdgKind::State => ("XDG_STATE_HOME", Some(".local/state")),
            XdgKind::Cache => ("XDG_CACHE_HOME", Some(".cache")),
            XdgKind::Runtime => ("XDG_RUNTIME_DIR", None),
        };
        let abs = |k: &str| self.var(k).map(PathBuf::from).filter(|p| p.is_absolute());
        let base =
            abs(var).or_else(|| home_rel.and_then(|rel| abs("HOME").map(|h| h.join(rel))))?;
        Some(base.join(APP_DIR_NAME))
    }
}

/// Real environment of the process.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemEnv;

impl Env for SystemEnv {
    fn var(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }

    fn xdg_dir(&self, kind: XdgKind) -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", APP_DIR_NAME)?;
        match kind {
            XdgKind::Config => Some(dirs.config_dir().to_path_buf()),
            XdgKind::Data => Some(dirs.data_dir().to_path_buf()),
            XdgKind::State => dirs.state_dir().map(Path::to_path_buf),
            XdgKind::Cache => Some(dirs.cache_dir().to_path_buf()),
            XdgKind::Runtime => dirs.runtime_dir().map(Path::to_path_buf),
        }
    }
}

/// Fake environment (tests).
#[derive(Debug, Clone, Default)]
pub struct MapEnv(pub HashMap<String, OsString>);

impl MapEnv {
    pub fn new(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), OsString::from(*v)))
                .collect(),
        )
    }
}

impl Env for MapEnv {
    fn var(&self, key: &str) -> Option<OsString> {
        self.0.get(key).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// `settings.json`, `dictionary.json`, `snippets.json`.
    pub config_dir: PathBuf,
    /// `history.json`.
    pub data_dir: PathBuf,
    /// Logs (under `logs_dir`) and volatile state.
    pub state_dir: PathBuf,
    /// Cache (default models).
    pub cache_dir: PathBuf,
    /// Sockets and lock files.
    pub runtime_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub models_dir: PathBuf,
}

/// Override by variable: empty = ignored, relative = explicit error.
fn env_override(env: &dyn Env, key: &str) -> Result<Option<PathBuf>> {
    match env.var(key) {
        None => Ok(None),
        Some(v) if v.is_empty() => Ok(None),
        Some(v) => {
            let p = PathBuf::from(v);
            if p.is_absolute() {
                Ok(Some(p))
            } else {
                Err(Error::msg(format!("{key} must be an absolute path")))
            }
        }
    }
}

impl AppPaths {
    pub fn resolve(env: &dyn Env) -> Result<Self> {
        Self::resolve_with(env, None)
    }

    /// `model_dir_setting`: `Settings.model_dir` setting (the `LINUXFLOW_MODEL_DIR` variable wins).
    pub fn resolve_with(env: &dyn Env, model_dir_setting: Option<&Path>) -> Result<Self> {
        let dir = |var: &str, kind: XdgKind| -> Result<PathBuf> {
            if let Some(p) = env_override(env, var)? {
                return Ok(p);
            }
            env.xdg_dir(kind)
                .ok_or_else(|| Error::msg(format!("folder not found: set {var} or HOME")))
        };
        let config_dir = dir(ENV_CONFIG_DIR, XdgKind::Config)?;
        let data_dir = dir(ENV_DATA_DIR, XdgKind::Data)?;
        let state_dir = dir(ENV_STATE_DIR, XdgKind::State)?;
        let cache_dir = dir(ENV_CACHE_DIR, XdgKind::Cache)?;
        // Without XDG_RUNTIME_DIR, fall back under the state folder (created with 0700).
        let runtime_dir = env
            .xdg_dir(XdgKind::Runtime)
            .unwrap_or_else(|| state_dir.join("run"));
        let models_dir = match env_override(env, ENV_MODEL_DIR)? {
            Some(p) => p,
            None => match model_dir_setting.filter(|p| p.is_absolute()) {
                Some(p) => p.to_path_buf(),
                None => cache_dir.join("models"),
            },
        };
        let logs_dir = state_dir.join("logs");
        Ok(Self {
            config_dir,
            data_dir,
            state_dir,
            cache_dir,
            runtime_dir,
            logs_dir,
            models_dir,
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }
    pub fn dictionary_file(&self) -> PathBuf {
        self.config_dir.join("dictionary.json")
    }
    pub fn snippets_file(&self) -> PathBuf {
        self.config_dir.join("snippets.json")
    }
    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("history.json")
    }
    pub fn log_file(&self) -> PathBuf {
        self.logs_dir.join(format!("{APP_DIR_NAME}.log"))
    }

    /// Creates the missing folders (0700 for config, data, state, logs, runtime; the
    /// models and the cache, not sensitive, follow the same rule).
    pub fn ensure_dirs(&self) -> io::Result<()> {
        for d in [
            &self.config_dir,
            &self.data_dir,
            &self.state_dir,
            &self.logs_dir,
            &self.cache_dir,
            &self.runtime_dir,
            &self.models_dir,
        ] {
            create_private_dir(d)?;
        }
        Ok(())
    }

    /// Opens a file for appending with 0600 permissions (logs), tightening an existing file.
    pub fn open_private_append(path: &Path) -> io::Result<fs::File> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_private_dir(dir)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn xdg_variables_are_used() {
        let env = MapEnv::new(&[
            ("XDG_CONFIG_HOME", "/x/cfg"),
            ("XDG_DATA_HOME", "/x/data"),
            ("XDG_STATE_HOME", "/x/state"),
            ("XDG_CACHE_HOME", "/x/cache"),
            ("XDG_RUNTIME_DIR", "/x/run"),
        ]);
        let p = AppPaths::resolve(&env).unwrap();
        assert_eq!(p.config_dir, Path::new("/x/cfg").join(APP_DIR_NAME));
        assert_eq!(p.data_dir, Path::new("/x/data").join(APP_DIR_NAME));
        assert_eq!(p.state_dir, Path::new("/x/state").join(APP_DIR_NAME));
        assert_eq!(p.cache_dir, Path::new("/x/cache").join(APP_DIR_NAME));
        assert_eq!(p.runtime_dir, Path::new("/x/run").join(APP_DIR_NAME));
        assert_eq!(p.logs_dir, p.state_dir.join("logs"));
        assert_eq!(p.models_dir, p.cache_dir.join("models"));
        assert_eq!(p.settings_file(), p.config_dir.join("settings.json"));
        assert_eq!(p.history_file(), p.data_dir.join("history.json"));
    }

    #[test]
    fn home_fallback_and_relative_xdg_ignored() {
        let env = MapEnv::new(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "relatif")]);
        let p = AppPaths::resolve(&env).unwrap();
        assert_eq!(p.config_dir, Path::new("/h/.config").join(APP_DIR_NAME));
        assert_eq!(p.data_dir, Path::new("/h/.local/share").join(APP_DIR_NAME));
        assert_eq!(p.state_dir, Path::new("/h/.local/state").join(APP_DIR_NAME));
        assert_eq!(p.cache_dir, Path::new("/h/.cache").join(APP_DIR_NAME));
        assert_eq!(p.runtime_dir, p.state_dir.join("run"));
    }

    #[test]
    fn linuxflow_overrides_win_over_xdg() {
        let env = MapEnv::new(&[
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", "/x/cfg"),
            (ENV_CONFIG_DIR, "/o/cfg"),
            (ENV_DATA_DIR, "/o/data"),
            (ENV_STATE_DIR, "/o/state"),
            (ENV_CACHE_DIR, "/o/cache"),
            (ENV_MODEL_DIR, "/o/models"),
        ]);
        let p = AppPaths::resolve_with(&env, Some(Path::new("/setting/models"))).unwrap();
        assert_eq!(p.config_dir, Path::new("/o/cfg"));
        assert_eq!(p.data_dir, Path::new("/o/data"));
        assert_eq!(p.state_dir, Path::new("/o/state"));
        assert_eq!(p.logs_dir, Path::new("/o/state/logs"));
        assert_eq!(p.cache_dir, Path::new("/o/cache"));
        assert_eq!(p.models_dir, Path::new("/o/models"));
    }

    #[test]
    fn model_dir_setting_beats_default_but_not_env() {
        let env = MapEnv::new(&[("HOME", "/h")]);
        let p = AppPaths::resolve_with(&env, Some(Path::new("/setting/models"))).unwrap();
        assert_eq!(p.models_dir, Path::new("/setting/models"));
        let p = AppPaths::resolve_with(&env, Some(Path::new("relatif"))).unwrap();
        assert_eq!(p.models_dir, p.cache_dir.join("models"));
        let p = AppPaths::resolve_with(&env, None).unwrap();
        assert_eq!(p.models_dir, p.cache_dir.join("models"));
    }

    #[test]
    fn empty_override_ignored_relative_override_rejected() {
        let env = MapEnv::new(&[("HOME", "/h"), (ENV_DATA_DIR, "")]);
        assert_eq!(
            AppPaths::resolve(&env).unwrap().data_dir,
            Path::new("/h/.local/share").join(APP_DIR_NAME)
        );
        let env = MapEnv::new(&[("HOME", "/h"), (ENV_CACHE_DIR, "rel/cache")]);
        let err = AppPaths::resolve(&env).unwrap_err().to_string();
        assert!(err.contains(ENV_CACHE_DIR));
    }

    #[test]
    fn no_home_no_xdg_is_a_clear_error() {
        let err = AppPaths::resolve(&MapEnv::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("LINUXFLOW_CONFIG_DIR"));
    }

    #[test]
    fn system_env_resolves_with_directories() {
        // Does not touch the disk: only checks that the `directories` fallback produces absolute paths.
        if let Ok(p) = AppPaths::resolve(&SystemEnv) {
            assert!(p.config_dir.is_absolute() && p.models_dir.is_absolute());
        }
    }

    #[test]
    fn ensure_dirs_creates_private_dirs() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().to_str().unwrap();
        let env = MapEnv::new(&[("HOME", base)]);
        let p = AppPaths::resolve(&env).unwrap();
        p.ensure_dirs().unwrap();
        for dir in [
            &p.config_dir,
            &p.data_dir,
            &p.state_dir,
            &p.logs_dir,
            &p.runtime_dir,
        ] {
            assert_eq!(
                fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700,
                "{}",
                dir.display()
            );
        }
        assert!(p.models_dir.is_dir());
        p.ensure_dirs().unwrap(); // idempotent
    }

    #[test]
    fn private_append_is_0600_and_tightens_existing() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("logs/x.log");
        drop(AppPaths::open_private_append(&f).unwrap());
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        drop(AppPaths::open_private_append(&f).unwrap());
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
