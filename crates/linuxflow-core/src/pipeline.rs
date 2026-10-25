//! Dictation pipeline: audio -> transcription -> formatting -> clipboard (+ paste) -> history.
//!
//! All the dependencies are injected through traits: no hardware access, deterministic tests.
//! The pipeline is blocking (it runs in a worker thread) and never sleeps: the
//! clipboard restoration is returned as a deferred action ([`DeferredRestore`]) that
//! the caller executes after `after_ms`.

use crate::constants::*;
use crate::domain::*;
use crate::error::{Error, SttError};
use crate::traits::{ActiveWindow, Clipboard, Clock, Paster, SpeechToText};
use std::sync::Arc;

/// Formatting of the raw text (implemented by `format.rs`).
pub trait TextFormatter: Send + Sync {
    fn format(&self, raw: &str, language: &str) -> String;
    fn count_words(&self, text: &str) -> u32;
}

/// History destination (the cap of 2000 entries is handled by the implementation).
pub trait HistorySink: Send + Sync {
    fn add(&self, entry: HistoryEntry);
}

/// Chooses the paste shortcut and the delay according to the foreground application.
pub trait PasteModeResolver: Send + Sync {
    fn resolve(&self, window: Option<&WindowInfo>, default_delay_ms: u64) -> (PasteMode, u64);
}

/// Dictionary words passed to the engine as hints (`HINTS_MAX` at most).
pub trait DictionaryHints: Send + Sync {
    fn hints(&self) -> Vec<String>;
}

/// Step reached, so that the caller can feed the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineProgress {
    Transcribing,
    Formatting,
}

/// Pipeline error (the message is meant for the log and the pill).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    #[error("{0}")]
    Stt(#[from] SttError),
    #[error("presse-papiers : {0}")]
    Clipboard(String),
}

/// Parameters of a pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineInput {
    pub audio: Recording,
    /// Weak microphone boost chosen in the settings.
    pub mic_boost: MicBoost,
    /// `auto` or language code.
    pub language: String,
    pub auto_paste: bool,
    pub paste_delay_ms: u64,
    pub restore_clipboard_ms: u64,
    pub to_hub: bool,
    pub no_paste: bool,
    /// False if the history is disabled in the settings.
    pub record_history: bool,
    /// Instant (ms) of the end of the capture, basis of the latency; `None` = start of the pipeline.
    pub stopped_at_ms: Option<u64>,
}

impl PipelineInput {
    pub fn from_settings(
        audio: Recording,
        settings: &Settings,
        to_hub: bool,
        no_paste: bool,
        stopped_at_ms: Option<u64>,
    ) -> Self {
        Self {
            audio,
            mic_boost: settings.mic_boost,
            language: settings.language.clone(),
            auto_paste: settings.auto_paste,
            paste_delay_ms: settings.paste_delay_ms,
            restore_clipboard_ms: settings.restore_clipboard_ms,
            to_hub,
            no_paste,
            record_history: settings.history_enabled,
            stopped_at_ms,
        }
    }
}

/// Deferred clipboard restoration, to be executed by the caller after `after_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredRestore {
    pub after_ms: u64,
    /// Old content to put back.
    pub previous: String,
    /// Dictated text: only restored if the clipboard still contains it exactly.
    pub expected: String,
}

