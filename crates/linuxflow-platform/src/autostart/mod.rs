//! Launch at login: XDG Autostart `.desktop` file.
//!
//! Creates/removes `$XDG_CONFIG_HOME/autostart/<APP_ID>.desktop`. The configuration folder comes
//! from the `directories` crate (which honours `XDG_CONFIG_HOME`, fallback `~/.config`), never from a
//! hard-coded path. `Exec` points to the current executable, escaped according to the specification
//! Desktop Entry.
//!
//! Limit: in a Flatpak sandbox autostart goes through the Background portal (not handled
//! here); `current_exe()` returns a path internal to the sandbox there.

use linuxflow_core::constants::{APP_ID, APP_NAME};
use std::io;
use std::path::{Path, PathBuf};

/// Option passed to the executable to start without opening the hub.
pub const BACKGROUND_FLAG: &str = "--background";

/// Manages the autostart file in a given configuration folder.
#[derive(Debug, Clone)]
pub struct Autostart {
    config_dir: PathBuf,
    exe: PathBuf,
}

impl Autostart {
    /// User configuration folder (`XDG_CONFIG_HOME`) and current executable.
    pub fn system() -> io::Result<Self> {
        let dirs = directories::BaseDirs::new()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
        Ok(Self::new(
            dirs.config_dir().to_path_buf(),
            current_exe_path()?,
        ))
    }

    /// Explicit variant (tests, executable installed elsewhere).
    pub fn new(config_dir: PathBuf, exe: PathBuf) -> Self {
        Self { config_dir, exe }
    }

    /// Path of the managed `.desktop` file.
    pub fn file_path(&self) -> PathBuf {
        self.config_dir
            .join("autostart")
            .join(format!("{APP_ID}.desktop"))
    }

    /// Creates (or rewrites, to keep `Exec` up to date) the autostart file.
    pub fn enable(&self, start_minimized: bool) -> io::Result<()> {
        // GLib refuses to launch an `Exec` whose path contains `%` (even escaped `%%`): a clear
        // error is preferred to an unusable autostart file.
        if self.exe.to_string_lossy().contains('%') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the application path contains '%': move LinuxFlow to a path without it to use autostart",
            ));
        }
        let content = desktop_entry(&self.exe, start_minimized)?;
        let path = self.file_path();
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid autostart path"))?;
        std::fs::create_dir_all(dir)?;
        write_atomic(&path, content.as_bytes())
    }

    /// Deletes the file; its absence is not an error.
    pub fn disable(&self) -> io::Result<()> {
        match std::fs::remove_file(self.file_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// True if the file exists and is not disabled (`Hidden=true`, `X-GNOME-Autostart-enabled=false`).
    pub fn is_enabled(&self) -> bool {
        std::fs::read_to_string(self.file_path())
            .map(|c| entry_is_active(&c))
            .unwrap_or(false)
    }

    /// Aligns the file with the `launchAtLogin` setting.
    pub fn set_enabled(&self, enabled: bool, start_minimized: bool) -> io::Result<()> {
        if enabled {
            self.enable(start_minimized)
        } else {
            self.disable()
        }
    }
}

/// Path of the current executable, without the ` (deleted)` suffix that Linux appends after an update.
/// Path to launch to start this application (the `.AppImage` file if we are one).
pub fn launch_exe() -> io::Result<PathBuf> {
    current_exe_path()
}

fn current_exe_path() -> io::Result<PathBuf> {
    Ok(choose_exe(
        strip_deleted(std::env::current_exe()?),
        std::env::var_os("APPIMAGE"),
        std::env::var_os("APPDIR"),
    ))
}

/// In an AppImage, the current executable is a temporary path of the mount point, which
/// disappears on exit: the start-up entry must target the `.AppImage` file itself.
fn choose_exe(
    current: PathBuf,
    appimage: Option<std::ffi::OsString>,
    appdir: Option<std::ffi::OsString>,
) -> PathBuf {
    match (appimage, appdir) {
        (Some(image), Some(dir))
            if !image.is_empty()
                && PathBuf::from(&image).is_absolute()
                && current.starts_with(&dir) =>
        {
            PathBuf::from(image)
        }
        _ => current,
    }
}

fn strip_deleted(exe: PathBuf) -> PathBuf {
    let text = exe.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(clean) => PathBuf::from(clean),
        None => exe,
    }
}

