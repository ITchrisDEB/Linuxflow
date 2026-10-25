//! Full application start-up under a private GDK Broadway server and a private session D-Bus
//! bus (`dbus-run-session`): the user's screen and session are never
//! touched. Ignored by default: `cargo test -p linuxflow --test smoke_broadway -- --ignored`.

use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
#[ignore = "starts gtk4-broadwayd and dbus-run-session; run by hand"]
#[allow(clippy::unwrap_used, clippy::expect_used)]
fn app_starts_shows_hub_answers_and_quits() {
    let bin = env!("CARGO_BIN_EXE_linuxflow");
    let tmp = std::env::temp_dir().join(format!("linuxflow-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let display = format!(":{}", 60 + std::process::id() % 30);
    let mut broadway = Command::new("gtk4-broadwayd")
        .arg(&display)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("gtk4-broadwayd");
    std::thread::sleep(Duration::from_millis(800));

    let script = r#"
        DIRS="--config-dir $T/c --data-dir $T/d --cache-dir $T/ca --state-dir $T/s --models-dir $T/m"
        "$BIN" --version || exit 11
        "$BIN" --no-engine --display-name Smoke $DIRS &
        PID=$!
        sleep 7
        kill -0 $PID || { echo DEAD_EARLY; exit 10; }
        "$BIN" $DIRS --page settings; echo PAGE=$?
        "$BIN" $DIRS --toggle; echo TOGGLE=$?
        "$BIN" $DIRS --cancel; echo CANCEL=$?
        sleep 1
        kill -0 $PID || { echo DEAD_BEFORE_QUIT; exit 12; }
        "$BIN" $DIRS --quit; echo QUIT=$?
        wait $PID; echo EXIT=$?
    "#;
    let out = Command::new("dbus-run-session")
        .args(["--", "sh", "-c", script])
        .env("BIN", bin)
        .env("T", &tmp)
        .env("GDK_BACKEND", "broadway")
        .env("BROADWAY_DISPLAY", &display)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("XDG_SESSION_TYPE")
        .env("LINUXFLOW_LOG", "debug")
        .output()
        .expect("dbus-run-session");
    let _ = broadway.kill();
    let _ = broadway.wait();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    println!("{stdout}\n--- stderr ---\n{stderr}");
    let log: String = std::fs::read_dir(tmp.join("s/logs"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .collect()
        })
        .unwrap_or_default();
    println!("--- log ---\n{log}");
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(stdout.contains("linuxflow "), "version");
    assert!(stdout.contains("PAGE=0"), "page");
    assert!(stdout.contains("QUIT=0"), "quit");
    assert!(stdout.contains("EXIT=0"), "primary exit");
    assert!(!stderr.contains("panicked"), "no panic");
    assert!(log.contains("starting"));
}