impl DeferredRestore {
    /// Executes the restoration. Returns `true` if the old content was put back; `false` if the
    /// clipboard changed in the meantime (the user copied something else: it is left alone).
    pub fn execute(&self, clipboard: &dyn Clipboard) -> crate::Result<bool> {
        if clipboard.get_text()?.as_deref() == Some(self.expected.as_str()) {
            clipboard.set_text(&self.previous)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// Empty text after formatting ("Nothing heard").
    Empty,
    /// Finished (pasted, sent to the hub or simply copied).
    Done,
    /// Paste blocked: the text stays in the clipboard.
    PasteBlocked,
}

/// Result of the pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineOutcome {
    pub kind: OutcomeKind,
    /// Final text (empty if `Empty`).
    pub text: String,
    pub raw: String,
    pub latency_ms: u64,
    pub stt_ms: u64,
    pub audio_ms: u64,
    /// Final disposition (`None` if `Empty`).
    pub pasted: Option<PasteDisposition>,
    /// Text to insert in the hub's test area (`Event::InsertText`) when `to_hub`.
    pub insert_text: Option<String>,
    /// Clipboard restoration to schedule.
    pub restore: Option<DeferredRestore>,
    /// History entry built (already sent to `HistorySink` if the history is active).
    pub entry: Option<HistoryEntry>,
}

/// Dependencies of the pipeline.
#[derive(Clone)]
pub struct PipelineDeps {
    pub stt: Arc<dyn SpeechToText>,
    pub clipboard: Arc<dyn Clipboard>,
    pub paster: Arc<dyn Paster>,
    pub window: Arc<dyn ActiveWindow>,
    pub clock: Arc<dyn Clock>,
    pub formatter: Arc<dyn TextFormatter>,
    pub history: Arc<dyn HistorySink>,
    pub paste_modes: Arc<dyn PasteModeResolver>,
    pub hints: Arc<dyn DictionaryHints>,
}

pub struct Pipeline {
    deps: PipelineDeps,
}

impl Pipeline {
    pub fn new(deps: PipelineDeps) -> Self {
        Self { deps }
    }

    pub fn run(&self, input: PipelineInput) -> Result<PipelineOutcome, PipelineError> {
        self.run_with_progress(input, &mut |_| {})
    }

