//! Pure labels and formats of the hub (no widget): greeting, numbers, relative time,
//! loading labels and engine pill. All the texts go through `linuxflow_i18n`.

use gtk::glib;
use gtk4 as gtk;
use linuxflow_core::domain::{
    AppState, DeviceKind, DownloadProgress, EngineInfo, EnginePhase, Phase,
};
use linuxflow_i18n::{
    format_bytes, format_number, format_percent, format_relative, tr, tr_fmt, trn,
};

/// Greeting: "Good morning" from 5 am to 6 pm (excluded), otherwise "Good evening", followed by the first name.
pub fn greeting(hour: u32, name: Option<&str>) -> String {
    let morning = (5..18).contains(&hour);
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(capitalize_first);
    match (morning, name) {
        (true, Some(n)) => tr_fmt("Good morning, {}", &[&n]),
        (true, None) => tr("Good morning"),
        (false, Some(n)) => tr_fmt("Good evening, {}", &[&n]),
        (false, None) => tr("Good evening"),
    }
}

/// Capitalises the first letter (the rest is kept).
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Integer with the separators of the active language.
pub fn format_count(n: u64) -> String {
    format_number(n as f64, 0)
}

/// Average speed, rounded (words per minute).
pub fn format_wpm(wpm: f64) -> String {
    format_count(wpm.max(0.0).round() as u64)
}

/// Size in bytes according to the active language.
pub fn format_size(bytes: u64) -> String {
    format_bytes(bytes)
}

/// Latency duration in seconds with one decimal (`—` if zero).
pub fn format_latency_ms(ms: f64) -> String {
    if ms > 0.0 {
        tr_fmt("{} s", &[&format_number(ms / 1000.0, 1)])
    } else {
        "—".to_string()
    }
}

/// Parses an ISO 8601 timestamp (UTC by default) into Unix seconds.
pub fn iso_to_unix(ts: &str) -> Option<i64> {
    glib::DateTime::from_iso8601(ts, Some(&glib::TimeZone::utc()))
        .ok()
        .map(|d| d.to_unix())
}

/// Relative time of an ISO timestamp (delegated to the i18n crate).
pub fn relative_iso(ts: &str) -> String {
    iso_to_unix(ts).map(format_relative).unwrap_or_default()
}

/// Download percentage capped at 99 (100 % is only reached after verification).
pub fn download_percent(done: u64, total: u64) -> u32 {
    if total == 0 {
        return 0;
    }
    ((done.saturating_mul(100)) / total).min(99) as u32
}

/// Engine loading label.
pub fn loading_label(download: Option<&DownloadProgress>, short: bool) -> String {
    let Some(d) = download else {
        return tr("Loading model…");
    };
    if d.total == 0 {
        return if short {
            tr("Downloading…")
        } else {
            tr_fmt("Downloading model · {}", &[&format_size(d.done)])
        };
    }
    let pct = format_percent(f64::from(download_percent(d.done, d.total)) / 100.0, 0);
    if short {
        tr_fmt("Downloading · {}", &[&pct])
    } else {
        tr_fmt(
            "Downloading model · {} ({} / {})",
            &[&pct, &format_size(d.done), &format_size(d.total)],
        )
    }
}

/// Visual family of the engine pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillKind {
    Loading,
    Ready,
    Error,
    Listening,
}

impl PillKind {
    pub fn css_class(self) -> &'static str {
        match self {
            PillKind::Loading => "loading",
            PillKind::Ready => "ready",
            PillKind::Error => "error",
            PillKind::Listening => "listening",
        }
    }
}

/// Hardware label (GPU/CPU).
pub fn device_short(kind: DeviceKind) -> String {
    match kind {
        DeviceKind::Gpu => tr("GPU"),
        DeviceKind::Cpu => tr("CPU"),
    }
}

/// Engine pill. Priority: listening > ready > error > loading.
pub fn engine_pill(state: &AppState) -> (PillKind, String) {
    let eng = &state.engine;
    if state.phase == Phase::Listening {
        return (PillKind::Listening, tr("Listening…"));
    }
    if eng.ready {
        let model = eng.model.as_deref().unwrap_or("—");
        return (
            PillKind::Ready,
            tr_fmt("{} · {} · ready", &[&device_short(eng.device), model]),
        );
    }
    if eng.error.is_some() && !eng.loading || eng.phase == EnginePhase::Error {
        return (PillKind::Error, tr("Engine unavailable"));
    }
    (
        PillKind::Loading,
        loading_label(eng.download.as_ref(), true),
    )
}

