//! Help shown when no notification area accepts the icon (GNOME without the AppIndicator
//! extension, desktop without a StatusNotifier host). The application stays fully usable:
//! the text points to the other ways in.

use linuxflow_core::domain::Capabilities;
use linuxflow_i18n::tr;

/// Distribution family (from `ID` / `ID_LIKE` of `/etc/os-release`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distro {
    Ubuntu,
    Debian,
    Fedora,
    Arch,
    OpenSuse,
    Other,
}

/// Deduces the family from the content of an `os-release` file.
pub fn detect_distro(os_release: &str) -> Distro {
    let mut ids: Vec<String> = Vec::new();
    for line in os_release.lines() {
        let line = line.trim();
        for key in ["ID=", "ID_LIKE="] {
            if let Some(v) = line.strip_prefix(key) {
                ids.extend(
                    v.trim_matches(|c| c == '"' || c == '\'')
                        .split_whitespace()
                        .map(str::to_lowercase),
                );
            }
        }
    }
    let has = |names: &[&str]| ids.iter().any(|i| names.contains(&i.as_str()));
    // Order matters: Ubuntu derivatives also declare `debian`.
    if has(&["ubuntu"]) {
        Distro::Ubuntu
    } else if has(&["fedora", "rhel", "centos"]) {
        Distro::Fedora
    } else if has(&["arch", "cachyos", "manjaro", "endeavouros"]) {
        Distro::Arch
    } else if has(&["opensuse", "suse", "opensuse-tumbleweed", "opensuse-leap"]) {
        Distro::OpenSuse
    } else if has(&["debian"]) {
        Distro::Debian
    } else {
        Distro::Other
    }
}

/// Hint to show (Troubleshooting page, single toast).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayAdvice {
    pub title: String,
    pub body: String,
}

/// Hint for the current machine (reads `/etc/os-release`).
pub fn advice(caps: &Capabilities) -> Option<TrayAdvice> {
    let os_release = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    advice_for(caps, detect_distro(&os_release))
}

/// Pure variant: distribution family supplied.
pub fn advice_for(caps: &Capabilities, distro: Distro) -> Option<TrayAdvice> {
    if caps.tray_watcher {
        return None;
    }
    let gnome = caps.desktop.to_lowercase().contains("gnome");
    let title = tr("No notification-area icon");
    let access = tr(
        "LinuxFlow keeps working without the icon: launching it again opens the main window, and the launcher actions (--toggle, --cancel, --page, --quit) control dictation.",
    );
    let how = if !gnome {
        tr("Your desktop does not show notification-area icons. Enable the system tray (StatusNotifier) of your panel or bar, then restart LinuxFlow.")
    } else {
        match distro {
            Distro::Ubuntu => tr("The AppIndicator extension is installed with Ubuntu: open the Extensions application and turn on \"Ubuntu AppIndicators\", then log out and back in if the icon does not appear."),
            Distro::Fedora => tr("Install the package gnome-shell-extension-appindicator (sudo dnf install gnome-shell-extension-appindicator), turn it on in the Extensions application, then log out and back in."),
            Distro::Arch => tr("Install the package gnome-shell-extension-appindicator (sudo pacman -S gnome-shell-extension-appindicator), turn it on in the Extensions application, then log out and back in."),
            Distro::OpenSuse => tr("Install the package gnome-shell-extension-appindicator (sudo zypper install gnome-shell-extension-appindicator), turn it on in the Extensions application, then log out and back in."),
            Distro::Debian | Distro::Other => tr("Install the \"AppIndicator and KStatusNotifierItem Support\" GNOME extension (package gnome-shell-extension-appindicator on most distributions), turn it on in the Extensions application, then log out and back in."),
        }
    };
    Some(TrayAdvice {
        title,
        body: format!("{how}\n\n{access}"),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn caps(desktop: &str, watcher: bool) -> Capabilities {
        Capabilities {
            desktop: desktop.into(),
            tray_watcher: watcher,
            ..Capabilities::default()
        }
    }

    #[test]
    fn no_advice_when_watcher_present() {
        assert_eq!(advice_for(&caps("gnome", true), Distro::Fedora), None);
        assert_eq!(advice_for(&caps("kde", true), Distro::Arch), None);
    }

    #[test]
    fn gnome_advice_per_family() {
        let body = |d| advice_for(&caps("gnome", false), d).expect("conseil").body;
        assert!(body(Distro::Ubuntu).contains("Ubuntu AppIndicators"));
        assert!(body(Distro::Fedora).contains("dnf install gnome-shell-extension-appindicator"));
        assert!(body(Distro::Arch).contains("pacman -S gnome-shell-extension-appindicator"));
        assert!(
            body(Distro::OpenSuse).contains("zypper install gnome-shell-extension-appindicator")
        );
        assert!(body(Distro::Debian).contains("AppIndicator and KStatusNotifierItem Support"));
        assert!(body(Distro::Other).contains("gnome-shell-extension-appindicator"));
    }

    #[test]
    fn advice_mentions_other_ways_to_control_the_app() {
        let a = advice_for(&caps("gnome", false), Distro::Fedora).expect("conseil");
        assert_eq!(a.title, "No notification-area icon");
        for flag in [
            "--toggle",
            "--cancel",
            "--page",
            "--quit",
            "opens the main window",
        ] {
            assert!(a.body.contains(flag), "{flag}");
        }
    }

    #[test]
    fn non_gnome_desktops_get_generic_advice() {
        let a = advice_for(&caps("sway", false), Distro::Arch).expect("conseil");
        assert!(a.body.contains("system tray (StatusNotifier)"));
        assert!(!a.body.contains("pacman"));
    }

    #[test]
    fn distro_detection() {
        assert_eq!(
            detect_distro("NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n"),
            Distro::Ubuntu
        );
        assert_eq!(
            detect_distro("ID=pop\nID_LIKE=\"ubuntu debian\"\n"),
            Distro::Ubuntu
        );
        assert_eq!(detect_distro("ID=debian\n"), Distro::Debian);
        assert_eq!(detect_distro("ID=fedora\n"), Distro::Fedora);
        assert_eq!(
            detect_distro("ID=rocky\nID_LIKE=\"rhel centos fedora\"\n"),
            Distro::Fedora
        );
        assert_eq!(detect_distro("ID=arch\n"), Distro::Arch);
        assert_eq!(detect_distro("ID=cachyos\nID_LIKE=arch\n"), Distro::Arch);
        assert_eq!(
            detect_distro("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n"),
            Distro::OpenSuse
        );
        assert_eq!(detect_distro("ID=nixos\n"), Distro::Other);
        assert_eq!(detect_distro(""), Distro::Other);
    }
}
