//! Bridge between the audio source (real-time callback) and the processing thread.
//!
//! The callback only writes to a lock-free SPSC ring (`rtrb`): no allocation, no
//! mutex, no blocking call. Stream errors go through [`ErrorReporter`] (off the real-time
//! path: the cpal error callback is not in the capture loop).

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use linuxflow_core::domain::MicrophoneInfo;
use linuxflow_core::Error;

use crate::convert::{mix_frame, PcmSample};

/// Ring capacity in mono `f32` samples: 2^18, i.e. 1 MiB (~5.4 s at 48 kHz).
pub const RING_CAPACITY: usize = 1 << 18;

/// Producer side of the ring, used by the audio callback.
pub struct CaptureWriter {
    producer: rtrb::Producer<f32>,
    dropped: Arc<AtomicU64>,
}

impl CaptureWriter {
    /// Writes interleaved data (`i16`, `u16`, `f32`...), mixed down to mono. Returns the
    /// number of frames accepted; those that do not fit in the ring are counted as
    /// lost.
    ///
    /// Real time: no allocation, no lock.
    pub fn push_interleaved<T: PcmSample>(&mut self, data: &[T], channels: usize) -> usize {
        let accepted = self.try_push_interleaved(data, channels);
        let channels = channels.max(1);
        let total = data.len() / channels;
        if accepted < total {
            self.dropped
                .fetch_add((total - accepted) as u64, Ordering::Relaxed);
        }
        accepted
    }

    /// Like [`CaptureWriter::push_interleaved`] but without counting the losses: the caller decides
    /// (synthetic source that retries when the ring is full).
    pub fn try_push_interleaved<T: PcmSample>(&mut self, data: &[T], channels: usize) -> usize {
        let channels = channels.max(1);
        let frames = data.len() / channels;
        let n = frames.min(self.producer.slots());
        if n == 0 {
            return 0;
        }
        match self.producer.write_chunk_uninit(n) {
            Ok(chunk) => chunk.fill_from_iter(data.chunks_exact(channels).map(mix_frame)),
            Err(_) => 0,
        }
    }
}

/// Consumer side, used by the processing thread.
pub struct CaptureReader {
    consumer: rtrb::Consumer<f32>,
    dropped: Arc<AtomicU64>,
}

impl CaptureReader {
    /// Appends everything available to `out`; returns the number of samples read.
    pub fn drain_into(&mut self, out: &mut Vec<f32>) -> usize {
        let available = self.consumer.slots();
        if available == 0 {
            return 0;
        }
        match self.consumer.read_chunk(available) {
            Ok(chunk) => {
                let (a, b) = chunk.as_slices();
                out.extend_from_slice(a);
                out.extend_from_slice(b);
                chunk.commit_all();
                available
            }
            Err(_) => 0,
        }
    }

    /// Number of frames lost (ring full) since the opening.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Creates the ring and its two ends.
pub fn capture_ring(capacity: usize) -> (CaptureWriter, CaptureReader) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        CaptureWriter {
            producer,
            dropped: Arc::clone(&dropped),
        },
        CaptureReader { consumer, dropped },
    )
}

/// Severity of a stream error reported by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// No consequence on the audio (route change followed automatically, etc.).
    Ignore,
    /// A possible gap in the audio (xrun): logged, the capture continues.
    Warn,
    /// The capture is lost (device gone, permission denied...): the recording fails.
    Fatal,
}

/// Reporting of stream errors. Cloneable; usable from the driver's error callback.
#[derive(Clone, Default)]
pub struct ErrorReporter {
    fatal: Arc<AtomicBool>,
    message: Arc<Mutex<Option<String>>>,
    warnings: Arc<AtomicU64>,
}

impl ErrorReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an error; only the first fatal error is kept.
    pub fn report(&self, severity: Severity, message: &str) {
        match severity {
            Severity::Ignore => tracing::debug!("audio : {message}"),
            Severity::Warn => {
                self.warnings.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("audio : {message}");
            }
            Severity::Fatal => {
                tracing::error!("audio : {message}");
                if let Ok(mut slot) = self.message.lock() {
                    if slot.is_none() {
                        *slot = Some(message.to_string());
                    }
                }
                self.fatal.store(true, Ordering::Release);
            }
        }
    }

    pub fn is_fatal(&self) -> bool {
        self.fatal.load(Ordering::Acquire)
    }

    /// Message of the first fatal error.
    pub fn fatal_message(&self) -> Option<String> {
        self.message.lock().ok().and_then(|slot| slot.clone())
    }

    /// Number of warnings (xruns) received.
    pub fn warnings(&self) -> u64 {
        self.warnings.load(Ordering::Relaxed)
    }
}

/// Open source: the guard keeps the stream alive; dropping it stops the capture.
pub struct OpenedSource {
    /// Real sampling rate of the stream (Hz).
    pub input_rate: u32,
    /// Guard of the stream (not `Send`: created and dropped in the capture thread).
    pub guard: Box<dyn Any>,
}

/// Injectable audio source: real microphone ([`crate::CpalSource`]) or synthetic
/// ([`crate::SyntheticSource`]).
pub trait AudioSource: Send + Sync {
    /// Opens the stream and starts it. Called from the capture thread.
    /// `device`: `None` or `"default"` = default microphone; an absent identifier must fall back
    /// to the default microphone.
    fn open(
        &self,
        device: Option<&str>,
        writer: CaptureWriter,
        errors: ErrorReporter,
    ) -> Result<OpenedSource, Error>;

    /// Available microphones (the `default` entry is always present).
    fn microphones(&self) -> Vec<MicrophoneInfo>;
}
