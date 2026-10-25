//! `CpalRecorder`: implementation of `linuxflow_core::traits::Recorder`.
//!
//! Architecture: a dedicated capture thread opens the source (the cpal stream is not `Send`), then
//! loops every ~10 ms: read the lock-free ring, resample to 16 kHz,
//! compute the level, accumulate. The audio callback only writes to the ring.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use linuxflow_core::constants::SAMPLE_RATE_HZ;
use linuxflow_core::domain::{MicrophoneInfo, Recording};
use linuxflow_core::traits::Recorder;
use linuxflow_core::{Error, Result};

use crate::capture::{capture_ring, AudioSource, CaptureReader, ErrorReporter, RING_CAPACITY};
use crate::convert::sanitize;
use crate::cpal_source::CpalSource;
use crate::level::LevelMeter;
use crate::resample::StreamResampler;

type LevelSink = Arc<dyn Fn(f32) + Send + Sync>;

/// Period of the processing loop (the level latency depends on it a little).
const POLL: Duration = Duration::from_millis(10);
/// Maximum delay to open the microphone before giving up.
const START_TIMEOUT: Duration = Duration::from_secs(10);

enum Command {
    Stop,
    Cancel,
}

/// Observable state of a capture, shared with the thread.
struct SessionStatus {
    limit_reached: AtomicBool,
    errors: ErrorReporter,
}

struct Session {
    commands: Sender<Command>,
    handle: JoinHandle<Result<Recording>>,
}

pub struct CpalRecorder {
    source: Arc<dyn AudioSource>,
    sink: Arc<Mutex<Option<LevelSink>>>,
    current: Mutex<Option<Session>>,
    status: Mutex<Arc<SessionStatus>>,
}

impl Default for CpalRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl CpalRecorder {
    /// Recorder on the real microphone (cpal).
    pub fn new() -> Self {
        Self::with_source(Arc::new(CpalSource))
    }

    /// Recorder on any source (synthetic source for the tests).
    pub fn with_source(source: Arc<dyn AudioSource>) -> Self {
        Self {
            source,
            sink: Arc::new(Mutex::new(None)),
            current: Mutex::new(None),
            status: Mutex::new(Arc::new(SessionStatus {
                limit_reached: AtomicBool::new(false),
                errors: ErrorReporter::new(),
            })),
        }
    }

    /// Microphones available from the source (`default` entry always first).
    pub fn microphones(&self) -> Vec<MicrophoneInfo> {
        self.source.microphones()
    }

    /// True if the last capture reached the maximum duration (the stream was then stopped;
    /// `stop()` returns the audio truncated to that duration).
    pub fn limit_reached(&self) -> bool {
        self.current_status().limit_reached.load(Ordering::Acquire)
    }

    /// First fatal stream error of the current or last capture, if any
    /// (device lost...). Lets the caller interrupt without waiting for `stop()`.
    pub fn failure(&self) -> Option<String> {
        self.current_status().errors.fatal_message()
    }

