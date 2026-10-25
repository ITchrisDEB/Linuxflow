#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Real messages translated with `LANGUAGE=fr`: `po/fr.po` is compiled by `msgfmt` in a temporary
//! folder. The translation state is global to the process: the check runs in a child
//! process (this same test binary), so as not to disturb the other tests.
//! Skipped cleanly if `msgfmt` is missing.

use crate::testkit::service_rig;
use crate::ui_text::user_message;
use linuxflow_core::dictation::UserMessage;
use linuxflow_core::domain::{DownloadProgress, EngineInfo, EnginePhase};
use linuxflow_core::events::Event;
use linuxflow_core::traits::Controller;
use std::process::Command;

const CHILD_ENV: &str = "LF_I18N_CHILD";
const TEST_NAME: &str = "i18n_tests::french_messages_end_to_end";

fn child_checks() {
    linuxflow_i18n::init("linuxflow", None);
    assert_eq!(linuxflow_i18n::current_language(), "fr");

    assert_eq!(user_message(&UserMessage::NothingHeard), "Rien entendu");
    assert_eq!(
        user_message(&UserMessage::ModelDownloading { percent: Some(50) }),
        "Modèle en téléchargement · 50\u{202f}%"
    );
    assert_eq!(
        user_message(&UserMessage::PasteBlocked),
        "Collage bloqué — texte copié dans le presse-papiers"
    );

    // Service: shortcut validation messages and conflict.
    let r = service_rig(None, false);
    let c: &dyn Controller = &*r.service;
    assert_eq!(
        c.shortcut_test(""),
        Err("Appuie sur au moins une combinaison.".to_string())
    );
    *r.system.probe.lock().unwrap() = Some(crate::service::ProbeError::Conflict);
    assert_eq!(
        c.shortcut_test("Ctrl+Alt+K"),
        Err("Déjà utilisé par une autre application.".to_string())
    );

    // Orchestration: the pill message is translated.
    r.service.dictator().engine_status(EngineInfo {
        phase: EnginePhase::Downloading,
        loading: true,
        download: Some(DownloadProgress {
            model: "small".into(),
            done: 50,
            total: 100,
        }),
        ..EngineInfo::default()
    });
    c.dictation_toggle(false);
    assert!(r.events.events().iter().any(|e| matches!(e,
        Event::Phase { message: Some(m), .. } if m == "Modèle en téléchargement · 50\u{202f}%")));

    // Command line: the help is in French.
    let help = crate::cli::parse_from(["linuxflow", "--help"]).expect_err("help");
    assert!(help.text.contains("Dictée vocale locale"), "{}", help.text);
}

#[test]
fn french_messages_end_to_end() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_checks();
        return;
    }
    if Command::new("msgfmt").arg("--version").output().is_err() {
        eprintln!("msgfmt not found: test skipped");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("fr").join("LC_MESSAGES");
    std::fs::create_dir_all(&dir).unwrap();
    let po = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../po/fr.po");
    let ok = Command::new("msgfmt")
        .arg("-o")
        .arg(dir.join("linuxflow.mo"))
        .arg(&po)
        .status()
        .unwrap()
        .success();
    assert!(ok, "msgfmt fr.po");
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .env("LANGUAGE", "fr")
        .env(linuxflow_i18n::LOCALE_DIR_ENV, tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
