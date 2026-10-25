//! Test source: injects a known signal into the whole pipeline without a microphone.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use linuxflow_core::domain::MicrophoneInfo;
use linuxflow_core::Error;

use crate::capture::{AudioSource, CaptureWriter, ErrorReporter, OpenedSource, Severity};
use crate::convert::PcmSample;
use crate::microphones::default_entry;

/// Format in which the signal is presented to the pipeline (exercises the conversions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticFormat {
    F32,
    I16,
    U16,
}

/// Synthetic source. The mono signal is duplicated onto `channels` channels.
pub struct SyntheticSource {
    signal: Arc<Vec<f32>>,
    rate: u32,
    channels: usize,
    format: SyntheticFormat,
    realtime: bool,
    fail_after: Option<(usize, String)>,
    open_error: Option<String>,
    exhausted: Arc<AtomicBool>,
    opened_with: Mutex<Vec<Option<String>>>,
}

impl SyntheticSource {
    /// Any mono signal (values in [-1, 1]; beyond that, saturation can be tested).
    pub fn from_samples(samples: Vec<f32>, rate: u32) -> Self {
        Self {
            signal: Arc::new(samples),
            rate,
            channels: 1,
            format: SyntheticFormat::F32,
            realtime: false,
            fail_after: None,
            open_error: None,
            exhausted: Arc::new(AtomicBool::new(false)),
            opened_with: Mutex::new(Vec::new()),
        }
    }

    pub fn sine(freq_hz: f64, amplitude: f64, secs: f64, rate: u32) -> Self {
        let n = (f64::from(rate) * secs) as usize;
        let samples = (0..n)
            .map(|i| {
                (amplitude
                    * (2.0 * std::f64::consts::PI * freq_hz * i as f64 / f64::from(rate)).sin())
                    as f32
            })
            .collect();
        Self::from_samples(samples, rate)
    }

    pub fn silence(secs: f64, rate: u32) -> Self {
        Self::from_samples(vec![0.0; (f64::from(rate) * secs) as usize], rate)
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels.max(1);
        self
    }

    pub fn with_format(mut self, format: SyntheticFormat) -> Self {
        self.format = format;
        self
    }

    /// Real-time pace (the duration of the signal is really waited for); by default, as fast
    /// as the ring accepts it.
    pub fn realtime(mut self) -> Self {
        self.realtime = true;
        self
    }

    /// Injects a fatal stream error after `frames` frames.
    pub fn with_stream_error_after(mut self, frames: usize, message: &str) -> Self {
        self.fail_after = Some((frames, message.to_string()));
        self
    }

    /// Makes `open` fail (device missing, busy...).
    pub fn with_open_error(mut self, message: &str) -> Self {
        self.open_error = Some(message.to_string());
        self
    }

    /// True when the whole signal has been pushed into the ring.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Acquire)
    }

    /// Waits for the end of the injection (`timeout` at most).
    pub fn wait_exhausted(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while !self.is_exhausted() {
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    /// Devices requested at each open (to check the selection).
    pub fn opened_with(&self) -> Vec<Option<String>> {
        self.opened_with
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }
}

struct Guard {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn convert<T: PcmSample>(block: &[f32], channels: usize, f: impl Fn(f32) -> T) -> Vec<T> {
    let mut out = Vec::with_capacity(block.len() * channels);
    for &s in block {
        for _ in 0..channels {
            out.push(f(s));
        }
    }
    out
}

fn push_all<T: PcmSample>(
    writer: &mut CaptureWriter,
    data: &[T],
    channels: usize,
    stop: &AtomicBool,
) {
    let mut offset = 0;
    while offset < data.len() && !stop.load(Ordering::Acquire) {
        let accepted = writer.try_push_interleaved(&data[offset..], channels);
        if accepted == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        offset += accepted * channels;
    }
}

impl AudioSource for SyntheticSource {
    fn open(
        &self,
        device: Option<&str>,
        mut writer: CaptureWriter,
        errors: ErrorReporter,
    ) -> Result<OpenedSource, Error> {
        if let Ok(mut list) = self.opened_with.lock() {
            list.push(device.map(str::to_string));
        }
        if let Some(message) = &self.open_error {
            return Err(Error::msg(message.clone()));
        }
        self.exhausted.store(false, Ordering::Release);
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&self.signal);
        let exhausted = Arc::clone(&self.exhausted);
        let (rate, channels, format, realtime) =
            (self.rate, self.channels, self.format, self.realtime);
        let fail_after = self.fail_after.clone();
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("linuxflow-synth".into())
            .spawn(move || {
                let block_frames = (rate as usize / 100).max(1); // 10 ms
                let start = Instant::now();
                let mut sent = 0usize;
                for block in signal.chunks(block_frames) {
                    if thread_stop.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some((limit, message)) = &fail_after {
                        if sent >= *limit {
                            errors.report(Severity::Fatal, message);
                            exhausted.store(true, Ordering::Release);
                            return;
                        }
                    }
                    match format {
                        SyntheticFormat::F32 => push_all(
                            &mut writer,
                            &convert(block, channels, |s| s),
                            channels,
                            &thread_stop,
                        ),
                        SyntheticFormat::I16 => push_all(
                            &mut writer,
                            &convert(block, channels, |s| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
                            channels,
                            &thread_stop,
                        ),
                        SyntheticFormat::U16 => push_all(
                            &mut writer,
                            &convert(block, channels, |s| {
                                ((s.clamp(-1.0, 1.0) * 32767.0) + 32768.0) as u16
                            }),
                            channels,
                            &thread_stop,
                        ),
                    }
                    sent += block.len();
                    if realtime {
                        let due = Duration::from_secs_f64(sent as f64 / f64::from(rate));
                        if let Some(wait) = due.checked_sub(start.elapsed()) {
                            std::thread::sleep(wait);
                        }
                    }
                }
                exhausted.store(true, Ordering::Release);
            })
            .map_err(|e| Error::msg(e.to_string()))?;
        let guard: Box<dyn Any> = Box::new(Guard {
            stop,
            handle: Some(handle),
        });
        Ok(OpenedSource {
            input_rate: self.rate,
            guard,
        })
    }

    fn microphones(&self) -> Vec<MicrophoneInfo> {
        vec![default_entry()]
    }
}
