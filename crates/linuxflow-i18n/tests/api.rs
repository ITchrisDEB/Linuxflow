//! Tests of the public API (source language). Outside `src/` so that xgettext does not extract them.

use linuxflow_i18n::*;
use std::path::PathBuf;

#[test]
fn source_language_returns_msgid() {
    let t = Translator::source();
    assert_eq!(t.tr("Hello"), "Hello");
    assert_eq!(t.trn("{} file", "{} files", 1), "{} file");
    assert_eq!(t.trn("{} file", "{} files", 0), "{} files");
}

#[test]
fn tr_fmt_is_safe() {
    let t = Translator::source();
    assert_eq!(t.tr_fmt("{} of {}", &["1", "2"]), "1 of 2");
    assert_eq!(t.tr_fmt("{} of {}", &["1"]), "1 of {}");
    assert_eq!(t.tr_fmt("{}", &["a", "b"]), "a");
    assert_eq!(t.tr_fmt("{1} then {0}", &["a", "b"]), "b then a");
    assert_eq!(t.tr_fmt("{9} {x} { } {", &["a"]), "{9} {x} { } {");
    assert_eq!(t.tr_fmt("{} {}", &["{}", "z"]), "{} z");
    assert_eq!(t.tr_fmt("é{}è", &["ü"]), "éüè");
    assert_eq!(t.tr_fmt("", &[]), "");
}

#[test]
fn english_formats() {
    let t = Translator::source();
    assert_eq!(t.format_bytes(512), "512 B");
    assert_eq!(t.format_bytes(1_500), "2 KB");
    assert_eq!(t.format_bytes(244_000_000), "244.0 MB");
    assert_eq!(t.format_bytes(3_100_000_000), "3.1 GB");
    assert_eq!(t.format_number(1234.5, 1), "1,234.5");
}

#[test]
fn english_relative_time() {
    let t = Translator::source();
    let now = 1_792_850_700 + 3600 * 5; // 2026-10-24 19:05 UTC
    assert_eq!(t.format_relative_at(now - 10, now, 0), "just now");
    assert_eq!(t.format_relative_at(now + 100, now, 0), "just now");
    assert_eq!(t.format_relative_at(now - 300, now, 0), "5 min ago");
    assert_eq!(
        t.format_relative_at(1_792_850_700, now, 0),
        "today at 14:05"
    );
    assert_eq!(
        t.format_relative_at(1_792_850_700 - 86_400, now, 0),
        "Oct 23, 14:05"
    );
    assert_eq!(
        t.format_relative_at(1_792_850_700 - 400 * 86_400, now, 0),
        "Sep 19 2025, 14:05"
    );
    // The time offset changes the local day: 23:30 UTC the day before is "today" at UTC+2.
    let ts = now - 19 * 3600 - 1800; // 2026-10-23 23:35 UTC => 01:35 on the 24th at UTC+2
    assert_eq!(t.format_relative_at(ts, now, 2 * 3600), "today at 01:35");
}

#[test]
fn broken_catalog_dir_falls_back() {
    let t = Translator::load(
        "linuxflow",
        &["fr".to_string()],
        &[PathBuf::from("/nonexistent/dir/for/test")],
    );
    assert_eq!(t.language(), "en");
    assert_eq!(t.tr("x"), "x");
}

#[test]
fn global_api_never_panics_before_init() {
    // Does not alter the global state: only reads.
    let _ = tr("x");
    let _ = format_relative(0);
    let _ = format_bytes(u64::MAX);
}
