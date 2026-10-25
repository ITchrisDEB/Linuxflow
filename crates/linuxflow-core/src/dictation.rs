//! Dictation state machine.
//!
//! The machine is PURE: no timer, no thread, no input/output. It receives timestamped
//! [`Input`]s (`now_ms`) and returns [`Effect`]s that the caller executes (change the visible
//! phase, arm/cancel a timer, start the capture, show the pill...).
//!
//! Timer contract: `ScheduleTimer{id, ..}` replaces any timer with the same `id` already armed;
//! `CancelTimer(id)` disarms it. When a timer expires, the caller sends `Input::TimerFired(id)`.
//! A stale `TimerFired` (timer disarmed in the meantime) is ignored by the machine.
//!
//! Messages meant for the user are [`UserMessage`]s (never text): the interface
//! translates them.

use crate::constants::*;
use crate::domain::{DownloadProgress, EngineInfo, EnginePhase, Phase};
use crate::pipeline::{OutcomeKind, PipelineOutcome};

/// Identifier of a timer managed by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimerId {
    /// Return to `idle` (`scheduleIdle`).
    Idle,
    /// Hiding of the pill, 420 ms after the return to `idle`.
    Hide,
    /// Safety timer: automatic stop after `max_dictation_sec`.
    MaxDictation,
}

/// Message meant for the user, to be translated by the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserMessage {
    /// "Model downloading · NN %" (`None`: unknown percentage).
    ModelDownloading { percent: Option<u8> },
    /// "Model loading…".
    ModelLoading,
    /// "Transcription engine unavailable".
    EngineUnavailable,
    /// "Nothing heard".
    NothingHeard,
    /// "Microphone unavailable"; `detail` = raw message of the audio capture if any.
    MicUnavailable { detail: Option<String> },
    /// "Paste blocked — text copied to the clipboard".
    PasteBlocked,
    /// Pipeline error (raw technical message).
    PipelineError { message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Starts a dictation (refused if the phase is not `idle`).
    Start {
        to_hub: bool,
        no_paste: bool,
    },
    Stop,
    Cancel,
    Toggle {
        to_hub: bool,
    },
    /// The captured audio is available (duration in ms). `run`: the run that produced it.
    RecordingFinished {
        duration_ms: u64,
        run: u64,
    },
    /// Capture failure (at start or while running). Empty `message` = no detail.
    RecordingFailed {
        message: String,
        run: u64,
    },
    PipelineStarted {
        run: u64,
    },
    PipelineFormatting {
        run: u64,
    },
    PipelineFinished {
        outcome: PipelineOutcome,
        run: u64,
    },
    PipelineFailed {
        message: String,
        run: u64,
    },
    TimerFired(TimerId),
    /// Last known state of the transcription engine.
    EngineStatus(EngineInfo),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// New visible phase (pill, hub, tray). `latency_ms` is only filled in for `Done`.
    SetPhase {
        phase: Phase,
        message: Option<UserMessage>,
        latency_ms: Option<u64>,
    },
    ScheduleTimer {
        id: TimerId,
        delay_ms: u64,
    },
    CancelTimer(TimerId),
    StartRecording {
        max_sec: u64,
        run: u64,
    },
    /// Stop the capture; the caller will then send `RecordingFinished` (same `run`).
    StopRecording {
        run: u64,
    },
    CancelRecording,
    /// Run the pipeline on the captured audio (follows a sufficiently long `RecordingFinished`).
    RunPipeline {
        to_hub: bool,
        no_paste: bool,
        stopped_at_ms: u64,
        run: u64,
    },
    ShowOverlay,
    HideOverlay,
    /// Push a complete snapshot of the state to the views (`pushState`).
    PushState,
}

#[derive(Debug, Clone)]
pub struct Dictation {
    phase: Phase,
    to_hub: bool,
    no_paste: bool,
    started_at_ms: u64,
    stopped_at_ms: u64,
    max_sec: u64,
    engine: EngineInfo,
    idle_armed: bool,
    max_armed: bool,
    /// Identifier of the current run: incremented at every accepted start and every
    /// cancellation. Any input tied to a run (capture, pipeline) carrying another
    /// identifier is stale and ignored (the pipeline itself cannot be interrupted).
    run: u64,
}

impl Default for Dictation {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_DICTATION_SEC)
    }
}

