//! Dictionary and snippets: `dictionary.json` / `snippets.json` files (0600), rules of
//! the dictionary and the snippets.

use linuxflow_core::domain::{DictionaryEntry, Snippet};
use linuxflow_core::error::{Error, Result};
use linuxflow_core::store::{FileMode, Store, WarnFn};
use linuxflow_core::vocab;
use linuxflow_i18n::tr;
use serde_json::Value;
use std::path::PathBuf;

/// Personal dictionary. The file is read tolerantly (old format: a list of strings).
pub struct DictionaryStore {
    store: Store<Value>,
}

impl DictionaryStore {
    pub fn open(path: impl Into<PathBuf>, warn: WarnFn) -> Self {
        Self {
            store: Store::<Value>::open_with(path, FileMode::Private, warn),
        }
    }

    pub fn list(&self) -> Vec<DictionaryEntry> {
        vocab::normalize_value(&self.store.get())
    }

    /// ATOMIC read-modify-write: the store lock is held during the whole
    /// modification, so two simultaneous modifications are not lost.
    fn modify(&self, change: impl FnOnce(&mut Vec<DictionaryEntry>) -> Result<()>) -> Result<()> {
        self.store.try_update(|value| {
            let mut list = vocab::normalize_value(value);
            change(&mut list)?;
            *value = serde_json::to_value(vocab::normalize_vocab(&list))?;
            Ok(())
        })
    }

    /// Rewrites the file in the current format if it is in the old format or not normalised.
    pub fn migrate(&self) -> Result<()> {
        if !self.store.exists() {
            return Ok(());
        }
        let current = self.store.get();
        let normalized = serde_json::to_value(self.list())?;
        if current != normalized {
            self.store.set(normalized)?;
        }
        Ok(())
    }

    fn position(list: &[DictionaryEntry], word: &str) -> Option<usize> {
        let key = word.trim().to_lowercase();
        list.iter().position(|e| e.word.to_lowercase() == key)
    }

    /// Adds a word; if it already exists (case ignored), merges the sounds.
    pub fn add(&self, word: &str, sounds_csv: &str) -> Result<()> {
        let word = word.trim();
        if word.is_empty() {
            return Err(Error::msg(tr("Enter a word first.")));
        }
        let sounds = vocab::parse_sounds(sounds_csv);
        self.modify(|list| {
            match Self::position(list, word) {
                Some(i) => {
                    let merged = vocab::merge_sounds(&list[i].sounds, &sounds);
                    list[i].sounds = merged;
                }
                None => list.push(DictionaryEntry {
                    word: word.to_string(),
                    sounds,
                }),
            }
            Ok(())
        })
    }

    /// Replaces `previous` with the new word and its sounds (adds it if `previous` is unknown).
    pub fn update(&self, previous: &str, word: &str, sounds_csv: &str) -> Result<()> {
        let word = word.trim();
        if word.is_empty() {
            return Err(Error::msg(tr("Enter a word first.")));
        }
        let sounds = vocab::parse_sounds(sounds_csv);
        let key = if previous.trim().is_empty() {
            word
        } else {
            previous
        };
        self.modify(|list| {
            match Self::position(list, key) {
                Some(i) => {
                    list[i] = DictionaryEntry {
                        word: word.to_string(),
                        sounds,
                    };
                }
                None => list.push(DictionaryEntry {
                    word: word.to_string(),
                    sounds,
                }),
            }
            Ok(())
        })
    }

    /// Removes a word (exact comparison).
    pub fn remove(&self, word: &str) -> Result<()> {
        let word = word.trim();
        self.modify(|list| {
            list.retain(|e| e.word != word);
            Ok(())
        })
    }

    /// Adds the missing default words.
    pub fn restore_defaults(&self) -> Result<()> {
        self.modify(|list| {
            for d in vocab::normalize_vocab(&vocab::default_vocab()) {
                if Self::position(list, &d.word).is_none() {
                    list.push(d);
                }
            }
            Ok(())
        })
    }
}

/// Text shortcuts (snippets).
pub struct SnippetStore {
    store: Store<Vec<Snippet>>,
}

impl SnippetStore {
    pub fn open(path: impl Into<PathBuf>, warn: WarnFn) -> Self {
        Self {
            store: Store::<Vec<Snippet>>::open_with(path, FileMode::Private, warn),
        }
    }

