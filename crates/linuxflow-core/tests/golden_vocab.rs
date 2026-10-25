//! Golden tests of the vocabulary (expected values from the reference data set).

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod golden_common;

use std::collections::BTreeMap;

use golden_common::{dictionary, load};
use linuxflow_core::domain::DictionaryEntry;
use linuxflow_core::vocab::{apply_vocabulary, fold, normalize_value, parse_sounds};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Apply {
    input: String,
    dict: String,
    expected: String,
}
#[derive(Deserialize)]
struct Normalize {
    input: Value,
    expected: Vec<DictionaryEntry>,
}
#[derive(Deserialize)]
struct Strs {
    input: String,
    expected: Value,
}
#[derive(Deserialize)]
struct File {
    dictionaries: BTreeMap<String, Vec<DictionaryEntry>>,
    apply: Vec<Apply>,
    normalize: Vec<Normalize>,
    parse_sounds: Vec<Strs>,
    fold: Vec<Strs>,
}

#[test]
fn apply_vocabulary_matches_js() {
    let f: File = load("vocab.json");
    let mut bad = Vec::new();
    let mut ordered: Vec<&Apply> = f.apply.iter().collect();
    ordered.sort_by(|a, b| a.dict.cmp(&b.dict));
    for c in ordered {
        let dict = dictionary(&f.dictionaries, &c.dict);
        let got = apply_vocabulary(&c.input, &dict);
        if got != c.expected {
            bad.push(format!(
                "{:?} [{}]\n  attendu {:?}\n  obtenu  {:?}",
                c.input, c.dict, c.expected, got
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} écart(s) :\n{}",
        bad.len(),
        bad.iter().take(25).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn normalize_vocab_matches_js() {
    let f: File = load("vocab.json");
    for c in &f.normalize {
        assert_eq!(normalize_value(&c.input), c.expected, "entrée {}", c.input);
    }
}

#[test]
fn parse_sounds_matches_js() {
    let f: File = load("vocab.json");
    for c in &f.parse_sounds {
        let got = Value::from(parse_sounds(&c.input));
        assert_eq!(got, c.expected, "entrée {:?}", c.input);
    }
}

#[test]
fn fold_matches_js() {
    let f: File = load("vocab.json");
    for c in &f.fold {
        assert_eq!(
            Value::from(fold(&c.input)),
            c.expected,
            "entrée {:?}",
            c.input
        );
    }
}
