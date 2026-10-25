//! Notification area icons: theme names (installed by the package) and fallback on
//! ARGB pixmaps generated here, with no file or image dependency.

use super::model::TrayIconState;
use linuxflow_core::constants::APP_ID;
use std::path::{Path, PathBuf};

/// Theme icon name for a state: `<APP_ID>-loading`, `<APP_ID>`, `<APP_ID>-rec`.
pub fn icon_name(state: TrayIconState) -> String {
    match state {
        TrayIconState::Loading => format!("{APP_ID}-loading"),
        TrayIconState::Ready => APP_ID.to_string(),
        TrayIconState::Recording => format!("{APP_ID}-rec"),
    }
}

/// Icon chosen for a state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconChoice {
    /// Theme name (symbolic or coloured).
    Themed(String),
    /// No theme icon installed: generated ARGB pixmaps.
    Pixmap,
}

/// Coloured name of the icon (the logo keeps its colours: the panel does not recolour it), otherwise the
/// generated pixmaps, which draw the same logo.
pub fn choose_icon(state: TrayIconState, installed: &dyn Fn(&str) -> bool) -> IconChoice {
    let name = icon_name(state);
    if installed(&name) {
        IconChoice::Themed(name)
    } else {
        IconChoice::Pixmap
    }
}

/// All the theme names that may be requested.
pub fn all_icon_names() -> Vec<String> {
    [
        TrayIconState::Loading,
        TrayIconState::Ready,
        TrayIconState::Recording,
    ]
    .into_iter()
    .map(icon_name)
    .collect()
}

/// Removes the data folders of the AppImage (`APPDIR`): the desktop panel cannot read
/// the icons it bundles. Without this, the icon would be requested by name, not found, hence empty;
/// with this filter, the application sends its generated icons (pixmaps) instead.
pub fn without_appdir(dirs: Vec<PathBuf>, appdir: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    match appdir.filter(|a| !a.is_empty()) {
        Some(a) => dirs.into_iter().filter(|d| !d.starts_with(a)).collect(),
        None => dirs,
    }
}

/// XDG data folders (`$XDG_DATA_HOME`, `$XDG_DATA_DIRS`, defaults of the specification).
pub fn data_dirs(get: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match get("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(home) => out.push(PathBuf::from(home)),
        None => {
            if let Some(dirs) = directories::BaseDirs::new() {
                out.push(dirs.data_dir().to_path_buf());
            }
        }
    }
    let system = get("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    out.extend(
        system
            .split(':')
            .filter(|p| !p.is_empty())
            .map(PathBuf::from),
    );
    out
}

/// True if a theme (or `pixmaps/`) provides `name` as PNG or SVG under one of the given folders.
pub fn icon_installed(name: &str, data_dirs: &[PathBuf]) -> bool {
    let exts = ["png", "svg"];
    let file_exists = |dir: &Path| {
        exts.iter()
            .any(|e| dir.join(format!("{name}.{e}")).is_file())
    };
    data_dirs.iter().any(|base| {
        if file_exists(&base.join("pixmaps")) {
            return true;
        }
        let Ok(themes) = std::fs::read_dir(base.join("icons")) else {
            return false;
        };
        themes.flatten().any(|theme| {
            std::fs::read_dir(theme.path())
                .into_iter()
                .flatten()
                .flatten()
                .any(|size| file_exists(&size.path().join("apps")))
        })
    })
}

/// Sizes offered to the host; it picks the one closest to its need.
pub const PIXMAP_SIZES: [u32; 3] = [22, 32, 48];

/// Background of the logo, and bars at rest (same colours as `packaging/icons/app.svg`).
const DARK: [u8; 3] = [0x1a, 0x1a, 0x1a];
const CREAM: [u8; 3] = [0xff, 0xff, 0xeb];
/// Bars while dictating.
const GREEN: [u8; 3] = [0x3d, 0xdc, 0x84];
/// Background while the engine is loading.
const GREY: [u8; 3] = [0x71, 0x71, 0x6e];

/// Relative heights of the seven bars of the logo.
const BARS: [f32; 7] = [0.28, 0.55, 0.92, 0.70, 0.40, 0.80, 0.50];

/// Coverage (0..1) of a pixel whose centre is at signed distance `d` from an edge.
fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// Signed distance of a point to the rounded rectangle of half-sides `hx`, `hy` and radius `r`.
fn rounded_box(px: f32, py: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = px.abs() - (hx - r);
    let qy = py.abs() - (hy - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// Square ARGB32 image (network order: A, R, G, B) of `size` pixels for the given state: the application
/// logo (dark rounded square, seven bars), green bars while dictating, grey background
/// grey while loading.
pub fn pixmap_argb(state: TrayIconState, size: u32) -> Vec<u8> {
    let s = size as f32;
    let (background, bars) = match state {
        TrayIconState::Loading => (GREY, CREAM),
        TrayIconState::Ready => (DARK, CREAM),
        TrayIconState::Recording => (DARK, GREEN),
    };
    let pad = s * 0.18;
    let inner = s - 2.0 * pad;
    let gap = inner / (BARS.len() as f32 * 2.0 - 1.0);
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let alpha = coverage(rounded_box(
                px - s / 2.0,
                py - s / 2.0,
                s / 2.0,
                s / 2.0,
                s * 0.22,
            ));
            let mut bar = 0.0_f32;
            for (i, h) in BARS.iter().enumerate() {
                let cx = pad + i as f32 * 2.0 * gap + gap / 2.0;
                let half_h = inner * 0.9 * h / 2.0;
                let d = rounded_box(px - cx, py - s / 2.0, gap / 2.0, half_h, gap / 2.0);
                bar = bar.max(coverage(d));
            }
            let rgb = mix(background, bars, bar);
            data.extend_from_slice(&[(alpha * 255.0).round() as u8, rgb[0], rgb[1], rgb[2]]);
        }
    }
    data
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let m = |i: usize| (f32::from(a[i]) * (1.0 - t) + f32::from(b[i]) * t).round() as u8;
    [m(0), m(1), m(2)]
}

