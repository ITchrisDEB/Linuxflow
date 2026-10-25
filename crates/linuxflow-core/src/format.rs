//! Light, deterministic formatting of the transcription.
//!
//! 11-step pipeline: language, spaces, fillers, stray "E," (fr), leading punctuation,
//! snippets, vocabulary protection, punctuation, double spaces, capital letters,
//! vocabulary restoration.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use fancy_regex::Regex;

use crate::domain::{DictionaryEntry, Snippet};
use crate::vocab::{
    compile_re, is_js_ws, js_trim, protect_vocabulary, replace_all_lit, replace_all_with,
    JS_WS_INNER, PH_CLOSE, PH_OPEN, WORD_END, WORD_START,
};

const FILLERS: &[&str] = &[
    "euh+", "heu+", "hum+", "hmm+", "mmh+", "um+", "uh+", "uhm", "erm", "mh+",
];
const REPEATED: &[&str] = &["like", "genre", "voilà", "du coup", "you know", "tu vois"];
/// Narrow no-break space (French typography).
const NNBSP: char = '\u{202F}';
/// No-break space.
const NBSP: char = '\u{00A0}';

/// Formatting options.
#[derive(Debug, Clone, Copy, Default)]
pub struct FormatOptions<'a> {
    /// Language code (only the first two letters count); empty = `fr`.
    pub language: &'a str,
    pub snippets: &'a [Snippet],
    pub dictionary: &'a [DictionaryEntry],
}

/// JavaScript-style `\s`.
fn ws() -> String {
    format!("[{JS_WS_INNER}]")
}

fn word(re: &str) -> String {
    format!("{WORD_START}(?:{re}){WORD_END}")
}

static FILLER_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    let s = ws();
    compile_re(&format!("(?i){s}*,?{s}*{}[,]?", word(&FILLERS.join("|"))))
});

/// One expression per repeated marker: "genre genre" -> "genre".
static REPEATED_RES: LazyLock<Vec<(&'static str, Option<Regex>)>> = LazyLock::new(|| {
    let s = ws();
    REPEATED
        .iter()
        .map(|w| {
            let e = fancy_regex::escape(w);
            let re = compile_re(&format!("(?i){}(?:{s}*,?{s}+{e}{WORD_END})+", word(&e)));
            (*w, re)
        })
        .collect()
});

static LEADING_E_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("(^|[.!?,;]{})E,{}", ws(), ws())));
static LEADING_PUNCT_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("^[{JS_WS_INNER},.;:!?]+")));
static MULTI_SPACE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("{}+", ws())));
static DOUBLE_SPACE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("{}{{2,}}", ws())));

static P_BEFORE_CLOSE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("{}+([,.)\\]}}…])", ws())));
static P_AFTER_OPEN: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("([(\\[{{]){}+", ws())));
static P_COMMA_SPACE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!(",(?=[\\p{{L}}{PH_OPEN}])")));
static P_SENTENCE_SPACE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re("([.!?…])(?=\\p{Lu})"));
static P_COMMA_COMMA: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!(",{}*,", ws())));
static P_DOT_COMMA: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("\\.{}*,", ws())));
static P_COMMA_DOT: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!(",{}*\\.", ws())));
static P_FR_HIGH: LazyLock<Option<Regex>> = LazyLock::new(|| {
    compile_re(&format!(
        "(?<=[\\p{{L}}\\p{{N}}»)\\]{PH_CLOSE}])[ \\x{{A0}}\\x{{202F}}]*([?!;:])(?![\\p{{N}}/])"
    ))
});
static P_FR_OPEN_QUOTE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re("«[ \\x{A0}\\x{202F}]*"));
static P_FR_CLOSE_QUOTE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re("[ \\x{A0}\\x{202F}]*»"));
static P_OTHER_HIGH: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("{}+([?!;:])", ws())));

static CAPITALISE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re(&format!("(^|[.!?…][\"»”]?{}+)(\\p{{Ll}})", ws())));
static COUNT_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile_re("[\\p{L}\\p{N}]+(?:['’-][\\p{L}\\p{N}]+)*"));

/// Applies a static expression; if it did not compile (a programming error
/// caught by the tests), the text passes through unchanged.
fn apply(re: &LazyLock<Option<Regex>>, text: &str, rep: &str) -> String {
    match re.as_ref() {
        Some(re) => replace_all_lit(re, text, rep),
        None => text.to_string(),
    }
}

