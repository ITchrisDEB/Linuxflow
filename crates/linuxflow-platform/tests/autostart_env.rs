#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `Autostart::system()` honours `XDG_CONFIG_HOME`. Test isolated in its own process because it
//! modifie l'environnement.

use linuxflow_core::constants::APP_ID;
use linuxflow_platform::autostart::Autostart;

#[test]
fn system_autostart_follows_xdg_config_home() {
    let dir = tempfile::tempdir().expect("tmp");
    std::env::set_var("XDG_CONFIG_HOME", dir.path());
    let a = Autostart::system().expect("autostart");
    assert_eq!(
        a.file_path(),
        dir.path()
            .join("autostart")
            .join(format!("{APP_ID}.desktop"))
    );
    a.enable(true).expect("enable");
    assert!(a.is_enabled());
    let text = std::fs::read_to_string(a.file_path()).expect("read");
    assert!(text.contains("--background"));
    let exe = std::env::current_exe().expect("exe");
    assert!(text.contains(exe.to_str().expect("utf8")) || text.contains('"'));
    a.disable().expect("disable");
    assert!(!a.is_enabled());
}
