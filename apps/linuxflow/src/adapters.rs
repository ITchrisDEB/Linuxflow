//! Adapters between the application stores and the pipeline traits.

use crate::library::{DictionaryStore, SnippetStore};
use linuxflow_core::domain::HistoryEntry;
use linuxflow_core::error::Result;
use linuxflow_core::format::{count_words, format_text, FormatOptions};
use linuxflow_core::history::HistoryStore;
use linuxflow_core::pipeline::{DictionaryHints, HistorySink, TextFormatter};
use linuxflow_core::settings::SettingsStore;
use linuxflow_core::traits::Clipboard;
use std::sync::{Arc, Mutex};

/// Formatting with the current dictionary and snippets.
pub struct AppFormatter {
    pub dictionary: Arc<DictionaryStore>,
    pub snippets: Arc<SnippetStore>,
}

impl TextFormatter for AppFormatter {
    fn format(&self, raw: &str, language: &str) -> String {
        let dictionary = self.dictionary.list();
        let snippets = self.snippets.list();
        format_text(
            raw,
            &FormatOptions {
                language,
                snippets: &snippets,
                dictionary: &dictionary,
            },
        )
    }

    fn count_words(&self, text: &str) -> u32 {
        count_words(text)
    }
}

/// Hints for the engine: the dictionary words.
pub struct StoreHints(pub Arc<DictionaryStore>);

impl DictionaryHints for StoreHints {
    fn hints(&self) -> Vec<String> {
        self.0.list().into_iter().map(|e| e.word).collect()
    }
}

/// Real history (honours `historyEnabled` / `historyLimit`).
pub struct StoreHistory {
    pub history: Arc<HistoryStore>,
    pub settings: Arc<SettingsStore>,
}

impl HistorySink for StoreHistory {
    fn add(&self, entry: HistoryEntry) {
        if let Err(e) = self.history.add(entry, &self.settings.get()) {
            tracing::warn!("history write failed: {e}");
        }
    }
}

/// History that writes nothing (test tools).
pub struct NoHistory;

impl HistorySink for NoHistory {
    fn add(&self, _entry: HistoryEntry) {}
}

/// In-memory clipboard: the test tools never touch the real one.
#[derive(Default)]
pub struct MemoryClipboard(Mutex<Option<String>>);

impl Clipboard for MemoryClipboard {
    fn get_text(&self) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
    fn set_text(&self, text: &str) -> Result<()> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.to_string());
        Ok(())
    }
}

/// Paste unavailable (no injector could be built): the text stays copied.
pub struct UnavailablePaster(pub String);

impl linuxflow_core::traits::Paster for UnavailablePaster {
    fn paste(
        &self,
        _mode: linuxflow_core::domain::PasteMode,
        _delay_ms: u64,
    ) -> linuxflow_core::domain::PasteResult {
        linuxflow_core::domain::PasteResult::Failed(self.0.clone())
    }
}

/// No active window (test tools).
pub struct NoopWindow;

impl linuxflow_core::traits::ActiveWindow for NoopWindow {
    fn current(&self) -> Option<linuxflow_core::domain::WindowInfo> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn formatter_uses_dictionary_and_snippets() {
        let dir = tempfile::tempdir().unwrap();
        let warn: linuxflow_core::store::WarnFn = Arc::new(|_| {});
        let d = Arc::new(DictionaryStore::open(
            dir.path().join("d.json"),
            warn.clone(),
        ));
        let s = Arc::new(SnippetStore::open(dir.path().join("s.json"), warn));
        d.add("Kubernetes", "cooper nettys").unwrap();
        s.add("sig", "Best regards", 1).unwrap();
        let f = AppFormatter {
            dictionary: d.clone(),
            snippets: s,
        };
        assert_eq!(
            f.format("euh i use cooper nettys sig", "en"),
            "I use Kubernetes Best regards"
        );
        assert_eq!(f.count_words("l'été est là"), 3);
        assert_eq!(StoreHints(d).hints(), vec!["Kubernetes"]);
    }

    #[test]
    fn memory_clipboard_roundtrip() {
        let c = MemoryClipboard::default();
        assert_eq!(c.get_text().unwrap(), None);
        c.set_text("x").unwrap();
        assert_eq!(c.get_text().unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn history_adapter_respects_settings() {
        let dir = tempfile::tempdir().unwrap();
        let warn: linuxflow_core::store::WarnFn = Arc::new(|_| {});
        let settings = Arc::new(SettingsStore::open(dir.path().join("s.json"), warn.clone()));
        let history = Arc::new(HistoryStore::open(dir.path().join("h.json"), warn));
        let sink = StoreHistory {
            history: history.clone(),
            settings: settings.clone(),
        };
        let e = HistoryEntry {
            id: "a".into(),
            ts: "2020-01-01T00:00:00Z".into(),
            text: "t".into(),
            raw: "t".into(),
            words: 1,
            audio_ms: 1,
            stt_ms: 1,
            latency_ms: 1,
            language: None,
            app: None,
            pasted: linuxflow_core::domain::PasteDisposition::Clipboard,
        };
        sink.add(e.clone());
        assert_eq!(history.len(), 1);
        settings
            .patch(serde_json::json!({"historyEnabled": false}))
            .unwrap();
        sink.add(e);
        assert_eq!(history.len(), 1);
    }
}
