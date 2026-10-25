//! User settings (`settings.json`): writing the defaults, migration of old files, tolerant
//! field-by-field reading, validation of patches.
//!
//! The error messages are English msgids (the interface translates them); they cite the JSON key.

use crate::domain::Settings;
use crate::error::{Error, Result};
use crate::store::{backup_corrupt, write_atomic, FileMode, Store, WarnFn};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const MAX_PASTE_DELAY_MS: u64 = 5_000;
pub const MAX_RESTORE_CLIPBOARD_MS: u64 = 60_000;
pub const MIN_DICTATION_SEC: u64 = 1;
pub const MAX_DICTATION_SEC: u64 = 3_600;
pub const MIN_HISTORY_LIMIT: usize = 1;
pub const MAX_HISTORY_LIMIT: usize = 100_000;
const MAX_LANGUAGE_LEN: usize = 16;

pub struct SettingsStore {
    store: Store<Settings>,
}

impl SettingsStore {
    /// Opens `settings.json`: migrates an old file, repairs field by field, writes the defaults
    /// if it does not exist. Never fails: an impossible write is reported through `warn`.
    pub fn open(path: impl Into<PathBuf>, warn: WarnFn) -> Self {
        let path = path.into();
        let rewrite_defaults = repair_file(&path, &*warn);
        let store = Store::<Settings>::open_with(&path, FileMode::Private, warn.clone());
        let this = Self { store };
        let loaded = this.store.get();
        let mut fixed = loaded.clone();
        normalize_loaded(&mut fixed, &*warn);
        if rewrite_defaults || !this.store.exists() || fixed != loaded {
            if let Err(e) = this.store.set(fixed) {
                warn(&format!("cannot write {}: {e}", path.display()));
            }
        }
        this
    }

    /// Variant without a log (tests, tools).
    pub fn open_silent(path: impl Into<PathBuf>) -> Self {
        Self::open(path, Arc::new(|_| {}))
    }

    pub fn path(&self) -> &Path {
        self.store.path()
    }

    pub fn get(&self) -> Settings {
        self.store.get()
    }

    /// Shallow merge of a JSON object into the settings. Only the keys of the patch are
    /// validated (an already loaded state never blocks a valid patch); on error, neither the
    /// file nor the memory changes.
    pub fn patch(&self, patch: Value) -> Result<Settings> {
        let Value::Object(mut incoming) = patch else {
            return Err(Error::msg("The settings patch must be a JSON object"));
        };
        // Old name.
        if let Some(v) = incoming.remove("modelCache") {
            incoming.entry("modelDir").or_insert(v);
        }
        self.store.try_update(|current| {
            let Value::Object(mut obj) = serde_json::to_value(&*current)? else {
                return Err(Error::msg("Settings cannot be serialized"));
            };
            for (k, v) in &incoming {
                obj.insert(k.clone(), v.clone());
            }
            let mut next = match serde_json::from_value::<Settings>(Value::Object(obj)) {
                Ok(s) => s,
                Err(e) => return Err(explain_error(current, &incoming, e)),
            };
            if next
                .model_dir
                .as_deref()
                .is_some_and(|p| p.as_os_str().is_empty())
            {
                next.model_dir = None;
            }
            validate_keys(&next, |k| incoming.contains_key(k))?;
            *current = next;
            Ok(current.clone())
        })
    }

    /// Replaces all the settings (validated as a whole).
    pub fn replace(&self, settings: Settings) -> Result<Settings> {
        validate(&settings)?;
        self.store.set(settings.clone())?;
        Ok(settings)
    }
}

/// Names the faulty key when the global deserialisation fails.
fn explain_error(
    current: &Settings,
    incoming: &Map<String, Value>,
    global: serde_json::Error,
) -> Error {
    if let Ok(Value::Object(base)) = serde_json::to_value(current) {
        for (k, v) in incoming {
            let mut one = base.clone();
            one.insert(k.clone(), v.clone());
            if let Err(e) = serde_json::from_value::<Settings>(Value::Object(one)) {
                return Error::msg(format!("Setting \"{k}\" is invalid: {e}"));
            }
        }
    }
    Error::msg(format!("Invalid settings: {global}"))
}