fn entry_is_active(content: &str) -> bool {
    let mut hidden = false;
    let mut gnome_disabled = false;
    let mut has_exec = false;
    for line in content.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Hidden=") {
            hidden = v.trim().eq_ignore_ascii_case("true");
        } else if let Some(v) = line.strip_prefix("X-GNOME-Autostart-enabled=") {
            gnome_disabled = v.trim().eq_ignore_ascii_case("false");
        } else if line.starts_with("Exec=") {
            has_exec = true;
        }
    }
    has_exec && !hidden && !gnome_disabled
}

fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("desktop.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Content of the `.desktop` file.
pub fn desktop_entry(exe: &Path, start_minimized: bool) -> io::Result<String> {
    let exe = exe.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "executable path is not UTF-8")
    })?;
    if !exe.starts_with('/') || exe.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable path must be absolute and free of control characters",
        ));
    }
    let mut exec = escape_exec_arg(exe);
    if start_minimized {
        exec.push(' ');
        exec.push_str(BACKGROUND_FLAG);
    }
    let comment = linuxflow_i18n::tr("Local voice dictation");
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={APP_NAME}\n\
         Comment={}\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n\
         Hidden=false\n",
        escape_value(&comment),
    ))
}

/// Escapes a `string`-type value (backslashes and line ends).
fn escape_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Escapes an `Exec` argument (Desktop Entry specification, "The Exec key").
///
/// An argument containing a reserved character is wrapped in quotes; inside,
/// `"`, `` ` ``, `$` and `\` are preceded by a backslash. `%` becomes `%%`. Finally,
/// as the value is a `string` of the file, every backslash is doubled.
pub fn escape_exec_arg(arg: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    let needs_quotes = arg.is_empty() || arg.chars().any(|c| RESERVED.contains(c));
    let mut quoted = String::with_capacity(arg.len() + 2);
    if needs_quotes {
        quoted.push('"');
    }
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' if needs_quotes => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    if needs_quotes {
        quoted.push('"');
    }
    quoted.replace('\\', "\\\\")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tmp_autostart(exe: &str) -> (tempfile::TempDir, Autostart) {
        let dir = tempfile::tempdir().expect("tmp");
        let a = Autostart::new(dir.path().to_path_buf(), PathBuf::from(exe));
        (dir, a)
    }

    #[test]
    fn a_path_with_percent_is_refused_with_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let a = Autostart::new(
            dir.path().to_path_buf(),
            PathBuf::from("/opt/100%/linuxflow"),
        );
        let err = a.enable(true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains('%'));
        assert!(!a.file_path().exists(), "no unusable file must be written");
    }

    #[test]
    fn enable_creates_valid_entry_and_disable_removes_it() {
        let (_d, a) = tmp_autostart("/opt/linuxflow/bin/linuxflow");
        assert!(!a.is_enabled());
        a.enable(false).expect("enable");
        let path = a.file_path();
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("{APP_ID}.desktop").as_str())
        );
        assert!(path.parent().is_some_and(|p| p.ends_with("autostart")));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("\nType=Application\n"));
        assert!(text.contains("\nExec=/opt/linuxflow/bin/linuxflow\n"));
        assert!(text.contains("\nX-GNOME-Autostart-enabled=true\n"));
        assert!(text.contains("\nHidden=false\n"));
        assert!(!text.contains("--background"));
        assert!(a.is_enabled());
        a.disable().expect("disable");
        assert!(!path.exists());
        assert!(!a.is_enabled());
        // Idempotent.
        a.disable().expect("disable again");
    }

    #[test]
    fn minimized_adds_background_flag() {
        let (_d, a) = tmp_autostart("/usr/bin/linuxflow");
        a.enable(true).expect("enable");
        let text = std::fs::read_to_string(a.file_path()).expect("read");
        assert!(text.contains("\nExec=/usr/bin/linuxflow --background\n"));
    }

    #[test]
    fn enable_rewrites_exec() {
        let (d, a) = tmp_autostart("/old/linuxflow");
        a.enable(false).expect("enable");
        let b = Autostart::new(d.path().to_path_buf(), PathBuf::from("/new/linuxflow"));
        b.enable(true).expect("rewrite");
        let text = std::fs::read_to_string(b.file_path()).expect("read");
        assert!(text.contains("Exec=/new/linuxflow --background"));
        assert!(!text.contains("/old/"));
        let leftovers: Vec<_> = std::fs::read_dir(d.path().join("autostart"))
            .expect("ls")
            .collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn disabled_flags_are_honoured() {
        assert!(entry_is_active("[Desktop Entry]\nExec=/x\nHidden=false\n"));
        assert!(!entry_is_active("[Desktop Entry]\nExec=/x\nHidden=true\n"));
        assert!(!entry_is_active(
            "[Desktop Entry]\nExec=/x\nX-GNOME-Autostart-enabled=false\n"
        ));
        assert!(!entry_is_active("[Desktop Entry]\nHidden=false\n"));
    }

    #[test]
    fn exec_escaping_follows_the_spec() {
        assert_eq!(escape_exec_arg("/usr/bin/linuxflow"), "/usr/bin/linuxflow");
        assert_eq!(escape_exec_arg("/opt/my app/lf"), "\"/opt/my app/lf\"");
        assert_eq!(escape_exec_arg("/opt/100%/lf"), "/opt/100%%/lf");
        // Quote: \" at the Exec level, then doubled backslash at the string level.
        assert_eq!(escape_exec_arg("/a b/\"q\""), "\"/a b/\\\\\"q\\\\\"\"");
        assert_eq!(escape_exec_arg("/a b/$x"), "\"/a b/\\\\$x\"");
        assert_eq!(escape_exec_arg("/a b/c\\d"), "\"/a b/c\\\\\\\\d\"");
        assert_eq!(escape_exec_arg(""), "\"\"");
    }

    #[test]
    fn invalid_exe_paths_are_rejected() {
        assert!(desktop_entry(Path::new("relative/linuxflow"), false).is_err());
        assert!(desktop_entry(Path::new("/bad\npath"), false).is_err());
    }

    #[test]
    fn exe_with_space_is_quoted_in_file() {
        let (_d, a) = tmp_autostart("/home/x y/linuxflow");
        a.enable(true).expect("enable");
        let text = std::fs::read_to_string(a.file_path()).expect("read");
        assert!(text.contains("\nExec=\"/home/x y/linuxflow\" --background\n"));
    }

    #[test]
    fn deleted_suffix_is_stripped() {
        assert_eq!(
            strip_deleted(PathBuf::from("/usr/bin/linuxflow (deleted)")),
            PathBuf::from("/usr/bin/linuxflow")
        );
        assert_eq!(
            strip_deleted(PathBuf::from("/usr/bin/lf")),
            PathBuf::from("/usr/bin/lf")
        );
    }

    #[test]
    fn inside_an_appimage_the_entry_points_to_the_appimage_file() {
        let inside = PathBuf::from("/tmp/.mount_LinuxF1/usr/bin/linuxflow");
        let image = Some(std::ffi::OsString::from("/data/apps/LinuxFlow.AppImage"));
        let dir = Some(std::ffi::OsString::from("/tmp/.mount_LinuxF1"));
        assert_eq!(
            choose_exe(inside.clone(), image.clone(), dir.clone()),
            PathBuf::from("/data/apps/LinuxFlow.AppImage")
        );
        // not in an AppImage, or a doubtful relative path: the current executable is kept
        let normal = PathBuf::from("/opt/linuxflow/bin/linuxflow");
        assert_eq!(
            choose_exe(normal.clone(), image.clone(), dir.clone()),
            normal
        );
        assert_eq!(choose_exe(inside.clone(), None, dir.clone()), inside);
        assert_eq!(
            choose_exe(inside.clone(), Some("relative.AppImage".into()), dir),
            inside
        );
    }
}
