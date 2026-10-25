//! `--test-pipeline` with a real model. Skipped without `LINUXFLOW_TEST_MODEL_DIR` and
//! `LINUXFLOW_TEST_SAMPLES_DIR`. No paste, no access to the real clipboard.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn transcribes_a_sample_with_the_real_engine() {
    let (Some(models), Some(samples)) = (
        std::env::var_os("LINUXFLOW_TEST_MODEL_DIR"),
        std::env::var_os("LINUXFLOW_TEST_SAMPLES_DIR"),
    ) else {
        eprintln!("test variables missing: test skipped");
        return;
    };
    let wav = std::path::Path::new(&samples).join("phrase.wav");
    if !wav.exists() {
        eprintln!("phrase.wav missing: test skipped");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("linuxflow-tp-{}", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_linuxflow"))
        .arg("--test-pipeline")
        .arg(&wav)
        .arg("--exit")
        .args(["--config-dir"])
        .arg(tmp.join("c"))
        .arg("--data-dir")
        .arg(tmp.join("d"))
        .arg("--cache-dir")
        .arg(tmp.join("ca"))
        .arg("--state-dir")
        .arg(tmp.join("s"))
        .env("LINUXFLOW_TEST_MODEL_DIR", &models)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("TEST_SIDECAR {"));
    let line = stdout
        .lines()
        .find(|l| l.starts_with("TEST_PIPELINE_RESULT "))
        .expect("result");
    let v: serde_json::Value =
        serde_json::from_str(&line["TEST_PIPELINE_RESULT ".len()..]).unwrap();
    assert_eq!(v["kind"], "done");
    assert!(!v["text"].as_str().unwrap().is_empty());
    assert_eq!(v["text"], v["clipboard"]);
}