fn apply_with<F>(re: &LazyLock<Option<Regex>>, text: &str, f: F) -> String
where
    F: FnMut(&fancy_regex::Captures<'_, str>) -> String,
{
    match re.as_ref() {
        Some(re) => replace_all_with(re, text, f),
        None => text.to_string(),
    }
}

/// Text of group `i` (empty if it did not take part).
fn group<'a>(c: &'a fancy_regex::Captures<'_, str>, i: usize) -> &'a str {
    c.get(i).map_or("", |m| m.as_str())
}

/// Removes hesitation words and collapses repeated discourse markers.
pub fn strip_fillers(text: &str) -> String {
    let mut out = apply(&FILLER_RE, text, " ");
    for (w, re) in REPEATED_RES.iter() {
        if let Some(re) = re {
            out = replace_all_lit(re, &out, w);
        }
    }
    out
}

/// Bounded cache of trigger expressions (avoids recompiling at every dictation).
static SNIPPET_CACHE: Mutex<Option<HashMap<String, Option<Regex>>>> = Mutex::new(None);

fn snippet_regex(trig: &str) -> Option<Regex> {
    let mut guard = SNIPPET_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 256 {
        map.clear();
    }
    map.entry(trig.to_string())
        .or_insert_with(|| {
            let s = ws();
            let parts: Vec<String> = trig
                .split(is_js_ws)
                .filter(|p| !p.is_empty())
                .map(|p| fancy_regex::escape(p).into_owned())
                .collect();
            compile_re(&format!("(?i){}", word(&parts.join(&format!("{s}+")))))
        })
        .clone()
}

/// Replaces each trigger (whole word, case ignored, flexible spaces) with its
/// expansion, inserted literally. Snippets apply one after the other, in order.
pub fn apply_snippets(text: &str, snippets: &[Snippet]) -> String {
    let mut out = text.to_string();
    for sn in snippets {
        let trig = js_trim(&sn.trigger);
        if trig.is_empty() {
            continue;
        }
        // A trigger that does not compile is ignored.
        if let Some(re) = snippet_regex(trig) {
            out = replace_all_lit(&re, &out, &sn.expansion);
        }
    }
    out
}

/// Spacing of the punctuation (French typography if `language == "fr"`).
pub fn fix_punctuation(text: &str, language: &str) -> String {
    let mut out = apply_with(&P_BEFORE_CLOSE, text, |c| group(c, 1).to_string());
    out = apply_with(&P_AFTER_OPEN, &out, |c| group(c, 1).to_string());
    out = apply(&P_COMMA_SPACE, &out, ", ");
    out = apply_with(&P_SENTENCE_SPACE, &out, |c| format!("{} ", group(c, 1)));
    out = apply(&P_COMMA_COMMA, &out, ",");
    out = apply(&P_DOT_COMMA, &out, ".");
    out = apply(&P_COMMA_DOT, &out, ".");
    if language == "fr" {
        out = apply_with(&P_FR_HIGH, &out, |c| format!("{NNBSP}{}", group(c, 1)));
        out = apply(&P_FR_OPEN_QUOTE, &out, &format!("«{NBSP}"));
        out = apply(&P_FR_CLOSE_QUOTE, &out, &format!("{NBSP}»"));
    } else {
        out = apply_with(&P_OTHER_HIGH, &out, |c| group(c, 1).to_string());
    }
    out
}

/// Capital letter at the start of the text and after `. ! ? …` (closing quote tolerated).
pub fn capitalise(text: &str) -> String {
    apply_with(&CAPITALISE_RE, text, |c| {
        format!("{}{}", group(c, 1), group(c, 2).to_uppercase())
    })
}

/// Word count (`l'été` and `peut-être` count as one).
pub fn count_words(text: &str) -> u32 {
    match COUNT_RE.as_ref() {
        Some(re) => {
            let n = re.find_iter(text).filter(Result::is_ok).count();
            u32::try_from(n).unwrap_or(u32::MAX)
        }
        None => 0,
    }
}

/// Formats a raw transcription.
pub fn format_text(raw: &str, opts: &FormatOptions<'_>) -> String {
    if raw.is_empty() {
        return String::new();
    }
    // 1. language: first two letters, lower case, `fr` by default.
    let lang: String = if opts.language.is_empty() {
        "fr".to_string()
    } else {
        opts.language
            .chars()
            .take(2)
            .collect::<String>()
            .to_lowercase()
    };
    // 2. multiple spaces -> a single one, trim.
    let mut t = js_trim(&apply(&MULTI_SPACE_RE, raw, " ")).to_string();
    // 3. fillers.
    t = strip_fillers(&t);
    // 4. "euh" heard as the letter E right after a punctuation mark (capital E only).
    if lang == "fr" {
        t = apply_with(&LEADING_E_RE, &t, |c| group(c, 1).to_string());
    }
    // 5. leading punctuation left by a removed filler.
    t = apply(&LEADING_PUNCT_RE, &t, "");
    // 6. snippets.
    t = apply_snippets(&t, opts.snippets);
    // 7. vocabulary protected by tokens.
    let vocab = protect_vocabulary(&t, opts.dictionary);
    t = vocab.text.clone();
    // 8. ponctuation.
    t = fix_punctuation(&t, &lang);
    // 9. espaces doubles.
    t = js_trim(&apply(&DOUBLE_SPACE_RE, &t, " ")).to_string();
    // 10. capital letters (protected words keep their case).
    t = capitalise(&t);
    // 11. restauration.
    vocab.restore(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_static_regexes_compile() {
        let all: Vec<(&str, &LazyLock<Option<Regex>>)> = vec![
            ("FILLER_RE", &FILLER_RE),
            ("LEADING_E_RE", &LEADING_E_RE),
            ("LEADING_PUNCT_RE", &LEADING_PUNCT_RE),
            ("MULTI_SPACE_RE", &MULTI_SPACE_RE),
            ("DOUBLE_SPACE_RE", &DOUBLE_SPACE_RE),
            ("P_BEFORE_CLOSE", &P_BEFORE_CLOSE),
            ("P_AFTER_OPEN", &P_AFTER_OPEN),
            ("P_COMMA_SPACE", &P_COMMA_SPACE),
            ("P_SENTENCE_SPACE", &P_SENTENCE_SPACE),
            ("P_COMMA_COMMA", &P_COMMA_COMMA),
            ("P_DOT_COMMA", &P_DOT_COMMA),
            ("P_COMMA_DOT", &P_COMMA_DOT),
            ("P_FR_HIGH", &P_FR_HIGH),
            ("P_FR_OPEN_QUOTE", &P_FR_OPEN_QUOTE),
            ("P_FR_CLOSE_QUOTE", &P_FR_CLOSE_QUOTE),
            ("P_OTHER_HIGH", &P_OTHER_HIGH),
            ("CAPITALISE_RE", &CAPITALISE_RE),
            ("COUNT_RE", &COUNT_RE),
        ];
        for (name, re) in all {
            assert!(re.is_some(), "expression statique invalide : {name}");
        }
        for (w, re) in REPEATED_RES.iter() {
            assert!(re.is_some(), "expression de répétition invalide : {w}");
        }
    }

    fn fr(raw: &str) -> String {
        format_text(
            raw,
            &FormatOptions {
                language: "fr",
                ..Default::default()
            },
        )
    }

    fn en(raw: &str) -> String {
        format_text(
            raw,
            &FormatOptions {
                language: "en",
                ..Default::default()
            },
        )
    }

    #[test]
    fn empty_and_default_language() {
        assert_eq!(fr(""), "");
        assert_eq!(
            format_text("bonjour !", &FormatOptions::default()),
            "Bonjour\u{202F}!"
        );
        assert_eq!(en("really ?"), "Really?");
    }

    #[test]
    fn french_punctuation() {
        assert_eq!(
            fr("Bonjour ! Comment vas-tu ?"),
            "Bonjour\u{202F}! Comment vas-tu\u{202F}?"
        );
        assert_eq!(fr("il est 10:30"), "Il est 10:30");
        assert_eq!(fr("http://x.fr"), "Http://x.fr");
        assert_eq!(
            fr("elle a dit «bonjour»"),
            "Elle a dit «\u{00A0}bonjour\u{00A0}»"
        );
        assert_eq!(fr("phrase.Suivante"), "Phrase. Suivante");
        assert_eq!(fr("a,b"), "A, b");
    }

    #[test]
    fn fillers_and_e_comma() {
        assert_eq!(fr("fin. E, le modèle"), "Fin. Le modèle");
        assert_eq!(fr("fin. e, le modèle"), "Fin. E, le modèle");
        assert_eq!(fr("? début"), "Début");
        assert_eq!(fr("genre genre bon"), "Genre bon");
        assert_eq!(fr("du coup du coup oui"), "Du coup oui");
        assert_eq!(
            fr("euh je pense que, hum, ça marche"),
            "Je pense que ça marche"
        );
    }

    #[test]
    fn snippets_chain_and_literal() {
        let sn = |t: &str, e: &str| Snippet {
            id: "x".into(),
            trigger: t.into(),
            expansion: e.into(),
        };
        let snippets = vec![sn("aa", "bb cc"), sn("bb cc", "zz"), sn("  ", "never")];
        let out = format_text(
            "aa bb",
            &FormatOptions {
                language: "en",
                snippets: &snippets,
                dictionary: &[],
            },
        );
        assert_eq!(out, "Zz bb");
    }

    #[test]
    fn count_words_cases() {
        assert_eq!(count_words("l'été"), 1);
        assert_eq!(count_words("peut-être"), 1);
        assert_eq!(count_words("a b c"), 3);
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn capitalise_cases() {
        assert_eq!(capitalise("a. b! c? d… e"), "A. B! C? D… E");
        assert_eq!(
            capitalise("il dit. « ok » oui. »x"),
            "Il dit. « ok » oui. »x"
        );
        assert_eq!(capitalise("fin.» suite"), "Fin.» Suite");
    }
}
