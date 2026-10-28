//! Desktop integration of an AppImage.
//!
//! Wayland has no per-window icon: the shell looks the window's application id up in a `.desktop` file and takes the
//! icon from there. An AppImage that is simply run is not installed anywhere, so GNOME shows a generic gear in the dock.
//! At start-up an AppImage therefore registers itself for the current user: a launcher entry that starts the `.AppImage`
//! file, and its icons, under `$XDG_DATA_HOME`. Nothing is written when the program is not running from an AppImage, and
//! nothing is rewritten when the files are already up to date (the entry follows the file if it is moved).
//!
//! A launcher entry in the user's directory takes priority over a system-wide one with the same id, so it must never
//! hide a real installation: when the application is installed (an entry for it exists in the system data
//! directories), the entry written by an AppImage is removed and none is written.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Line added to the entries written here, so they can be told from an entry of a real installation.
const MARKER: &str = "X-LinuxFlow-AppImage=true";

/// Registers the running AppImage for the current user. `Ok(false)` when nothing was registered (not running from an
/// AppImage, or the application is installed on the system).
pub fn integrate(app_id: &str) -> io::Result<bool> {
    let data_home = match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".local/share"),
            None => return Ok(false),
        },
    };
    let system_dirs = system_data_dirs();
    if installed_on_system(app_id, &system_dirs) {
        remove_own_entry(app_id, &data_home)?;
        return Ok(false);
    }
    let (Some(appimage), Some(appdir)) = (
        std::env::var_os("APPIMAGE").map(PathBuf::from),
        std::env::var_os("APPDIR").map(PathBuf::from),
    ) else {
        return Ok(false);
    };
    if !appimage.is_absolute() || !appdir.is_dir() {
        return Ok(false);
    }
    integrate_into(app_id, &appimage, &appdir, &data_home)
}

/// The system-wide data directories (`XDG_DATA_DIRS`, or its default), without the AppImage's own: its `AppRun` puts
/// `$APPDIR/usr/share` first in `XDG_DATA_DIRS`, and that folder holds the application's own launcher entry, which says
/// nothing about an installation.
fn system_data_dirs() -> Vec<PathBuf> {
    system_data_dirs_from(
        std::env::var("XDG_DATA_DIRS").ok().as_deref(),
        std::env::var_os("APPDIR").map(PathBuf::from).as_deref(),
    )
}

fn system_data_dirs_from(xdg_data_dirs: Option<&str>, appdir: Option<&Path>) -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = match xdg_data_dirs {
        Some(v) if !v.is_empty() => v
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .collect(),
        _ => vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ],
    };
    dirs.into_iter()
        .filter(|d| appdir.is_none_or(|a| !d.starts_with(a)))
        .collect()
}

/// Is there a launcher entry for the application in the system data directories (a real installation)?
pub fn installed_on_system(app_id: &str, system_dirs: &[PathBuf]) -> bool {
    system_dirs.iter().any(|d| {
        d.join("applications")
            .join(format!("{app_id}.desktop"))
            .is_file()
    })
}

/// Was this entry written by an AppImage (marker, or the older form: an `Exec` that starts a `.AppImage` file)?
fn written_by_an_appimage(content: &str) -> bool {
    content.lines().any(|l| l.trim() == MARKER)
        || content
            .lines()
            .any(|l| l.starts_with("Exec=\"") && l.contains(".AppImage\""))
}

/// Removes the user's entry for the application when an AppImage wrote it (never an entry written by anything else).
fn remove_own_entry(app_id: &str, data_home: &Path) -> io::Result<()> {
    let entry = data_home
        .join("applications")
        .join(format!("{app_id}.desktop"));
    if let Ok(content) = fs::read_to_string(&entry) {
        if written_by_an_appimage(&content) {
            fs::remove_file(&entry)?;
        }
    }
    Ok(())
}

/// Same as [`integrate`] with explicit locations, for an AppImage that is not installed (testable).
pub fn integrate_into(
    app_id: &str,
    appimage: &Path,
    appdir: &Path,
    data_home: &Path,
) -> io::Result<bool> {
    let template = fs::read_to_string(
        appdir
            .join("usr/share/applications")
            .join(format!("{app_id}.desktop")),
    )?;
    let entry = entry_for_appimage(&template, appimage)?;
    copy_icons(
        app_id,
        &appdir.join("usr/share/icons/hicolor"),
        &data_home.join("icons/hicolor"),
    )?;
    let target = data_home
        .join("applications")
        .join(format!("{app_id}.desktop"));
    if fs::read_to_string(&target).ok().as_deref() != Some(entry.as_str()) {
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&target, entry)?;
    }
    Ok(true)
}

