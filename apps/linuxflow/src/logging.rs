//! `tracing` logging in `logs_dir`: daily rotation (tracing-appender), 0600
//! files, level adjustable with `LINUXFLOW_LOG`, no personal path or token.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

/// Environment variable for the logging level (`EnvFilter` syntax).
pub const ENV_LOG: &str = "LINUXFLOW_LOG";
const KEEP_FILES: usize = 7;
const HARDEN_EVERY: Duration = Duration::from_secs(300);

/// Guard to keep until the end of the program (flushes the write buffer).
pub struct LogGuard(#[allow(dead_code)] Option<WorkerGuard>);

/// Replaces the home directory with `~` in a message meant for the log.
pub fn redact(message: &str) -> String {
    redact_with(
        message,
        directories::BaseDirs::new()
            .map(|d| d.home_dir().to_string_lossy().into_owned())
            .as_deref(),
    )
}

pub fn redact_with(message: &str, home: Option<&str>) -> String {
    match home {
        Some(h) if h.len() > 1 => message.replace(h, "~"),
        _ => message.to_string(),
    }
}

/// Tightens the log files of the folder to 0600 (rotation created under the process umask).
pub fn harden_dir(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        if name.to_string_lossy().starts_with("linuxflow") {
            let _ = fs::set_permissions(e.path(), fs::Permissions::from_mode(0o600));
        }
    }
}

/// Writer that re-applies 0600 to the rotation files from time to time.
struct PrivateWriter {
    inner: RollingFileAppender,
    dir: PathBuf,
    last: Instant,
}

impl Write for PrivateWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        if self.last.elapsed() > HARDEN_EVERY {
            harden_dir(&self.dir);
            self.last = Instant::now();
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Installs the log. Never panics: without a writable folder, no log is written.
pub fn init(logs_dir: &Path) -> LogGuard {
    let filter = EnvFilter::try_from_env(ENV_LOG).unwrap_or_else(|_| EnvFilter::new("info"));
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("linuxflow")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(logs_dir);
    let Ok(inner) = appender else {
        return LogGuard(None);
    };
    harden_dir(logs_dir);
    let writer = PrivateWriter {
        inner,
        dir: logs_dir.to_path_buf(),
        last: Instant::now(),
    };
    let (nb, guard) = tracing_appender::non_blocking(writer);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(nb)
        .with_ansi(false)
        .with_target(false)
        .try_init();
    LogGuard(Some(guard))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn home_directory_is_redacted() {
        assert_eq!(
            redact_with("cannot write /home/alice/.config/x", Some("/home/alice")),
            "cannot write ~/.config/x"
        );
        assert_eq!(redact_with("nothing", Some("/")), "nothing");
        assert_eq!(redact_with("same", None), "same");
    }

    #[test]
    fn harden_makes_files_private() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("linuxflow.2026-01-01.log");
        fs::write(&f, "x").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        let other = dir.path().join("other.txt");
        fs::write(&other, "x").unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o644)).unwrap();
        harden_dir(dir.path());
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&other).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn init_writes_a_private_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let guard = init(dir.path());
        tracing::info!("hello log");
        drop(guard);
        let files: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let mode = files[0].metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
