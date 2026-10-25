//! Dictation history (`history.json`, 0600, most recent first).
//!
//! Entries are kept as raw JSON in the file: an entry in an old format
//! is never lost because of another one, and is only decoded when read.

use crate::constants::{HISTORY_MAX, HISTORY_TO_UI};
use crate::domain::{HistoryEntry, Settings};
use crate::error::Result;
use crate::store::{FileMode, Store, WarnFn};
use serde_json::Value;
use std::path::{Path, PathBuf};

const BASE36: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(BASE36[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Identifier: base36 of the time (ms) + 4 random base36 characters.
pub fn new_id(now_ms: u64) -> String {
    new_id_with(now_ms, &mut fastrand::Rng::new())
}

pub fn new_id_with(now_ms: u64, rng: &mut fastrand::Rng) -> String {
    let mut id = base36(now_ms);
    for _ in 0..4 {
        id.push(char::from(BASE36[rng.usize(..36)]));
    }
    id
}

/// Completes an old-format entry (missing fields, unknown `pasted`) then decodes it.
fn decode(mut v: Value) -> Option<HistoryEntry> {
    let obj = v.as_object_mut()?;
    for k in ["text", "raw", "ts", "id"] {
        if !obj.get(k).is_some_and(Value::is_string) {
            if k == "id" || k == "ts" {
                return None;
            }
            obj.insert(k.into(), Value::String(String::new()));
        }
    }
    for k in ["words", "audioMs", "sttMs", "latencyMs"] {
        if !obj.get(k).is_some_and(Value::is_u64) {
            // An old history can contain non-integer milliseconds (e.g. 1234.6).
            let rounded = obj
                .get(k)
                .and_then(Value::as_f64)
                .filter(|f| f.is_finite() && *f >= 0.0)
                .map_or(0, |f| f.round() as u64);
            obj.insert(k.into(), rounded.into());
        }
    }
    // Old files also write `blocked`: the text had then stayed in the clipboard.
    let known = matches!(
        obj.get("pasted").and_then(Value::as_str),
        Some("pasted" | "hub" | "clipboard")
    );
    if !known {
        obj.insert("pasted".into(), "clipboard".into());
    }
    serde_json::from_value(v).ok()
}

pub struct HistoryStore {
    store: Store<Vec<Value>>,
    /// Already decoded entries (up to 2000): `state()` is called at 10 Hz during a download
    /// and must not decode the whole history again each time. Invalidated at every write.
    decoded: std::sync::Mutex<Option<std::sync::Arc<Vec<HistoryEntry>>>>,
}

impl HistoryStore {
    pub fn open(path: impl Into<PathBuf>, warn: WarnFn) -> Self {
        Self {
            store: Store::open_with(path, FileMode::Private, warn),
            decoded: std::sync::Mutex::new(None),
        }
    }

    pub fn open_silent(path: impl Into<PathBuf>) -> Self {
        Self {
            store: Store::open(path, FileMode::Private),
            decoded: std::sync::Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        self.store.path()
    }

    pub fn len(&self) -> usize {
        self.store.get().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Adds at the head and truncates to `history_limit`. Returns `false` (writing nothing) if
    /// the history is disabled.
    pub fn add(&self, entry: HistoryEntry, settings: &Settings) -> Result<bool> {
        if !settings.history_enabled {
            return Ok(false);
        }
        let limit = if settings.history_limit == 0 {
            HISTORY_MAX
        } else {
            settings.history_limit
        };
        let value = serde_json::to_value(&entry)?;
        self.store.update(|list| {
            list.insert(0, value);
            list.truncate(limit);
        })?;
        self.invalidate();
        Ok(true)
    }

    /// Removes the entry with identifier `id`; `false` if it does not exist (nothing is written).
    pub fn delete(&self, id: &str) -> Result<bool> {
        let removed = self.store.try_update(|list| {
            let before = list.len();
            list.retain(|v| v.get("id").and_then(Value::as_str) != Some(id));
            Ok(list.len() != before)
        })?;
        self.invalidate();
        Ok(removed)
    }

    pub fn clear(&self) -> Result<()> {
        self.store.set(Vec::new())?;
        self.invalidate();
        Ok(())
    }

    fn invalidate(&self) {
        if let Ok(mut cache) = self.decoded.lock() {
            *cache = None;
        }
    }

    /// Re-applies a lower limit (setting change).
    pub fn enforce_limit(&self, limit: usize) -> Result<()> {
        if self.len() > limit {
            self.store.update(|list| list.truncate(limit))?;
            self.invalidate();
        }
        Ok(())
    }

    /// All readable entries, most recent first (unreadable entry ignored), decoded once
    /// as long as the history does not change.
    pub fn snapshot(&self) -> std::sync::Arc<Vec<HistoryEntry>> {
        if let Ok(cache) = self.decoded.lock() {
            if let Some(entries) = cache.as_ref() {
                return entries.clone();
            }
        }
        let entries = std::sync::Arc::new(
            self.store
                .get()
                .into_iter()
                .filter_map(decode)
                .collect::<Vec<_>>(),
        );
        if let Ok(mut cache) = self.decoded.lock() {
            *cache = Some(entries.clone());
        }
        entries
    }

    /// All readable entries, most recent first (unreadable entry ignored).
    pub fn all(&self) -> Vec<HistoryEntry> {
        self.snapshot().as_ref().clone()
    }

    /// At most `HISTORY_TO_UI` entries, most recent first.
    pub fn list_for_ui(&self) -> Vec<HistoryEntry> {
        self.snapshot()
            .iter()
            .take(HISTORY_TO_UI)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::PasteDisposition;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn entry(id: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            ts: "2026-10-24T10:00:00.000Z".into(),
            text: format!("texte {id}"),
            raw: "brut".into(),
            words: 2,
            audio_ms: 1500,
            stt_ms: 300,
            latency_ms: 450,
            language: Some("fr".into()),
            app: None,
            pasted: PasteDisposition::Pasted,
        }
    }

    fn store(d: &Path) -> HistoryStore {
        HistoryStore::open_silent(d.join("history.json"))
    }

    #[test]
    fn non_integer_milliseconds_from_an_old_history_are_rounded() {
        let e = decode(serde_json::json!({
            "id":"x","ts":"2026-01-01T00:00:00.000Z","text":"a","raw":"a",
            "words":1,"audioMs":1234.6,"sttMs":50.4,"latencyMs":-3.0,"pasted":"pasted"
        }))
        .unwrap();
        assert_eq!((e.audio_ms, e.stt_ms, e.latency_ms), (1235, 50, 0));
    }

    #[test]
    fn add_puts_newest_first_and_persists_0600() {
        let d = tempfile::tempdir().unwrap();
        let h = store(d.path());
        let s = Settings::default();
        assert!(h.add(entry("a"), &s).unwrap());
        assert!(h.add(entry("b"), &s).unwrap());
        let ids: Vec<_> = h.all().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["b", "a"]);
        let p = d.path().join("history.json");
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(store(d.path()).all().len(), 2);
        assert_eq!(store(d.path()).all()[0], {
            let mut e = entry("b");
            e.pasted = PasteDisposition::Pasted;
            e
        });
    }

    #[test]
    fn disabled_history_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let h = store(d.path());
        let s = Settings {
            history_enabled: false,
            ..Settings::default()
        };
        assert!(!h.add(entry("a"), &s).unwrap());
        assert!(h.is_empty());
        assert!(!d.path().join("history.json").exists());
    }

    #[test]
    fn truncates_to_limit() {
        let d = tempfile::tempdir().unwrap();
        let h = store(d.path());
        let s = Settings {
            history_limit: 3,
            ..Settings::default()
        };
        for i in 0..5 {
            h.add(entry(&i.to_string()), &s).unwrap();
        }
        let ids: Vec<_> = h.all().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["4", "3", "2"]);
        h.enforce_limit(1).unwrap();
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn default_limit_is_2000() {
        let d = tempfile::tempdir().unwrap();
        let h = store(d.path());
        let existing: Vec<Value> = (0..2005)
            .map(|i| serde_json::to_value(entry(&i.to_string())).unwrap())
            .collect();
        h.store.set(existing).unwrap();
        h.add(entry("new"), &Settings::default()).unwrap();
        assert_eq!(h.len(), 2000);
        assert_eq!(h.all()[0].id, "new");
    }

    #[test]
    fn delete_and_clear() {
        let d = tempfile::tempdir().unwrap();
        let h = store(d.path());
        let s = Settings::default();
        for id in ["a", "b", "c"] {
            h.add(entry(id), &s).unwrap();
        }
        assert!(h.delete("b").unwrap());
        assert!(!h.delete("zzz").unwrap());
        let ids: Vec<_> = h.all().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["c", "a"]);
        h.clear().unwrap();
        assert!(h.all().is_empty());
        assert_eq!(
            fs::read_to_string(d.path().join("history.json")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn list_for_ui_caps_at_300_newest_first() {
        let d = tempfile::tempdir().unwrap();
        let h = store(d.path());
        let existing: Vec<Value> = (0..350)
            .rev()
            .map(|i| serde_json::to_value(entry(&i.to_string())).unwrap())
            .collect();
        h.store.set(existing).unwrap();
        let ui = h.list_for_ui();
        assert_eq!(ui.len(), HISTORY_TO_UI);
        assert_eq!(ui[0].id, "349");
        assert_eq!(h.all().len(), 350);
    }

    #[test]
    fn legacy_history_is_read_without_losing_entries() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("history.json"),
            r#"[{"id":"x1","ts":"2026-01-01T00:00:00.000Z","text":"a b","raw":"a b","words":2,"audioMs":1000,"sttMs":50,"latencyMs":80,"language":"fr","app":"firefox","pasted":"blocked"},
                {"id":"x2","ts":"2026-01-01T00:00:00.000Z","text":"c","words":1,"pasted":"hub","language":null,"app":null},
                {"not":"an entry"}]"#,
        )
        .unwrap();
        let h = store(d.path());
        let all = h.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].pasted, PasteDisposition::Clipboard);
        assert_eq!(all[1].audio_ms, 0);
        assert_eq!(all[1].raw, "");
        assert_eq!(h.len(), 3); // the unreadable entry stays in the file
    }

    #[test]
    fn decoded_entries_are_cached_and_invalidated_by_every_write() {
        let d = tempfile::tempdir().unwrap();
        let h = store(d.path());
        let settings = Settings::default();
        assert!(h.snapshot().is_empty());
        h.add(entry("a"), &settings).unwrap();
        let first = h.snapshot();
        let second = h.snapshot();
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "no re-decoding without a write"
        );
        h.add(entry("b"), &settings).unwrap();
        assert_eq!(h.snapshot().len(), 2);
        assert_eq!(h.snapshot()[0].id, "b");
        h.delete("a").unwrap();
        assert_eq!(h.snapshot().len(), 1);
        h.enforce_limit(0).unwrap();
        assert!(h.snapshot().is_empty());
        h.add(entry("c"), &settings).unwrap();
        h.clear().unwrap();
        assert!(h.snapshot().is_empty());
    }

    #[test]
    fn new_id_format() {
        let id = new_id(1_761_300_000_000);
        assert_eq!(&id[..id.len() - 4], base36(1_761_300_000_000));
        assert_eq!(id.len(), 8 + 4);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()));
        let mut a = fastrand::Rng::with_seed(7);
        let mut b = fastrand::Rng::with_seed(7);
        assert_eq!(new_id_with(5, &mut a), new_id_with(5, &mut b));
        assert_eq!(&new_id_with(35, &mut a)[..1], "z");
        assert_eq!(&new_id_with(0, &mut a)[..1], "0");
        assert_eq!(&new_id_with(36, &mut a)[..2], "10");
        assert_ne!(new_id(1), new_id(1)); // 36^4 combinaisons : collision improbable
    }
}
