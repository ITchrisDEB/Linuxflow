//! Vocabulary correction: "what the model heard" -> "the intended word".
//!
//! Each entry is `{ word, sounds }`. The
//! comparison ignores case, accents, spaces/hyphens/dots between the
//! pieces, and treats numbers written in letters and in digits as
//! equivalent ("huit" == "8" == "eight"). The word itself is also a sound,
//! which fixes its case.
//!
//! Deliberate design choice: the replacements are protected by
//! tokens whose index is written with characters of the private-use area
//! (not digits), so that no numeric rule ("zéro", "un"…)
//! can recognise them any more.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use fancy_regex::{Captures, Regex};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::domain::DictionaryEntry;

/// Start of a protected-word token.
pub const PH_OPEN: char = '\u{E000}';
/// End of a protected-word token.
pub const PH_CLOSE: char = '\u{E001}';
/// The digit `d` of the index is written `PH_DIGIT_BASE + d` (private use, never a digit).
const PH_DIGIT_BASE: u32 = 0xE010;

/// Content (without brackets) of the JavaScript-style `\s` class (Unicode mode).
pub(crate) const JS_WS_INNER: &str = "\\t\\n\\x0B\\x0C\\r \\x{A0}\\x{1680}\\x{2000}-\\x{200A}\\x{2028}\\x{2029}\\x{202F}\\x{205F}\\x{3000}\\x{FEFF}";
/// Unicode word start (the ASCII-only `\b` would not do).
pub(crate) const WORD_START: &str = "(?<![\\p{L}\\p{N}])";
/// End of a Unicode word.
pub(crate) const WORD_END: &str = "(?![\\p{L}\\p{N}])";

/// True for the characters that `\s` and a JavaScript-style `trim` consider as blank.
pub(crate) fn is_js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// Equivalent of a JavaScript-style `trim`.
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

/// Compiles an expression; `None` if it is invalid (programming error,
/// detected by the compilation tests of all the static expressions).
pub(crate) fn compile_re(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

/// Replaces all the occurrences, computing the replacement with a function.
/// The replacement text is literal (no `$` interpretation). On a run-time
/// error of the expression (backtracking limit), the rest of the text is kept as is.
pub(crate) fn replace_all_with<F>(re: &Regex, text: &str, mut f: F) -> String
where
    F: FnMut(&Captures<'_, str>) -> String,
{
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let Ok(caps) = caps else { break };
        let Some(m) = caps.get(0) else { continue };
        out.push_str(&text[last..m.start()]);
        out.push_str(&f(&caps));
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Replaces all the occurrences with a literal string.
pub(crate) fn replace_all_lit(re: &Regex, text: &str, rep: &str) -> String {
    replace_all_with(re, text, |_| rep.to_string())
}

/// Lower case + NFD decomposition + removal of the combining marks U+0300-U+036F.
pub fn fold(s: &str) -> String {
    s.to_lowercase()
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .collect()
}

/// Equivalent numbers (digit and words).
const NUMBERS: &[&[&str]] = &[
    &["0", "zero", "zéro"],
    &["1", "un", "une", "one"],
    &["2", "deux", "two"],
    &["3", "trois", "three"],
    &["4", "quatre", "four"],
    &["5", "cinq", "five"],
    &["6", "six"],
    &["7", "sept", "seven"],
    &["8", "huit", "eight"],
    &["9", "neuf", "nine"],
    &["10", "dix", "ten"],
];

/// Classes d'accents.
const FOLD_CLASSES: &[(&str, &str)] = &[
    ("a", "aàâäáã"),
    ("e", "eéèêë"),
    ("i", "iîïíì"),
    ("o", "oôöóò"),
    ("u", "uùûüú"),
    ("c", "cç"),
    ("y", "yÿ"),
    ("n", "nñ"),
];

/// Index "folded word -> number group" (the last group wins, like `Map.set`).
static NUM_INDEX: LazyLock<HashMap<String, &'static [&'static str]>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for group in NUMBERS {
        for w in *group {
            map.insert(fold(w), *group);
        }
    }
    map
});

/// Separators between pieces of a sound.
fn is_sep(c: char) -> bool {
    is_js_ws(c) || matches!(c, '-' | '–' | '—' | '\'' | '’' | '.' | ',' | '_')
}

