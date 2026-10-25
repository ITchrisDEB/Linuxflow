//! Fake components for the tests (built with `cfg(test)` or the `testing` feature).
//!
//! Reusable by the other crates (`linuxflow-core = { features = ["testing"] }` as a
//! dev-dependency). Every fake is `Clone`: the clones SHARE the same internal state, which
//! makes it possible to give one clone to the code under test (`Arc::new(fake.clone())`) and keep the other to
//! program the answers and inspect the calls.
//!
//! | Fake | Implements | Programming | Inspection |
//! |---|---|---|---|
//! | [`FakeStt`] | `SpeechToText` | `push`, `push_text`, `push_error`, `set_info`, `set_delay` | `requests` |
//! | [`FakeRecorder`] | `Recorder` | `set_recording`, `fail_start`, `fail_stop` | `calls`, `emit_level` |
//! | [`FakeClipboard`] | `Clipboard` | `set_content`, `fail_get`, `fail_set` | `content`, `set_calls` |
//! | [`FakePaster`] | `Paster` | `push` (queue; `Pasted` by default) | `calls` |
//! | [`FakeWindow`] | `ActiveWindow` | `set` | |
//! | [`FakeClock`] | `Clock` | `advance`, `set` | `now_iso_value` |
//! | [`FakeFormatter`] | `TextFormatter` | `set_fixed` | `languages` |
//! | [`FakeHistory`] | `HistorySink` | | `entries` |
//! | [`FakePasteModes`] | `PasteModeResolver` | `set` | `calls` |
//! | [`FakeHints`] | `DictionaryHints` | `set` | |
//!
//! These fakes never sleep and touch no hardware. Time only passes through
//! [`FakeClock::advance`] (or [`FakeStt::set_delay`], which advances the clock during a transcription).

use crate::domain::*;
use crate::error::{Error, Result, SttError};
use crate::pipeline::{DictionaryHints, HistorySink, PasteModeResolver, TextFormatter};
use crate::traits::{ActiveWindow, Clipboard, Clock, Paster, Recorder, SpeechToText};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- Horloge ----

/// Clock with manual advance. `now_iso` is computed from Unix ms (UTC).
#[derive(Clone, Debug)]
pub struct FakeClock {
    ms: Arc<AtomicU64>,
}

impl FakeClock {
    pub fn new(start_ms: u64) -> Self {
        Self {
            ms: Arc::new(AtomicU64::new(start_ms)),
        }
    }
    pub fn advance(&self, ms: u64) {
        self.ms.fetch_add(ms, Ordering::SeqCst);
    }
    pub fn set(&self, ms: u64) {
        self.ms.store(ms, Ordering::SeqCst);
    }
    /// Current value of `now_iso()` (convenient for assertions).
    pub fn now_iso_value(&self) -> String {
        self.now_iso()
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Civil date (year, month, day) from a Unix day count (algorithm by H. Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.ms.load(Ordering::SeqCst)
    }
    fn now_iso(&self) -> String {
        let ms = self.now_ms();
        let (y, mo, d) = civil_from_days((ms / 86_400_000) as i64);
        let day_ms = ms % 86_400_000;
        format!(
            "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
            day_ms / 3_600_000,
            day_ms / 60_000 % 60,
            day_ms / 1000 % 60,
            day_ms % 1000
        )
    }
    fn today_local(&self) -> String {
        let (y, mo, d) = civil_from_days((self.now_ms() / 86_400_000) as i64);
        format!("{y:04}-{mo:02}-{d:02}")
    }
}

// ---- Transcription ----

#[derive(Default)]
struct SttState {
    queue: VecDeque<std::result::Result<Transcription, SttError>>,
    requests: Vec<TranscribeRequest>,
    info: EngineInfo,
    delay: Option<(FakeClock, u64)>,
}

/// Programmable transcription engine: returns the pushed results, in order. Empty queue:
/// `Err(SttError::NotReady)`.
#[derive(Clone, Default)]
pub struct FakeStt {
    inner: Arc<Mutex<SttState>>,
}

impl FakeStt {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&self, result: std::result::Result<Transcription, SttError>) {
        lock(&self.inner).queue.push_back(result);
    }
    /// Simple successful result (unknown language, 100 ms of engine, 1 s of audio).
    pub fn push_text(&self, text: &str) {
        self.push(Ok(Transcription {
            text: text.into(),
            language: None,
            stt_ms: 100,
            audio_ms: 1000,
        }));
    }
    pub fn push_error(&self, error: SttError) {
        self.push(Err(error));
    }
    pub fn set_info(&self, info: EngineInfo) {
        lock(&self.inner).info = info;
    }
    /// Each transcription advances `clock` by `ms` (simulates the inference time).
    pub fn set_delay(&self, clock: &FakeClock, ms: u64) {
        lock(&self.inner).delay = Some((clock.clone(), ms));
    }
    pub fn requests(&self) -> Vec<TranscribeRequest> {
        lock(&self.inner).requests.clone()
    }
}

