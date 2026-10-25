//! Golden tests of `format::format_text`: the expected values are frozen reference data sets.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod golden_common;

use std::collections::BTreeMap;

use golden_common::{dictionary, load, snippets, RawSnippet};
use linuxflow_core::domain::{DictionaryEntry, Snippet};
use linuxflow_core::format::{format_text, FormatOptions};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    input: String,
    #[serde(default)]
    language: Option<String>,
    dict: String,
    snippets: String,
    expected: String,
}

#[derive(Deserialize)]
struct File {
    dictionaries: BTreeMap<String, Vec<DictionaryEntry>>,
    snippets: BTreeMap<String, Vec<RawSnippet>>,
    cases: Vec<Case>,
}

#[test]
fn format_text_matches_js() {
    let file: File = load("format.json");
    assert!(file.cases.len() >= 200, "corpus trop petit");
    let mut failures = Vec::new();
    // The vocabulary cache has a single entry: the cases are grouped by dictionary.
    let mut ordered: Vec<&Case> = file.cases.iter().collect();
    ordered.sort_by(|a, b| a.dict.cmp(&b.dict));
    for c in ordered {
        let dict = dictionary(&file.dictionaries, &c.dict);
        let sn = snippets(&file.snippets, &c.snippets);
        let got = format_text(
            &c.input,
            &FormatOptions {
                language: c.language.as_deref().unwrap_or(""),
                snippets: &sn,
                dictionary: &dict,
            },
        );
        if got != c.expected {
            failures.push(format!(
                "entrée {:?} (langue {:?}, dico {}, snippets {})\n  attendu {:?}\n  obtenu  {:?}",
                c.input, c.language, c.dict, c.snippets, c.expected, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} écart(s) sur {} cas :\n{}",
        failures.len(),
        file.cases.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Hand-written cases (see the test plan), with the invisible characters as escapes.
#[test]
fn plan_cases() {
    let fr = |s: &str| {
        format_text(
            s,
            &FormatOptions {
                language: "fr",
                ..Default::default()
            },
        )
    };
    assert_eq!(
        fr("Bonjour ! Comment vas-tu ?"),
        "Bonjour\u{202F}! Comment vas-tu\u{202F}?"
    );
    assert_eq!(
        fr("Elle a dit «bonjour»"),
        "Elle a dit «\u{00A0}bonjour\u{00A0}»"
    );
    assert_eq!(fr("fin. E, le modèle"), "Fin. Le modèle");
    assert_eq!(fr("fin. e, le modèle"), "Fin. E, le modèle");
    assert_eq!(fr("? début"), "Début");
    assert_eq!(fr("Il est 10:30"), "Il est 10:30");
    assert_eq!(fr("http://x.fr"), "Http://x.fr");
    assert_eq!(fr("phrase.Suivante"), "Phrase. Suivante");
    assert_eq!(fr("a,b"), "A, b");
    assert_eq!(fr("du coup du coup oui"), "Du coup oui");
    assert_eq!(fr(""), "");
}

/// Deliberate deviations from the reference implementation. Each case states what the reference did
/// (recorded in tests/golden/deviations.json) and what this implementation does.
mod intentional_deviations {
    use super::*;
    use serde_json::Value;

    fn sn(trigger: &str, expansion: &str) -> Snippet {
        Snippet {
            id: "t".into(),
            trigger: trigger.into(),
            expansion: expansion.into(),
        }
    }

    fn run(input: &str, snippets: &[Snippet], dictionary: &[DictionaryEntry]) -> String {
        format_text(
            input,
            &FormatOptions {
                language: "fr",
                snippets,
                dictionary,
            },
        )
    }

    fn entry(word: &str, sounds: &[&str]) -> DictionaryEntry {
        DictionaryEntry {
            word: word.into(),
            sounds: sounds.iter().map(|s| s.to_string()).collect(),
        }
    }

    // (a) Literal snippet expansion: the reference interpreted `$&`, `$$`, `` $` `` and `$'`.

    #[test]
    fn snippet_ampersand_is_literal() {
        // Reference: "XbonjourY".
        assert_eq!(run("bonjour", &[sn("bonjour", "X$&Y")], &[]), "X$&Y");
    }

    #[test]
    fn snippet_double_dollar_is_literal() {
        // Reference: "$10".
        assert_eq!(run("prix 10", &[sn("prix", "$$")], &[]), "$$ 10");
    }

    #[test]
    fn snippet_group_reference_is_literal() {
        // Reference: "$1" too (no group); this implementation gives the same result.
        assert_eq!(run("a", &[sn("a", "$1")], &[]), "$1");
    }

    #[test]
    fn snippet_prefix_suffix_references_are_literal() {
        // Reference: "Avantavant|aprèsaprès".
        assert_eq!(
            run("avant mot après", &[sn("mot", "$`|$'")], &[]),
            "Avant $`|$' après"
        );
    }

    // (b) Vocabulary tokens: their digits must not trigger "zéro", "un"…

    #[test]
    fn placeholder_digits_do_not_match_numeric_rules() {
        // Reference: "Nul" (the "0" token was read as "zéro").
        let dict = vec![entry("Bêta", &["beta"]), entry("Nul", &["zéro"])];
        assert_eq!(run("beta", &[], &dict), "Bêta");
    }

    #[test]
    fn placeholder_digits_second_slot() {
        // Reference: "Nul et Nul".
        let dict = vec![entry("Bêta", &["beta"]), entry("Nul", &["zéro"])];
        assert_eq!(run("zero et beta", &[], &dict), "Nul et Bêta");
    }

    #[test]
    fn placeholder_digit_one_does_not_match_un() {
        let dict = vec![entry("Un", &["un"]), entry("Bêta", &["beta"])];
        // Two tokens (index 0 and 1): the index "1" must not be read again as "un".
        assert_eq!(run("un beta", &[], &dict), "Un Bêta");
    }

    #[test]
    fn placeholder_many_slots_keep_order() {
        let dict = vec![entry("Z", &["zz"]), entry("Dix", &["10"])];
        let input = "zz ".repeat(12);
        let out = run(input.trim(), &[], &dict);
        assert_eq!(out, ["Z"; 12].join(" "));
    }

    #[test]
    fn documented_js_outputs_are_still_recorded() {
        // The reference left unrestored tokens (U+E000/U+E001) around "Nul".
        // Safeguard: the documentation file exists and does contain the reference behaviour.
        let v: Value = load("deviations.json");
        let list = v.as_array().expect("tableau");
        assert!(list.iter().any(|d| d["kind"] == "placeholder_digits"
            && d["js_output"]
                .as_str()
                .is_some_and(|o| o.contains("Nul") && o != "Nul")));
        assert!(list
            .iter()
            .any(|d| d["kind"] == "snippet_dollar" && d["js_output"] == "XbonjourY"));
    }
}