/// ksni pixmaps of all sizes for a state.
pub fn pixmaps(state: TrayIconState) -> Vec<ksni::Icon> {
    PIXMAP_SIZES
        .iter()
        .map(|&size| ksni::Icon {
            width: size as i32,
            height: size as i32,
            data: pixmap_argb(state, size),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn pixel(data: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    #[test]
    fn names_follow_app_id() {
        assert_eq!(icon_name(TrayIconState::Ready), APP_ID);
        assert_eq!(
            icon_name(TrayIconState::Loading),
            format!("{APP_ID}-loading")
        );
        assert_eq!(icon_name(TrayIconState::Recording), format!("{APP_ID}-rec"));
    }

    #[test]
    fn one_colored_icon_per_state() {
        assert_eq!(all_icon_names().len(), 3);
        assert!(all_icon_names().iter().all(|n| !n.ends_with("-symbolic")));
    }

    #[test]
    fn icon_choice_uses_the_colored_logo() {
        let st = TrayIconState::Recording;
        let col = icon_name(st);
        assert_eq!(
            choose_icon(st, &|n| n == col),
            IconChoice::Themed(col.clone())
        );
        assert_eq!(choose_icon(st, &|_| false), IconChoice::Pixmap);
        // The "ready" state must not take the listening icon.
        let only_col = |n: &str| n == col;
        assert_eq!(
            choose_icon(TrayIconState::Ready, &only_col),
            IconChoice::Pixmap
        );
    }

    #[test]
    fn pixmaps_have_expected_geometry() {
        for state in [
            TrayIconState::Loading,
            TrayIconState::Ready,
            TrayIconState::Recording,
        ] {
            for icon in pixmaps(state) {
                assert_eq!(icon.width, icon.height);
                assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
                // Coin transparent.
                assert_eq!(pixel(&icon.data, icon.width as u32, 0, 0)[0], 0);
            }
        }
    }

    #[test]
    fn the_logo_bars_turn_green_while_listening() {
        let size = 48;
        let mid = size / 2;
        // Centre of the tallest bar (3rd): x = pad + 4 * gap + gap / 2.
        let s = size as f32;
        let gap = (s - 2.0 * s * 0.18) / 13.0;
        let bar_x = (s * 0.18 + 4.0 * gap + gap / 2.0) as u32;
        let ready = pixmap_argb(TrayIconState::Ready, size);
        let rec = pixmap_argb(TrayIconState::Recording, size);
        let load = pixmap_argb(TrayIconState::Loading, size);
        assert_eq!(pixel(&ready, size, bar_x, mid), [255, 0xff, 0xff, 0xeb]);
        assert_eq!(pixel(&rec, size, bar_x, mid), [255, 0x3d, 0xdc, 0x84]);
        // The background stays dark whatever the listening state; it turns grey while loading.
        assert_eq!(pixel(&ready, size, 4, mid), [255, 0x1a, 0x1a, 0x1a]);
        assert_eq!(pixel(&rec, size, 4, mid), [255, 0x1a, 0x1a, 0x1a]);
        assert_eq!(pixel(&load, size, 4, mid), [255, 0x71, 0x71, 0x6e]);
        assert_ne!(ready, rec);
        assert_ne!(ready, load);
    }

    #[test]
    fn data_dirs_follow_environment() {
        let get = |k: &str| match k {
            "XDG_DATA_HOME" => Some("/h/share".to_string()),
            "XDG_DATA_DIRS" => Some("/a:/b::".to_string()),
            _ => None,
        };
        assert_eq!(
            data_dirs(&get),
            vec![
                PathBuf::from("/h/share"),
                PathBuf::from("/a"),
                PathBuf::from("/b")
            ]
        );
        let none = |_: &str| None;
        let dirs = data_dirs(&none);
        assert!(dirs.contains(&PathBuf::from("/usr/share")));
    }

    #[test]
    fn icon_lookup_in_theme_and_pixmaps() {
        let dir = tempfile::tempdir().expect("tmp");
        let base = dir.path().to_path_buf();
        let dirs = vec![base.clone()];
        assert!(!icon_installed("foo-rec", &dirs));
        let apps = base.join("icons/hicolor/scalable/apps");
        std::fs::create_dir_all(&apps).expect("mkdir");
        std::fs::write(apps.join("foo-rec.svg"), "<svg/>").expect("write");
        assert!(icon_installed("foo-rec", &dirs));
        assert!(!icon_installed("foo", &dirs));
        std::fs::create_dir_all(base.join("pixmaps")).expect("mkdir");
        std::fs::write(base.join("pixmaps/foo.png"), "x").expect("write");
        assert!(icon_installed("foo", &dirs));
    }

    #[test]
    fn the_appimage_data_dirs_are_not_trusted_for_the_panel_icon() {
        let dirs = vec![
            PathBuf::from("/tmp/.mount_x/usr/share"),
            PathBuf::from("/usr/share"),
        ];
        let kept = without_appdir(dirs.clone(), Some(std::ffi::OsStr::new("/tmp/.mount_x")));
        assert_eq!(kept, vec![PathBuf::from("/usr/share")]);
        assert_eq!(without_appdir(dirs.clone(), None), dirs);
        assert_eq!(
            without_appdir(dirs.clone(), Some(std::ffi::OsStr::new(""))),
            dirs
        );
    }
}
