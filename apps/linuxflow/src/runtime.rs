//! Dictation orchestration: executes the `Effect`s of the `core::dictation` machine.
//!
//! The logic is independent of GTK: timers, thread executors and event output
//! are traits, which makes it possible to test the complete sequence with the fakes of the core.
//! The inputs (`Input`) go through a queue: the effects of an input are executed in order
//! before the next input is processed (even if it is produced by an effect).

use crate::ui_text::user_message;
use linuxflow_core::dictation::{Dictation, Effect, Input, TimerId};
use linuxflow_core::domain::{EngineInfo, Phase, Recording, Settings};
use linuxflow_core::events::{Event, OverlayState};
use linuxflow_core::pipeline::{OutcomeKind, Pipeline, PipelineInput, PipelineProgress};
use linuxflow_core::traits::{Clipboard, Clock, Recorder};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Destination of the events pushed to the interface.
pub trait EventSink: Send + Sync {
    fn send(&self, event: Event);
}

/// Cancellable timers, one per `TimerId`, plus anonymous deferred tasks.
pub trait TimerHost: Send + Sync {
    /// Arms `id` (replaces a timer with the same identifier).
    fn schedule(&self, id: TimerId, delay_ms: u64, fire: Box<dyn FnOnce() + Send>);
    fn cancel(&self, id: TimerId);
    /// Non-cancellable deferred task (clipboard restoration).
    fn after(&self, delay_ms: u64, job: Box<dyn FnOnce() + Send>);
    /// Runs IMMEDIATELY the deferred tasks still pending (application shutdown: a
    /// clipboard restoration must not be lost with the runtime). Default: nothing.
    fn flush(&self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Serial queue: start, stop and cancellation of the capture in order.
    Audio,
    /// One thread per pipeline run.
    Pipeline,
}

pub trait Executor: Send + Sync {
    fn run(&self, lane: Lane, job: Box<dyn FnOnce() + Send>);
}

type PhaseListener = Box<dyn Fn(Phase) + Send + Sync>;
type PushStateFn = Box<dyn Fn() + Send + Sync>;

pub struct DictatorDeps {
    pub recorder: Arc<dyn Recorder>,
    pub pipeline: Arc<Pipeline>,
    pub clipboard: Arc<dyn Clipboard>,
    pub clock: Arc<dyn Clock>,
    pub settings: Arc<dyn Fn() -> Settings + Send + Sync>,
    pub events: Arc<dyn EventSink>,
    pub timers: Arc<dyn TimerHost>,
    pub executor: Arc<dyn Executor>,
}

struct Inner {
    deps: DictatorDeps,
    machine: Mutex<Dictation>,
    queue: Mutex<VecDeque<Input>>,
    draining: AtomicBool,
    /// Captured audio and the run (`run`) that produced it: never consumed by another run.
    pending_audio: Mutex<Option<(u64, Recording)>>,
    /// Incremented at every start and cancellation: discards the late results of a cancelled pipeline.
    phase_listeners: Mutex<Vec<PhaseListener>>,
    push_state: Mutex<Option<PushStateFn>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Dictation driver (cloneable).
#[derive(Clone)]
pub struct Dictator {
    inner: Arc<Inner>,
}

impl Dictator {
    pub fn new(deps: DictatorDeps) -> Self {
        let max = (deps.settings)().max_dictation_sec;
        let events = deps.events.clone();
        deps.recorder
            .set_level_sink(Box::new(move |l| events.send(Event::Level(l))));
        Self {
            inner: Arc::new(Inner {
                deps,
                machine: Mutex::new(Dictation::new(max)),
                queue: Mutex::new(VecDeque::new()),
                draining: AtomicBool::new(false),
                pending_audio: Mutex::new(None),
                phase_listeners: Mutex::new(Vec::new()),
                push_state: Mutex::new(None),
            }),
        }
    }