impl Dictation {
    pub fn new(max_dictation_sec: u64) -> Self {
        Self {
            phase: Phase::Idle,
            to_hub: false,
            no_paste: false,
            started_at_ms: 0,
            stopped_at_ms: 0,
            max_sec: max_dictation_sec,
            engine: EngineInfo::default(),
            idle_armed: false,
            max_armed: false,
            run: 0,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }
    /// Identifier of the current run (see [`Input::RecordingFinished`] and the following).
    pub fn run(&self) -> u64 {
        self.run
    }
    pub fn to_hub(&self) -> bool {
        self.to_hub
    }
    pub fn no_paste(&self) -> bool {
        self.no_paste
    }
    /// Instant (ms) of the start of the current dictation.
    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }
    /// Instant (ms) of the end of the capture (0 if not stopped yet).
    pub fn stopped_at_ms(&self) -> u64 {
        self.stopped_at_ms
    }

    /// Updates `maxDictationSec` (taken into account at the next dictation).
    pub fn set_max_dictation_sec(&mut self, sec: u64) {
        self.max_sec = sec;
    }

    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        match input {
            Input::Start { to_hub, no_paste } => self.start(to_hub, no_paste, now_ms, &mut fx),
            Input::Stop => self.stop(now_ms, &mut fx),
            Input::Cancel => self.cancel(&mut fx),
            Input::Toggle { to_hub } => match self.phase {
                Phase::Listening => self.stop(now_ms, &mut fx),
                Phase::Idle => self.start(to_hub, false, now_ms, &mut fx),
                _ => {}
            },
            Input::RecordingFinished { duration_ms, run } => {
                if run == self.run {
                    self.recording_finished(duration_ms, now_ms, &mut fx)
                }
            }
            Input::RecordingFailed { message, run } => {
                if run == self.run && matches!(self.phase, Phase::Listening | Phase::Transcribing) {
                    self.disarm_max(&mut fx);
                    let detail = (!message.is_empty()).then_some(message);
                    self.set_phase(
                        Phase::Error,
                        Some(UserMessage::MicUnavailable { detail }),
                        None,
                        &mut fx,
                    );
                    self.schedule_idle(IDLE_MIC_ERROR_MS, &mut fx);
                }
            }
            Input::PipelineStarted { run } => {
                if run == self.run && self.phase != Phase::Transcribing {
                    self.set_phase(Phase::Transcribing, None, None, &mut fx);
                }
            }
            Input::PipelineFormatting { run } => {
                if run == self.run && self.phase == Phase::Transcribing {
                    self.set_phase(Phase::Formatting, None, None, &mut fx);
                }
            }
            Input::PipelineFinished { outcome, run } => {
                if run == self.run {
                    self.pipeline_finished(&outcome, &mut fx)
                }
            }
            Input::PipelineFailed { message, run } => {
                if run == self.run {
                    self.set_phase(
                        Phase::Error,
                        Some(UserMessage::PipelineError { message }),
                        None,
                        &mut fx,
                    );
                    self.schedule_idle(IDLE_ERROR_MS, &mut fx);
                }
            }
            Input::TimerFired(id) => self.timer_fired(id, now_ms, &mut fx),
            Input::EngineStatus(info) => self.engine = info,
        }
        fx
    }

    // ---- Transitions ----

    fn start(&mut self, to_hub: bool, no_paste: bool, now_ms: u64, fx: &mut Vec<Effect>) {
        if self.phase != Phase::Idle {
            return;
        }
        if !self.engine.ready {
            let message = engine_message(&self.engine);
            self.flash(Phase::Error, message, IDLE_FLASH_MS, fx);
            return;
        }
        self.disarm_idle(fx);
        self.run += 1;
        self.to_hub = to_hub;
        self.no_paste = no_paste;
        self.started_at_ms = now_ms;
        self.stopped_at_ms = 0;
        fx.push(Effect::ShowOverlay);
        self.set_phase(Phase::Listening, None, None, fx);
        let max_sec = if self.max_sec == 0 {
            DEFAULT_MAX_DICTATION_SEC
        } else {
            self.max_sec
        };
        fx.push(Effect::StartRecording {
            max_sec,
            run: self.run,
        });
        self.max_armed = true;
        fx.push(Effect::ScheduleTimer {
            id: TimerId::MaxDictation,
            delay_ms: max_sec.saturating_mul(1000),
        });
    }

    fn stop(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        if self.phase != Phase::Listening {
            return;
        }
        self.disarm_max(fx);
        self.stopped_at_ms = now_ms;
        self.set_phase(Phase::Transcribing, None, None, fx);
        fx.push(Effect::StopRecording { run: self.run });
    }

    fn cancel(&mut self, fx: &mut Vec<Effect>) {
        // Nothing to cancel at rest.
        if self.phase == Phase::Idle {
            return;
        }
        // Invalidates any late result of the capture or of the running pipeline.
        self.run += 1;
        self.disarm_max(fx);
        fx.push(Effect::CancelRecording);
        self.set_phase(Phase::Idle, None, None, fx);
        self.schedule_idle(0, fx);
    }

    fn recording_finished(&mut self, duration_ms: u64, now_ms: u64, fx: &mut Vec<Effect>) {
        match self.phase {
            Phase::Transcribing => {}
            // The capture stopped by itself (maximum duration reached on the audio side): implicit stop.
            Phase::Listening => {
                self.disarm_max(fx);
                self.stopped_at_ms = now_ms;
                self.set_phase(Phase::Transcribing, None, None, fx);
            }
            _ => return,
        }
        if duration_ms < MIN_AUDIO_MS {
            self.set_phase(Phase::Error, Some(UserMessage::NothingHeard), None, fx);
            self.schedule_idle(IDLE_EMPTY_MS, fx);
            return;
        }
        fx.push(Effect::RunPipeline {
            to_hub: self.to_hub,
            no_paste: self.no_paste,
            stopped_at_ms: self.stopped_at_ms,
            run: self.run,
        });
    }

    fn pipeline_finished(&mut self, outcome: &PipelineOutcome, fx: &mut Vec<Effect>) {
        match outcome.kind {
            OutcomeKind::Empty => {
                self.set_phase(Phase::Error, Some(UserMessage::NothingHeard), None, fx);
                self.schedule_idle(IDLE_EMPTY_MS, fx);
            }
            OutcomeKind::Done => {
                self.set_phase(Phase::Done, None, Some(outcome.latency_ms), fx);
                self.schedule_idle(IDLE_DONE_MS, fx);
                fx.push(Effect::PushState);
            }
            OutcomeKind::PasteBlocked => {
                // `done`, then idle is armed at 2200 ms, then `error` right away.
                self.set_phase(Phase::Done, None, Some(outcome.latency_ms), fx);
                self.schedule_idle(IDLE_ERROR_MS, fx);
                self.set_phase(Phase::Error, Some(UserMessage::PasteBlocked), None, fx);
                fx.push(Effect::PushState);
            }
        }
    }

    fn timer_fired(&mut self, id: TimerId, now_ms: u64, fx: &mut Vec<Effect>) {
        match id {
            TimerId::Idle => {
                if !self.idle_armed {
                    return;
                }
                self.idle_armed = false;
                self.set_phase(Phase::Idle, None, None, fx);
                fx.push(Effect::ScheduleTimer {
                    id: TimerId::Hide,
                    delay_ms: OVERLAY_HIDE_DELAY_MS,
                });
                fx.push(Effect::PushState);
            }
            TimerId::Hide => {
                if self.phase == Phase::Idle {
                    fx.push(Effect::HideOverlay);
                }
            }
            TimerId::MaxDictation => {
                if !self.max_armed {
                    return;
                }
                self.max_armed = false;
                self.stop(now_ms, fx);
            }
        }
    }

    // ---- Aides ----

    fn set_phase(
        &mut self,
        phase: Phase,
        message: Option<UserMessage>,
        latency_ms: Option<u64>,
        fx: &mut Vec<Effect>,
    ) {
        self.phase = phase;
        fx.push(Effect::SetPhase {
            phase,
            message,
            latency_ms,
        });
    }

    fn schedule_idle(&mut self, delay_ms: u64, fx: &mut Vec<Effect>) {
        self.idle_armed = true;
        fx.push(Effect::ScheduleTimer {
            id: TimerId::Idle,
            delay_ms,
        });
    }

    fn disarm_idle(&mut self, fx: &mut Vec<Effect>) {
        if self.idle_armed {
            self.idle_armed = false;
            fx.push(Effect::CancelTimer(TimerId::Idle));
        }
    }

    fn disarm_max(&mut self, fx: &mut Vec<Effect>) {
        if self.max_armed {
            self.max_armed = false;
            fx.push(Effect::CancelTimer(TimerId::MaxDictation));
        }
    }

    /// `flashOverlay`: shows the pill, changes phase, arms the return to `idle`.
    fn flash(&mut self, phase: Phase, message: UserMessage, delay_ms: u64, fx: &mut Vec<Effect>) {
        fx.push(Effect::ShowOverlay);
        self.set_phase(phase, Some(message), None, fx);
        self.schedule_idle(delay_ms, fx);
    }
}