/// Text of the engine state in the "Local engine" card.
pub fn engine_state_text(eng: &EngineInfo) -> String {
    if eng.ready {
        tr("Ready")
    } else if eng.error.is_some() && !eng.loading || eng.phase == EnginePhase::Error {
        tr("Error")
    } else if eng.phase == EnginePhase::Downloading {
        loading_label(eng.download.as_ref(), false)
    } else {
        tr("Loading…")
    }
}

/// Hardware label in the "Local engine" card (GPU name if known).
pub fn engine_device_text(eng: &EngineInfo) -> String {
    if !eng.ready && eng.model.is_none() {
        return "—".into();
    }
    match (eng.device, eng.device_name.as_deref()) {
        (DeviceKind::Gpu, Some(name)) => tr_fmt("GPU ({})", &[name]),
        (DeviceKind::Gpu, None) => tr("GPU"),
        (DeviceKind::Cpu, _) => tr("CPU"),
    }
}

/// Status text of the test area according to the dictation phase.
pub fn try_status(phase: Phase, hub_dictation: bool) -> String {
    let name = match phase {
        Phase::Idle => tr("Ready"),
        Phase::Listening => tr("Listening… click to stop"),
        Phase::Transcribing => tr("Transcribing…"),
        Phase::Formatting => tr("Formatting…"),
        Phase::Done => tr("Done"),
        Phase::Error => tr("Error"),
    };
    if hub_dictation || phase == Phase::Idle {
        name
    } else {
        tr_fmt("Dictation in progress ({})", &[&name])
    }
}

/// `N word(s)` according to the plural.
pub fn word_count(n: u64) -> String {
    fill(&trn("{} word", "{} words", n), &[&n.to_string()])
}

/// Replaces the successive `{}` of an ALREADY translated template (result of `trn`).
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = template.to_string();
    for a in args {
        if let Some(i) = out.find("{}") {
            out.replace_range(i..i + 2, a);
        }
    }
    out
}

/// Separator to insert before a dictated text: a space if the previous text does not end with a blank.
pub fn insert_separator(before: &str) -> &'static str {
    match before.chars().last() {
        Some(c) if !c.is_whitespace() => " ",
        _ => "",
    }
}

/// Label of a shortcut key (`Space` → "Space", `Up` → "↑").
pub fn key_label(token: &str) -> String {
    match token {
        "Ctrl" => tr("Ctrl"),
        "Alt" => tr("Alt"),
        "Shift" => tr("Shift"),
        "Super" => tr("Super"),
        "Space" => tr("Space"),
        "Enter" => tr("Enter"),
        "Backspace" => tr("Backspace"),
        "Delete" => tr("Delete"),
        "Insert" => tr("Insert"),
        "Capslock" => tr("Caps Lock"),
        "Up" => "↑".into(),
        "Down" => "↓".into(),
        "Left" => "←".into(),
        "Right" => "→".into(),
        other => {
            linuxflow_core::accelerator::numpad_label(other).unwrap_or_else(|| other.to_string())
        }
    }
}