impl SpeechToText for FakeStt {
    fn info(&self) -> EngineInfo {
        lock(&self.inner).info.clone()
    }
    fn transcribe(
        &self,
        request: &TranscribeRequest,
    ) -> std::result::Result<Transcription, SttError> {
        let mut s = lock(&self.inner);
        s.requests.push(request.clone());
        if let Some((clock, ms)) = &s.delay {
            clock.advance(*ms);
        }
        s.queue.pop_front().unwrap_or(Err(SttError::NotReady))
    }
}

// ---- Capture ----

/// Call received by [`FakeRecorder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderCall {
    Start {
        device: Option<String>,
        max_sec: u64,
    },
    Stop,
    Cancel,
}

#[derive(Default)]
struct RecorderState {
    calls: Vec<RecorderCall>,
    recording: Recording,
    fail_start: Option<String>,
    fail_stop: Option<String>,
    sink: Option<Arc<dyn Fn(f32) + Send + Sync>>,
}

/// Fake microphone: `stop` returns the programmed recording (`set_recording`, empty by default).
#[derive(Clone, Default)]
pub struct FakeRecorder {
    inner: Arc<Mutex<RecorderState>>,
}

impl FakeRecorder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_recording(&self, recording: Recording) {
        lock(&self.inner).recording = recording;
    }
    /// Silence of `ms` milliseconds at 16 kHz.
    pub fn set_silence_ms(&self, ms: u64) {
        let n = (ms * u64::from(crate::constants::SAMPLE_RATE_HZ) / 1000) as usize;
        self.set_recording(Recording {
            samples: vec![0.0; n],
        });
    }
    pub fn fail_start(&self, message: Option<&str>) {
        lock(&self.inner).fail_start = message.map(str::to_string);
    }
    pub fn fail_stop(&self, message: Option<&str>) {
        lock(&self.inner).fail_stop = message.map(str::to_string);
    }
    pub fn calls(&self) -> Vec<RecorderCall> {
        lock(&self.inner).calls.clone()
    }
    /// Sends a sound level to the receiver registered by `set_level_sink`.
    pub fn emit_level(&self, level: f32) {
        let sink = lock(&self.inner).sink.clone();
        if let Some(sink) = sink {
            sink(level);
        }
    }
}

impl Recorder for FakeRecorder {
    fn start(&self, device: Option<&str>, max_sec: u64) -> Result<()> {
        let mut s = lock(&self.inner);
        s.calls.push(RecorderCall::Start {
            device: device.map(str::to_string),
            max_sec,
        });
        match &s.fail_start {
            Some(m) => Err(Error::msg(m.clone())),
            None => Ok(()),
        }
    }
    fn stop(&self) -> Result<Recording> {
        let mut s = lock(&self.inner);
        s.calls.push(RecorderCall::Stop);
        match &s.fail_stop {
            Some(m) => Err(Error::msg(m.clone())),
            None => Ok(s.recording.clone()),
        }
    }
    fn cancel(&self) {
        lock(&self.inner).calls.push(RecorderCall::Cancel);
    }
    fn set_level_sink(&self, sink: Box<dyn Fn(f32) + Send + Sync>) {
        lock(&self.inner).sink = Some(Arc::from(sink));
    }
}

// ---- Presse-papiers ----

#[derive(Default)]
struct ClipboardState {
    content: Option<String>,
    set_calls: Vec<String>,
    fail_get: bool,
    fail_set: bool,
}