fn char_class(ch: char) -> String {
    let f = fold(&ch.to_string());
    if let Some((_, class)) = FOLD_CLASSES.iter().find(|(k, _)| *k == f) {
        return format!("[{class}]");
    }
    fancy_regex::escape(&ch.to_lowercase().collect::<String>()).into_owned()
}

fn literal(w: &str) -> String {
    w.chars().map(char_class).collect()
}

fn token_re(tok: &str) -> String {
    match NUM_INDEX.get(&fold(tok)) {
        Some(group) => {
            let alts: Vec<String> = group.iter().map(|w| literal(w)).collect();
            format!("(?:{})", alts.join("|"))
        }
        None => literal(tok),
    }
}

/// Source of the expression of a sound; `None` if it is empty.
fn sound_pattern(sound: &str) -> Option<String> {
    let toks: Vec<&str> = js_trim(sound)
        .split(is_sep)
        .filter(|t| !t.is_empty())
        .collect();
    if toks.is_empty() {
        return None;
    }
    let sep = format!("[{JS_WS_INNER}\\-–—'’.,_]*");
    Some(
        toks.iter()
            .map(|t| token_re(t))
            .collect::<Vec<_>>()
            .join(&sep),
    )
}

/// `String(x)`-style conversion of a simple JSON value; `None` if the value is an object/array.
fn js_string(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("undefined".into()),
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `String(v || '')`-style: falsy values (null, 0, false, "") give the empty string.
fn js_string_or_empty(v: &Value) -> String {
    let falsy = match v {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        _ => false,
    };
    if falsy {
        return String::new();
    }
    js_string(v).unwrap_or_default()
}

/// Raw input before normalisation: a word and the list (already split) of its sounds.
struct RawEntry {
    word: String,
    sounds: Vec<String>,
}

fn normalize_raw(items: Vec<RawEntry>) -> Vec<DictionaryEntry> {
    let mut out: Vec<DictionaryEntry> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for e in items {
        let word = js_trim(&e.word).to_string();
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        if seen.contains(&lower) {
            continue;
        }
        seen.push(lower);
        let fw = fold(&word);
        let mut clean: Vec<String> = Vec::new();
        for s in &e.sounds {
            let v = js_trim(s);
            if v.is_empty() {
                continue;
            }
            let fv = fold(v);
            if !clean.iter().any(|x| fold(x) == fv) && fv != fw {
                clean.push(v.to_string());
            }
        }
        out.push(DictionaryEntry {
            word,
            sounds: clean,
        });
    }
    out
}

/// Normalises a dictionary: trimmed words, de-duplicated (case ignored), empty sounds or
/// sounds identical to the word (after folding) discarded.
pub fn normalize_vocab(list: &[DictionaryEntry]) -> Vec<DictionaryEntry> {
    normalize_raw(
        list.iter()
            .map(|e| RawEntry {
                word: e.word.clone(),
                sounds: e.sounds.clone(),
            })
            .collect(),
    )
}

/// Old format: a simple list of words, without sounds.
pub fn normalize_legacy(list: &[String]) -> Vec<DictionaryEntry> {
    normalize_raw(
        list.iter()
            .map(|w| RawEntry {
                word: w.clone(),
                sounds: Vec::new(),
            })
            .collect(),
    )
}

/// Normalises any JSON value: array of strings
/// (old format) and/or of `{word, sounds}` objects where `sounds` is an array or a
/// comma-separated string. Any other value gives an empty dictionary.
pub fn normalize_value(value: &Value) -> Vec<DictionaryEntry> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let raw = items
        .iter()
        .map(|e| match e {
            Value::String(s) => RawEntry {
                word: s.clone(),
                sounds: Vec::new(),
            },
            Value::Object(o) => {
                let word = o.get("word").map(js_string_or_empty).unwrap_or_default();
                let sounds = match o.get("sounds") {
                    Some(Value::Array(a)) => a.iter().map(js_string_or_empty).collect(),
                    Some(other) => js_string_or_empty(other)
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                    None => Vec::new(),
                };
                RawEntry { word, sounds }
            }
            _ => RawEntry {
                word: String::new(),
                sounds: Vec::new(),
            },
        })
        .collect();
    normalize_raw(raw)
}