/// Check of all the values (the types are already guaranteed by serde).
pub fn validate(s: &Settings) -> Result<()> {
    validate_keys(s, |_| true)
}

/// Check of only the keys (camelCase JSON names) for which `wanted` answers true.
pub fn validate_keys(s: &Settings, wanted: impl Fn(&str) -> bool) -> Result<()> {
    fn range(name: &str, v: u64, min: u64, max: u64) -> Result<()> {
        if (min..=max).contains(&v) {
            Ok(())
        } else {
            Err(Error::msg(format!(
                "Setting \"{name}\" is out of range: {v} (expected {min} to {max})"
            )))
        }
    }
    if wanted("shortcut") && s.shortcut.trim().is_empty() {
        return Err(Error::msg("Setting \"shortcut\" must not be empty"));
    }
    if wanted("language") && !language_ok(&s.language) {
        return Err(Error::msg(
            "Setting \"language\" must be \"auto\" or a language code (letters, digits, - and _, at most 16 characters)",
        ));
    }
    if wanted("microphoneId") && s.microphone_id.trim().is_empty() {
        return Err(Error::msg("Setting \"microphoneId\" must not be empty"));
    }
    if wanted("pasteDelayMs") {
        range("pasteDelayMs", s.paste_delay_ms, 0, MAX_PASTE_DELAY_MS)?;
    }
    if wanted("restoreClipboardMs") {
        range(
            "restoreClipboardMs",
            s.restore_clipboard_ms,
            0,
            MAX_RESTORE_CLIPBOARD_MS,
        )?;
    }
    if wanted("maxDictationSec") {
        range(
            "maxDictationSec",
            s.max_dictation_sec,
            MIN_DICTATION_SEC,
            MAX_DICTATION_SEC,
        )?;
    }
    if wanted("historyLimit") {
        range(
            "historyLimit",
            s.history_limit as u64,
            MIN_HISTORY_LIMIT as u64,
            MAX_HISTORY_LIMIT as u64,
        )?;
    }
    if wanted("modelDir") && s.model_dir.as_ref().is_some_and(|d| !d.is_absolute()) {
        return Err(Error::msg("Setting \"modelDir\" must be an absolute path"));
    }
    Ok(())
}