    fn current_status(&self) -> Arc<SessionStatus> {
        match self.status.lock() {
            Ok(status) => Arc::clone(&status),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    fn take_session(&self) -> Option<Session> {
        match self.current.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

impl Drop for CpalRecorder {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Processing chain: ring -> resampling -> level -> 16 kHz samples.
struct Pipeline {
    resampler: StreamResampler,
    meter: LevelMeter,
    sink: Arc<Mutex<Option<LevelSink>>>,
    samples: Vec<f32>,
    max_samples: Option<usize>,
    raw: Vec<f32>,
    fresh: Vec<f32>,
}

impl Pipeline {
    fn limit_hit(&self) -> bool {
        self.max_samples
            .is_some_and(|max| self.samples.len() >= max)
    }

    fn drain(&mut self, reader: &mut CaptureReader) -> Result<()> {
        self.raw.clear();
        if reader.drain_into(&mut self.raw) == 0 {
            return Ok(());
        }
        self.fresh.clear();
        self.resampler.push(&self.raw, &mut self.fresh)?;
        self.absorb();
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.fresh.clear();
        self.resampler.finish(&mut self.fresh)?;
        self.absorb();
        Ok(())
    }

    /// Takes over `fresh`: saturation, maximum duration, level, accumulation.
    fn absorb(&mut self) {
        for sample in &mut self.fresh {
            *sample = sanitize(*sample);
        }
        if let Some(max) = self.max_samples {
            let room = max.saturating_sub(self.samples.len());
            self.fresh.truncate(room);
        }
        let sink = self.sink.lock().ok().and_then(|slot| slot.clone());
        self.meter.push(&self.fresh, |level| {
            if let Some(sink) = &sink {
                sink(level);
            }
        });
        self.samples.extend_from_slice(&self.fresh);
    }
}

fn cancelled() -> Error {
    Error::msg(linuxflow_i18n::tr("Recording was cancelled"))
}

#[allow(clippy::too_many_arguments)]
fn capture_thread(
    source: Arc<dyn AudioSource>,
    device: Option<String>,
    max_samples: Option<usize>,
    sink: Arc<Mutex<Option<LevelSink>>>,
    commands: mpsc::Receiver<Command>,
    ready: Sender<std::result::Result<(), String>>,
    status: Arc<SessionStatus>,
) -> Result<Recording> {
    let (writer, mut reader) = capture_ring(RING_CAPACITY);
    let opened = match source.open(device.as_deref(), writer, status.errors.clone()) {
        Ok(opened) => opened,
        Err(err) => {
            let _ = ready.send(Err(err.to_string()));
            return Err(err);
        }
    };
    let input_rate = opened.input_rate;
    let mut guard = Some(opened.guard);
    let resampler = match StreamResampler::new(input_rate, SAMPLE_RATE_HZ) {
        Ok(resampler) => resampler,
        Err(err) => {
            drop(guard);
            let _ = ready.send(Err(err.to_string()));
            return Err(err);
        }
    };
    let _ = ready.send(Ok(()));
    let mut pipeline = Pipeline {
        resampler,
        meter: LevelMeter::new(),
        sink,
        samples: Vec::new(),
        max_samples,
        raw: Vec::with_capacity(RING_CAPACITY / 8),
        fresh: Vec::with_capacity(RING_CAPACITY / 8),
    };

    let command = loop {
        match commands.recv_timeout(POLL) {
            Ok(command) => break command,
            Err(RecvTimeoutError::Disconnected) => break Command::Cancel,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if guard.is_none() {
            continue; // maximum duration reached: simply wait for stop() or cancel()
        }
        pipeline.drain(&mut reader)?;
        if status.errors.is_fatal() {
            drop(guard);
            return Err(stream_failure(&status.errors));
        }
        if pipeline.limit_hit() {
            // Clean stop: the stream is closed, the audio kept is exactly the maximum duration.
            guard = None;
            status.limit_reached.store(true, Ordering::Release);
        }
    };

    match command {
        Command::Cancel => {
            drop(guard);
            Err(cancelled())
        }
        Command::Stop => {
            // Close the stream first: no more writes into the ring during the final drain.
            let still_open = guard.is_some();
            drop(guard);
            if still_open {
                pipeline.drain(&mut reader)?;
                if !pipeline.limit_hit() {
                    pipeline.finish()?;
                }
            }
            if status.errors.is_fatal() {
                return Err(stream_failure(&status.errors));
            }
            let lost = reader.dropped_frames();
            if lost > 0 {
                tracing::warn!("audio : {lost} trames perdues (anneau plein)");
            }
            if lost > u64::from(input_rate) / 2 {
                return Err(Error::msg(linuxflow_i18n::tr(
                    "Audio was lost because the system was too busy; please try again",
                )));
            }
            if pipeline.limit_hit() {
                status.limit_reached.store(true, Ordering::Release);
            }
            let samples = std::mem::take(&mut pipeline.samples);
            Ok(Recording { samples })
        }
    }
}

fn stream_failure(errors: &ErrorReporter) -> Error {
    let detail = errors.fatal_message().unwrap_or_default();
    Error::msg(linuxflow_i18n::tr_fmt(
        "Microphone error during recording: {}",
        &[&detail],
    ))
}

impl Recorder for CpalRecorder {
    fn start(&self, device: Option<&str>, max_sec: u64) -> Result<()> {
        // A capture already in progress is abandoned.
        self.cancel();
        let status = Arc::new(SessionStatus {
            limit_reached: AtomicBool::new(false),
            errors: ErrorReporter::new(),
        });
        match self.status.lock() {
            Ok(mut slot) => *slot = Arc::clone(&status),
            Err(poisoned) => *poisoned.into_inner() = Arc::clone(&status),
        }
        // 0 = aucune limite.
        let max_samples = (max_sec > 0).then(|| {
            (max_sec.saturating_mul(u64::from(SAMPLE_RATE_HZ))).min(usize::MAX as u64) as usize
        });
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let source = Arc::clone(&self.source);
        let sink = Arc::clone(&self.sink);
        let device = device.map(str::to_string);
        let handle = std::thread::Builder::new()
            .name("linuxflow-audio".into())
            .spawn(move || {
                capture_thread(source, device, max_samples, sink, cmd_rx, ready_tx, status)
            })
            .map_err(|e| {
                Error::msg(linuxflow_i18n::tr_fmt(
                    "Could not start audio capture: {}",
                    &[&e.to_string()],
                ))
            })?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                let _ = handle.join();
                return Err(Error::msg(message));
            }
            Err(_) => {
                let _ = cmd_tx.send(Command::Cancel);
                return Err(Error::msg(linuxflow_i18n::tr(
                    "The microphone did not respond in time",
                )));
            }
        }
        let session = Session {
            commands: cmd_tx,
            handle,
        };
        match self.current.lock() {
            Ok(mut slot) => *slot = Some(session),
            Err(poisoned) => *poisoned.into_inner() = Some(session),
        }
        Ok(())
    }

    fn stop(&self) -> Result<Recording> {
        let Some(session) = self.take_session() else {
            return Err(Error::msg(linuxflow_i18n::tr("No recording in progress")));
        };
        let _ = session.commands.send(Command::Stop);
        session.handle.join().unwrap_or_else(|_| {
            Err(Error::msg(linuxflow_i18n::tr(
                "The audio thread stopped unexpectedly",
            )))
        })
    }

    fn cancel(&self) {
        if let Some(session) = self.take_session() {
            let _ = session.commands.send(Command::Cancel);
            let _ = session.handle.join();
        }
    }

    fn set_level_sink(&self, sink: Box<dyn Fn(f32) + Send + Sync>) {
        let sink: LevelSink = Arc::from(sink);
        match self.sink.lock() {
            Ok(mut slot) => *slot = Some(sink),
            Err(poisoned) => *poisoned.into_inner() = Some(sink),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::synthetic::{SyntheticFormat, SyntheticSource};

    const WAIT: Duration = Duration::from_secs(10);

    fn recorder(source: SyntheticSource) -> (CpalRecorder, Arc<SyntheticSource>) {
        let source = Arc::new(source);
        (CpalRecorder::with_source(source.clone()), source)
    }

    fn energy_per_sample(s: &[f32]) -> f64 {
        s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len().max(1) as f64
    }

    #[test]
    fn pipeline_48k_mono() {
        let (rec, src) = recorder(SyntheticSource::sine(440.0, 0.5, 1.0, 48_000));
        rec.start(None, 30).expect("start");
        assert!(src.wait_exhausted(WAIT));
        let out = rec.stop().expect("stop");
        assert!(
            (out.samples.len() as i64 - 16_000).abs() <= 1,
            "{}",
            out.samples.len()
        );
        assert!((energy_per_sample(&out.samples) - 0.125).abs() < 0.01);
        assert!(out.samples.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn pipeline_44k1_stereo_i16() {
        let src = SyntheticSource::sine(440.0, 0.5, 1.0, 44_100)
            .with_channels(2)
            .with_format(SyntheticFormat::I16);
        let (rec, src) = recorder(src);
        rec.start(Some("default"), 30).expect("start");
        assert!(src.wait_exhausted(WAIT));
        let out = rec.stop().expect("stop");
        assert!((out.samples.len() as i64 - 16_000).abs() <= 1);
        assert!((energy_per_sample(&out.samples) - 0.125).abs() < 0.01);
    }

    #[test]
    fn pipeline_u16_and_16k_passthrough() {
        let src = SyntheticSource::sine(440.0, 0.5, 0.5, 16_000).with_format(SyntheticFormat::U16);
        let (rec, src) = recorder(src);
        rec.start(None, 30).expect("start");
        assert!(src.wait_exhausted(WAIT));
        let out = rec.stop().expect("stop");
        assert_eq!(out.samples.len(), 8_000);
        assert!((energy_per_sample(&out.samples) - 0.125).abs() < 0.01);
    }

    #[test]
    fn saturation_is_bounded() {
        let (rec, src) = recorder(SyntheticSource::sine(300.0, 1.8, 0.5, 48_000));
        rec.start(None, 30).expect("start");
        assert!(src.wait_exhausted(WAIT));
        let out = rec.stop().expect("stop");
        assert!(!out.samples.is_empty());
        assert!(out
            .samples
            .iter()
            .all(|s| s.is_finite() && (-1.0..=1.0).contains(s)));
        assert!(out.samples.iter().any(|s| s.abs() > 0.99));
    }

    #[test]
    fn max_duration_stops_cleanly() {
        let (rec, src) = recorder(SyntheticSource::sine(440.0, 0.5, 3.0, 48_000));
        rec.start(None, 1).expect("start");
        assert!(src.wait_exhausted(WAIT));
        let start = std::time::Instant::now();
        while !rec.limit_reached() && start.elapsed() < WAIT {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(rec.limit_reached());
        let out = rec.stop().expect("stop");
        assert_eq!(out.samples.len(), 16_000);
    }

    #[test]
    fn cancel_discards_and_silences_sink() {
        let (rec, _src) = recorder(SyntheticSource::sine(440.0, 0.5, 5.0, 48_000).realtime());
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        rec.set_level_sink(Box::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        rec.start(None, 30).expect("start");
        std::thread::sleep(Duration::from_millis(300));
        rec.cancel();
        let after_cancel = calls.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.load(Ordering::SeqCst), after_cancel);
        assert!(rec.stop().is_err());
    }

    #[test]
    fn level_sink_is_called_in_range() {
        let (rec, src) = recorder(SyntheticSource::sine(440.0, 0.5, 1.0, 48_000));
        let levels = Arc::new(Mutex::new(Vec::new()));
        let l = Arc::clone(&levels);
        rec.set_level_sink(Box::new(move |v| {
            if let Ok(mut list) = l.lock() {
                list.push(v);
            }
        }));
        rec.start(None, 30).expect("start");
        assert!(src.wait_exhausted(WAIT));
        rec.stop().expect("stop");
        let levels = levels.lock().expect("lock").clone();
        assert!((19..=20).contains(&levels.len()), "{}", levels.len()); // 1 s / 50 ms
        assert!(levels.iter().all(|v| (0.0..=1.0).contains(v)));
        // Sine 0.5: rms 0.354 -> (2.12)^0.7 -> saturated at 1.
        assert!(levels[5] > 0.99);
    }

    #[test]
    fn silence_gives_zero_levels() {
        let (rec, src) = recorder(SyntheticSource::silence(0.5, 16_000));
        let max = Arc::new(Mutex::new(0.0f32));
        let m = Arc::clone(&max);
        rec.set_level_sink(Box::new(move |v| {
            if let Ok(mut x) = m.lock() {
                *x = x.max(v);
            }
        }));
        rec.start(None, 30).expect("start");
        assert!(src.wait_exhausted(WAIT));
        rec.stop().expect("stop");
        assert_eq!(*max.lock().expect("lock"), 0.0);
    }

    #[test]
    fn stream_error_gives_clear_err() {
        let src = SyntheticSource::sine(440.0, 0.5, 2.0, 48_000)
            .with_stream_error_after(4_800, "device lost");
        let (rec, src) = recorder(src);
        rec.start(None, 30).expect("start");
        assert!(src.wait_exhausted(WAIT));
        let err = rec.stop().expect_err("must fail").to_string();
        assert!(err.contains("device lost"), "{err}");
    }

    #[test]
    fn open_error_is_returned_by_start() {
        let (rec, _src) =
            recorder(SyntheticSource::silence(0.1, 16_000).with_open_error("no such device"));
        let err = rec.start(None, 30).expect_err("must fail").to_string();
        assert!(err.contains("no such device"));
        assert!(rec.stop().is_err());
    }

    #[test]
    fn stop_without_start_and_restart() {
        let (rec, src) = recorder(SyntheticSource::sine(440.0, 0.5, 0.3, 16_000));
        assert!(rec.stop().is_err());
        rec.start(Some("mic-x"), 30).expect("start 1");
        rec.start(None, 30).expect("start 2 replaces the first one");
        assert!(src.wait_exhausted(WAIT));
        assert!(rec.stop().is_ok());
        assert_eq!(src.opened_with(), vec![Some("mic-x".to_string()), None]);
    }

    /// Real microphone: never started automatically.
    #[test]
    #[ignore = "opens the real microphone: LINUXFLOW_TEST_MIC=1 cargo test -p linuxflow-audio -- --ignored"]
    fn real_microphone_smoke() {
        if std::env::var("LINUXFLOW_TEST_MIC").as_deref() != Ok("1") {
            return;
        }
        let rec = CpalRecorder::new();
        rec.start(None, 5).expect("start");
        std::thread::sleep(Duration::from_millis(500));
        let out = rec.stop().expect("stop");
        assert!(!out.samples.is_empty());
    }
}
