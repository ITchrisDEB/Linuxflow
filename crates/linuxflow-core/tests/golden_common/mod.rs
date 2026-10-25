//! Utilities shared by the golden tests (reading the reference JSON files).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use linuxflow_core::domain::{DictionaryEntry, Snippet};
use serde::Deserialize;
use serde_json::Value;

/// Reads `tests/golden/<name>` and deserialises it.
pub fn load<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("lecture de {} impossible : {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("JSON invalide {name} : {e}"))
}

/// Snippet as written by the generator (the expansion may be absent).
#[derive(Debug, Deserialize)]
pub struct RawSnippet {
    pub id: String,
    pub trigger: String,
    #[serde(default)]
    pub expansion: Option<String>,
}

impl From<&RawSnippet> for Snippet {
    fn from(r: &RawSnippet) -> Self {
        Snippet {
            id: r.id.clone(),
            trigger: r.trigger.clone(),
            expansion: r.expansion.clone().unwrap_or_default(),
        }
    }
}

pub fn snippets(sets: &BTreeMap<String, Vec<RawSnippet>>, name: &str) -> Vec<Snippet> {
    sets.get(name)
        .unwrap_or_else(|| panic!("jeu de snippets inconnu : {name}"))
        .iter()
        .map(Snippet::from)
        .collect()
}

pub fn dictionary(
    dicts: &BTreeMap<String, Vec<DictionaryEntry>>,
    name: &str,
) -> Vec<DictionaryEntry> {
    dicts
        .get(name)
        .unwrap_or_else(|| panic!("dictionnaire inconnu : {name}"))
        .clone()
}

/// Raw JSON value (normalisation inputs).
pub type Json = Value;
