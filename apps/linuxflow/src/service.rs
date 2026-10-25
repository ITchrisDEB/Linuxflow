//! `AppService`: implementation of `linuxflow_core::traits::Controller`.
//!
//! Side effects on the system (global shortcut, launch at login,
//! folders, interface clipboard...) go through the [`System`] trait, simulated in the tests.

use crate::engine_ctl::EngineController;
use crate::library::{DictionaryStore, SnippetStore};
use crate::models::{ModelsEvent, ModelsService, VAD_ID};
use crate::runtime::{Dictator, EventSink};
use linuxflow_core::accelerator::{self, AcceleratorError};
use linuxflow_core::domain::*;
use linuxflow_core::error::{Error, Result};
use linuxflow_core::events::Event;
use linuxflow_core::format::{format_text, FormatOptions};
use linuxflow_core::history::HistoryStore;
use linuxflow_core::settings::SettingsStore;
use linuxflow_core::stats::compute_stats;
use linuxflow_core::traits::{Clock, Controller};
use linuxflow_i18n::{tr, tr_fmt};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

/// Known folders that the application can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    Logs,
    Data,
    Models,
}

/// State of the global shortcut.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShortcutInfo {
    pub ok: bool,
    /// Key actually assigned when the desktop chooses it (portal): it may
    /// differ from the requested one. `None` for the other mechanisms.
    pub desktop_trigger: Option<String>,
    /// Message of the shortcut failure (named conflict, missing service…).
    pub message: Option<String>,
    /// The backend reports the release (real push-to-talk possible).
    pub ptt_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// Key already taken by another application.
    Conflict,
    /// No shortcut mechanism: message already translated.
    Unavailable(String),
}

pub trait System: Send + Sync {
    /// (Re)starts the global shortcut with these settings (asynchronous).
    fn apply_shortcut(&self, settings: &Settings);
    fn suspend_shortcut(&self, suspended: bool);
    fn shortcut_status(&self) -> ShortcutInfo;
    /// Can the shortcut be taken? (conflict, missing backend)
    fn probe_shortcut(
        &self,
        accelerator: &accelerator::Accelerator,
        mode: ShortcutMode,
    ) -> std::result::Result<(), ProbeError>;
    fn set_autostart(&self, enabled: bool) -> std::io::Result<()>;
    fn microphones(&self) -> Vec<MicrophoneInfo>;
    fn capabilities(&self) -> Capabilities;
    fn open_folder(&self, folder: Folder) -> std::result::Result<(), String>;
    /// Copy to the clipboard (explicit user action).
    fn copy_text(&self, text: &str) -> Result<()>;
    fn free_disk_bytes(&self, dir: &Path) -> Option<u64>;
    /// The interface language changed: the interface rebuilds itself.
    fn ui_language_changed(&self);
    fn request_quit(&self);
}

/// Models folder for a given `modelDir` setting.
pub type ModelsDirFn = Arc<dyn Fn(Option<&Path>) -> PathBuf + Send + Sync>;

pub struct ServiceDeps {
    pub version: String,
    pub display_name: Option<String>,
    pub settings: Arc<SettingsStore>,
    pub dictionary: Arc<DictionaryStore>,
    pub snippets: Arc<SnippetStore>,
    pub history: Arc<HistoryStore>,
    pub clock: Arc<dyn Clock>,
    pub dictator: Dictator,
    pub engine: Arc<EngineController>,
    pub models: Arc<ModelsService>,
    pub system: Arc<dyn System>,
    pub events: Arc<dyn EventSink>,
    /// Models folder for a given `modelDir` setting (environment variables resolved).
    pub models_dir_for: ModelsDirFn,
}