    pub fn phase(&self) -> Phase {
        lock(&self.inner.machine).phase()
    }

    /// Runs the pending deferred tasks right away (orderly application shutdown).
    pub fn flush_pending(&self) {
        self.inner.deps.timers.flush();
    }

    /// Called at every visible phase change (synchronises push-to-talk, the tray).
    pub fn on_phase(&self, f: impl Fn(Phase) + Send + Sync + 'static) {
        lock(&self.inner.phase_listeners).push(Box::new(f));
    }

    /// Called for every `Effect::PushState`.
    pub fn on_push_state(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.inner.push_state) = Some(Box::new(f));
    }

    pub fn set_max_dictation_sec(&self, sec: u64) {
        lock(&self.inner.machine).set_max_dictation_sec(sec);
    }

    pub fn engine_status(&self, info: EngineInfo) {
        self.dispatch(Input::EngineStatus(info));
    }

    pub fn toggle(&self, to_hub: bool) -> Phase {
        self.dispatch(Input::Toggle { to_hub });
        self.phase()
    }

    pub fn start(&self, to_hub: bool, no_paste: bool) -> Phase {
        self.dispatch(Input::Start { to_hub, no_paste });
        self.phase()
    }

    pub fn stop(&self) -> Phase {
        self.dispatch(Input::Stop);
        self.phase()
    }

    pub fn cancel(&self) -> Phase {
        self.dispatch(Input::Cancel);
        self.phase()
    }

    /// Queues an input and drains the queue if nobody is already doing so.
    pub fn dispatch(&self, input: Input) {
        lock(&self.inner.queue).push_back(input);
        if self.inner.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        loop {
            let next = lock(&self.inner.queue).pop_front();
            match next {
                Some(i) => self.step(i),
                None => {
                    self.inner.draining.store(false, Ordering::SeqCst);
                    if lock(&self.inner.queue).is_empty()
                        || self.inner.draining.swap(true, Ordering::SeqCst)
                    {
                        break;
                    }
                }
            }
        }
    }

    fn step(&self, input: Input) {
        let now = self.inner.deps.clock.now_ms();
        let effects = lock(&self.inner.machine).handle(input, now);
        for e in effects {
            self.apply(e);
        }
    }

    fn apply(&self, effect: Effect) {
        let d = &self.inner.deps;
        match effect {
            Effect::SetPhase {
                phase,
                message,
                latency_ms,
            } => {
                let text = message.as_ref().map(user_message);
                d.events.send(Event::Phase {
                    phase,
                    message: text.clone(),
                });
                d.events.send(Event::Overlay(OverlayState {
                    phase,
                    message: text,
                    latency_ms,
                }));
                for l in lock(&self.inner.phase_listeners).iter() {
                    l(phase);
                }
            }
            Effect::ScheduleTimer { id, delay_ms } => {
                let this = self.clone();
                d.timers.schedule(
                    id,
                    delay_ms,
                    Box::new(move || this.dispatch(Input::TimerFired(id))),
                );
            }
            Effect::CancelTimer(id) => d.timers.cancel(id),
            Effect::StartRecording { max_sec, run } => {
                let this = self.clone();
                d.executor.run(
                    Lane::Audio,
                    Box::new(move || {
                        let device = (this.inner.deps.settings)().microphone_id;
                        let device = (!device.trim().is_empty()).then_some(device);
                        if let Err(e) = this.inner.deps.recorder.start(device.as_deref(), max_sec) {
                            tracing::warn!("capture start failed: {e}");
                            this.dispatch(Input::RecordingFailed {
                                message: e.to_string(),
                                run,
                            });
                        } else {
                            // The log traces the dictation (never the text).
                            tracing::info!("dictation: listening (max {max_sec} s)");
                        }
                    }),
                );
            }
            Effect::StopRecording { run } => {
                let this = self.clone();
                d.executor.run(
                    Lane::Audio,
                    Box::new(move || match this.inner.deps.recorder.stop() {
                        Ok(rec) => {
                            let duration_ms = rec.duration_ms();
                            tracing::info!("audio: {duration_ms} ms captured");
                            *lock(&this.inner.pending_audio) = Some((run, rec));
                            this.dispatch(Input::RecordingFinished { duration_ms, run });
                        }
                        Err(e) => {
                            tracing::warn!("capture stop failed: {e}");
                            this.dispatch(Input::RecordingFailed {
                                message: e.to_string(),
                                run,
                            });
                        }
                    }),
                );
            }
            Effect::CancelRecording => {
                *lock(&self.inner.pending_audio) = None;
                let this = self.clone();
                d.executor.run(
                    Lane::Audio,
                    Box::new(move || this.inner.deps.recorder.cancel()),
                );
            }
            Effect::RunPipeline {
                to_hub,
                no_paste,
                stopped_at_ms,
                run,
            } => self.run_pipeline(to_hub, no_paste, stopped_at_ms, run),
            Effect::ShowOverlay => {} // the pill shows as soon as it receives an active phase
            Effect::HideOverlay => d.events.send(Event::Overlay(OverlayState {
                phase: Phase::Idle,
                message: None,
                latency_ms: None,
            })),
            Effect::PushState => {
                if let Some(f) = lock(&self.inner.push_state).as_ref() {
                    f();
                }
            }
        }
    }

