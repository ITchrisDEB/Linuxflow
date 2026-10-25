//! Real implementation of `service::System`: shortcuts, autostart, folders, clipboard,
//! capabilities. Commands meant for the interface go through `UiCommand`.

use crate::hotkeys::{CapsCell, HotkeyController};
use crate::models::ModelsService;
use crate::service::{Folder, ProbeError, ShortcutInfo, System};
use linuxflow_core::accelerator::Accelerator;
use linuxflow_core::domain::{Capabilities, MicrophoneInfo, Settings, ShortcutMode};
use linuxflow_core::error::Result;
use linuxflow_core::events::Page;
use linuxflow_core::paths::AppPaths;
use linuxflow_core::traits::Clipboard;
use linuxflow_i18n::tr;
use linuxflow_platform::autostart::Autostart;
use linuxflow_platform::open::{open_dir, KnownDir, KnownDirs};
use linuxflow_platform::tray::{advice_for, TrayAdvice};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Commands for the GTK thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Shows the hub, possibly on a page.
    ShowHub(Option<Page>),
    /// Rebuilds the windows (interface language change).
    RebuildUi,
    About,
    Quit,
}

pub struct RealSystem {
    pub hotkeys: OnceLock<Arc<HotkeyController>>,
    pub caps: Arc<CapsCell>,
    pub tray_available: Arc<AtomicBool>,
    pub paths: AppPaths,
    pub models: Arc<ModelsService>,
    pub ui_clipboard: Arc<dyn Clipboard>,
    pub ui_tx: async_channel::Sender<UiCommand>,
    pub versions: (String, String),
}

/// Completes the capability report: interface versions, real state of the tray and hint, absence
/// of Vulkan at build time.
pub fn complete_capabilities(
    mut caps: Capabilities,
    gtk_adw: &(String, String),
    tray_ok: bool,
    gpu_compiled: bool,
    advice: &dyn Fn(&Capabilities) -> Option<TrayAdvice>,
) -> Capabilities {
    caps.gtk_version = gtk_adw.0.clone();
    caps.adw_version = gtk_adw.1.clone();
    caps.tray_watcher = tray_ok;
    caps.hints.retain(|(b, _)| b != "tray");
    if let Some(a) = advice(&caps) {
        caps.hints.push((a.title, a.body));
    }
    if !gpu_compiled {
        caps.hints.push((
            "vulkan".into(),
            tr("This build has no GPU support: transcription runs on the CPU."),
        ));
    }
    caps
}

/// Free space of the file system that contains `dir`.
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a valid NUL-terminated C string; `st` is an area large enough
    // for a `statvfs`, initialised by the call when it succeeds (return 0).
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: the call succeeded, the structure is initialised.
    let st = unsafe { st.assume_init() };
    // The `statvfs` types vary by platform: explicit conversions.
    #[allow(clippy::unnecessary_cast)]
    let free = (st.f_bavail as u64).saturating_mul(st.f_frsize as u64);
    Some(free)
}

impl RealSystem {
    fn known_dirs(&self) -> KnownDirs {
        KnownDirs {
            logs: self.paths.logs_dir.clone(),
            data: self.paths.data_dir.clone(),
            config: self.paths.config_dir.clone(),
            models: self.models.dir(),
        }
    }

    fn hotkeys(&self) -> Option<&Arc<HotkeyController>> {
        self.hotkeys.get()
    }
}

impl System for RealSystem {
    fn apply_shortcut(&self, settings: &Settings) {
        if let Some(h) = self.hotkeys() {
            h.apply(settings);
        }
    }

    fn suspend_shortcut(&self, suspended: bool) {
        if let Some(h) = self.hotkeys() {
            h.suspend(suspended);
        }
    }

    fn shortcut_status(&self) -> ShortcutInfo {
        self.hotkeys().map(|h| h.info()).unwrap_or_default()
    }

    fn probe_shortcut(
        &self,
        accelerator: &Accelerator,
        mode: ShortcutMode,
    ) -> std::result::Result<(), ProbeError> {
        match self.hotkeys() {
            Some(h) => h.probe(accelerator, mode),
            None => Ok(()),
        }
    }

    fn set_autostart(&self, enabled: bool) -> std::io::Result<()> {
        Autostart::system()?.set_enabled(enabled, true)
    }

    fn microphones(&self) -> Vec<MicrophoneInfo> {
        linuxflow_audio::list_microphones()
    }

    fn capabilities(&self) -> Capabilities {
        complete_capabilities(
            self.caps.get().unwrap_or_default(),
            &self.versions,
            self.tray_available.load(Ordering::SeqCst),
            crate::engine::gpu_available_here(),
            &|c| {
                advice_for(
                    c,
                    linuxflow_platform::tray::detect_distro(&read_os_release()),
                )
            },
        )
    }

    fn open_folder(&self, folder: Folder) -> std::result::Result<(), String> {
        let kind = match folder {
            Folder::Logs => KnownDir::Logs,
            Folder::Data => KnownDir::Data,
            Folder::Models => KnownDir::Models,
        };
        let _ = std::fs::create_dir_all(self.known_dirs().get(kind));
        open_dir(kind, &self.known_dirs()).map_err(|e| {
            tracing::warn!("open folder failed: {e}");
            tr("The folder could not be opened.")
        })
    }

    fn copy_text(&self, text: &str) -> Result<()> {
        self.ui_clipboard.set_text(text)
    }

    fn free_disk_bytes(&self, dir: &Path) -> Option<u64> {
        free_space(dir).or_else(|| dir.parent().and_then(free_space))
    }

    fn ui_language_changed(&self) {
        let _ = self.ui_tx.try_send(UiCommand::RebuildUi);
    }

    fn request_quit(&self) {
        let _ = self.ui_tx.try_send(UiCommand::Quit);
    }
}

fn read_os_release() -> String {
    std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_completed_honestly() {
        let base = Capabilities {
            session: "wayland".into(),
            desktop: "gnome".into(),
            hints: vec![
                ("tray".into(), "old".into()),
                ("wayland".into(), "x".into()),
            ],
            ..Capabilities::default()
        };
        let versions = ("4.12.0".to_string(), "1.4.0".to_string());
        let c = complete_capabilities(base.clone(), &versions, false, false, &|c| {
            assert!(!c.tray_watcher);
            Some(TrayAdvice {
                title: "T".into(),
                body: "B".into(),
            })
        });
        assert_eq!(c.gtk_version, "4.12.0");
        assert_eq!(c.adw_version, "1.4.0");
        assert!(!c.tray_watcher);
        assert!(c.hints.contains(&("T".into(), "B".into())));
        assert!(!c.hints.iter().any(|(b, _)| b == "tray"));
        assert!(c.hints.iter().any(|(b, _)| b == "vulkan"));
        let ok = complete_capabilities(base, &versions, true, true, &|_| None);
        assert!(ok.tray_watcher);
        assert!(!ok.hints.iter().any(|(b, _)| b == "vulkan" || b == "tray"));
    }

    #[test]
    fn free_space_of_an_existing_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_space(d.path()).is_some());
        assert!(free_space(Path::new("/definitely/not/here")).is_none());
    }
}