/// In-memory clipboard.
#[derive(Clone, Default)]
pub struct FakeClipboard {
    inner: Arc<Mutex<ClipboardState>>,
}

impl FakeClipboard {
    pub fn new() -> Self {
        Self::default()
    }
    /// Changes the content without counting as a `set_text` (simulates a user copy).
    pub fn set_content(&self, text: Option<&str>) {
        lock(&self.inner).content = text.map(str::to_string);
    }
    pub fn content(&self) -> Option<String> {
        lock(&self.inner).content.clone()
    }
    /// Texts written through `set_text`, in order.
    pub fn set_calls(&self) -> Vec<String> {
        lock(&self.inner).set_calls.clone()
    }
    pub fn fail_get(&self, fail: bool) {
        lock(&self.inner).fail_get = fail;
    }
    pub fn fail_set(&self, fail: bool) {
        lock(&self.inner).fail_set = fail;
    }
}

impl Clipboard for FakeClipboard {
    fn get_text(&self) -> Result<Option<String>> {
        let s = lock(&self.inner);
        if s.fail_get {
            return Err(Error::msg("clipboard read impossible"));
        }
        Ok(s.content.clone())
    }
    fn set_text(&self, text: &str) -> Result<()> {
        let mut s = lock(&self.inner);
        if s.fail_set {
            return Err(Error::msg("clipboard write impossible"));
        }
        s.content = Some(text.to_string());
        s.set_calls.push(text.to_string());
        Ok(())
    }
}

// ---- Paste, window ----

#[derive(Default)]
struct PasterState {
    queue: VecDeque<PasteResult>,
    calls: Vec<(PasteMode, u64)>,
}

/// Fake paste: returns the pushed results (`Pasted` when the queue is empty).
#[derive(Clone, Default)]
pub struct FakePaster {
    inner: Arc<Mutex<PasterState>>,
}

impl FakePaster {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&self, result: PasteResult) {
        lock(&self.inner).queue.push_back(result);
    }
    /// Calls received: (mode, delay).
    pub fn calls(&self) -> Vec<(PasteMode, u64)> {
        lock(&self.inner).calls.clone()
    }
}

impl Paster for FakePaster {
    fn paste(&self, mode: PasteMode, delay_ms: u64) -> PasteResult {
        let mut s = lock(&self.inner);
        s.calls.push((mode, delay_ms));
        s.queue.pop_front().unwrap_or(PasteResult::Pasted)
    }
}

/// Programmable foreground window (none by default).
#[derive(Clone, Default)]
pub struct FakeWindow {
    inner: Arc<Mutex<Option<WindowInfo>>>,
}

impl FakeWindow {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&self, window: Option<WindowInfo>) {
        *lock(&self.inner) = window;
    }
}

impl ActiveWindow for FakeWindow {
    fn current(&self) -> Option<WindowInfo> {
        lock(&self.inner).clone()
    }
}

// ---- Formatting, history, resolution, hints ----

#[derive(Default)]
struct FormatterState {
    fixed: Option<String>,
    languages: Vec<String>,
}

/// Fake formatter. By default: multiple spaces collapsed and `trim`. `set_fixed(Some(t))`
/// forces the formatted text `t` (including empty). Counts the words by splitting on spaces.
#[derive(Clone, Default)]
pub struct FakeFormatter {
    inner: Arc<Mutex<FormatterState>>,
}

impl FakeFormatter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_fixed(&self, text: Option<&str>) {
        lock(&self.inner).fixed = text.map(str::to_string);
    }
    /// Languages received by `format`, in order.
    pub fn languages(&self) -> Vec<String> {
        lock(&self.inner).languages.clone()
    }
}

impl TextFormatter for FakeFormatter {
    fn format(&self, raw: &str, language: &str) -> String {
        let mut s = lock(&self.inner);
        s.languages.push(language.to_string());
        match &s.fixed {
            Some(t) => t.clone(),
            None => raw.split_whitespace().collect::<Vec<_>>().join(" "),
        }
    }
    fn count_words(&self, text: &str) -> u32 {
        text.split_whitespace().count() as u32
    }
}