    fn run_pipeline(&self, to_hub: bool, no_paste: bool, stopped_at_ms: u64, run: u64) {
        // Only the audio of THIS run is used: audio from a cancelled run is discarded.
        let audio = {
            let mut pending = lock(&self.inner.pending_audio);
            match pending.take() {
                Some((r, rec)) if r == run => Some(rec),
                other => {
                    *pending = other.filter(|(r, _)| *r > run);
                    None
                }
            }
        };
        let Some(audio) = audio else {
            self.dispatch(Input::PipelineFailed {
                message: "no audio".into(),
                run,
            });
            return;
        };
        let this = self.clone();
        self.inner.deps.executor.run(
            Lane::Pipeline,
            Box::new(move || {
                let d = &this.inner.deps;
                let settings = (d.settings)();
                let input = PipelineInput::from_settings(
                    audio,
                    &settings,
                    to_hub,
                    no_paste,
                    Some(stopped_at_ms),
                );
                let progress_this = this.clone();
                let result = d.pipeline.run_with_progress(input, &mut |p| {
                    progress_this.dispatch(match p {
                        PipelineProgress::Transcribing => Input::PipelineStarted { run },
                        PipelineProgress::Formatting => Input::PipelineFormatting { run },
                    })
                });
                match result {
                    Ok(outcome) => {
                        if outcome.kind == OutcomeKind::Empty {
                            tracing::info!(
                                "dictation: nothing heard (audio {} ms, stt {} ms)",
                                outcome.audio_ms,
                                outcome.stt_ms
                            );
                        } else {
                            tracing::info!(
                                "dictation: {} words, stt {} ms, latency {} ms, paste={:?}",
                                outcome.text.split_whitespace().count(),
                                outcome.stt_ms,
                                outcome.latency_ms,
                                outcome.pasted
                            );
                        }
                        // No new dictation or cancellation since: otherwise the text is not inserted.
                        let current = lock(&this.inner.machine).run() == run;
                        if let Some(restore) = outcome.restore.clone() {
                            let clipboard = d.clipboard.clone();
                            d.timers.after(
                                restore.after_ms,
                                Box::new(move || {
                                    if let Err(e) = restore.execute(&*clipboard) {
                                        tracing::warn!("clipboard restore failed: {e}");
                                    }
                                }),
                            );
                        }
                        if current && outcome.kind != OutcomeKind::Empty {
                            if let Some(text) = outcome.insert_text.clone() {
                                d.events.send(Event::InsertText(text));
                            }
                        }
                        this.dispatch(Input::PipelineFinished { outcome, run });
                    }
                    Err(e) => {
                        tracing::warn!("pipeline failed: {e}");
                        this.dispatch(Input::PipelineFailed {
                            message: e.to_string(),
                            run,
                        });
                    }
                }
            }),
        );
    }
}

// ---- Production implementations -------------------------------------------------------------

/// Timers on a tokio runtime.
/// Deferred task executed once.
type DeferredJob = Box<dyn FnOnce() + Send>;

pub struct TokioTimers {
    handle: tokio::runtime::Handle,
    /// Armed timers: (sequence number, handle). The number discards a replaced task that
    /// had already left its wait when `abort()` arrives.
    tasks: Arc<Mutex<std::collections::HashMap<TimerKey, (u64, tokio::task::AbortHandle)>>>,
    /// Pending deferred tasks, executable all at once at shutdown.
    deferred: Arc<Mutex<std::collections::HashMap<u64, DeferredJob>>>,
    seq: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
pub struct TimerKey(u8);

fn key(id: TimerId) -> TimerKey {
    TimerKey(match id {
        TimerId::Idle => 0,
        TimerId::Hide => 1,
        TimerId::MaxDictation => 2,
    })
}

impl TokioTimers {
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self {
            handle,
            tasks: Arc::new(Mutex::new(Default::default())),
            deferred: Arc::new(Mutex::new(Default::default())),
            seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl TimerHost for TokioTimers {
    fn schedule(&self, id: TimerId, delay_ms: u64, fire: Box<dyn FnOnce() + Send>) {
        let seq = self.next_seq();
        let k = key(id);
        let tasks = self.tasks.clone();
        let task = self.handle.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            // Is this timer still the current one? Otherwise it was replaced or cancelled.
            {
                let mut armed = lock(&tasks);
                match armed.get(&k) {
                    Some((current, _)) if *current == seq => {
                        armed.remove(&k);
                    }
                    _ => return,
                }
            }
            let _ = tokio::task::spawn_blocking(fire).await;
        });
        if let Some((_, old)) = lock(&self.tasks).insert(k, (seq, task.abort_handle())) {
            old.abort();
        }
    }

