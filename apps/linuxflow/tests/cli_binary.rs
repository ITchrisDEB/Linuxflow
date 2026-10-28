//! The binary answers `--version`, `--help` and rejects an unknown option, without GTK.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    let tmp = std::env::temp_dir().join(format!("linuxflow-cli-{}", std::process::id()));
    Command::new(env!("CARGO_BIN_EXE_linuxflow"))
        .args(args)
        .arg("--state-dir")
        .arg(tmp.join("s"))
        .arg("--config-dir")
        .arg(tmp.join("c"))
        .arg("--data-dir")
        .arg(tmp.join("d"))
        .arg("--cache-dir")
        .arg(tmp.join("ca"))
        .env("LANGUAGE", "en")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        // Cut every way to the user's session: the application is a single-instance one, so `--quit` sent over the
        // session bus would stop a LinuxFlow that is really running. No bus address, and a runtime folder without the
        // `bus` socket, leave the binary with nobody to talk to.
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env("XDG_RUNTIME_DIR", tmp.join("run"))
        .output()
        .unwrap()
}

#[test]
fn version_and_help() {
    let v = run(&["--version"]);
    assert!(v.status.success());
    assert!(String::from_utf8_lossy(&v.stdout).starts_with("linuxflow "));
    let h = run(&["--help"]);
    assert!(h.status.success());
    let text = String::from_utf8_lossy(&h.stdout);
    for opt in [
        "--toggle",
        "--cancel",
        "--page",
        "--quit",
        "--background",
        "--no-engine",
    ] {
        assert!(text.contains(opt), "{opt} missing from the help");
    }
}

#[test]
fn unknown_option_fails_with_status_2() {
    let o = run(&["--definitely-not-an-option"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("Invalid command line"));
}

#[test]
fn quit_without_instance_exits_quietly() {
    // Without an instance (and without a session bus), `--quit` starts nothing.
    let o = run(&["--quit"]);
    assert!(o.status.success(), "{:?}", o.status);
}