/// Splits an input of sounds separated by `,`, `;` or a line break.
pub fn parse_sounds(text: &str) -> Vec<String> {
    text.split([',', ';', '\n'])
        .map(|s| js_trim(s).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Appends to `existing` the sounds of `new` that are not already there (folded comparison,
/// empty sounds ignored), keeping the order.
pub fn merge_sounds(existing: &[String], new: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in existing.iter().chain(new.iter()) {
        let v = js_trim(s);
        if v.is_empty() {
            continue;
        }
        let f = fold(v);
        if !out.iter().any(|x| fold(x) == f) {
            out.push(v.to_string());
        }
    }
    out
}

/// Starting vocabulary: common technical proper nouns, with the usual transcription
/// mistakes in French and English. Seeded only once at first launch (setting
/// `vocabSeeded`) or on request ("Restore default words"). No personal vocabulary
/// is shipped.
pub fn default_vocab() -> Vec<DictionaryEntry> {
    const WORDS: &[(&str, &[&str])] = &[
        (
            "Claude Code",
            &["clode code", "clod code", "cloud code", "claude codes"],
        ),
        ("Claude", &["clode", "clôde"]),
        (
            "GitHub",
            &["git hub", "guit hub", "guite hub", "guithub", "githab"],
        ),
        (
            "ChatGPT",
            &["chat gpt", "chat g p t", "tchat gpt", "tchat g p t"],
        ),
        ("GPT", &["g p t", "gé pé té"]),
        ("MCP", &["m c p", "aime c p", "em c p"]),
        ("API", &["a p i", "a pé i"]),
        ("OpenRouter", &["open router", "open routeur"]),
        ("Anthropic", &["anthropique", "anthropik", "antropic"]),
        ("DeepSeek", &["deep seek", "dip sik", "deep sic"]),
        ("Midjourney", &["mid journey", "mide journey"]),
        ("TikTok", &["tic toc", "tik tok", "tiktoc"]),
        ("YouTube", &["you tube", "youtoube"]),
        ("Kubernetes", &["cubernetes", "kuber nettes", "kubernétès"]),
        (
            "PostgreSQL",
            &["postgres q l", "post gress q l", "postgresse q l"],
        ),
        ("Linux", &["linuxe", "li nux"]),
    ];
    WORDS
        .iter()
        .map(|(word, sounds)| DictionaryEntry {
            word: (*word).to_string(),
            sounds: sounds.iter().map(|s| (*s).to_string()).collect(),
        })
        .collect()
}

struct Rule {
    word: String,
    len: usize,
    re: Regex,
}

/// Compiled rules of a dictionary, sorted from the longest sound to the shortest.
pub struct CompiledVocab {
    rules: Vec<Rule>,
}

/// Single-entry cache (original dictionary, compiled rules).
type CacheEntry = (Vec<DictionaryEntry>, Arc<CompiledVocab>);
static CACHE: Mutex<Option<CacheEntry>> = Mutex::new(None);

impl CompiledVocab {
    /// Compiles a dictionary (without a cache).
    pub fn compile(vocab: &[DictionaryEntry]) -> Self {
        let mut rules = Vec::new();
        for entry in normalize_vocab(vocab) {
            let all = std::iter::once(&entry.word).chain(entry.sounds.iter());
            for s in all {
                let Some(p) = sound_pattern(s) else { continue };
                let len = fold(s)
                    .chars()
                    .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    .count();
                let pattern = format!("(?i){WORD_START}(?:{p}){WORD_END}");
                // A rule that does not compile is ignored rather than making the dictation fail.
                if let Some(re) = compile_re(&pattern) {
                    rules.push(Rule {
                        word: entry.word.clone(),
                        len,
                        re,
                    });
                }
            }
        }
        // Longest first ("Claude Code" before "Claude"); stable sort.
        rules.sort_by_key(|r| std::cmp::Reverse(r.len));
        Self { rules }
    }

    /// Compiles with a single-entry cache: the same dictionary is compiled only once.
    pub fn cached(vocab: &[DictionaryEntry]) -> Arc<Self> {
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((key, compiled)) = guard.as_ref() {
            if key.as_slice() == vocab {
                return Arc::clone(compiled);
            }
        }
        let compiled = Arc::new(Self::compile(vocab));
        *guard = Some((vocab.to_vec(), Arc::clone(&compiled)));
        compiled
    }

    /// Number of rules (a word + its non-empty sounds).
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Canonical words in the order the rules are applied (for the tests).
    pub fn rule_words(&self) -> Vec<&str> {
        self.rules.iter().map(|r| r.word.as_str()).collect()
    }

    /// Replaces each heard variant with its canonical word, behind tokens
    /// that the following steps cannot alter; `Protected::restore` puts them back at the end.
    pub fn protect(&self, text: &str) -> Protected {
        let mut slots: Vec<String> = Vec::new();
        let mut out = text.to_string();
        for r in &self.rules {
            out = replace_all_with(&r.re, &out, |_| {
                slots.push(r.word.clone());
                placeholder(slots.len() - 1)
            });
        }
        Protected { text: out, slots }
    }
}

/// `PH_OPEN` token + index (private-use digits) + `PH_CLOSE`.
fn placeholder(index: usize) -> String {
    let mut s = String::new();
    s.push(PH_OPEN);
    for d in index.to_string().bytes() {
        let ch = char::from_u32(PH_DIGIT_BASE + u32::from(d - b'0')).unwrap_or(PH_CLOSE);
        s.push(ch);
    }
    s.push(PH_CLOSE);
    s
}

fn digit_of(c: char) -> Option<usize> {
    let v = u32::from(c);
    (PH_DIGIT_BASE..PH_DIGIT_BASE + 10)
        .contains(&v)
        .then(|| (v - PH_DIGIT_BASE) as usize)
}

/// Text with protected words, and what is needed to restore them.
#[derive(Debug, Clone)]
pub struct Protected {
    /// Text containing the tokens.
    pub text: String,
    slots: Vec<String>,
}

impl Protected {
    /// Replaces each token with its word (an unknown token becomes an empty string).
    pub fn restore(&self, t: &str) -> String {
        let chars: Vec<char> = t.chars().collect();
        let mut out = String::with_capacity(t.len());
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == PH_OPEN {
                let mut j = i + 1;
                let mut index: Option<usize> = None;
                let mut ndigits = 0;
                while j < chars.len() {
                    match digit_of(chars[j]) {
                        Some(d) => {
                            index = Some(index.unwrap_or(0).saturating_mul(10).saturating_add(d));
                            ndigits += 1;
                            j += 1;
                        }
                        None => break,
                    }
                }
                if ndigits > 0 && j < chars.len() && chars[j] == PH_CLOSE {
                    if let Some(w) = index.and_then(|k| self.slots.get(k)) {
                        out.push_str(w);
                    }
                    i = j + 1;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }
}

/// Protects the vocabulary of a text (see [`CompiledVocab::protect`]), with the cache.
pub fn protect_vocabulary(text: &str, vocab: &[DictionaryEntry]) -> Protected {
    CompiledVocab::cached(vocab).protect(text)
}

/// Applies the vocabulary and restores right away.
pub fn apply_vocabulary(text: &str, vocab: &[DictionaryEntry]) -> String {
    let p = protect_vocabulary(text, vocab);
    p.restore(&p.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test fixtures only: these words are not part of the product.
    fn entry(word: &str, sounds: &[&str]) -> DictionaryEntry {
        DictionaryEntry {
            word: word.into(),
            sounds: sounds.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn fixture() -> Vec<DictionaryEntry> {
        vec![
            entry("n8n", &["n huit n", "n eight n"]),
            entry("Claude Code", &["clode code"]),
            entry("Claude", &["clode"]),
            entry("GitHub", &["git hub", "guit hub"]),
            entry("GPT", &["g p t"]),
        ]
    }

    #[test]
    fn fold_removes_accents_and_case() {
        assert_eq!(fold("Éléphant Çà"), "elephant ca");
        assert_eq!(fold("ÀÂÄ"), "aaa");
    }

    #[test]
    fn default_vocab_is_generic_and_well_formed() {
        let list = default_vocab();
        assert!(!list.is_empty());
        // no duplicate, no empty sound or sound identical to the word, nothing personal
        assert_eq!(normalize_vocab(&list).len(), list.len());
        for e in &list {
            assert!(!e.sounds.is_empty(), "{}", e.word);
        }
        let words: Vec<String> = list.iter().map(|e| e.word.to_lowercase()).collect();
        for personal in ["n8n", "rea"] {
            assert!(!words.contains(&personal.to_string()), "{personal}");
        }
    }

    #[test]
    fn default_vocab_corrects_known_misrecognitions() {
        assert_eq!(
            apply_vocabulary("j'utilise git hub et clode code", &default_vocab()),
            "j'utilise GitHub et Claude Code"
        );
    }

    #[test]
    fn parse_sounds_splits_on_separators() {
        assert_eq!(parse_sounds("a, b;c\nd"), ["a", "b", "c", "d"]);
        assert!(parse_sounds(" , ;\n ").is_empty());
    }

    #[test]
    fn merge_sounds_dedups_by_fold() {
        let m = merge_sounds(
            &["clode".to_string()],
            &["Clôde".to_string(), " ".to_string(), "claude".to_string()],
        );
        assert_eq!(m, ["clode", "claude"]);
    }

    #[test]
    fn normalize_dedups_and_cleans() {
        let n = normalize_vocab(&[
            entry(" Foo ", &["bar", "BAR", "", "foo", "bär"]),
            entry("foo", &["x"]),
            entry("", &["y"]),
        ]);
        assert_eq!(n, vec![entry("Foo", &["bar"])]);
    }

    #[test]
    fn normalize_legacy_strings() {
        let n = normalize_legacy(&["a".into(), "A".into(), " ".into(), "b".into()]);
        assert_eq!(n, vec![entry("a", &[]), entry("b", &[])]);
    }

    #[test]
    fn normalize_value_mixed() {
        let v: Value = serde_json::json!(["x", {"word": "Y", "sounds": "a, b ,a"}, {"word": "Z", "sounds": ["q", 5]}, 3, null]);
        let n = normalize_value(&v);
        assert_eq!(
            n,
            vec![
                entry("x", &[]),
                entry("Y", &["a", "b"]),
                entry("Z", &["q", "5"])
            ]
        );
        assert!(normalize_value(&serde_json::json!("nope")).is_empty());
    }

    #[test]
    fn number_equivalence() {
        let d = fixture();
        for t in [
            "n huit n",
            "N-8-N",
            "N, huit, N",
            "n 8 n",
            "n8n",
            "N8N",
            "n eight n",
        ] {
            assert_eq!(apply_vocabulary(t, &d), "n8n", "{t}");
        }
    }

    #[test]
    fn longest_first_and_accents() {
        let d = fixture();
        assert_eq!(apply_vocabulary("clode code", &d), "Claude Code");
        assert_eq!(apply_vocabulary("clôde", &d), "Claude");
        assert_eq!(
            apply_vocabulary("Git Hub guit hub g p t", &d),
            "GitHub GitHub GPT"
        );
        assert_eq!(apply_vocabulary("agpt", &d), "agpt");
    }

    #[test]
    fn rules_sorted_stably_by_length() {
        let c = CompiledVocab::compile(&fixture());
        let words = c.rule_words();
        assert_eq!(words[0], "Claude Code");
        assert!(c.rule_count() >= 10);
    }

    #[test]
    fn cache_returns_same_compilation() {
        let d = vec![entry("CacheProbe", &["cash probe"])];
        let a = CompiledVocab::cached(&d);
        let b = CompiledVocab::cached(&d);
        assert!(Arc::ptr_eq(&a, &b));
        let other = vec![entry("Other", &[])];
        let c = CompiledVocab::cached(&other);
        assert!(!Arc::ptr_eq(&a, &c));
    }

    #[test]
    fn restore_handles_unknown_and_malformed_tokens() {
        let p = protect_vocabulary("x", &[]);
        let unknown = format!(
            "{}{}{}",
            PH_OPEN,
            char::from_u32(0xE019).unwrap_or('?'),
            PH_CLOSE
        );
        assert_eq!(p.restore(&unknown), "");
        let malformed = format!("{PH_OPEN}abc");
        assert_eq!(p.restore(&malformed), malformed);
    }

    #[test]
    fn placeholders_do_not_trigger_numeric_rules() {
        let d = vec![entry("Bêta", &["beta"]), entry("Nul", &["zéro"])];
        assert_eq!(apply_vocabulary("beta", &d), "Bêta");
        assert_eq!(apply_vocabulary("zero et beta", &d), "Nul et Bêta");
    }

    #[test]
    fn js_ws_matches_regex_class() {
        let re = compile_re(&format!("^[{JS_WS_INNER}]$"));
        let Some(re) = re else {
            panic!("classe JS_WS_INNER invalide");
        };
        for c in ['\u{A0}', '\u{202F}', '\u{FEFF}', ' ', '\n', '\u{3000}'] {
            assert!(is_js_ws(c));
            assert!(re.is_match(&c.to_string()).unwrap_or(false), "{c:?}");
        }
        assert!(!is_js_ws('\u{85}'));
    }
}