/// Readable shortcut ("Ctrl + Alt + Space").
pub fn pretty_accelerator(acc: &str) -> String {
    acc.split('+')
        .filter(|p| !p.is_empty())
        .map(key_label)
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Splits a shortcut into key labels.
pub fn accelerator_keys(acc: &str) -> Vec<String> {
    acc.split('+')
        .filter(|p| !p.is_empty())
        .map(key_label)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpad_keys_are_shown_like_on_the_keyboard() {
        assert_eq!(pretty_accelerator("Ctrl+numadd"), "Ctrl + Num+");
        assert_eq!(accelerator_keys("Alt+num3"), vec!["Alt", "Num3"]);
    }
    use linuxflow_core::domain::{AppState, EngineInfo};

    fn state_with(engine: EngineInfo, phase: Phase) -> AppState {
        AppState {
            engine,
            phase,
            ..AppState::default()
        }
    }

    #[test]
    fn greeting_depends_on_hour() {
        assert_eq!(greeting(4, None), "Good evening");
        assert_eq!(greeting(5, None), "Good morning");
        assert_eq!(greeting(17, Some("sam")), "Good morning, Sam");
        assert_eq!(greeting(18, Some("sam")), "Good evening, Sam");
        assert_eq!(greeting(10, Some("  ")), "Good morning");
    }

    #[test]
    fn counts_are_grouped_in_source_locale() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_wpm(142.6), "143");
        assert_eq!(format_wpm(-3.0), "0");
    }

    #[test]
    fn capitalize_handles_unicode() {
        assert_eq!(capitalize_first("émile"), "Émile");
        assert_eq!(capitalize_first(""), "");
    }

    #[test]
    fn percent_is_capped_at_99() {
        assert_eq!(download_percent(0, 0), 0);
        assert_eq!(download_percent(50, 100), 50);
        assert_eq!(download_percent(100, 100), 99);
        assert_eq!(download_percent(u64::MAX, 100), 99);
    }

    #[test]
    fn loading_labels() {
        assert_eq!(loading_label(None, false), "Loading model…");
        let unknown = DownloadProgress {
            model: "m".into(),
            done: 10,
            total: 0,
        };
        assert_eq!(loading_label(Some(&unknown), true), "Downloading…");
        let d = DownloadProgress {
            model: "m".into(),
            done: 420_000_000,
            total: 1_000_000_000,
        };
        assert_eq!(loading_label(Some(&d), true), "Downloading · 42%");
        let long = loading_label(Some(&d), false);
        assert!(long.starts_with("Downloading model · 42% ("), "{long}");
    }

    #[test]
    fn pill_priority() {
        let ready = EngineInfo {
            ready: true,
            model: Some("small".into()),
            ..EngineInfo::default()
        };
        let (k, t) = engine_pill(&state_with(ready.clone(), Phase::Idle));
        assert_eq!(k, PillKind::Ready);
        assert_eq!(t, "CPU · small · ready");
        // Listening takes precedence over "ready".
        assert_eq!(
            engine_pill(&state_with(ready, Phase::Listening)).0,
            PillKind::Listening
        );
        let err = EngineInfo {
            error: Some("boom".into()),
            phase: EnginePhase::Error,
            ..EngineInfo::default()
        };
        assert_eq!(
            engine_pill(&state_with(err, Phase::Idle)).0,
            PillKind::Error
        );
        // An error during a reload stays "loading".
        let reloading = EngineInfo {
            error: Some("old".into()),
            loading: true,
            ..EngineInfo::default()
        };
        assert_eq!(
            engine_pill(&state_with(reloading, Phase::Idle)).0,
            PillKind::Loading
        );
        let dl = EngineInfo {
            loading: true,
            phase: EnginePhase::Downloading,
            download: Some(DownloadProgress {
                model: "m".into(),
                done: 1,
                total: 4,
            }),
            ..EngineInfo::default()
        };
        assert_eq!(
            engine_pill(&state_with(dl, Phase::Idle)).1,
            "Downloading · 25%"
        );
    }

    #[test]
    fn try_status_texts() {
        assert_eq!(try_status(Phase::Idle, false), "Ready");
        assert_eq!(
            try_status(Phase::Listening, true),
            "Listening… click to stop"
        );
        assert_eq!(
            try_status(Phase::Transcribing, false),
            "Dictation in progress (Transcribing…)"
        );
    }

    #[test]
    fn plural_and_separator() {
        assert_eq!(word_count(1), "1 word");
        assert_eq!(word_count(12), "12 words");
        assert_eq!(insert_separator(""), "");
        assert_eq!(insert_separator("Hello"), " ");
        assert_eq!(insert_separator("Hello "), "");
        assert_eq!(insert_separator("a\n"), "");
    }

    #[test]
    fn accelerator_labels() {
        assert_eq!(pretty_accelerator("Ctrl+Alt+Space"), "Ctrl + Alt + Space");
        assert_eq!(pretty_accelerator("Super+Up"), "Super + ↑");
        assert_eq!(
            accelerator_keys("Ctrl+F9"),
            vec!["Ctrl".to_string(), "F9".to_string()]
        );
    }

    #[test]
    fn iso_parsing() {
        assert_eq!(iso_to_unix("2026-10-24T12:00:00Z"), Some(1_792_843_200));
        assert!(iso_to_unix("2026-10-24T12:00:00.123Z").is_some());
        assert!(iso_to_unix("not a date").is_none());
        assert_eq!(relative_iso("not a date"), "");
        assert!(!relative_iso("2000-01-01T00:00:00Z").is_empty());
    }
}