    fn cancel(&self, id: TimerId) {
        if let Some((_, t)) = lock(&self.tasks).remove(&key(id)) {
            t.abort();
        }
    }

    fn after(&self, delay_ms: u64, job: Box<dyn FnOnce() + Send>) {
        let seq = self.next_seq();
        lock(&self.deferred).insert(seq, job);
        let deferred = self.deferred.clone();
        self.handle.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            // Already executed by `flush`? Then it is no longer in the table.
            let job = lock(&deferred).remove(&seq);
            if let Some(job) = job {
                let _ = tokio::task::spawn_blocking(job).await;
            }
        });
    }

    fn flush(&self) {
        let pending: Vec<_> = lock(&self.deferred).drain().map(|(_, job)| job).collect();
        for job in pending {
            job();
        }
    }
}

/// Production executor: one serial thread for audio, one thread per pipeline.
pub struct ThreadExecutor {
    audio: Mutex<std::sync::mpsc::Sender<Box<dyn FnOnce() + Send>>>,
}

impl ThreadExecutor {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let spawned = std::thread::Builder::new()
            .name("linuxflow-audio".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job();
                }
            });
        if let Err(e) = spawned {
            tracing::error!("audio thread could not start: {e}");
        }
        Self {
            audio: Mutex::new(tx),
        }
    }
}

impl Default for ThreadExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor for ThreadExecutor {
    fn run(&self, lane: Lane, job: Box<dyn FnOnce() + Send>) {
        match lane {
            Lane::Audio => {
                if lock(&self.audio).send(job).is_err() {
                    tracing::error!("audio thread is gone");
                }
            }
            Lane::Pipeline => {
                if let Err(e) = std::thread::Builder::new()
                    .name("linuxflow-pipeline".into())
                    .spawn(job)
                {
                    tracing::error!("pipeline thread could not start: {e}");
                }
            }
        }
    }
}