/// Rewrites every `Exec=` line of a desktop entry (the main one and those of the actions) so that it starts the
/// AppImage file, keeping the arguments. Whatever the template used as program (`@BINDIR@/linuxflow`, `linuxflow`)
/// is replaced.
pub fn entry_for_appimage(template: &str, appimage: &Path) -> io::Result<String> {
    let path = appimage.to_string_lossy();
    // Characters that need escaping inside a quoted Exec argument: refuse rather than guess.
    if path.contains(['"', '`', '$', '\\', '%', '\n']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the AppImage path contains characters a launcher entry cannot hold",
        ));
    }
    let mut out = String::with_capacity(template.len() + 64);
    for line in template.lines() {
        if line.trim() == "[Desktop Entry]" {
            out.push_str(line);
            out.push('\n');
            out.push_str(MARKER);
            out.push('\n');
            continue;
        }
        match line.strip_prefix("Exec=") {
            Some(command) => {
                let args = command.find(' ').map_or("", |i| &command[i..]);
                out.push_str(&format!("Exec=\"{path}\"{args}\n"));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Ok(out)
}

/// Copies `<from>/<size>/apps/<app_id>.(png|svg)` to the same place under `to`, when missing or different.
fn copy_icons(app_id: &str, from: &Path, to: &Path) -> io::Result<()> {
    let Ok(sizes) = fs::read_dir(from) else {
        return Ok(());
    };
    for size in sizes.flatten() {
        for ext in ["png", "svg"] {
            let name = format!("{app_id}.{ext}");
            let src = size.path().join("apps").join(&name);
            let Ok(bytes) = fs::read(&src) else {
                continue;
            };
            let dst = to.join(size.file_name()).join("apps").join(&name);
            if fs::read(&dst).ok().as_deref() != Some(bytes.as_slice()) {
                if let Some(dir) = dst.parent() {
                    fs::create_dir_all(dir)?;
                }
                fs::write(&dst, bytes)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    const TEMPLATE: &str = "[Desktop Entry]\nName=LinuxFlow\nExec=@BINDIR@/linuxflow\nIcon=app.id\nActions=Toggle;\n\n[Desktop Action Toggle]\nName=Toggle\nExec=@BINDIR@/linuxflow --toggle\n";

    #[test]
    fn every_exec_line_starts_the_appimage_and_keeps_its_arguments() {
        let out =
            entry_for_appimage(TEMPLATE, Path::new("/opt/My Apps/LinuxFlow.AppImage")).unwrap();
        assert!(
            out.contains("Exec=\"/opt/My Apps/LinuxFlow.AppImage\"\n"),
            "{out}"
        );
        assert!(
            out.contains("Exec=\"/opt/My Apps/LinuxFlow.AppImage\" --toggle\n"),
            "{out}"
        );
        assert!(!out.contains("@BINDIR@"));
        assert!(out.contains("Icon=app.id\n") && out.contains("Actions=Toggle;\n"));
    }

    #[test]
    fn a_path_a_launcher_cannot_hold_is_refused() {
        assert!(entry_for_appimage(TEMPLATE, Path::new("/tmp/a\"b.AppImage")).is_err());
        assert!(entry_for_appimage(TEMPLATE, Path::new("/tmp/a$b.AppImage")).is_err());
    }

    fn appdir(root: &Path) -> PathBuf {
        let dir = root.join("mnt");
        fs::create_dir_all(dir.join("usr/share/applications")).unwrap();
        fs::write(dir.join("usr/share/applications/app.id.desktop"), TEMPLATE).unwrap();
        fs::create_dir_all(dir.join("usr/share/icons/hicolor/256x256/apps")).unwrap();
        fs::write(
            dir.join("usr/share/icons/hicolor/256x256/apps/app.id.png"),
            b"png",
        )
        .unwrap();
        fs::create_dir_all(dir.join("usr/share/icons/hicolor/scalable/apps")).unwrap();
        fs::write(
            dir.join("usr/share/icons/hicolor/scalable/apps/app.id.svg"),
            b"svg",
        )
        .unwrap();
        dir
    }

    #[test]
    fn registers_the_entry_and_the_icons_and_follows_a_move() {
        let root = tempfile::tempdir().unwrap();
        let dir = appdir(root.path());
        let data = root.path().join("data");
        let image = root.path().join("LinuxFlow.AppImage");
        assert!(integrate_into("app.id", &image, &dir, &data).unwrap());
        let entry = data.join("applications/app.id.desktop");
        assert!(fs::read_to_string(&entry)
            .unwrap()
            .contains(image.to_str().unwrap()));
        assert_eq!(
            fs::read(data.join("icons/hicolor/256x256/apps/app.id.png")).unwrap(),
            b"png"
        );
        assert_eq!(
            fs::read(data.join("icons/hicolor/scalable/apps/app.id.svg")).unwrap(),
            b"svg"
        );
        // Unchanged: the file is left alone.
        let before = fs::metadata(&entry).unwrap().modified().unwrap();
        integrate_into("app.id", &image, &dir, &data).unwrap();
        assert_eq!(fs::metadata(&entry).unwrap().modified().unwrap(), before);
        // The AppImage moved: the entry follows.
        let moved = root.path().join("elsewhere.AppImage");
        integrate_into("app.id", &moved, &dir, &data).unwrap();
        assert!(fs::read_to_string(&entry)
            .unwrap()
            .contains("elsewhere.AppImage"));
    }

    #[test]
    fn an_installed_application_is_never_hidden_by_an_appimage_entry() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let system = root.path().join("system");
        fs::create_dir_all(system.join("applications")).unwrap();
        let apps = data.join("applications");
        fs::create_dir_all(&apps).unwrap();
        // Not installed yet: no system entry.
        assert!(!installed_on_system(
            "app.id",
            std::slice::from_ref(&system)
        ));
        // An entry written by an AppImage (new form, then the older form without the marker) is removed once installed.
        let own = [
            entry_for_appimage(TEMPLATE, Path::new("/opt/apps/LinuxFlow.AppImage")).unwrap(),
            "[Desktop Entry]\nName=LinuxFlow\nExec=\"/opt/apps/LinuxFlow.AppImage\"\n".to_string(),
        ];
        fs::write(system.join("applications/app.id.desktop"), TEMPLATE).unwrap();
        assert!(installed_on_system("app.id", &[system]));
        for content in own {
            assert!(written_by_an_appimage(&content), "{content}");
            fs::write(apps.join("app.id.desktop"), content).unwrap();
            remove_own_entry("app.id", &data).unwrap();
            assert!(!apps.join("app.id.desktop").exists());
        }
        // An entry the user wrote themselves is left alone.
        fs::write(
            apps.join("app.id.desktop"),
            "[Desktop Entry]\nName=Mine\nExec=/usr/bin/x\n",
        )
        .unwrap();
        remove_own_entry("app.id", &data).unwrap();
        assert!(apps.join("app.id.desktop").exists());
    }

    #[test]
    fn the_appimages_own_data_folder_is_not_a_system_installation() {
        // What AppRun sets inside an AppImage: its own share folder first, then the system's.
        let dirs = system_data_dirs_from(
            Some("/tmp/.mount_abc/usr/share:/usr/local/share:/usr/share"),
            Some(Path::new("/tmp/.mount_abc")),
        );
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
        // Without the variable, the standard defaults; outside an AppImage nothing is filtered.
        assert_eq!(system_data_dirs_from(None, None).len(), 2);
        assert_eq!(
            system_data_dirs_from(Some("/opt/share:/usr/share"), None),
            vec![PathBuf::from("/opt/share"), PathBuf::from("/usr/share")]
        );
    }

    #[test]
    fn the_registered_entry_carries_the_marker() {
        let out = entry_for_appimage(TEMPLATE, Path::new("/opt/x.AppImage")).unwrap();
        assert!(
            out.starts_with("[Desktop Entry]\nX-LinuxFlow-AppImage=true\n"),
            "{out}"
        );
    }

    #[test]
    fn outside_an_appimage_nothing_happens() {
        // The test runner is not an AppImage.
        if std::env::var_os("APPIMAGE").is_none() {
            assert!(!integrate("app.id").unwrap());
        }
    }
}