fn language_ok(lang: &str) -> bool {
    !lang.trim().is_empty()
        && lang.len() <= MAX_LANGUAGE_LEN
        && lang
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Brings back within bounds whatever an old or hand-edited file may contain that is unusable.
fn normalize_loaded(s: &mut Settings, warn: &dyn Fn(&str)) {
    let d = Settings::default();
    if let Some(dir) = &s.model_dir {
        if dir.as_os_str().is_empty() {
            s.model_dir = None;
        } else if !dir.is_absolute() {
            warn("modelDir ignored: not an absolute path (old Windows path?)");
            s.model_dir = None;
        }
    }
    let clamp = |name: &str, v: &mut u64, min: u64, max: u64| {
        let c = (*v).clamp(min, max);
        if c != *v {
            warn(&format!("{name} out of range ({v}): brought back to {c}"));
            *v = c;
        }
    };
    clamp("pasteDelayMs", &mut s.paste_delay_ms, 0, MAX_PASTE_DELAY_MS);
    clamp(
        "restoreClipboardMs",
        &mut s.restore_clipboard_ms,
        0,
        MAX_RESTORE_CLIPBOARD_MS,
    );
    clamp(
        "maxDictationSec",
        &mut s.max_dictation_sec,
        MIN_DICTATION_SEC,
        MAX_DICTATION_SEC,
    );
    let mut limit = s.history_limit as u64;
    clamp(
        "historyLimit",
        &mut limit,
        MIN_HISTORY_LIMIT as u64,
        MAX_HISTORY_LIMIT as u64,
    );
    s.history_limit = limit as usize;
    if s.shortcut.trim().is_empty() {
        warn("shortcut is empty: default restored");
        s.shortcut = d.shortcut;
    }
    if !language_ok(&s.language) {
        warn("language is invalid: default restored");
        s.language = d.language;
    }
    if s.microphone_id.trim().is_empty() {
        warn("microphoneId is empty: default restored");
        s.microphone_id = d.microphone_id;
    }
}

/// Repairs `settings.json` before the typed load. Returns true if the file is unreadable
/// (the `Store` then makes a `.corrupt` copy) and must be rewritten with the defaults.
///
/// - `modelCache` (old name) becomes `modelDir`: a file that contained both keys would make
///   the serde alias fail (duplicate field).
/// - A mistyped field is removed (it falls back to its default value), the others are kept,
///   like a `{...defaults, ...data}` merge; the original is first copied to `.corrupt`.
fn repair_file(path: &Path, warn: &dyn Fn(&str)) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return true;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return false;
    }
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(text) else {
        return true;
    };

    let mut changed = false;
    if let Some(old) = obj.remove("modelCache") {
        changed = true;
        if !obj.contains_key("modelDir") && old.as_str().is_some_and(|s| !s.is_empty()) {
            obj.insert("modelDir".into(), old);
        }
    }
    let mut dropped = Vec::new();
    if serde_json::from_value::<Settings>(Value::Object(obj.clone())).is_err() {
        if let Ok(Value::Object(defaults)) = serde_json::to_value(Settings::default()) {
            for (k, v) in obj.clone() {
                let mut probe = defaults.clone();
                probe.insert(k.clone(), v);
                if serde_json::from_value::<Settings>(Value::Object(probe)).is_err() {
                    obj.remove(&k);
                    dropped.push(k);
                }
            }
        }
    }
    if !dropped.is_empty() {
        changed = true;
        warn(&format!(
            "invalid settings ignored (defaults used): {}",
            dropped.join(", ")
        ));
        match backup_corrupt(path) {
            Ok(Some(b)) => warn(&format!("original settings kept in {}", b.display())),
            Ok(None) => {}
            Err(e) => warn(&format!("settings backup failed: {e}")),
        }
    }
    if changed {
        let result = serde_json::to_string_pretty(&Value::Object(obj))
            .map_err(std::io::Error::other)
            .and_then(|t| write_atomic(path, t.as_bytes(), FileMode::Private));
        if let Err(e) = result {
            warn(&format!("cannot repair {}: {e}", path.display()));
        }
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn open(dir: &Path) -> SettingsStore {
        SettingsStore::open_silent(dir.join("settings.json"))
    }

    #[test]
    fn first_start_writes_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cfg/settings.json");
        let s = SettingsStore::open_silent(&p);
        assert_eq!(s.get(), Settings::default());
        let on_disk: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(on_disk["shortcut"], "Ctrl+Alt+Space");
        assert_eq!(on_disk["historyLimit"], 2000);
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn legacy_file_is_migrated() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(
            &p,
            "\u{feff}{\"shortcut\":\"Ctrl+Shift+D\",\"mode\":\"ptt\",\"language\":\"fr\",\"modelCache\":\"/data/models\",\"sidecarPort\":48123,\"customKey\":{\"a\":1}}",
        )
        .unwrap();
        let s = open(d.path()).get();
        assert_eq!(s.shortcut, "Ctrl+Shift+D");
        assert_eq!(s.language, "fr");
        assert_eq!(s.model_dir.as_deref(), Some(Path::new("/data/models")));
        assert_eq!(s.extra.get("sidecarPort"), Some(&json!(48123)));
        assert_eq!(s.extra.get("customKey"), Some(&json!({"a": 1})));
        assert!(s.auto_paste); // default for the absent key
        let text = fs::read_to_string(&p).unwrap();
        assert!(!text.contains("modelCache"));
        assert!(text.contains("\"modelDir\""));
        assert!(text.contains("sidecarPort"));
        // Stable reopening.
        assert_eq!(open(d.path()).get(), s);
    }

    #[test]
    fn legacy_empty_or_windows_model_cache_becomes_none() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, r#"{"modelCache":""}"#).unwrap();
        assert_eq!(open(d.path()).get().model_dir, None);
        fs::write(
            &p,
            r#"{"modelCache":"C:\\Users\\x\\AppData\\Local\\Dictation\\models"}"#,
        )
        .unwrap();
        let warned = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let w = warned.clone();
        let s = SettingsStore::open(&p, Arc::new(move |m| w.lock().unwrap().push(m.to_owned())));
        assert_eq!(s.get().model_dir, None);
        assert!(warned
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("modelDir")));
    }

    #[test]
    fn both_old_and_new_keys_do_not_break_loading() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("settings.json"),
            r#"{"modelCache":"/old","modelDir":"/new","language":"en"}"#,
        )
        .unwrap();
        let s = open(d.path()).get();
        assert_eq!(s.model_dir.as_deref(), Some(Path::new("/new")));
        assert_eq!(s.language, "en");
    }

    #[test]
    fn corrupt_file_gives_defaults_and_is_rewritten() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("settings.json"), "{oops").unwrap();
        assert_eq!(open(d.path()).get(), Settings::default());
    }

    #[test]
    fn patch_merges_shallowly_and_persists() {
        let d = tempfile::tempdir().unwrap();
        let s = open(d.path());
        let out = s
            .patch(json!({"language": "fr", "mode": "ptt", "myNewKey": 3}))
            .unwrap();
        assert_eq!(out.language, "fr");
        assert_eq!(out.mode, crate::domain::ShortcutMode::Ptt);
        assert_eq!(out.extra.get("myNewKey"), Some(&json!(3)));
        assert_eq!(out.shortcut, "Ctrl+Alt+Space");
        assert_eq!(open(d.path()).get(), out);
    }

    #[test]
    fn patch_accepts_legacy_model_cache_and_empty_resets() {
        let d = tempfile::tempdir().unwrap();
        let s = open(d.path());
        assert_eq!(
            s.patch(json!({"modelCache": "/m"}))
                .unwrap()
                .model_dir
                .as_deref(),
            Some(Path::new("/m"))
        );
        assert_eq!(s.patch(json!({"modelDir": ""})).unwrap().model_dir, None);
        assert_eq!(
            s.patch(json!({"modelDir": "/n"}))
                .unwrap()
                .model_dir
                .as_deref(),
            Some(Path::new("/n"))
        );
        assert_eq!(s.patch(json!({"modelDir": null})).unwrap().model_dir, None);
    }

    fn assert_rejected(s: &SettingsStore, patch: Value, needle: &str) {
        let before = s.get();
        let disk = fs::read(s.path()).unwrap();
        let err = s.patch(patch.clone()).unwrap_err().to_string();
        assert!(err.contains(needle), "{patch}: {err}");
        assert_eq!(s.get(), before, "memory modified by {patch}");
        assert_eq!(
            fs::read(s.path()).unwrap(),
            disk,
            "file modified by {patch}"
        );
    }

    #[test]
    fn invalid_patches_have_no_partial_effect() {
        let d = tempfile::tempdir().unwrap();
        let s = open(d.path());
        // A valid value placed before the invalid one must not be applied.
        assert_rejected(&s, json!({"language": "fr", "mode": "voix"}), "mode");
        assert_rejected(&s, json!({"language": ""}), "language");
        assert_rejected(&s, json!({"language": "   "}), "language");
        assert_rejected(&s, json!({"language": "fr FR"}), "language");
        assert_rejected(&s, json!({"shortcut": " "}), "shortcut");
        assert_rejected(&s, json!({"microphoneId": ""}), "microphoneId");
        assert_rejected(&s, json!({"pasteDelayMs": 999_999}), "pasteDelayMs");
        assert_rejected(
            &s,
            json!({"restoreClipboardMs": 10_000_000}),
            "restoreClipboardMs",
        );
        assert_rejected(&s, json!({"maxDictationSec": 0}), "maxDictationSec");
        assert_rejected(&s, json!({"maxDictationSec": 100_000}), "maxDictationSec");
        assert_rejected(&s, json!({"historyLimit": 0}), "historyLimit");
        assert_rejected(&s, json!({"historyLimit": 10_000_000}), "historyLimit");
        assert_rejected(&s, json!({"historyLimit": -5}), "historyLimit");
        assert_rejected(&s, json!({"pasteDelayMs": "vite"}), "pasteDelayMs");
        assert_rejected(&s, json!({"autoPaste": "oui"}), "autoPaste");
        assert_rejected(&s, json!({"modelDir": "relatif/chemin"}), "modelDir");
        assert_rejected(&s, json!(["pas", "un", "objet"]), "JSON object");
        assert_rejected(&s, json!(42), "JSON object");
    }

    #[test]
    fn out_of_range_file_values_do_not_block_later_patches() {
        // Regression case: an old/hand-edited file went out of bounds and
        // no setting could be modified any more.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(
            &p,
            r#"{"shortcut":"F9","maxDictationSec":7200,"pasteDelayMs":999999,"language":"fr"}"#,
        )
        .unwrap();
        let s = open(d.path());
        assert_eq!(s.get().shortcut, "F9");
        assert_eq!(s.get().max_dictation_sec, MAX_DICTATION_SEC); // brought back within bounds
        let after = s.patch(json!({"autoPaste": false})).unwrap();
        assert!(!after.auto_paste);
        assert_eq!(after.shortcut, "F9");
    }

    #[test]
    fn one_badly_typed_field_keeps_the_other_settings() {
        // Regression case: a single mistyped field reset ALL the settings.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let original = r#"{"shortcut":"F9","pasteDelayMs":-5,"language":"fr"}"#;
        fs::write(&p, original).unwrap();
        let s = open(d.path());
        let got = s.get();
        assert_eq!(got.shortcut, "F9");
        assert_eq!(got.language, "fr");
        assert_eq!(got.paste_delay_ms, Settings::default().paste_delay_ms); // champ fautif seul
                                                                            // The original file is kept before the repair.
        let backup = fs::read_to_string(d.path().join("settings.json.corrupt")).unwrap();
        assert_eq!(backup, original);
    }

    #[test]
    fn mic_boost_is_automatic_by_default_and_a_bad_value_does_not_reset_the_rest() {
        use crate::domain::MicBoost;
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("settings.json"),
            r#"{"shortcut":"F9","micBoost":"loud"}"#,
        )
        .unwrap();
        let s = open(d.path());
        assert_eq!(s.get().shortcut, "F9");
        assert_eq!(s.get().mic_boost, MicBoost::Auto);
        assert_eq!(Settings::default().mic_boost, MicBoost::Auto);
        // the user can turn it off, and the choice is kept across restarts
        let out = s.patch(json!({"micBoost": "off"})).unwrap();
        assert_eq!(out.mic_boost, MicBoost::Off);
        assert_eq!(open(d.path()).get().mic_boost, MicBoost::Off);
        assert!(s.patch(json!({"micBoost": "enormous"})).is_err());
        assert_eq!(s.get().mic_boost, MicBoost::Off);
    }

    #[test]
    fn boundary_values_accepted() {
        let d = tempfile::tempdir().unwrap();
        let s = open(d.path());
        let out = s
            .patch(json!({"historyLimit": 1, "maxDictationSec": 3600, "pasteDelayMs": 0, "language": "auto"}))
            .unwrap();
        assert_eq!(
            (out.history_limit, out.max_dictation_sec, out.paste_delay_ms),
            (1, 3600, 0)
        );
        s.patch(json!({"historyLimit": 100_000, "maxDictationSec": 1, "language": "zh-TW"}))
            .unwrap();
    }

    #[test]
    fn replace_validates() {
        let d = tempfile::tempdir().unwrap();
        let s = open(d.path());
        let bad = Settings {
            language: String::new(),
            ..Settings::default()
        };
        assert!(s.replace(bad).is_err());
        assert_eq!(s.get(), Settings::default());
    }
}