/// In-memory history (no cap).
#[derive(Clone, Default)]
pub struct FakeHistory {
    inner: Arc<Mutex<Vec<HistoryEntry>>>,
}

impl FakeHistory {
    pub fn new() -> Self {
        Self::default()
    }
    /// Entries in order of addition.
    pub fn entries(&self) -> Vec<HistoryEntry> {
        lock(&self.inner).clone()
    }
}

impl HistorySink for FakeHistory {
    fn add(&self, entry: HistoryEntry) {
        lock(&self.inner).push(entry);
    }
}

#[derive(Default)]
struct PasteModesState {
    forced: Option<(PasteMode, u64)>,
    calls: Vec<(Option<WindowInfo>, u64)>,
}

/// Paste-mode resolver: `CtrlV` with the default delay, except `set(mode, delay)`.
#[derive(Clone, Default)]
pub struct FakePasteModes {
    inner: Arc<Mutex<PasteModesState>>,
}

impl FakePasteModes {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&self, mode: PasteMode, delay_ms: u64) {
        lock(&self.inner).forced = Some((mode, delay_ms));
    }
    pub fn calls(&self) -> Vec<(Option<WindowInfo>, u64)> {
        lock(&self.inner).calls.clone()
    }
}

impl PasteModeResolver for FakePasteModes {
    fn resolve(&self, window: Option<&WindowInfo>, default_delay_ms: u64) -> (PasteMode, u64) {
        let mut s = lock(&self.inner);
        s.calls.push((window.cloned(), default_delay_ms));
        s.forced.unwrap_or((PasteMode::CtrlV, default_delay_ms))
    }
}

/// Programmable dictionary hints (empty by default).
#[derive(Clone, Default)]
pub struct FakeHints {
    inner: Arc<Mutex<Vec<String>>>,
}

impl FakeHints {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&self, hints: Vec<String>) {
        *lock(&self.inner) = hints;
    }
}

impl DictionaryHints for FakeHints {
    fn hints(&self) -> Vec<String> {
        lock(&self.inner).clone()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_iso_and_date() {
        let c = FakeClock::new(1_700_000_000_123);
        assert_eq!(c.now_iso(), "2023-11-14T22:13:20.123Z");
        assert_eq!(c.today_local(), "2023-11-14");
        c.advance(1000);
        assert_eq!(c.now_ms(), 1_700_000_001_123);
        assert_eq!(FakeClock::new(0).now_iso(), "1970-01-01T00:00:00.000Z");
        // 29 February of a leap year.
        assert_eq!(
            FakeClock::new(1_709_164_800_000).today_local(),
            "2024-02-29"
        );
    }

    #[test]
    fn clones_share_state() {
        let a = FakeClipboard::new();
        let b = a.clone();
        b.set_text("x").unwrap_or_default();
        assert_eq!(a.content().as_deref(), Some("x"));
        assert_eq!(a.set_calls(), vec!["x".to_string()]);
    }

    #[test]
    fn stt_queue_and_default() {
        let s = FakeStt::new();
        let req = TranscribeRequest {
            audio: Recording::default(),
            language: None,
            hints: vec![],
        };
        assert_eq!(s.transcribe(&req), Err(SttError::NotReady));
        s.push_text("a");
        assert_eq!(s.transcribe(&req).map(|t| t.text), Ok("a".to_string()));
        assert_eq!(s.requests().len(), 2);
    }

    #[test]
    fn recorder_logs_calls_and_levels() {
        let r = FakeRecorder::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        r.set_level_sink(Box::new(move |l| lock(&seen2).push(l)));
        r.set_silence_ms(500);
        assert!(r.start(Some("mic"), 10).is_ok());
        r.emit_level(0.5);
        assert_eq!(r.stop().map(|x| x.duration_ms()).unwrap_or(0), 500);
        r.cancel();
        assert_eq!(
            r.calls(),
            vec![
                RecorderCall::Start {
                    device: Some("mic".into()),
                    max_sec: 10
                },
                RecorderCall::Stop,
                RecorderCall::Cancel
            ]
        );
        assert_eq!(*lock(&seen), vec![0.5]);
        r.fail_start(Some("no microphone"));
        assert!(r.start(None, 1).is_err());
    }
}