/// Message to show when the engine is not ready (downloading, loading or unavailable).
fn engine_message(info: &EngineInfo) -> UserMessage {
    match info.phase {
        EnginePhase::Downloading => UserMessage::ModelDownloading {
            percent: info.download.as_ref().and_then(percent),
        },
        EnginePhase::Loading => UserMessage::ModelLoading,
        EnginePhase::Starting if info.loading => UserMessage::ModelLoading,
        _ => UserMessage::EngineUnavailable,
    }
}

/// Percentage capped at 99 (`None` if the total size is unknown).
fn percent(d: &DownloadProgress) -> Option<u8> {
    if d.total == 0 {
        return None;
    }
    let p = (u128::from(d.done) * 100 / u128::from(d.total)).min(99);
    Some(p as u8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::{DeviceKind, PasteDisposition};

    fn ready() -> EngineInfo {
        EngineInfo {
            phase: EnginePhase::Ready,
            ready: true,
            device: DeviceKind::Cpu,
            ..EngineInfo::default()
        }
    }

    fn machine() -> Dictation {
        let mut d = Dictation::new(300);
        d.handle(Input::EngineStatus(ready()), 0);
        d
    }

    fn outcome(kind: OutcomeKind) -> PipelineOutcome {
        PipelineOutcome {
            kind,
            text: "t".into(),
            raw: "t".into(),
            latency_ms: 640,
            stt_ms: 100,
            audio_ms: 2000,
            pasted: Some(PasteDisposition::Pasted),
            insert_text: None,
            restore: None,
            entry: None,
        }
    }

    fn set(phase: Phase, message: Option<UserMessage>, latency_ms: Option<u64>) -> Effect {
        Effect::SetPhase {
            phase,
            message,
            latency_ms,
        }
    }

    fn sched(id: TimerId, delay_ms: u64) -> Effect {
        Effect::ScheduleTimer { id, delay_ms }
    }

    #[test]
    fn start_when_idle_listens() {
        let mut d = machine();
        let fx = d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            1000,
        );
        assert_eq!(
            fx,
            vec![
                Effect::ShowOverlay,
                set(Phase::Listening, None, None),
                Effect::StartRecording {
                    max_sec: 300,
                    run: 1
                },
                sched(TimerId::MaxDictation, 300_000),
            ]
        );
        assert_eq!(d.phase(), Phase::Listening);
        assert_eq!(d.started_at_ms(), 1000);
    }

    #[test]
    fn start_refused_when_not_idle() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(d
            .handle(
                Input::Start {
                    to_hub: false,
                    no_paste: false
                },
                10
            )
            .is_empty());
        d.handle(Input::Stop, 20);
        assert!(d
            .handle(
                Input::Start {
                    to_hub: true,
                    no_paste: false
                },
                30
            )
            .is_empty());
        assert!(!d.to_hub());
    }

    #[test]
    fn engine_not_ready_downloading_with_percent() {
        let mut d = Dictation::new(300);
        d.handle(
            Input::EngineStatus(EngineInfo {
                phase: EnginePhase::Downloading,
                loading: true,
                download: Some(DownloadProgress {
                    model: "m".into(),
                    done: 420,
                    total: 1000,
                }),
                ..EngineInfo::default()
            }),
            0,
        );
        let fx = d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert_eq!(
            fx,
            vec![
                Effect::ShowOverlay,
                set(
                    Phase::Error,
                    Some(UserMessage::ModelDownloading { percent: Some(42) }),
                    None
                ),
                sched(TimerId::Idle, IDLE_FLASH_MS),
            ]
        );
        assert_eq!(d.phase(), Phase::Error);
    }

    #[test]
    fn download_percent_is_capped_at_99_and_unknown_total_is_none() {
        let dl = |done, total| DownloadProgress {
            model: "m".into(),
            done,
            total,
        };
        assert_eq!(percent(&dl(1000, 1000)), Some(99));
        assert_eq!(percent(&dl(5, 0)), None);
        assert_eq!(percent(&dl(0, 10)), Some(0));
    }

    #[test]
    fn engine_not_ready_loading_message() {
        let mut d = Dictation::new(300);
        d.handle(
            Input::EngineStatus(EngineInfo {
                phase: EnginePhase::Loading,
                loading: true,
                ..EngineInfo::default()
            }),
            0,
        );
        let fx = d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(fx.contains(&set(Phase::Error, Some(UserMessage::ModelLoading), None)));
    }

    #[test]
    fn engine_unavailable_message() {
        let mut d = Dictation::new(300);
        d.handle(
            Input::EngineStatus(EngineInfo {
                phase: EnginePhase::Error,
                error: Some("boom".into()),
                ..EngineInfo::default()
            }),
            0,
        );
        let fx = d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(fx.contains(&set(
            Phase::Error,
            Some(UserMessage::EngineUnavailable),
            None
        )));
        // Engine never reported: same message.
        let mut d2 = Dictation::new(300);
        let fx = d2.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(fx.contains(&set(
            Phase::Error,
            Some(UserMessage::EngineUnavailable),
            None
        )));
    }

    #[test]
    fn flash_returns_to_idle_then_hides() {
        let mut d = Dictation::new(300);
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        let fx = d.handle(Input::TimerFired(TimerId::Idle), 1600);
        assert_eq!(
            fx,
            vec![
                set(Phase::Idle, None, None),
                sched(TimerId::Hide, 420),
                Effect::PushState
            ]
        );
        assert_eq!(
            d.handle(Input::TimerFired(TimerId::Hide), 2020),
            vec![Effect::HideOverlay]
        );
    }

    #[test]
    fn hide_is_skipped_if_a_new_dictation_started() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(d.handle(Input::TimerFired(TimerId::Hide), 100).is_empty());
    }

    #[test]
    fn stale_idle_timer_is_ignored() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(d.handle(Input::TimerFired(TimerId::Idle), 100).is_empty());
        assert_eq!(d.phase(), Phase::Listening);
    }

    #[test]
    fn stop_only_valid_when_listening() {
        let mut d = machine();
        assert!(d.handle(Input::Stop, 0).is_empty());
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        let fx = d.handle(Input::Stop, 5000);
        assert_eq!(
            fx,
            vec![
                Effect::CancelTimer(TimerId::MaxDictation),
                set(Phase::Transcribing, None, None),
                Effect::StopRecording { run: 1 }
            ]
        );
        assert_eq!(d.stopped_at_ms(), 5000);
        assert!(d.handle(Input::Stop, 5001).is_empty());
    }

    #[test]
    fn max_dictation_timer_stops_recording() {
        let mut d = Dictation::new(2);
        d.handle(Input::EngineStatus(ready()), 0);
        let fx = d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(fx.contains(&Effect::StartRecording { max_sec: 2, run: 1 }));
        assert!(fx.contains(&sched(TimerId::MaxDictation, 2000)));
        let fx = d.handle(Input::TimerFired(TimerId::MaxDictation), 2000);
        assert_eq!(
            fx,
            vec![
                set(Phase::Transcribing, None, None),
                Effect::StopRecording { run: 1 }
            ]
        );
    }

    #[test]
    fn max_sec_zero_falls_back_to_default() {
        let mut d = Dictation::new(0);
        d.handle(Input::EngineStatus(ready()), 0);
        let fx = d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        assert!(fx.contains(&Effect::StartRecording {
            max_sec: DEFAULT_MAX_DICTATION_SEC,
            run: 1
        }));
    }

    #[test]
    fn stale_max_timer_after_stop_is_ignored() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 10);
        assert!(d
            .handle(Input::TimerFired(TimerId::MaxDictation), 20)
            .is_empty());
    }

    #[test]
    fn cancel_while_listening_goes_idle_immediately() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        let fx = d.handle(Input::Cancel, 100);
        assert_eq!(
            fx,
            vec![
                Effect::CancelTimer(TimerId::MaxDictation),
                Effect::CancelRecording,
                set(Phase::Idle, None, None),
                sched(TimerId::Idle, 0),
            ]
        );
        assert_eq!(d.phase(), Phase::Idle);
        // The timer at 0 ms follows the hiding of the pill.
        let fx = d.handle(Input::TimerFired(TimerId::Idle), 100);
        assert_eq!(
            fx,
            vec![
                set(Phase::Idle, None, None),
                sched(TimerId::Hide, 420),
                Effect::PushState
            ]
        );
    }

    #[test]
    fn cancel_when_idle_is_a_noop() {
        let mut d = machine();
        assert!(d.handle(Input::Cancel, 0).is_empty());
    }

    #[test]
    fn cancel_during_transcription_discards_late_results() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 10);
        d.handle(Input::Cancel, 20);
        assert!(d
            .handle(
                Input::PipelineFinished {
                    outcome: outcome(OutcomeKind::Done),
                    run: 1
                },
                30
            )
            .is_empty());
        assert!(d
            .handle(
                Input::PipelineFailed {
                    message: "x".into(),
                    run: 1
                },
                31
            )
            .is_empty());
        assert_eq!(d.phase(), Phase::Idle);
        // A new dictation re-arms the taking into account of results.
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            40,
        );
        assert_eq!(d.phase(), Phase::Listening);
    }

    #[test]
    fn toggle_starts_stops_and_ignores_busy_phases() {
        let mut d = machine();
        let fx = d.handle(Input::Toggle { to_hub: true }, 0);
        assert!(fx.contains(&Effect::StartRecording {
            max_sec: 300,
            run: 1
        }));
        assert!(d.to_hub());
        let fx = d.handle(Input::Toggle { to_hub: false }, 10);
        assert!(fx.contains(&Effect::StopRecording { run: 1 }));
        assert!(d.handle(Input::Toggle { to_hub: false }, 20).is_empty());
        assert_eq!(d.phase(), Phase::Transcribing);
    }

    #[test]
    fn late_result_of_a_cancelled_pipeline_cannot_corrupt_the_next_dictation() {
        // Regression scenario: Start, Stop, pipeline running, Cancel, Idle, new Start,
        // then the result of the FIRST pipeline arrives. Before: the phase moved to `Done` during
        // listening, the Idle timer left the capture orphaned, Stop/Toggle were ignored.
        let mut d = machine();
        let start = || Input::Start {
            to_hub: false,
            no_paste: false,
        };
        d.handle(start(), 0);
        d.handle(Input::Stop, 100);
        d.handle(
            Input::RecordingFinished {
                duration_ms: 2000,
                run: 1,
            },
            110,
        );
        d.handle(Input::PipelineStarted { run: 1 }, 120);
        d.handle(Input::Cancel, 200);
        d.handle(Input::TimerFired(TimerId::Idle), 300);
        d.handle(start(), 400);
        assert_eq!(d.phase(), Phase::Listening);

        // Late results of run 1: all ignored.
        assert!(d
            .handle(
                Input::PipelineFinished {
                    outcome: outcome(OutcomeKind::Done),
                    run: 1
                },
                500
            )
            .is_empty());
        assert!(d
            .handle(
                Input::PipelineFailed {
                    message: "late".into(),
                    run: 1
                },
                510
            )
            .is_empty());
        assert!(d.handle(Input::PipelineStarted { run: 1 }, 520).is_empty());
        assert!(d
            .handle(Input::PipelineFormatting { run: 1 }, 530)
            .is_empty());
        assert!(d
            .handle(
                Input::RecordingFinished {
                    duration_ms: 2000,
                    run: 1
                },
                540
            )
            .is_empty());
        assert_eq!(
            d.phase(),
            Phase::Listening,
            "the current dictation is intact"
        );

        // The new dictation can be stopped normally (Stop is not ignored).
        let fx = d.handle(Input::Stop, 600);
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::StopRecording { run: 3 })),
            "{fx:?}"
        );
    }

    #[test]
    fn audio_shorter_than_250ms_is_nothing_heard() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 100);
        let fx = d.handle(
            Input::RecordingFinished {
                duration_ms: MIN_AUDIO_MS - 1,
                run: 1,
            },
            110,
        );
        assert_eq!(
            fx,
            vec![
                set(Phase::Error, Some(UserMessage::NothingHeard), None),
                sched(TimerId::Idle, IDLE_EMPTY_MS)
            ]
        );
    }

    #[test]
    fn audio_of_exactly_250ms_runs_pipeline_with_flags() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: true,
                no_paste: true,
            },
            0,
        );
        d.handle(Input::Stop, 700);
        let fx = d.handle(
            Input::RecordingFinished {
                duration_ms: MIN_AUDIO_MS,
                run: 1,
            },
            710,
        );
        assert_eq!(
            fx,
            vec![Effect::RunPipeline {
                to_hub: true,
                no_paste: true,
                stopped_at_ms: 700,
                run: 1,
            }]
        );
    }

    #[test]
    fn recording_finished_while_listening_acts_as_implicit_stop() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        let fx = d.handle(
            Input::RecordingFinished {
                duration_ms: 3000,
                run: 1,
            },
            3000,
        );
        assert_eq!(
            fx,
            vec![
                Effect::CancelTimer(TimerId::MaxDictation),
                set(Phase::Transcribing, None, None),
                Effect::RunPipeline {
                    to_hub: false,
                    no_paste: false,
                    stopped_at_ms: 3000,
                    run: 1,
                },
            ]
        );
    }

    #[test]
    fn recording_finished_when_idle_is_ignored() {
        let mut d = machine();
        assert!(d
            .handle(
                Input::RecordingFinished {
                    duration_ms: 5000,
                    run: 1
                },
                0
            )
            .is_empty());
    }

    #[test]
    fn recording_failure_shows_mic_error_for_2s() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        let fx = d.handle(
            Input::RecordingFailed {
                message: "no device".into(),
                run: 1,
            },
            5,
        );
        assert_eq!(
            fx,
            vec![
                Effect::CancelTimer(TimerId::MaxDictation),
                set(
                    Phase::Error,
                    Some(UserMessage::MicUnavailable {
                        detail: Some("no device".into())
                    }),
                    None
                ),
                sched(TimerId::Idle, IDLE_MIC_ERROR_MS),
            ]
        );
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        let fx = d.handle(
            Input::RecordingFailed {
                message: String::new(),
                run: 1,
            },
            5,
        );
        assert!(fx.contains(&set(
            Phase::Error,
            Some(UserMessage::MicUnavailable { detail: None }),
            None
        )));
    }

    #[test]
    fn pipeline_empty_text_is_nothing_heard() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 1);
        let fx = d.handle(
            Input::PipelineFinished {
                outcome: outcome(OutcomeKind::Empty),
                run: 1,
            },
            2,
        );
        assert_eq!(
            fx,
            vec![
                set(Phase::Error, Some(UserMessage::NothingHeard), None),
                sched(TimerId::Idle, IDLE_EMPTY_MS)
            ]
        );
    }

    #[test]
    fn pipeline_done_shows_latency_then_idle_in_900ms() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 1);
        let fx = d.handle(
            Input::PipelineFinished {
                outcome: outcome(OutcomeKind::Done),
                run: 1,
            },
            2,
        );
        assert_eq!(
            fx,
            vec![
                set(Phase::Done, None, Some(640)),
                sched(TimerId::Idle, IDLE_DONE_MS),
                Effect::PushState
            ]
        );
    }

    #[test]
    fn paste_blocked_passes_through_done_then_error_for_2200ms() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 1);
        let fx = d.handle(
            Input::PipelineFinished {
                outcome: outcome(OutcomeKind::PasteBlocked),
                run: 1,
            },
            2,
        );
        assert_eq!(
            fx,
            vec![
                set(Phase::Done, None, Some(640)),
                sched(TimerId::Idle, IDLE_ERROR_MS),
                set(Phase::Error, Some(UserMessage::PasteBlocked), None),
                Effect::PushState,
            ]
        );
        assert_eq!(d.phase(), Phase::Error);
    }

    #[test]
    fn pipeline_failure_is_error_2200ms() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 1);
        let fx = d.handle(
            Input::PipelineFailed {
                message: "stt exploded".into(),
                run: 1,
            },
            2,
        );
        assert_eq!(
            fx,
            vec![
                set(
                    Phase::Error,
                    Some(UserMessage::PipelineError {
                        message: "stt exploded".into()
                    }),
                    None
                ),
                sched(TimerId::Idle, IDLE_ERROR_MS),
            ]
        );
    }

    #[test]
    fn pipeline_started_forces_transcribing_only_if_needed() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Stop, 1);
        assert!(d.handle(Input::PipelineStarted { run: 1 }, 2).is_empty());
        let fx = d.handle(Input::PipelineFormatting { run: 1 }, 3);
        assert_eq!(fx, vec![set(Phase::Formatting, None, None)]);
    }

    /// Complete sequence with a manual clock: idle -> listening -> transcribing -> formatting -> done -> idle.
    #[test]
    fn full_sequence_with_fake_clock() {
        use crate::testing::FakeClock;
        use crate::traits::Clock;
        let clock = FakeClock::new(10_000);
        let mut d = machine();
        let mut phases = vec![d.phase()];
        let run = |d: &mut Dictation, input: Input, phases: &mut Vec<Phase>| {
            let fx = d.handle(input, clock.now_ms());
            for e in &fx {
                if let Effect::SetPhase { phase, .. } = e {
                    phases.push(*phase);
                }
            }
            fx
        };
        run(&mut d, Input::Toggle { to_hub: false }, &mut phases);
        clock.advance(3000);
        run(&mut d, Input::Toggle { to_hub: false }, &mut phases);
        assert_eq!(d.stopped_at_ms(), 13_000);
        clock.advance(10);
        let fx = run(
            &mut d,
            Input::RecordingFinished {
                duration_ms: 3000,
                run: 1,
            },
            &mut phases,
        );
        assert!(matches!(
            fx.as_slice(),
            [Effect::RunPipeline {
                stopped_at_ms: 13_000,
                ..
            }]
        ));
        run(&mut d, Input::PipelineStarted { run: 1 }, &mut phases);
        run(&mut d, Input::PipelineFormatting { run: 1 }, &mut phases);
        clock.advance(600);
        run(
            &mut d,
            Input::PipelineFinished {
                outcome: outcome(OutcomeKind::Done),
                run: 1,
            },
            &mut phases,
        );
        clock.advance(IDLE_DONE_MS);
        run(&mut d, Input::TimerFired(TimerId::Idle), &mut phases);
        let fx = run(&mut d, Input::TimerFired(TimerId::Hide), &mut phases);
        assert_eq!(fx, vec![Effect::HideOverlay]);
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
    }

    #[test]
    fn start_cancels_pending_idle_timer() {
        let mut d = machine();
        d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            0,
        );
        d.handle(Input::Cancel, 1);
        // idle 0 ms armed but not fired yet; new dictation refused while phase != idle? Here phase == idle.
        let fx = d.handle(
            Input::Start {
                to_hub: false,
                no_paste: false,
            },
            2,
        );
        assert_eq!(fx.first(), Some(&Effect::CancelTimer(TimerId::Idle)));
    }

    fn start_input() -> Input {
        Input::Start {
            to_hub: false,
            no_paste: false,
        }
    }

    #[test]
    fn late_pipeline_result_of_a_cancelled_run_is_ignored() {
        let mut d = machine();
        d.handle(start_input(), 0);
        assert_eq!(d.run(), 1);
        d.handle(Input::Stop, 100);
        let fx = d.handle(
            Input::RecordingFinished {
                duration_ms: 2000,
                run: 1,
            },
            110,
        );
        assert!(matches!(
            fx.first(),
            Some(Effect::RunPipeline { run: 1, .. })
        ));
        d.handle(Input::PipelineStarted { run: 1 }, 120);
        // Cancellation during transcription, return to rest, new dictation.
        d.handle(Input::Cancel, 200);
        d.handle(Input::TimerFired(TimerId::Idle), 200);
        let fx = d.handle(start_input(), 300);
        assert_eq!(d.phase(), Phase::Listening);
        assert!(fx.contains(&Effect::StartRecording {
            max_sec: 300,
            run: d.run()
        }));
        assert_eq!(d.run(), 3);
        // The late results of the first pipeline have no effect.
        for stale in [
            Input::PipelineStarted { run: 1 },
            Input::PipelineFormatting { run: 1 },
            Input::PipelineFinished {
                outcome: outcome_for_tests(OutcomeKind::Done),
                run: 1,
            },
            Input::PipelineFailed {
                message: "boom".into(),
                run: 1,
            },
            Input::RecordingFinished {
                duration_ms: 2000,
                run: 1,
            },
            Input::RecordingFailed {
                message: "late".into(),
                run: 1,
            },
        ] {
            assert!(d.handle(stale, 400).is_empty());
            assert_eq!(d.phase(), Phase::Listening);
        }
        // The current dictation proceeds normally and its idle timer does not come from a stale result.
        let fx = d.handle(Input::Stop, 500);
        assert!(fx.contains(&Effect::StopRecording { run: 3 }));
        assert_eq!(d.phase(), Phase::Transcribing);
    }

    #[test]
    fn refused_start_does_not_change_the_run() {
        let mut d = machine();
        d.handle(start_input(), 0);
        assert!(d.handle(start_input(), 10).is_empty());
        assert_eq!(d.run(), 1);
        d.handle(Input::Stop, 20);
        assert!(d.handle(start_input(), 30).is_empty());
        assert_eq!(d.run(), 1);
    }

    fn outcome_for_tests(kind: OutcomeKind) -> PipelineOutcome {
        PipelineOutcome {
            kind,
            text: "t".into(),
            raw: "t".into(),
            latency_ms: 640,
            stt_ms: 100,
            audio_ms: 2000,
            pasted: Some(PasteDisposition::Pasted),
            insert_text: None,
            restore: None,
            entry: None,
        }
    }
}
