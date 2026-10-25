#![allow(clippy::expect_used)]
//! End-to-end test: compiles `po/fr.po` with `msgfmt` in a temporary folder, then checks
//! real translations with `LANGUAGE=fr`. A single test (it modifies the process environment).
//! Skipped cleanly if `msgfmt` is missing (gettext package).

use linuxflow_i18n as i18n;
use std::path::Path;
use std::process::Command;

fn msgfmt(po: &Path, out_root: &Path, lang: &str, domain: &str) -> bool {
    let dir = out_root.join(lang).join("LC_MESSAGES");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    Command::new("msgfmt")
        .arg("-o")
        .arg(dir.join(format!("{domain}.mo")))
        .arg(po)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn french_catalog_end_to_end() {
    if Command::new("msgfmt").arg("--version").output().is_err() {
        eprintln!("msgfmt introuvable : test sauté");
        return;
    }
    let tmp = tempfile::tempdir().expect("tmp");
    let fr_po = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../po/fr.po");
    assert!(
        msgfmt(&fr_po, tmp.path(), "fr", "linuxflow"),
        "msgfmt fr.po"
    );

    // Test catalogue with a plural, separate domain.
    let plural_po = tmp.path().join("plural.po");
    std::fs::write(
        &plural_po,
        "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\
         \"Plural-Forms: nplurals=2; plural=(n > 1);\\n\"\n\n\
         msgid \"{} file\"\nmsgid_plural \"{} files\"\nmsgstr[0] \"{} fichier\"\nmsgstr[1] \"{} fichiers\"\n",
    )
    .expect("write po");
    assert!(msgfmt(&plural_po, tmp.path(), "fr", "plural"));

    // Environment: LANGUAGE=fr, folder through the variable (no argument).
    std::env::set_var("LANGUAGE", "fr");
    std::env::set_var(i18n::LOCALE_DIR_ENV, tmp.path());
    i18n::init("linuxflow", None);
    assert_eq!(i18n::current_language(), "fr");
    assert_eq!(i18n::tr("Good morning"), "Bonjour");
    assert_eq!(i18n::tr("just now"), "à l’instant");
    assert_eq!(
        i18n::tr_fmt("today at {}", &["14:05"]),
        "aujourd’hui à 14:05"
    );
    assert_eq!(i18n::tr("Not a known msgid"), "Not a known msgid");
    assert_eq!(i18n::tr_fmt("Good morning, {}", &["Ada"]), "Bonjour, Ada");
    // Missing or extra arguments: never a panic.
    assert_eq!(i18n::tr_fmt("Good morning, {}", &[]), "Bonjour, {}");

    // French formats.
    assert_eq!(
        i18n::format_number(1234567.5, 1),
        "1\u{202f}234\u{202f}567,5"
    );
    assert_eq!(i18n::format_bytes(3_100_000_000), "3,1\u{a0}Go");
    assert_eq!(i18n::format_bytes(244_000_000), "244,0\u{a0}Mo");
    assert_eq!(i18n::format_percent(0.425, 1), "42,5\u{202f}%");
    let t = i18n::Translator::load("linuxflow", &["fr".into()], &[tmp.path().to_path_buf()]);
    let now = 1_792_850_700 + 3600 * 5; // 2026-10-24 19:05 UTC
    assert_eq!(t.format_relative_at(now - 10, now, 0), "à l’instant");
    assert_eq!(t.format_relative_at(now - 300, now, 0), "il y a 5 min");
    assert_eq!(
        t.format_relative_at(1_792_850_700, now, 0),
        "aujourd’hui à 14:05"
    );
    assert_eq!(
        t.format_relative_at(1_792_850_700 - 86_400, now, 0),
        "23 oct., 14:05"
    );

    // Plurals (ngettext) with the catalogue rule: 0 and 1 are singular in French.
    let tp = i18n::Translator::load("plural", &["fr".into()], &[tmp.path().to_path_buf()]);
    assert_eq!(tp.trn("{} file", "{} files", 0), "{} fichier");
    assert_eq!(tp.trn("{} file", "{} files", 1), "{} fichier");
    assert_eq!(tp.trn("{} file", "{} files", 2), "{} fichiers");
    assert_eq!(tp.trn("unknown", "unknowns", 2), "unknowns");

    // Live language switch, English fallback if the catalogue is missing.
    i18n::set_language(Some("en"));
    assert_eq!(i18n::tr("Good morning"), "Good morning");
    assert_eq!(i18n::format_bytes(3_100_000_000), "3.1 GB");
    i18n::set_language(Some("xx_YY"));
    assert_eq!(i18n::current_language(), "en");
    assert_eq!(i18n::tr("Good morning"), "Good morning");
    i18n::set_language(Some("fr_FR"));
    assert_eq!(i18n::tr("Good morning"), "Bonjour");
    // `None` = system language => LANGUAGE=fr.
    i18n::set_language(None);
    assert_eq!(i18n::tr("Good morning"), "Bonjour");
    std::env::set_var("LANGUAGE", "C");
    i18n::set_language(None);
    assert_eq!(i18n::tr("Good morning"), "Good morning");

    // Invalid folder: English, no panic.
    std::env::set_var(i18n::LOCALE_DIR_ENV, tmp.path().join("absent"));
    std::env::set_var("LANGUAGE", "fr");
    i18n::init("linuxflow", Some(Path::new("/nonexistent-for-test")));
    assert_eq!(i18n::tr("Good morning"), "Good morning");
}