pub struct AppService {
    d: ServiceDeps,
    manual_suspend: AtomicBool,
    last_engine: Mutex<Option<EngineInfo>>,
    me: Weak<AppService>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Translated message for a shortcut error.
pub fn accelerator_error_text(e: &AcceleratorError) -> String {
    match e {
        AcceleratorError::Empty => tr("Press at least one key combination."),
        AcceleratorError::MultipleKeys => tr("Use only one regular key in a shortcut."),
        AcceleratorError::ModifiersOnlyNeedTwo => {
            tr("With modifier keys only, you need at least two (e.g. Ctrl + Super).")
        }
        AcceleratorError::NeedsModifier => {
            tr("Add Ctrl, Alt, Shift or Super (or use an F1 to F12 key).")
        }
        AcceleratorError::Reserved(a) => tr_fmt(
            "{} is used by the system or by most applications. Choose another shortcut.",
            &[a],
        ),
        AcceleratorError::KeyNotRecognized(k) => {
            tr_fmt("The key “{}” is not recognised in hold-to-talk mode.", &[k])
        }
    }
}

/// Normalises `uiLanguage` in a patch: "auto" or null = system language.
fn normalize_ui_language(patch: &mut Value) -> Result<()> {
    let Some(obj) = patch.as_object_mut() else {
        return Ok(());
    };
    let Some(v) = obj.get("uiLanguage").cloned() else {
        return Ok(());
    };
    match v.as_str() {
        None if v.is_null() => Ok(()),
        Some("auto") | Some("") => {
            obj.insert("uiLanguage".into(), Value::Null);
            Ok(())
        }
        Some("en") | Some("fr") => Ok(()),
        _ => Err(Error::msg(tr("Unsupported interface language."))),
    }
}

impl AppService {
    pub fn new(deps: ServiceDeps) -> Arc<Self> {
        let this = Arc::new_cyclic(|me| Self {
            d: deps,
            manual_suspend: AtomicBool::new(false),
            last_engine: Mutex::new(None),
            me: me.clone(),
        });
        let weak: Weak<Self> = Arc::downgrade(&this);
        this.d.models.set_listener(move |e| {
            if let Some(s) = weak.upgrade() {
                s.on_models_event(e);
            }
        });
        let weak = Arc::downgrade(&this);
        this.d.dictator.on_push_state(move || {
            if let Some(s) = weak.upgrade() {
                s.push_state();
            }
        });
        this
    }

    fn open(&self, folder: Folder) {
        if let Err(e) = self.d.system.open_folder(folder) {
            tracing::warn!("open folder failed: {e}");
            self.toast(e);
        }
    }

    pub fn settings(&self) -> Settings {
        self.d.settings.get()
    }

    pub fn dictator(&self) -> &Dictator {
        &self.d.dictator
    }

    pub fn engine(&self) -> &Arc<EngineController> {
        &self.d.engine
    }

    pub fn push_state(&self) {
        self.d.events.send(Event::State(Box::new(self.state())));
    }

    fn toast(&self, text: String) {
        self.d.events.send(Event::Toast(text));
    }

    /// Start-up: migrations, default dictionary, shortcut, launch at login,
    /// engine. Slow operations go to threads.
    pub fn startup(&self) {
        if let Err(e) = self.d.dictionary.migrate() {
            tracing::warn!("dictionary migration failed: {e}");
        }
        let s = self.settings();
        if !s.vocab_seeded {
            if let Err(e) = self.d.dictionary.restore_defaults() {
                tracing::warn!("default dictionary failed: {e}");
            }
            if let Err(e) = self.d.settings.patch(json!({"vocabSeeded": true})) {
                tracing::warn!("settings update failed: {e}");
            }
        }
        if let Err(e) = self.d.history.enforce_limit(s.history_limit) {
            tracing::warn!("history limit failed: {e}");
        }
        // The autostart file is rewritten at every start (path of the executable kept up to date).
        if let Err(e) = self.d.system.set_autostart(s.launch_at_login) {
            tracing::warn!("autostart update failed: {e}");
        }
        self.d.system.apply_shortcut(&s);
        self.d.system.suspend_shortcut(!s.shortcut_enabled);
        self.reload_engine_async(None);
        self.start_monitor();
    }

