//! Golden tests of the isolated `format` stages (expected values from the reference data set).

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod golden_common;

use std::collections::BTreeMap;

use golden_common::{load, snippets, RawSnippet};
use linuxflow_core::format::{
    apply_snippets, capitalise, count_words, fix_punctuation, strip_fillers,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Plain {
    input: String,
    expected: String,
}
#[derive(Deserialize)]
struct Count {
    input: String,
    expected: u32,
}
#[derive(Deserialize)]
struct Punct {
    input: String,
    language: String,
    expected: String,
}
#[derive(Deserialize)]
struct Snip {
    input: String,
    snippets: String,
    expected: String,
}
#[derive(Deserialize)]
struct File {
    snippets: BTreeMap<String, Vec<RawSnippet>>,
    strip_fillers: Vec<Plain>,
    apply_snippets: Vec<Snip>,
    fix_punctuation: Vec<Punct>,
    capitalise: Vec<Plain>,
    count_words: Vec<Count>,
}

fn check<T: std::fmt::Debug + PartialEq>(
    stage: &str,
    failures: &mut Vec<String>,
    input: &str,
    exp: T,
    got: T,
) {
    if exp != got {
        failures.push(format!(
            "{stage} {input:?}\n  attendu {exp:?}\n  obtenu  {got:?}"
        ));
    }
}

fn report(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} écart(s) :\n{}",
        failures.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn strip_fillers_matches_js() {
    let f: File = load("stages.json");
    let mut bad = Vec::new();
    for c in &f.strip_fillers {
        check(
            "strip_fillers",
            &mut bad,
            &c.input,
            &c.expected,
            &strip_fillers(&c.input),
        );
    }
    report(bad);
}

#[test]
fn capitalise_matches_js() {
    let f: File = load("stages.json");
    let mut bad = Vec::new();
    for c in &f.capitalise {
        check(
            "capitalise",
            &mut bad,
            &c.input,
            &c.expected,
            &capitalise(&c.input),
        );
    }
    report(bad);
}

#[test]
fn count_words_matches_js() {
    let f: File = load("stages.json");
    let mut bad = Vec::new();
    for c in &f.count_words {
        check(
            "count_words",
            &mut bad,
            &c.input,
            c.expected,
            count_words(&c.input),
        );
    }
    report(bad);
}

#[test]
fn fix_punctuation_matches_js() {
    let f: File = load("stages.json");
    let mut bad = Vec::new();
    for c in &f.fix_punctuation {
        check(
            &format!("fix_punctuation[{}]", c.language),
            &mut bad,
            &c.input,
            &c.expected,
            &fix_punctuation(&c.input, &c.language),
        );
    }
    report(bad);
}

#[test]
fn apply_snippets_matches_js() {
    let f: File = load("stages.json");
    let mut bad = Vec::new();
    for c in &f.apply_snippets {
        let sn = snippets(&f.snippets, &c.snippets);
        check(
            &format!("apply_snippets[{}]", c.snippets),
            &mut bad,
            &c.input,
            &c.expected,
            &apply_snippets(&c.input, &sn),
        );
    }
    report(bad);
}
