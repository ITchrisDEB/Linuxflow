//! Complete fake controller for the offscreen preview and the hub widget tests.
//! No real data: everything is made up for the demonstration.

#![allow(dead_code)] // shared by the example and the tests: each one only uses part of it
use linuxflow_core::domain::*;
use linuxflow_core::error::{Error, Result};
use linuxflow_core::traits::Controller;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct FakeController {
    pub state: Mutex<AppState>,
    pub models: Mutex<Vec<ModelStatus>>,
    pub calls: Mutex<Vec<String>>,
    pub caps: Mutex<Capabilities>,
}

fn entry(
    id: &str,
    ts: &str,
    text: &str,
    words: u32,
    latency: u64,
    app: Option<&str>,
    lang: &str,
) -> HistoryEntry {
    HistoryEntry {
        id: id.into(),
        ts: ts.into(),
        text: text.into(),
        raw: text.into(),
        words,
        audio_ms: 4000,
        stt_ms: 600,
        latency_ms: latency,
        language: Some(lang.into()),
        app: app.map(str::to_string),
        pasted: PasteDisposition::Pasted,
    }
}

pub fn demo_state() -> AppState {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let iso = |ago: i64| {
        let t = now - ago;
        let (days, rem) = (t.div_euclid(86_400), t.rem_euclid(86_400));
        // Conversion days -> civil date (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    };
    AppState {
        version: "0.3.0".into(),
        user: Some("alex".into()),
        settings: Settings { first_run_done: true, selected_model: Some("small".into()), ..Settings::default() },
        dictionary: vec![
            DictionaryEntry { word: "Docker".into(), sounds: vec!["dock her".into(), "doc ker".into()] },
            DictionaryEntry { word: "Kubernetes".into(), sounds: vec!["cooper nettys".into()] },
            DictionaryEntry { word: "LinuxFlow".into(), sounds: vec!["linux flow".into(), "linux flo".into()] },
        ],
        snippets: vec![
            Snippet { id: "s1".into(), trigger: "my signature".into(), expansion: "Have a great day and talk soon!\nAlex".into() },
            Snippet { id: "s2".into(), trigger: "my address".into(), expansion: "12 Example Street, Sampletown".into() },
        ],
        history: vec![
            entry("h1", &iso(20), "Let's schedule the review for Thursday afternoon.", 7, 1400, Some("firefox"), "en"),
            entry("h2", &iso(600), "Remember to update the changelog before the release.", 8, 900, Some("code"), "en"),
            entry("h3", &iso(7200), "Bonjour, je voudrais confirmer notre rendez-vous de demain.", 8, 1100, None, "fr"),
            entry("h4", &iso(3 * 86_400), "A much longer dictation that wraps over several lines to check how the history row behaves when the text is long enough to need wrapping in the available width of the page.", 33, 2300, Some("thunderbird"), "en"),
        ],
        stats: Stats { words_today: 1280, total_words: 48_210, dictations: 312, avg_wpm: 142.4, avg_latency_ms: 1350.0, minutes_spoken: 340.0 },
        engine: EngineInfo {
            phase: EnginePhase::Ready,
            ready: true,
            device: DeviceKind::Gpu,
            device_name: Some("Vulkan GPU".into()),
            model: Some("small".into()),
            precision: Some("q5_0".into()),
            load_ms: Some(2300),
            ..EngineInfo::default()
        },
        ptt_available: true,
        shortcut_ok: true,
        shortcut_desktop_trigger: None,
        shortcut_message: None,
        shortcut_suspended: false,
        phase: Phase::Idle,
    }
}

pub fn demo_models() -> Vec<ModelStatus> {
    let m = |id: &str, name: &str, dl: u64, mem: u64, state, rec, in_use| ModelStatus {
        id: id.into(),
        name: name.into(),
        download_bytes: dl,
        disk_bytes: dl,
        memory_bytes: mem,
        state,
        recommended: rec,
        in_use,
    };
    vec![
        m(
            "small",
            "Whisper small",
            487_601_967,
            900_000_000,
            ModelState::Installed,
            true,
            true,
        ),
        m(
            "large-v3-q5_0",
            "Whisper large v3 (q5_0)",
            1_081_140_203,
            2_400_000_000,
            ModelState::Downloading {
                done: 432_000_000,
                total: 1_081_140_203,
            },
            false,
            false,
        ),
        m(
            "large-v3",
            "Whisper large v3 (f16)",
            3_095_033_483,
            4_800_000_000,
            ModelState::Paused {
                done: 900_000_000,
                total: 3_095_033_483,
            },
            false,
            false,
        ),
        m(
            "tiny",
            "Whisper tiny",
            77_700_000,
            300_000_000,
            ModelState::NotInstalled,
            false,
            false,
        ),
        m(
            "custom",
            "Local file",
            150_000_000,
            400_000_000,
            ModelState::Unverified,
            false,
            false,
        ),
        m(
            "broken",
            "Whisper base",
            147_951_465,
            500_000_000,
            ModelState::Error("Network unreachable: check your connection.".into()),
            false,
            false,
        ),
    ]
}

/// The models of the real catalogue (`crates/linuxflow-models/data/models.json`), for the preview.
/// `wizard`: first launch, nothing installed yet and the GPU model recommended; otherwise the
/// "Models" page with `small` installed and in use, and `large-v3 (q5_0)` downloading.
pub fn real_models(wizard: bool) -> Vec<ModelStatus> {
    let m = |id: &str, name: &str, dl: u64, mem: u64, state, rec, in_use| ModelStatus {
        id: id.into(),
        name: name.into(),
        download_bytes: dl,
        disk_bytes: dl,
        memory_bytes: mem,
        state,
        recommended: rec,
        in_use,
    };
    vec![
        m(
            "small",
            "Whisper small (multilingual)",
            487_601_967,
            1_073_741_824,
            if wizard {
                ModelState::NotInstalled
            } else {
                ModelState::Installed
            },
            false,
            !wizard,
        ),
        m(
            "large-v3-q5_0",
            "Whisper large-v3 (quantized q5_0)",
            1_081_140_203,
            3_221_225_472,
            if wizard {
                ModelState::NotInstalled
            } else {
                ModelState::Downloading {
                    done: 432_000_000,
                    total: 1_081_140_203,
                }
            },
            false,
            false,
        ),
        m(
            "large-v3",
            "Whisper large-v3 (full precision f16)",
            3_095_033_483,
            5_368_709_120,
            ModelState::NotInstalled,
            true,
            false,
        ),
    ]
}

pub fn demo_caps() -> Capabilities {
    Capabilities {
        session: "wayland".into(),
        desktop: "GNOME".into(),
        portal_global_shortcuts: true,
        portal_remote_desktop: true,
        portal_clipboard: false,
        layer_shell: false,
        data_control: true,
        vulkan_available: true,
        vulkan_devices: vec!["Vulkan GPU".into()],
        pipewire: true,
        tray_watcher: false,
        uinput_writable: false,
        input_group: false,
        gtk_version: "4.14.0".into(),
        adw_version: "1.5.0".into(),
        hints: vec![
            (
                "Clipboard portal".into(),
                "Install xdg-desktop-portal-gnome and restart your session.".into(),
            ),
            (
                "Notification area".into(),
                "Install an AppIndicator extension to see the tray icon.".into(),
            ),
        ],
    }
}

impl FakeController {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(demo_state()),
            models: Mutex::new(demo_models()),
            calls: Mutex::new(Vec::new()),
            caps: Mutex::new(demo_caps()),
        })
    }

    fn log(&self, call: impl Into<String>) {
        if let Ok(mut c) = self.calls.lock() {
            c.push(call.into());
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    pub fn with_state<R>(&self, f: impl FnOnce(&mut AppState) -> R) -> Option<R> {
        self.state.lock().ok().map(|mut s| f(&mut s))
    }
}

impl Controller for FakeController {
    fn state(&self) -> AppState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn set_settings(&self, patch: serde_json::Value) -> Result<()> {
        self.log(format!("set_settings {patch}"));
        let mut st = self.state.lock().map_err(|_| Error::msg("lock"))?;
        let mut value = serde_json::to_value(&st.settings)?;
        if let (Some(obj), Some(p)) = (value.as_object_mut(), patch.as_object()) {
            for (k, v) in p {
                obj.insert(k.clone(), v.clone());
            }
        }
        st.settings = serde_json::from_value(value)?;
        Ok(())
    }

    fn shortcut_test(&self, accelerator: &str) -> std::result::Result<(), String> {
        self.log(format!("shortcut_test {accelerator}"));
        if accelerator == "Ctrl+Q" {
            Err("This shortcut is already used by another application.".into())
        } else {
            Ok(())
        }
    }

    fn shortcut_suspend(&self, suspended: bool) {
        self.log(format!("shortcut_suspend {suspended}"));
    }

    fn dictionary_add(&self, word: &str, sounds: &str) -> Result<()> {
        self.log(format!("dictionary_add {word}|{sounds}"));
        let sounds: Vec<String> = sounds
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.with_state(|s| {
            s.dictionary.push(DictionaryEntry {
                word: word.into(),
                sounds,
            })
        });
        Ok(())
    }

    fn dictionary_update(&self, previous_word: &str, word: &str, sounds: &str) -> Result<()> {
        self.log(format!("dictionary_update {previous_word}|{word}|{sounds}"));
        Ok(())
    }

    fn dictionary_remove(&self, word: &str) -> Result<()> {
        self.log(format!("dictionary_remove {word}"));
        self.with_state(|s| s.dictionary.retain(|d| d.word != word));
        Ok(())
    }

    fn dictionary_restore_defaults(&self) -> Result<()> {
        self.log("dictionary_restore_defaults");
        Ok(())
    }

    fn format_preview(&self, text: &str) -> String {
        self.log(format!("format_preview {text}"));
        let t = text.replace("cooper nettys", "Kubernetes");
        let mut c = t.chars();
        match c.next() {
            Some(f) => f.to_uppercase().chain(c).collect(),
            None => String::new(),
        }
    }

    fn snippet_add(&self, trigger: &str, expansion: &str) -> Result<()> {
        self.log(format!("snippet_add {trigger}|{expansion}"));
        Ok(())
    }

    fn snippet_remove(&self, id: &str) -> Result<()> {
        self.log(format!("snippet_remove {id}"));
        Ok(())
    }

    fn history_delete(&self, id: &str) -> Result<()> {
        self.log(format!("history_delete {id}"));
        Ok(())
    }

    fn history_clear(&self) -> Result<()> {
        self.log("history_clear");
        Ok(())
    }

    fn dictation_toggle(&self, to_hub: bool) -> Phase {
        self.log(format!("dictation_toggle {to_hub}"));
        Phase::Listening
    }

    fn dictation_cancel(&self) -> Phase {
        self.log("dictation_cancel");
        Phase::Idle
    }

    fn copy_text(&self, text: &str) -> Result<()> {
        self.log(format!("copy_text {text}"));
        Ok(())
    }

    fn open_logs_dir(&self) {
        self.log("open_logs_dir");
    }

    fn open_data_dir(&self) {
        self.log("open_data_dir");
    }

    fn list_microphones(&self) -> Vec<MicrophoneInfo> {
        vec![
            MicrophoneInfo {
                id: "default".into(),
                label: "Default".into(),
            },
            MicrophoneInfo {
                id: "usb-mic".into(),
                label: "USB Microphone".into(),
            },
            MicrophoneInfo {
                id: "built-in".into(),
                label: "Built-in Audio".into(),
            },
        ]
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn models(&self) -> Vec<ModelStatus> {
        self.models.lock().map(|m| m.clone()).unwrap_or_default()
    }

    fn model_download(&self, id: &str) -> Result<()> {
        self.log(format!("model_download {id}"));
        Ok(())
    }

    fn model_pause(&self, id: &str) -> Result<()> {
        self.log(format!("model_pause {id}"));
        Ok(())
    }

    fn model_resume(&self, id: &str) -> Result<()> {
        self.log(format!("model_resume {id}"));
        Ok(())
    }

    fn model_cancel(&self, id: &str) -> Result<()> {
        self.log(format!("model_cancel {id}"));
        Ok(())
    }

    fn model_delete(&self, id: &str) -> Result<()> {
        self.log(format!("model_delete {id}"));
        Ok(())
    }

    fn model_verify(&self, id: &str) -> Result<()> {
        self.log(format!("model_verify {id}"));
        Ok(())
    }

    fn model_use(&self, id: &str) -> Result<()> {
        self.log(format!("model_use {id}"));
        Ok(())
    }

    fn model_use_local_file(&self, path: &Path) -> Result<()> {
        self.log(format!("model_use_local_file {}", path.display()));
        Ok(())
    }

    fn complete_first_run(&self, shortcut: &str, model_id: Option<&str>) -> Result<()> {
        self.log(format!(
            "complete_first_run {shortcut}|{}",
            model_id.unwrap_or("-")
        ));
        self.with_state(|s| s.settings.first_run_done = true);
        Ok(())
    }

    fn quit(&self) {
        self.log("quit");
    }
}