/// System clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn now_iso(&self) -> String {
        use time::format_description::well_known::Rfc3339;
        time::OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default()
    }

    fn today_local(&self) -> String {
        let iso = self.now_iso();
        linuxflow_core::stats::local_date_of(&iso, None)
            .unwrap_or_else(|| iso.chars().take(10).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxflow_core::domain::{
        DeviceKind, EnginePhase, PasteDisposition, PasteResult, WindowInfo,
    };
    use linuxflow_core::error::SttError;
    use linuxflow_core::pipeline::PipelineDeps;
    use linuxflow_core::testing::*;

    #[derive(Default)]
    struct VecSink(Mutex<Vec<Event>>);
    impl EventSink for VecSink {
        fn send(&self, e: Event) {
            lock(&self.0).push(e);
        }
    }

    /// Manual timers: nothing fires until the test pulls the trigger.
    type Job = Box<dyn FnOnce() + Send>;

    #[derive(Default)]
    struct ManualTimers {
        armed: Mutex<Vec<(TimerId, u64, Job)>>,
        after: Mutex<Vec<(u64, Job)>>,
    }
    impl TimerHost for ManualTimers {
        fn schedule(&self, id: TimerId, delay_ms: u64, fire: Box<dyn FnOnce() + Send>) {
            let mut a = lock(&self.armed);
            a.retain(|(i, _, _)| *i != id);
            a.push((id, delay_ms, fire));
        }
        fn cancel(&self, id: TimerId) {
            lock(&self.armed).retain(|(i, _, _)| *i != id);
        }
        fn after(&self, delay_ms: u64, job: Box<dyn FnOnce() + Send>) {
            lock(&self.after).push((delay_ms, job));
        }
    }
    impl ManualTimers {
        fn armed_ids(&self) -> Vec<(TimerId, u64)> {
            lock(&self.armed).iter().map(|(i, d, _)| (*i, *d)).collect()
        }
        fn fire(&self, id: TimerId) -> bool {
            let f = {
                let mut a = lock(&self.armed);
                let pos = a.iter().position(|(i, _, _)| *i == id);
                pos.map(|p| a.remove(p).2)
            };
            match f {
                Some(f) => {
                    f();
                    true
                }
                None => false,
            }
        }
        fn run_after(&self) -> Vec<u64> {
            let jobs: Vec<_> = lock(&self.after).drain(..).collect();
            let delays = jobs.iter().map(|(d, _)| *d).collect();
            for (_, j) in jobs {
                j();
            }
            delays
        }
    }

    struct Inline;
    impl Executor for Inline {
        fn run(&self, _lane: Lane, job: Box<dyn FnOnce() + Send>) {
            job();
        }
    }

    struct Rig {
        d: Dictator,
        events: Arc<VecSink>,
        timers: Arc<ManualTimers>,
        recorder: FakeRecorder,
        stt: FakeStt,
        clipboard: FakeClipboard,
        paster: FakePaster,
        window: FakeWindow,
        history: FakeHistory,
        clock: FakeClock,
        settings: Arc<Mutex<Settings>>,
        phases: Arc<Mutex<Vec<Phase>>>,
    }

    fn rig() -> Rig {
        let recorder = FakeRecorder::new();
        let stt = FakeStt::new();
        let clipboard = FakeClipboard::new();
        let paster = FakePaster::new();
        let window = FakeWindow::new();
        let history = FakeHistory::new();
        let clock = FakeClock::new(1_000_000);
        let settings = Arc::new(Mutex::new(Settings::default()));
        let events = Arc::new(VecSink::default());
        let timers = Arc::new(ManualTimers::default());
        let pipeline = Pipeline::new(PipelineDeps {
            stt: Arc::new(stt.clone()),
            clipboard: Arc::new(clipboard.clone()),
            paster: Arc::new(paster.clone()),
            window: Arc::new(window.clone()),
            clock: Arc::new(clock.clone()),
            formatter: Arc::new(FakeFormatter::new()),
            history: Arc::new(history.clone()),
            paste_modes: Arc::new(FakePasteModes::new()),
            hints: Arc::new(FakeHints::new()),
        });
        let s2 = settings.clone();
        let d = Dictator::new(DictatorDeps {
            recorder: Arc::new(recorder.clone()),
            pipeline: Arc::new(pipeline),
            clipboard: Arc::new(clipboard.clone()),
            clock: Arc::new(clock.clone()),
            settings: Arc::new(move || lock(&s2).clone()),
            events: events.clone(),
            timers: timers.clone(),
            executor: Arc::new(Inline),
        });
        let phases = Arc::new(Mutex::new(Vec::new()));
        let p2 = phases.clone();
        d.on_phase(move |p| lock(&p2).push(p));
        d.engine_status(EngineInfo {
            phase: EnginePhase::Ready,
            ready: true,
            device: DeviceKind::Cpu,
            ..EngineInfo::default()
        });
        recorder.set_silence_ms(2000);
        Rig {
            d,
            events,
            timers,
            recorder,
            stt,
            clipboard,
            paster,
            window,
            history,
            clock,
            settings,
            phases,
        }
    }

    fn phases(r: &Rig) -> Vec<Phase> {
        lock(&r.phases).clone()
    }

    fn timers() -> (tokio::runtime::Runtime, TokioTimers) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let t = TokioTimers::new(rt.handle().clone());
        (rt, t)
    }

    #[test]
    fn a_replaced_timer_never_fires_the_old_callback() {
        // Regression: a task that had already left its wait was not stopped by abort() and fired
        // while the new timer with the same identifier was armed.
        let (_rt, t) = timers();
        let fired = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let a = fired.clone();
        t.schedule(TimerId::Idle, 40, Box::new(move || lock(&a).push("old")));
        let b = fired.clone();
        t.schedule(TimerId::Idle, 120, Box::new(move || lock(&b).push("new")));
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(*lock(&fired), vec!["new"]);
    }

    #[test]
    fn a_cancelled_timer_never_fires() {
        let (_rt, t) = timers();
        let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let f = fired.clone();
        t.schedule(
            TimerId::Hide,
            30,
            Box::new(move || {
                f.fetch_add(1, Ordering::SeqCst);
            }),
        );
        t.cancel(TimerId::Hide);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn flush_runs_pending_deferred_jobs_once_at_shutdown() {
        // Regression: at shutdown, the scheduled clipboard restoration was lost.
        let (_rt, t) = timers();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = ran.clone();
        t.after(
            60_000,
            Box::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
            }),
        );
        t.flush();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        t.flush(); // idempotent
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn full_dictation_pastes_and_records_history() {
        let r = rig();
        r.window.set(Some(WindowInfo {
            app_id: Some("editor".into()),
            ..WindowInfo::default()
        }));
        r.clipboard.set_content(Some("previous"));
        r.stt.push_text("hello   world");
        assert_eq!(r.d.toggle(false), Phase::Listening);
        assert!(r
            .timers
            .armed_ids()
            .contains(&(TimerId::MaxDictation, 300_000)));
        r.clock.advance(500);
        // Shutdown: the phase ends at Done (everything is online), idle armed at 900 ms.
        r.d.toggle(false);
        assert_eq!(r.d.phase(), Phase::Done);
        assert_eq!(
            phases(&r),
            vec![
                Phase::Listening,
                Phase::Transcribing,
                Phase::Formatting,
                Phase::Done
            ]
        );
        assert!(!r
            .timers
            .armed_ids()
            .iter()
            .any(|(i, _)| *i == TimerId::MaxDictation));
        let entries = r.history.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "hello world");
        assert_eq!(entries[0].pasted, PasteDisposition::Pasted);
        assert_eq!(entries[0].app.as_deref(), Some("editor"));
        assert_eq!(r.paster.calls().len(), 1);
        // Deferred clipboard restoration.
        assert_eq!(r.clipboard.content().as_deref(), Some("hello world"));
        assert_eq!(r.timers.run_after(), vec![400]);
        assert_eq!(r.clipboard.content().as_deref(), Some("previous"));
        // Return to rest, then the pill is hidden 420 ms later.
        assert!(r.timers.armed_ids().contains(&(TimerId::Idle, 900)));
        assert!(r.timers.fire(TimerId::Idle));
        assert_eq!(r.d.phase(), Phase::Idle);
        assert!(r.timers.armed_ids().contains(&(TimerId::Hide, 420)));
        let before = lock(&r.events.0).len();
        assert!(r.timers.fire(TimerId::Hide));
        let ev = lock(&r.events.0).clone();
        assert_eq!(ev.len(), before + 1);
        assert!(matches!(
            ev.last(),
            Some(Event::Overlay(OverlayState {
                phase: Phase::Idle,
                ..
            }))
        ));
    }

    /// Endurance: 200 consecutive dictations. Each cycle returns to rest, the
    /// clipboard is restored, no timer or deferred task accumulates, the number of
    /// threads and file descriptors does not grow.
    #[test]
    fn two_hundred_consecutive_dictations_leave_nothing_behind() {
        fn process_counts() -> (usize, usize) {
            let threads = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find_map(|l| l.strip_prefix("Threads:").map(|v| v.trim().parse().ok()))
                        .flatten()
                })
                .unwrap_or(0);
            let fds = std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count());
            (threads, fds)
        }
        let r = rig();
        r.clipboard.set_content(Some("previous"));
        let (threads_before, fds_before) = process_counts();
        for i in 0..200 {
            r.stt.push_text(&format!("dictation number {i}"));
            assert_eq!(r.d.toggle(false), Phase::Listening, "cycle {i}");
            r.clock.advance(500);
            r.d.toggle(false);
            assert_eq!(r.d.phase(), Phase::Done, "cycle {i}");
            r.timers.run_after();
            assert_eq!(
                r.clipboard.content().as_deref(),
                Some("previous"),
                "cycle {i}"
            );
            assert!(r.timers.fire(TimerId::Idle), "cycle {i}");
            assert_eq!(r.d.phase(), Phase::Idle, "cycle {i}");
            assert!(r.timers.fire(TimerId::Hide), "cycle {i}");
            assert!(r.timers.armed_ids().is_empty(), "cycle {i}");
        }
        assert_eq!(r.history.entries().len(), 200);
        assert_eq!(r.paster.calls().len(), 200);
        assert!(lock(&r.timers.after).is_empty());
        let (threads_after, fds_after) = process_counts();
        // The other tests run in parallel: a small deviation is tolerated, never a growth
        // proportional to the number of dictations.
        assert!(
            threads_after <= threads_before + 16,
            "threads {threads_before} -> {threads_after}"
        );
        assert!(
            fds_after <= fds_before + 64,
            "fds {fds_before} -> {fds_after}"
        );
    }

    #[test]
    fn to_hub_inserts_text_and_does_not_paste() {
        let r = rig();
        r.stt.push_text("bonjour");
        r.d.toggle(true);
        r.d.toggle(true);
        assert_eq!(r.d.phase(), Phase::Done);
        assert!(r.paster.calls().is_empty());
        let ev = lock(&r.events.0).clone();
        assert!(ev.contains(&Event::InsertText("bonjour".into())));
        assert_eq!(r.history.entries()[0].pasted, PasteDisposition::Hub);
    }

    #[test]
    fn cancel_while_listening_returns_to_idle() {
        let r = rig();
        r.d.toggle(false);
        assert_eq!(r.d.cancel(), Phase::Idle);
        let calls = r.recorder.calls();
        assert!(calls.contains(&RecorderCall::Cancel));
        assert!(r.history.entries().is_empty());
        assert!(!r
            .timers
            .armed_ids()
            .iter()
            .any(|(i, _)| *i == TimerId::MaxDictation));
    }

    #[test]
    fn engine_error_shows_error_phase() {
        let r = rig();
        r.stt.push_error(SttError::Failed("boom".into()));
        r.d.toggle(false);
        r.d.toggle(false);
        assert_eq!(r.d.phase(), Phase::Error);
        let ev = lock(&r.events.0).clone();
        assert!(ev.iter().any(|e| matches!(e,
            Event::Phase { phase: Phase::Error, message: Some(m) } if m.contains("boom"))));
        assert!(r.timers.armed_ids().contains(&(TimerId::Idle, 2200)));
        assert!(r.history.entries().is_empty());
    }

    #[test]
    fn engine_not_ready_flashes_message() {
        let r = rig();
        r.d.engine_status(EngineInfo {
            phase: EnginePhase::Downloading,
            loading: true,
            download: Some(linuxflow_core::domain::DownloadProgress {
                model: "small".into(),
                done: 50,
                total: 100,
            }),
            ..EngineInfo::default()
        });
        assert_eq!(r.d.toggle(false), Phase::Error);
        let ev = lock(&r.events.0).clone();
        assert!(ev.iter().any(|e| matches!(e,
            Event::Phase { message: Some(m), .. } if m == "Model downloading · 50%")));
        assert!(r.recorder.calls().is_empty());
    }

    #[test]
    fn microphone_failure_is_reported() {
        let r = rig();
        r.recorder.fail_start(Some("no device"));
        r.d.toggle(false);
        assert_eq!(r.d.phase(), Phase::Error);
        assert!(r.timers.armed_ids().contains(&(TimerId::Idle, 2000)));
    }

    #[test]
    fn too_short_audio_is_nothing_heard() {
        let r = rig();
        r.recorder.set_silence_ms(100);
        r.d.toggle(false);
        r.d.toggle(false);
        assert_eq!(r.d.phase(), Phase::Error);
        assert!(r.timers.armed_ids().contains(&(TimerId::Idle, 1200)));
        assert!(r.stt.requests().is_empty());
    }

    #[test]
    fn blocked_paste_shows_error_after_done() {
        let r = rig();
        r.paster.push(PasteResult::Blocked);
        r.stt.push_text("texte");
        r.d.toggle(false);
        r.d.toggle(false);
        assert_eq!(r.d.phase(), Phase::Error);
        let p = phases(&r);
        assert_eq!(&p[p.len() - 2..], &[Phase::Done, Phase::Error]);
        assert!(r.timers.armed_ids().contains(&(TimerId::Idle, 2200)));
        assert_eq!(r.clipboard.content().as_deref(), Some("texte"));
    }

    #[test]
    fn max_dictation_timer_stops_recording() {
        let r = rig();
        lock(&r.settings).max_dictation_sec = 300;
        r.stt.push_text("long");
        r.d.toggle(false);
        assert!(r.timers.fire(TimerId::MaxDictation));
        assert_eq!(r.d.phase(), Phase::Done);
    }

    #[test]
    fn level_events_reach_the_ui() {
        let r = rig();
        r.recorder.emit_level(0.5);
        assert!(lock(&r.events.0).contains(&Event::Level(0.5)));
    }

    #[test]
    fn push_state_effect_calls_callback() {
        let r = rig();
        let n = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let n2 = n.clone();
        r.d.on_push_state(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        });
        r.stt.push_text("x y");
        r.d.toggle(false);
        r.d.toggle(false);
        assert!(n.load(Ordering::SeqCst) >= 1);
    }
}