    /// Like [`Pipeline::run`], reporting each step (for `Input::PipelineStarted`/`Formatting`).
    pub fn run_with_progress(
        &self,
        input: PipelineInput,
        progress: &mut dyn FnMut(PipelineProgress),
    ) -> Result<PipelineOutcome, PipelineError> {
        let d = &self.deps;
        let t0 = d.clock.now_ms();
        progress(PipelineProgress::Transcribing);

        // 1. Transcription.
        let hints: Vec<String> = d
            .hints
            .hints()
            .into_iter()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .take(HINTS_MAX)
            .collect();
        let requested = (input.language != "auto" && !input.language.is_empty())
            .then(|| input.language.clone());
        // Boost chosen by the user for a weak microphone (off by default, see `gain`).
        let mut audio = input.audio.clone();
        crate::gain::apply_mic_boost(&mut audio.samples, input.mic_boost);
        let request = TranscribeRequest {
            audio,
            language: requested.clone(),
            hints,
        };
        let tr = d.stt.transcribe(&request)?;

        // 2. Formatting.
        progress(PipelineProgress::Formatting);
        let language = tr.language.clone().or(requested).unwrap_or_default();
        let text = d.formatter.format(&tr.text, &language);
        let audio_ms = if tr.audio_ms > 0 {
            tr.audio_ms
        } else {
            input.audio.duration_ms()
        };
        let latency_of = |now: u64| now.saturating_sub(input.stopped_at_ms.unwrap_or(t0));
        if text.trim().is_empty() {
            return Ok(PipelineOutcome {
                kind: OutcomeKind::Empty,
                text: String::new(),
                raw: tr.text,
                latency_ms: latency_of(d.clock.now_ms()),
                stt_ms: tr.stt_ms,
                audio_ms,
                pasted: None,
                insert_text: None,
                restore: None,
                entry: None,
            });
        }

        // 3. Clipboard: reading the old content (a failed read does not lose the
        //    dictation: there will simply be nothing to restore), then writing the text.
        let previous = d.clipboard.get_text().ok().flatten();
        d.clipboard
            .set_text(&text)
            .map_err(|e| PipelineError::Clipboard(e.to_string()))?;

        // 4. Collage.
        let should_paste = input.auto_paste && !input.no_paste && !input.to_hub;
        let mut paste_result = None;
        // Dictation sent to the hub's test area: the "application" is LinuxFlow itself.
        let mut app = input.to_hub.then(|| crate::constants::APP_NAME.to_string());
        if should_paste {
            let window = d.window.current();
            app = window.as_ref().and_then(|w| w.app_id.clone());
            let (mode, delay) = d.paste_modes.resolve(window.as_ref(), input.paste_delay_ms);
            paste_result = Some(d.paster.paste(mode, delay));
        }
        let pasted_ok = matches!(paste_result, Some(PasteResult::Pasted));
        let blocked = matches!(paste_result, Some(PasteResult::Blocked));

        // 5. Restoration (only after a successful paste and if the old content differs).
        let restore = match previous {
            Some(prev) if pasted_ok && prev != text => Some(DeferredRestore {
                after_ms: if input.restore_clipboard_ms == 0 {
                    DEFAULT_RESTORE_CLIPBOARD_MS
                } else {
                    input.restore_clipboard_ms
                },
                previous: prev,
                expected: text.clone(),
            }),
            _ => None,
        };

        // 6. Historique.
        let pasted = if pasted_ok {
            PasteDisposition::Pasted
        } else if input.to_hub {
            PasteDisposition::Hub
        } else {
            PasteDisposition::Clipboard
        };
        let now = d.clock.now_ms();
        let latency_ms = latency_of(now);
        let entry = HistoryEntry {
            id: new_entry_id(now),
            ts: d.clock.now_iso(),
            text: text.clone(),
            raw: tr.text.clone(),
            words: d.formatter.count_words(&text),
            audio_ms,
            stt_ms: tr.stt_ms,
            latency_ms,
            language: tr.language.clone(),
            app,
            pasted,
        };
        if input.record_history {
            d.history.add(entry.clone());
        }

        Ok(PipelineOutcome {
            kind: if blocked {
                OutcomeKind::PasteBlocked
            } else {
                OutcomeKind::Done
            },
            insert_text: input.to_hub.then(|| text.clone()),
            text,
            raw: tr.text,
            latency_ms,
            stt_ms: tr.stt_ms,
            audio_ms,
            pasted: Some(pasted),
            restore,
            entry: Some(entry),
        })
    }
}

impl From<PipelineError> for Error {
    fn from(e: PipelineError) -> Self {
        Error::msg(e.to_string())
    }
}

/// Entry identifier: base-36 time + 4 random characters.
fn new_entry_id(now_ms: u64) -> String {
    let mut id = base36(now_ms);
    for _ in 0..4 {
        id.push(char::from_digit(fastrand::u32(0..36), 36).unwrap_or('0'));
    }
    id
}

fn base36(mut n: u64) -> String {
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::testing::*;

    struct Rig {
        stt: FakeStt,
        clipboard: FakeClipboard,
        paster: FakePaster,
        window: FakeWindow,
        clock: FakeClock,
        formatter: FakeFormatter,
        history: FakeHistory,
        paste_modes: FakePasteModes,
        hints: FakeHints,
        pipeline: Pipeline,
    }

    fn rig() -> Rig {
        let stt = FakeStt::new();
        let clipboard = FakeClipboard::new();
        let paster = FakePaster::new();
        let window = FakeWindow::new();
        let clock = FakeClock::new(1_000_000);
        let formatter = FakeFormatter::new();
        let history = FakeHistory::new();
        let paste_modes = FakePasteModes::new();
        let hints = FakeHints::new();
        let pipeline = Pipeline::new(PipelineDeps {
            stt: Arc::new(stt.clone()),
            clipboard: Arc::new(clipboard.clone()),
            paster: Arc::new(paster.clone()),
            window: Arc::new(window.clone()),
            clock: Arc::new(clock.clone()),
            formatter: Arc::new(formatter.clone()),
            history: Arc::new(history.clone()),
            paste_modes: Arc::new(paste_modes.clone()),
            hints: Arc::new(hints.clone()),
        });
        Rig {
            stt,
            clipboard,
            paster,
            window,
            clock,
            formatter,
            history,
            paste_modes,
            hints,
            pipeline,
        }
    }

    fn input() -> PipelineInput {
        PipelineInput {
            audio: Recording {
                samples: vec![0.0; 16_000],
            },
            mic_boost: MicBoost::Off,
            language: "auto".into(),
            auto_paste: true,
            paste_delay_ms: 60,
            restore_clipboard_ms: 400,
            to_hub: false,
            no_paste: false,
            record_history: true,
            stopped_at_ms: Some(1_000_000),
        }
    }

    #[test]
    fn auto_language_becomes_none_and_explicit_is_forwarded() {
        let r = rig();
        r.stt.push_text("bonjour");
        r.pipeline.run(input()).unwrap();
        assert_eq!(r.stt.requests()[0].language, None);
        r.stt.push_text("hello");
        let mut i = input();
        i.language = "en".into();
        r.pipeline.run(i).unwrap();
        assert_eq!(r.stt.requests()[1].language.as_deref(), Some("en"));
    }

    #[test]
    fn hints_are_cleaned_and_capped() {
        let r = rig();
        let mut hints: Vec<String> = (0..120).map(|i| format!(" mot{i} ")).collect();
        hints.insert(0, "  ".into());
        r.hints.set(hints);
        r.stt.push_text("x");
        r.pipeline.run(input()).unwrap();
        let sent = &r.stt.requests()[0].hints;
        assert_eq!(sent.len(), HINTS_MAX);
        assert_eq!(sent[0], "mot0");
    }

    #[test]
    fn successful_paste_restores_previous_clipboard_conditionally() {
        let r = rig();
        r.clipboard.set_content(Some("ancien"));
        r.stt.push_text("hello world");
        r.paster.push(PasteResult::Pasted);
        let out = r.pipeline.run(input()).unwrap();
        assert_eq!(out.kind, OutcomeKind::Done);
        assert_eq!(out.pasted, Some(PasteDisposition::Pasted));
        let restore = out.restore.clone().unwrap();
        assert_eq!(
            restore,
            DeferredRestore {
                after_ms: 400,
                previous: "ancien".into(),
                expected: "hello world".into()
            }
        );
        // No sleep in the pipeline: the clipboard still contains the dictated text.
        assert_eq!(r.clipboard.content().as_deref(), Some("hello world"));
        assert!(restore.execute(&r.clipboard).unwrap());
        assert_eq!(r.clipboard.content().as_deref(), Some("ancien"));
    }

    #[test]
    fn restore_is_skipped_when_clipboard_changed_meanwhile() {
        let r = rig();
        r.clipboard.set_content(Some("ancien"));
        r.stt.push_text("dictated");
        let out = r.pipeline.run(input()).unwrap();
        r.clipboard.set_content(Some("copied by the user"));
        assert!(!out.restore.unwrap().execute(&r.clipboard).unwrap());
        assert_eq!(r.clipboard.content().as_deref(), Some("copied by the user"));
    }

    #[test]
    fn no_restore_when_previous_equals_text_or_is_empty() {
        let r = rig();
        r.clipboard.set_content(Some("dictated"));
        r.stt.push_text("dictated");
        assert!(r.pipeline.run(input()).unwrap().restore.is_none());
        let r = rig();
        r.stt.push_text("dictated");
        assert!(r.pipeline.run(input()).unwrap().restore.is_none());
    }

    #[test]
    fn no_restore_when_paste_not_successful() {
        for result in [
            PasteResult::Blocked,
            PasteResult::Failed("x".into()),
            PasteResult::Unknown,
        ] {
            let r = rig();
            r.clipboard.set_content(Some("ancien"));
            r.stt.push_text("dictated");
            r.paster.push(result);
            let out = r.pipeline.run(input()).unwrap();
            assert!(out.restore.is_none());
            assert_eq!(r.clipboard.content().as_deref(), Some("dictated"));
        }
    }

    #[test]
    fn restore_delay_zero_falls_back_to_default() {
        let r = rig();
        r.clipboard.set_content(Some("ancien"));
        r.stt.push_text("dictated");
        let mut i = input();
        i.restore_clipboard_ms = 0;
        assert_eq!(
            r.pipeline.run(i).unwrap().restore.unwrap().after_ms,
            DEFAULT_RESTORE_CLIPBOARD_MS
        );
    }

    #[test]
    fn blocked_paste_has_distinct_outcome_and_text_stays_copied() {
        let r = rig();
        r.stt.push_text("dictated");
        r.paster.push(PasteResult::Blocked);
        let out = r.pipeline.run(input()).unwrap();
        assert_eq!(out.kind, OutcomeKind::PasteBlocked);
        assert_eq!(out.pasted, Some(PasteDisposition::Clipboard));
        assert_eq!(r.clipboard.content().as_deref(), Some("dictated"));
        assert_eq!(r.history.entries().len(), 1);
    }

    #[test]
    fn failed_or_unknown_paste_is_done_with_clipboard_disposition() {
        for result in [PasteResult::Failed("x".into()), PasteResult::Unknown] {
            let r = rig();
            r.stt.push_text("dictated");
            r.paster.push(result);
            let out = r.pipeline.run(input()).unwrap();
            assert_eq!(out.kind, OutcomeKind::Done);
            assert_eq!(out.pasted, Some(PasteDisposition::Clipboard));
        }
    }

    #[test]
    fn to_hub_never_pastes_and_returns_text_for_insertion() {
        let r = rig();
        r.stt.push_text("for the hub");
        let mut i = input();
        i.to_hub = true;
        let out = r.pipeline.run(i).unwrap();
        assert_eq!(r.paster.calls().len(), 0);
        assert_eq!(out.insert_text.as_deref(), Some("for the hub"));
        assert_eq!(out.pasted, Some(PasteDisposition::Hub));
        assert_eq!(out.entry.unwrap().app.as_deref(), Some("LinuxFlow"));
        assert_eq!(r.clipboard.content().as_deref(), Some("for the hub"));
    }

    #[test]
    fn no_paste_and_auto_paste_off_skip_paste() {
        let r = rig();
        r.stt.push_text("a");
        let mut i = input();
        i.no_paste = true;
        let out = r.pipeline.run(i).unwrap();
        assert!(r.paster.calls().is_empty());
        assert_eq!(out.pasted, Some(PasteDisposition::Clipboard));
        assert!(out.insert_text.is_none());

        r.stt.push_text("b");
        let mut i = input();
        i.auto_paste = false;
        r.pipeline.run(i).unwrap();
        assert!(r.paster.calls().is_empty());
    }

    #[test]
    fn empty_text_is_reported_without_touching_clipboard_or_history() {
        let r = rig();
        r.clipboard.set_content(Some("intact"));
        r.stt.push_text("euh");
        r.formatter.set_fixed(Some(""));
        let out = r.pipeline.run(input()).unwrap();
        assert_eq!(out.kind, OutcomeKind::Empty);
        assert_eq!(out.raw, "euh");
        assert!(out.entry.is_none());
        assert_eq!(r.clipboard.content().as_deref(), Some("intact"));
        assert!(r.clipboard.set_calls().is_empty());
        assert!(r.history.entries().is_empty());
        assert!(r.paster.calls().is_empty());
    }

    #[test]
    fn stt_error_is_propagated_and_nothing_is_written() {
        let r = rig();
        r.stt.push_error(SttError::Failed("boom".into()));
        let err = r.pipeline.run(input()).unwrap_err();
        assert_eq!(err, PipelineError::Stt(SttError::Failed("boom".into())));
        assert!(r.clipboard.set_calls().is_empty());
        assert!(r.history.entries().is_empty());
    }

    #[test]
    fn stt_not_ready_error_is_propagated() {
        let r = rig();
        r.stt.push_error(SttError::NotReady);
        assert_eq!(
            r.pipeline.run(input()).unwrap_err(),
            PipelineError::Stt(SttError::NotReady)
        );
    }

    #[test]
    fn clipboard_write_failure_is_a_pipeline_error() {
        let r = rig();
        r.stt.push_text("x");
        r.clipboard.fail_set(true);
        assert!(matches!(
            r.pipeline.run(input()),
            Err(PipelineError::Clipboard(_))
        ));
    }

    #[test]
    fn clipboard_read_failure_does_not_lose_the_dictation() {
        let r = rig();
        r.stt.push_text("x");
        r.clipboard.fail_get(true);
        let out = r.pipeline.run(input()).unwrap();
        assert_eq!(out.kind, OutcomeKind::Done);
        assert!(out.restore.is_none());
    }

    #[test]
    fn history_entry_has_all_fields() {
        let r = rig();
        r.window.set(Some(WindowInfo {
            app_id: Some("firefox".into()),
            pid: Some(1),
            title: None,
        }));
        r.stt.push(Ok(Transcription {
            text: "  bonjour   monde ".into(),
            language: Some("fr".into()),
            stt_ms: 321,
            audio_ms: 1500,
        }));
        r.formatter.set_fixed(Some("Bonjour monde."));
        r.clock.set(1_000_640);
        r.paster.push(PasteResult::Pasted);
        let out = r.pipeline.run(input()).unwrap();
        let e = out.entry.unwrap();
        assert_eq!(e.text, "Bonjour monde.");
        assert_eq!(e.raw, "  bonjour   monde ");
        assert_eq!(e.words, 2);
        assert_eq!(e.audio_ms, 1500);
        assert_eq!(e.stt_ms, 321);
        assert_eq!(e.latency_ms, 640);
        assert_eq!(out.latency_ms, 640);
        assert_eq!(e.language.as_deref(), Some("fr"));
        assert_eq!(e.app.as_deref(), Some("firefox"));
        assert_eq!(e.pasted, PasteDisposition::Pasted);
        assert_eq!(e.ts, r.clock.now_iso_value());
        assert_eq!(e.id.len(), base36(1_000_640).len() + 4);
        assert!(e.id.starts_with(&base36(1_000_640)));
        assert_eq!(r.history.entries(), vec![e]);
    }

    #[test]
    fn history_disabled_skips_sink_but_still_returns_entry() {
        let r = rig();
        r.stt.push_text("x");
        let mut i = input();
        i.record_history = false;
        let out = r.pipeline.run(i).unwrap();
        assert!(out.entry.is_some());
        assert!(r.history.entries().is_empty());
    }

    #[test]
    fn audio_ms_falls_back_to_recording_duration() {
        let r = rig();
        r.stt.push(Ok(Transcription {
            text: "x".into(),
            language: None,
            stt_ms: 1,
            audio_ms: 0,
        }));
        assert_eq!(r.pipeline.run(input()).unwrap().audio_ms, 1000);
    }

    #[test]
    fn latency_defaults_to_pipeline_start_without_stop_instant() {
        let r = rig();
        r.stt.push_text("x");
        r.stt.set_delay(&r.clock, 250);
        let mut i = input();
        i.stopped_at_ms = None;
        assert_eq!(r.pipeline.run(i).unwrap().latency_ms, 250);
    }

    #[test]
    fn paste_mode_resolver_receives_window_and_delay() {
        let r = rig();
        let w = WindowInfo {
            app_id: Some("alacritty".into()),
            pid: None,
            title: None,
        };
        r.window.set(Some(w.clone()));
        r.paste_modes.set(PasteMode::CtrlShiftV, 120);
        r.stt.push_text("x");
        r.pipeline.run(input()).unwrap();
        assert_eq!(r.paste_modes.calls(), vec![(Some(w), 60)]);
        assert_eq!(r.paster.calls(), vec![(PasteMode::CtrlShiftV, 120)]);
    }

    #[test]
    fn progress_reports_each_stage_in_order() {
        let r = rig();
        r.stt.push_text("x");
        let mut seen = Vec::new();
        r.pipeline
            .run_with_progress(input(), &mut |p| seen.push(p))
            .unwrap();
        assert_eq!(
            seen,
            vec![PipelineProgress::Transcribing, PipelineProgress::Formatting]
        );
    }

    #[test]
    fn formatter_receives_detected_language() {
        let r = rig();
        r.stt.push(Ok(Transcription {
            text: "x".into(),
            language: Some("de".into()),
            stt_ms: 1,
            audio_ms: 10,
        }));
        r.pipeline.run(input()).unwrap();
        assert_eq!(r.formatter.languages(), vec!["de".to_string()]);
    }

    #[test]
    fn base36_matches_known_values() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    /// Complete machine + pipeline chain with a manual clock.
    #[test]
    fn full_dictation_sequence_machine_plus_pipeline() {
        use crate::dictation::{Dictation, Effect, Input, TimerId};
        let r = rig();
        r.clipboard.set_content(Some("ancien"));
        r.stt.push_text("bonjour");
        r.paster.push(PasteResult::Pasted);
        let mut d = Dictation::new(300);
        let mut phases = vec![d.phase()];
        let mut feed = |d: &mut Dictation, i: Input, clock: &FakeClock| -> Vec<Effect> {
            let fx = d.handle(i, clock.now_ms());
            for e in &fx {
                if let Effect::SetPhase { phase, .. } = e {
                    phases.push(*phase);
                }
            }
            fx
        };
        let ready = EngineInfo {
            phase: EnginePhase::Ready,
            ready: true,
            ..EngineInfo::default()
        };
        feed(&mut d, Input::EngineStatus(ready), &r.clock);
        feed(&mut d, Input::Toggle { to_hub: false }, &r.clock);
        r.clock.advance(2000);
        feed(&mut d, Input::Toggle { to_hub: false }, &r.clock);
        let fx = feed(
            &mut d,
            Input::RecordingFinished {
                duration_ms: 2000,
                run: 1,
            },
            &r.clock,
        );
        let Some(Effect::RunPipeline {
            to_hub,
            no_paste,
            stopped_at_ms,
            run,
        }) = fx.first().cloned()
        else {
            panic!("pipeline attendu : {fx:?}");
        };
        let mut i = input();
        i.to_hub = to_hub;
        i.no_paste = no_paste;
        i.stopped_at_ms = Some(stopped_at_ms);
        r.stt.set_delay(&r.clock, 500);
        let mut stages = Vec::new();
        let out = r
            .pipeline
            .run_with_progress(i, &mut |p| stages.push(p))
            .unwrap();
        for s in stages {
            let inp = match s {
                PipelineProgress::Transcribing => Input::PipelineStarted { run },
                PipelineProgress::Formatting => Input::PipelineFormatting { run },
            };
            feed(&mut d, inp, &r.clock);
        }
        feed(
            &mut d,
            Input::PipelineFinished {
                outcome: out.clone(),
                run,
            },
            &r.clock,
        );
        assert_eq!(out.latency_ms, 500);
        r.clock.advance(IDLE_DONE_MS);
        feed(&mut d, Input::TimerFired(TimerId::Idle), &r.clock);
        assert_eq!(
            phases,
            vec![
                Phase::Idle,
                Phase::Listening,
                Phase::Transcribing,
                Phase::Formatting,
                Phase::Done,
                Phase::Idle
            ]
        );
        assert!(out.restore.unwrap().execute(&r.clipboard).unwrap());
        assert_eq!(r.clipboard.content().as_deref(), Some("ancien"));
    }
}