    /// Watches the engine state: passes it on to the dictation machine and to the views.
    pub fn start_monitor(&self) {
        let weak = self.me.clone();
        let spawned = std::thread::Builder::new()
            .name("linuxflow-engine-monitor".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                let Some(s) = weak.upgrade() else { return };
                s.poll_engine();
            });
        if let Err(e) = spawned {
            tracing::warn!("engine monitor could not start: {e}");
        }
    }

    /// One iteration of the watch (also called by the tests).
    pub fn poll_engine(&self) {
        use linuxflow_core::traits::SpeechToText;
        let info = self.d.engine.host().info();
        let changed = {
            let mut last = lock(&self.last_engine);
            let changed = last.as_ref() != Some(&info);
            if changed {
                *last = Some(info.clone());
            }
            changed
        };
        if changed {
            self.d.dictator.engine_status(info);
            self.push_state();
        }
    }

    /// Restarts the engine on a thread; `fallback`: model to restore if loading fails.
    fn reload_engine_async(&self, fallback: Option<String>) {
        let Some(this) = self.me.upgrade() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("linuxflow-engine-reload".into())
            .spawn(move || {
                this.d.engine.reload();
                this.push_state();
                let Some(prev) = fallback else { return };
                if let Err(e) = this.d.engine.wait_settled(Duration::from_secs(600)) {
                    tracing::warn!("model load failed: {e}");
                    if this.d.models.path_of(&prev).is_some() {
                        let _ = this.d.settings.patch(json!({"selectedModel": prev}));
                        this.d.engine.reload();
                        this.toast(tr(
                            "That model could not be loaded. The previous one is back.",
                        ));
                    } else {
                        this.toast(e);
                    }
                    this.push_state();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("engine reload thread failed: {e}");
        }
    }

    fn on_models_event(&self, e: ModelsEvent) {
        match e {
            ModelsEvent::Progress => self.push_state(),
            ModelsEvent::Notice(m) => {
                self.toast(m);
                self.push_state();
            }
            ModelsEvent::Adopted { id } => {
                // Local file chosen by the user: it becomes the model in use.
                let _ = self.d.settings.patch(json!({"selectedModel": id}));
                self.reload_engine_async(None);
                self.push_state();
            }
            ModelsEvent::Finished { id } => {
                let ready =
                    linuxflow_core::traits::SpeechToText::info(&**self.d.engine.host()).ready;
                let selected = self.settings().selected_model;
                // The VAD that has just arrived only counts when the engine reloads: without it,
                // silence could still produce invented text until the next start.
                if !ready || id == VAD_ID || selected.as_deref() == Some(id.as_str()) {
                    self.reload_engine_async(None);
                }
                self.push_state();
            }
        }
    }

    fn apply_side_effects(&self, before: &Settings, after: &Settings) {
        if before.shortcut != after.shortcut || before.mode != after.mode {
            self.d.system.apply_shortcut(after);
        }
        if before.shortcut_enabled != after.shortcut_enabled {
            self.d.system.suspend_shortcut(
                !after.shortcut_enabled || self.manual_suspend.load(Ordering::SeqCst),
            );
        }
        if before.launch_at_login != after.launch_at_login {
            if let Err(e) = self.d.system.set_autostart(after.launch_at_login) {
                tracing::warn!("autostart update failed: {e}");
                self.toast(tr("Could not change the launch-at-login setting."));
            }
        }
        if before.history_limit != after.history_limit {
            if let Err(e) = self.d.history.enforce_limit(after.history_limit) {
                tracing::warn!("history limit failed: {e}");
            }
        }
        if before.max_dictation_sec != after.max_dictation_sec {
            self.d
                .dictator
                .set_max_dictation_sec(after.max_dictation_sec);
        }
        if before.ui_language != after.ui_language {
            linuxflow_i18n::set_language(after.ui_language.as_deref());
            self.d.system.ui_language_changed();
        }
        let dir_changed = before.model_dir != after.model_dir;
        if dir_changed {
            self.d
                .models
                .set_dir((self.d.models_dir_for)(after.model_dir.as_deref()));
        }
        if dir_changed
            || before.selected_model != after.selected_model
            || before.device != after.device
        {
            self.reload_engine_async(None);
        }
    }
}

impl Controller for AppService {
    fn state(&self) -> AppState {
        let settings = self.d.settings.get();
        let all = self.d.history.snapshot();
        let stats = compute_stats(&all, &self.d.clock.today_local());
        let shortcut = self.d.system.shortcut_status();
        use linuxflow_core::traits::SpeechToText;
        AppState {
            version: self.d.version.clone(),
            user: self.d.display_name.clone(),
            dictionary: self.d.dictionary.list(),
            snippets: self.d.snippets.list(),
            history: self.d.history.list_for_ui(),
            stats,
            engine: self.d.engine.host().info(),
            ptt_available: shortcut.ptt_available,
            shortcut_ok: shortcut.ok,
            shortcut_desktop_trigger: shortcut.desktop_trigger.clone(),
            shortcut_message: shortcut.message.clone(),
            shortcut_suspended: !settings.shortcut_enabled
                || self.manual_suspend.load(Ordering::SeqCst),
            phase: self.d.dictator.phase(),
            settings,
        }
    }

    fn set_settings(&self, mut patch: Value) -> Result<()> {
        normalize_ui_language(&mut patch)?;
        let before = self.d.settings.get();
        let after = self.d.settings.patch(patch)?;
        self.apply_side_effects(&before, &after);
        self.push_state();
        Ok(())
    }

    fn shortcut_test(&self, text: &str) -> std::result::Result<(), String> {
        let acc = accelerator::parse(text).map_err(|e| accelerator_error_text(&e))?;
        let mode = self.d.settings.get().mode;
        accelerator::validate(&acc, mode).map_err(|e| accelerator_error_text(&e))?;
        let current = accelerator::parse(&self.d.settings.get().shortcut).ok();
        if current.as_ref() == Some(&acc) && self.d.system.shortcut_status().ok {
            return Ok(());
        }
        match self.d.system.probe_shortcut(&acc, mode) {
            Ok(()) => Ok(()),
            Err(ProbeError::Conflict) => Err(tr("Already used by another application.")),
            Err(ProbeError::Unavailable(m)) => Err(m),
        }
    }

    fn shortcut_suspend(&self, suspended: bool) {
        self.manual_suspend.store(suspended, Ordering::SeqCst);
        let enabled = self.d.settings.get().shortcut_enabled;
        self.d.system.suspend_shortcut(suspended || !enabled);
        self.push_state();
    }

    fn dictionary_add(&self, word: &str, sounds: &str) -> Result<()> {
        self.d.dictionary.add(word, sounds)?;
        self.push_state();
        Ok(())
    }

    fn dictionary_update(&self, previous: &str, word: &str, sounds: &str) -> Result<()> {
        self.d.dictionary.update(previous, word, sounds)?;
        self.push_state();
        Ok(())
    }

    fn dictionary_remove(&self, word: &str) -> Result<()> {
        self.d.dictionary.remove(word)?;
        self.push_state();
        Ok(())
    }

    fn dictionary_restore_defaults(&self) -> Result<()> {
        self.d.dictionary.restore_defaults()?;
        self.push_state();
        Ok(())
    }

    fn format_preview(&self, text: &str) -> String {
        let s = self.d.settings.get();
        let lang = if s.language.trim().to_lowercase().starts_with("en") {
            "en"
        } else {
            "fr"
        };
        let snippets = self.d.snippets.list();
        let dictionary = self.d.dictionary.list();
        format_text(
            text,
            &FormatOptions {
                language: lang,
                snippets: &snippets,
                dictionary: &dictionary,
            },
        )
    }

    fn snippet_add(&self, trigger: &str, expansion: &str) -> Result<()> {
        self.d
            .snippets
            .add(trigger, expansion, self.d.clock.now_ms())?;
        self.push_state();
        Ok(())
    }

    fn snippet_remove(&self, id: &str) -> Result<()> {
        self.d.snippets.remove(id)?;
        self.push_state();
        Ok(())
    }

    fn history_delete(&self, id: &str) -> Result<()> {
        self.d.history.delete(id)?;
        self.push_state();
        Ok(())
    }

    fn history_clear(&self) -> Result<()> {
        self.d.history.clear()?;
        self.push_state();
        Ok(())
    }

    fn dictation_toggle(&self, to_hub: bool) -> Phase {
        self.d.dictator.toggle(to_hub)
    }

    fn dictation_cancel(&self) -> Phase {
        self.d.dictator.cancel()
    }

    fn copy_text(&self, text: &str) -> Result<()> {
        self.d.system.copy_text(text)
    }

    fn open_logs_dir(&self) {
        self.open(Folder::Logs);
    }

    fn open_data_dir(&self) {
        self.open(Folder::Data);
    }

    fn open_models_dir(&self) {
        self.open(Folder::Models);
    }

    fn list_microphones(&self) -> Vec<MicrophoneInfo> {
        self.d.system.microphones()
    }

    fn capabilities(&self) -> Capabilities {
        self.d.system.capabilities()
    }

    fn models(&self) -> Vec<ModelStatus> {
        self.d.models.statuses()
    }

    fn model_download(&self, id: &str) -> Result<()> {
        self.d.models.download(id).map_err(Error::msg)?;
        self.push_state();
        Ok(())
    }

    fn model_pause(&self, id: &str) -> Result<()> {
        self.d.models.pause(id).map_err(Error::msg)
    }

    fn model_resume(&self, id: &str) -> Result<()> {
        self.d.models.resume(id).map_err(Error::msg)
    }

    fn model_cancel(&self, id: &str) -> Result<()> {
        self.d.models.cancel(id).map_err(Error::msg)
    }

    fn model_delete(&self, id: &str) -> Result<()> {
        self.d.models.delete(id).map_err(Error::msg)?;
        self.push_state();
        Ok(())
    }

    fn model_verify(&self, id: &str) -> Result<()> {
        self.d.models.verify(id).map_err(Error::msg)
    }

    fn models_dir(&self) -> PathBuf {
        self.d.models.dir()
    }

    fn free_disk_bytes(&self) -> Option<u64> {
        self.d.system.free_disk_bytes(&self.d.models.dir())
    }

    fn quit(&self) {
        self.d.system.request_quit();
    }

    fn model_use(&self, id: &str) -> Result<()> {
        self.d.engine.can_use(id).map_err(Error::msg)?;
        if self.d.models.path_of(id).is_none() {
            return Err(Error::msg(tr("This model is not installed.")));
        }
        let previous = self.d.engine.current_model();
        self.d.settings.patch(json!({"selectedModel": id}))?;
        let fallback = previous.filter(|p| p != id);
        self.reload_engine_async(fallback);
        self.push_state();
        Ok(())
    }

    fn model_use_local_file(&self, path: &Path) -> Result<()> {
        self.d.models.use_local_file(path).map_err(Error::msg)
    }

    fn complete_first_run(&self, shortcut: &str, model_id: Option<&str>) -> Result<()> {
        self.shortcut_test(shortcut).map_err(Error::msg)?;
        let mut patch = json!({"shortcut": shortcut, "firstRunDone": true});
        if let (Some(id), Some(obj)) = (model_id, patch.as_object_mut()) {
            obj.insert("selectedModel".into(), json!(id));
        }
        let before = self.d.settings.get();
        let after = self.d.settings.patch(patch)?;
        if let Some(id) = model_id {
            if self.d.models.path_of(id).is_none() {
                self.d.models.download(id).map_err(Error::msg)?;
            }
        }
        self.apply_side_effects(&before, &after);
        self.push_state();
        Ok(())
    }

    fn restart_engine(&self) {
        self.reload_engine_async(None);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::testkit::*;
    use linuxflow_core::testing::RecorderCall;

    fn entry(id: &str, ts: &str, words: u32) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            ts: ts.into(),
            text: "t".into(),
            raw: "t".into(),
            words,
            audio_ms: 60_000,
            stt_ms: 100,
            latency_ms: 500,
            language: None,
            app: None,
            pasted: PasteDisposition::Pasted,
        }
    }

    #[test]
    fn state_uses_display_name_and_version_only() {
        let r = service_rig(Some("Sam"), false);
        let s = r.service.state();
        assert_eq!(s.version, "9.9.9");
        assert_eq!(s.user.as_deref(), Some("Sam"));
        let r = service_rig(None, false);
        assert_eq!(r.service.state().user, None);
        assert!(!r.service.state().engine.ready);
    }

    #[test]
    fn state_aggregates_history_and_stats() {
        let r = service_rig(None, false);
        let s = r.settings.get();
        r.history_sink
            .add(entry("a", "2020-01-01T10:00:00Z", 60), &s)
            .unwrap();
        r.history_sink
            .add(entry("b", "2020-01-02T10:00:00Z", 40), &s)
            .unwrap();
        let st = r.service.state();
        assert_eq!(st.history.len(), 2);
        assert_eq!(st.history[0].id, "b");
        assert_eq!(st.stats.total_words, 100);
        assert_eq!(st.stats.dictations, 2);
        r.service.history_delete("b").unwrap();
        assert_eq!(r.service.state().history.len(), 1);
        r.service.history_clear().unwrap();
        assert!(r.service.state().history.is_empty());
        // Every change pushes a state to the interface.
        let pushes = r
            .events
            .events()
            .iter()
            .filter(|e| matches!(e, Event::State(_)))
            .count();
        assert_eq!(pushes, 2);
    }

    #[test]
    fn dictionary_and_snippets_through_controller() {
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        c.dictionary_add("Docker", "dock her").unwrap();
        c.dictionary_add("docker", "doc ker").unwrap();
        c.dictionary_update("Docker", "Dockers", "x").unwrap();
        assert_eq!(r.service.state().dictionary[0].word, "Dockers");
        c.dictionary_remove("Dockers").unwrap();
        assert!(r.service.state().dictionary.is_empty());
        c.dictionary_restore_defaults().unwrap();
        c.snippet_add("sig", "Best").unwrap();
        let id = r.service.state().snippets[0].id.clone();
        assert_eq!(c.format_preview("my sig"), "My Best");
        assert_eq!(c.format_preview("sig"), "Best");
        c.snippet_remove(&id).unwrap();
        assert!(r.service.state().snippets.is_empty());
        assert!(c.dictionary_add("", "").is_err());
    }

    #[test]
    fn format_preview_language_defaults_to_french() {
        let _language = language_guard();
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        // French: narrow space before "?".
        assert_eq!(
            c.format_preview("comment ça va ?"),
            "Comment ça va\u{202F}?"
        );
        c.set_settings(json!({"language": "en"})).unwrap();
        assert_eq!(c.format_preview("how are you ?"), "How are you?");
    }

    #[test]
    fn settings_patch_triggers_side_effects() {
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        c.set_settings(json!({"shortcut": "F9", "mode": "ptt"}))
            .unwrap();
        assert!(r
            .system
            .calls()
            .contains(&"apply_shortcut F9 Ptt".to_string()));
        c.set_settings(json!({"shortcutEnabled": false})).unwrap();
        assert!(r.system.calls().contains(&"suspend true".to_string()));
        assert!(r.service.state().shortcut_suspended);
        c.set_settings(json!({"launchAtLogin": true})).unwrap();
        assert!(r.system.calls().contains(&"autostart true".to_string()));
        c.set_settings(json!({"maxDictationSec": 5})).unwrap();
        assert_eq!(r.settings.get().max_dictation_sec, 5);
        // Invalid value: nothing changes, no effect.
        let n = r.system.calls().len();
        assert!(c.set_settings(json!({"maxDictationSec": 0})).is_err());
        assert_eq!(r.system.calls().len(), n);
    }

    #[test]
    fn autostart_failure_is_reported_as_toast() {
        let r = service_rig(None, false);
        *r.system.autostart_fails.lock().unwrap() = true;
        r.service
            .set_settings(json!({"launchAtLogin": true}))
            .unwrap();
        assert!(r
            .events
            .events()
            .iter()
            .any(|e| matches!(e, Event::Toast(m) if m.contains("launch-at-login"))));
    }

    #[test]
    fn history_limit_is_enforced() {
        let r = service_rig(None, false);
        let s = r.settings.get();
        for i in 0..5 {
            r.history_sink
                .add(entry(&format!("e{i}"), "2020-01-01T10:00:00Z", 1), &s)
                .unwrap();
        }
        r.service.set_settings(json!({"historyLimit": 2})).unwrap();
        assert_eq!(r.history_sink.len(), 2);
    }

    #[test]
    fn ui_language_values_are_validated() {
        let _language = language_guard();
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        c.set_settings(json!({"uiLanguage": "fr"})).unwrap();
        assert_eq!(r.settings.get().ui_language.as_deref(), Some("fr"));
        assert!(r
            .system
            .calls()
            .contains(&"ui_language_changed".to_string()));
        c.set_settings(json!({"uiLanguage": "auto"})).unwrap();
        assert_eq!(r.settings.get().ui_language, None);
        c.set_settings(json!({"uiLanguage": "en"})).unwrap();
        assert_eq!(r.settings.get().ui_language.as_deref(), Some("en"));
        assert!(c.set_settings(json!({"uiLanguage": "de"})).is_err());
        assert!(c.set_settings(json!({"uiLanguage": 3})).is_err());
        assert_eq!(r.settings.get().ui_language.as_deref(), Some("en"));
        linuxflow_i18n::set_language(None);
    }

    #[test]
    fn shortcut_test_messages() {
        let _language = language_guard();
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        assert_eq!(
            c.shortcut_test(""),
            Err("Press at least one key combination.".into())
        );
        assert_eq!(
            c.shortcut_test("Ctrl"),
            Err("With modifier keys only, you need at least two (e.g. Ctrl + Super).".into())
        );
        assert_eq!(
            c.shortcut_test("A"),
            Err("Add Ctrl, Alt, Shift or Super (or use an F1 to F12 key).".into())
        );
        let reserved = c.shortcut_test("Ctrl+V").expect_err("Ctrl+V is reserved");
        assert!(
            reserved.contains("Ctrl + V") || reserved.contains("Ctrl+V"),
            "{reserved}"
        );
        assert!(c.shortcut_test("Ctrl+Alt+K").is_ok());
        *r.system.probe.lock().unwrap() = Some(ProbeError::Conflict);
        assert_eq!(
            c.shortcut_test("Ctrl+Alt+K"),
            Err("Already used by another application.".into())
        );
        *r.system.probe.lock().unwrap() = Some(ProbeError::Unavailable("no backend".into()));
        assert_eq!(c.shortcut_test("Ctrl+Alt+K"), Err("no backend".into()));
        // The already active shortcut is accepted without a probe.
        r.system.status.lock().unwrap().ok = true;
        assert!(c.shortcut_test("Ctrl+Alt+Space").is_ok());
        c.set_settings(json!({"mode": "ptt"})).unwrap();
        assert_eq!(
            c.shortcut_test("Ctrl+Alt+Unobtainium"),
            Err("The key “Unobtainium” is not recognised in hold-to-talk mode.".into())
        );
    }

    #[test]
    fn shortcut_suspend_combines_with_setting() {
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        c.shortcut_suspend(true);
        assert!(r.service.state().shortcut_suspended);
        c.shortcut_suspend(false);
        assert!(!r.service.state().shortcut_suspended);
        assert_eq!(r.system.calls(), vec!["suspend true", "suspend false"]);
    }

    #[test]
    fn open_folders_only_known_ones_and_errors_become_toasts() {
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        c.open_logs_dir();
        c.open_data_dir();
        c.open_models_dir();
        assert_eq!(
            r.system.calls(),
            vec!["open Logs", "open Data", "open Models"]
        );
        *r.system.open_fails.lock().unwrap() = true;
        c.open_logs_dir();
        assert!(r
            .events
            .events()
            .contains(&Event::Toast("cannot open".into())));
        assert_eq!(c.free_disk_bytes(), Some(42));
        assert_eq!(c.models_dir(), r.dir.path().join("models"));
    }

    #[test]
    fn dictation_toggle_goes_to_the_machine() {
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        // Engine not ready: message, no capture.
        assert_eq!(c.dictation_toggle(false), Phase::Error);
        assert!(r.recorder.calls().is_empty());
        r.service.poll_engine(); // nothing ready
        std::fs::write(r.dir.path().join("models/tiny.bin"), b"abcd").unwrap();
        r.service.engine().reload();
        r.service.poll_engine();
        r.recorder.set_silence_ms(1000);
        r.stt.push_text("ok");
        assert_eq!(c.dictation_toggle(true), Phase::Error); // still in "flash": refused
        assert_eq!(c.dictation_cancel(), Phase::Idle);
    }

    #[test]
    fn model_use_errors_and_success() {
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        assert!(c.model_use("nope").is_err());
        assert!(c.model_use("silero-vad").is_err());
        assert!(c.model_use("tiny").is_err()); // not installed
        std::fs::write(r.dir.path().join("models/tiny.bin"), b"abcd").unwrap();
        c.model_use("tiny").unwrap();
        assert_eq!(r.settings.get().selected_model.as_deref(), Some("tiny"));
        // The reload runs on a thread.
        for _ in 0..50 {
            if !r.launcher.0.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(r.launcher.0.lock().unwrap()[0].model_id, "tiny");
    }

    #[test]
    fn startup_reapplies_everything() {
        let r = service_rig(None, false);
        r.settings.patch(json!({"launchAtLogin": true})).unwrap();
        r.service.startup();
        let calls = r.system.calls();
        assert!(calls.contains(&"autostart true".to_string()));
        assert!(calls.contains(&"apply_shortcut Ctrl+Alt+Space Toggle".to_string()));
        assert!(r.settings.get().vocab_seeded);
    }

    #[test]
    fn poll_engine_pushes_only_on_change() {
        let r = service_rig(None, false);
        r.service.poll_engine();
        let n1 = r.events.events().len();
        r.service.poll_engine();
        assert_eq!(r.events.events().len(), n1);
        std::fs::write(r.dir.path().join("models/tiny.bin"), b"abcd").unwrap();
        r.service.engine().reload();
        r.service.poll_engine();
        assert!(r.events.events().len() > n1);
    }

    #[test]
    fn complete_first_run_writes_shortcut_and_flag() {
        let r = service_rig(None, false);
        r.service.complete_first_run("Ctrl+Alt+K", None).unwrap();
        let s = r.settings.get();
        assert!(s.first_run_done);
        assert_eq!(s.shortcut, "Ctrl+Alt+K");
        assert!(r
            .system
            .calls()
            .contains(&"apply_shortcut Ctrl+Alt+K Toggle".to_string()));
        assert!(r.service.complete_first_run("A", None).is_err());
    }

    #[test]
    fn copy_quit_and_microphones() {
        let r = service_rig(None, false);
        let c: &dyn Controller = &*r.service;
        c.copy_text("abc").unwrap();
        c.quit();
        assert_eq!(c.list_microphones().len(), 1);
        assert_eq!(c.capabilities().session, "x11");
        assert_eq!(r.system.calls(), vec!["copy abc", "quit"]);
        let _ = RecorderCall::Cancel;
    }
}