    pub fn list(&self) -> Vec<Snippet> {
        self.store.get()
    }

    /// Adds a snippet; identifier = time in base 36 (made unique if already taken).
    pub fn add(&self, trigger: &str, expansion: &str, now_ms: u64) -> Result<()> {
        let trigger = trigger.trim();
        if trigger.is_empty() {
            return Err(Error::msg(tr("Enter a trigger first.")));
        }
        let mut id = base36(now_ms);
        let taken = |id: &str, list: &[Snippet]| list.iter().any(|s| s.id == id);
        self.store.update(|list| {
            while taken(&id, list) {
                id.push('x');
            }
            list.push(Snippet {
                id: id.clone(),
                trigger: trigger.to_string(),
                expansion: expansion.to_string(),
            });
        })
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.store.update(|list| list.retain(|s| s.id != id))
    }
}

/// Lower-case base 36 integer (equivalent of `Date.now().toString(36)`).
pub fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(char::from_digit((n % 36) as u32, 36).unwrap_or('0'));
        n /= 36;
    }
    digits.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dict() -> (tempfile::TempDir, DictionaryStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = DictionaryStore::open(dir.path().join("dictionary.json"), Arc::new(|_| {}));
        (dir, s)
    }

    #[test]
    fn concurrent_adds_are_never_lost() {
        let (_d, s) = dict();
        let s = Arc::new(s);
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let s = s.clone();
                std::thread::spawn(move || s.add(&format!("word{i}"), "").unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let mut words: Vec<String> = s.list().into_iter().map(|e| e.word).collect();
        words.sort();
        assert_eq!(words.len(), 16, "ajouts perdus : {words:?}");
    }

    #[test]
    fn unreadable_dictionary_is_backed_up_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(&path, "{ not json").unwrap();
        let s = DictionaryStore::open(&path, Arc::new(|_| {}));
        s.add("Kubernetes", "").unwrap();
        let saved = std::fs::read_to_string(dir.path().join("dictionary.json.corrupt")).unwrap();
        assert_eq!(saved, "{ not json");
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn add_merges_sounds_case_insensitively() {
        let (_d, s) = dict();
        s.add("Docker", "dock her").unwrap();
        s.add("docker", "doc ker; dock her").unwrap();
        let l = s.list();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].word, "Docker");
        assert_eq!(l[0].sounds, vec!["dock her", "doc ker"]);
    }

    #[test]
    fn add_rejects_empty_word() {
        let (_d, s) = dict();
        assert!(s.add("  ", "x").is_err());
    }

    #[test]
    fn update_replaces_previous() {
        let (_d, s) = dict();
        s.add("Kube", "cube").unwrap();
        s.update("kube", "Kubernetes", "cooper nettys, cube a")
            .unwrap();
        let l = s.list();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].word, "Kubernetes");
        assert_eq!(l[0].sounds, vec!["cooper nettys", "cube a"]);
        // Unknown previous word: addition.
        s.update("nope", "Other", "").unwrap();
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn remove_and_restore_defaults() {
        let (_d, s) = dict();
        s.add("A", "").unwrap();
        s.add("B", "").unwrap();
        s.remove("A").unwrap();
        assert_eq!(s.list().len(), 1);
        s.restore_defaults().unwrap();
        // The default dictionary is empty: nothing is added or lost.
        assert_eq!(s.list().len(), vocab::default_vocab().len() + 1);
    }

    #[test]
    fn legacy_string_list_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(&path, r#"["Alpha","beta","ALPHA"]"#).unwrap();
        let s = DictionaryStore::open(&path, Arc::new(|_| {}));
        assert_eq!(s.list().len(), 2);
        s.migrate().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"word\""));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn snippets_add_remove() {
        let dir = tempfile::tempdir().unwrap();
        let s = SnippetStore::open(dir.path().join("snippets.json"), Arc::new(|_| {}));
        assert!(s.add("   ", "x", 1).is_err());
        s.add("  my sig ", "Best,\nMe", 1_700_000_000_000).unwrap();
        s.add("other", "y", 1_700_000_000_000).unwrap();
        let l = s.list();
        assert_eq!(l[0].trigger, "my sig");
        assert_eq!(l[0].id, base36(1_700_000_000_000));
        assert_ne!(l[0].id, l[1].id);
        s.remove(&l[0].id).unwrap();
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn base36_matches_js() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }
}
