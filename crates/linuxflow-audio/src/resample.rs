//! Streaming resampling to 16 kHz (rubato 5.x, sinc, voice quality).
//!
//! rubato 5.0.1 API used: `Async::new_sinc(ratio, relative_max_ratio, params, chunk, channels,
//! FixedAsync::Input)`, `Resampler::process_into_buffer` with
//! `audioadapter_buffers::direct::InterleavedSlice` adapters, and `Indexing { partial_len, .. }` for the
//! last partial block. The filter delay (`output_delay`) is compensated: the output is aligned
//! with the input and its length is exactly `round(inputs * ratio)`.

use audioadapter_buffers::direct::InterleavedSlice;
use linuxflow_core::Error;
use rubato::{
    Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Size of an input block processed at once (in frames), ~21 ms at 48 kHz.
const CHUNK_FRAMES: usize = 1024;

pub struct StreamResampler {
    engine: Option<Engine>,
    ratio: f64,
    in_total: u64,
}

struct Engine {
    resampler: Async<f32>,
    /// Pending input (less than a whole block).
    pending: Vec<f32>,
    /// Reused output buffer (no allocation per block).
    out_buf: Vec<f32>,
    /// Output frames to throw away at the start (filter delay).
    skip: usize,
    /// Output frames emitted after delay compensation.
    emitted: u64,
}

fn fail(err: impl std::fmt::Display) -> Error {
    Error::msg(linuxflow_i18n::tr_fmt(
        "Audio resampling failed: {}",
        &[&err.to_string()],
    ))
}

impl StreamResampler {
    /// `from_hz == to_hz`: direct pass-through without a filter.
    pub fn new(from_hz: u32, to_hz: u32) -> Result<Self, Error> {
        if from_hz == 0 || to_hz == 0 {
            return Err(fail("invalid sample rate"));
        }
        let ratio = f64::from(to_hz) / f64::from(from_hz);
        if from_hz == to_hz {
            return Ok(Self {
                engine: None,
                ratio,
                in_total: 0,
            });
        }
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: None,
            interpolation: SincInterpolationType::Cubic,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler =
            Async::<f32>::new_sinc(ratio, 1.1, &params, CHUNK_FRAMES, 1, FixedAsync::Input)
                .map_err(fail)?;
        let out_buf = vec![0.0f32; resampler.output_frames_max()];
        let skip = resampler.output_delay();
        Ok(Self {
            engine: Some(Engine {
                resampler,
                pending: Vec::with_capacity(CHUNK_FRAMES * 2),
                out_buf,
                skip,
                emitted: 0,
            }),
            ratio,
            in_total: 0,
        })
    }

    /// Adds mono input; the available output samples are appended to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), Error> {
        self.in_total += input.len() as u64;
        let Some(engine) = self.engine.as_mut() else {
            out.extend_from_slice(input);
            return Ok(());
        };
        engine.pending.extend_from_slice(input);
        let mut pos = 0usize;
        loop {
            let need = engine.resampler.input_frames_next();
            if engine.pending.len() - pos < need {
                break;
            }
            engine.run(pos, need, None, out)?;
            pos += need;
        }
        engine.pending.drain(..pos);
        Ok(())
    }

    /// Ends the stream: processes the remainder (padded with silence) and flushes the filter delay.
    /// After this call, the total emitted is `round(inputs * ratio)`.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), Error> {
        let expected = (self.in_total as f64 * self.ratio).round() as u64;
        let Some(engine) = self.engine.as_mut() else {
            return Ok(());
        };
        let start_len = out.len();
        let emitted_before = engine.emitted;
        let remaining = engine.pending.len();
        let need = engine.resampler.input_frames_next();
        engine.pending.resize(need.max(remaining), 0.0);
        if remaining > 0 || engine.emitted < expected {
            engine.run(0, need, Some(remaining.min(need)), out)?;
        }
        engine.pending.clear();
        // Flushes the delay: feeds silence as long as frames are missing (safety bound).
        let mut guard = 0;
        while engine.emitted < expected && guard < 64 {
            engine
                .pending
                .resize(engine.resampler.input_frames_next(), 0.0);
            engine.pending.fill(0.0);
            let n = engine.pending.len();
            engine.run(0, n, Some(0), out)?;
            guard += 1;
        }
        engine.pending.clear();
        let total_new = (out.len() - start_len) as u64;
        let allowed = expected.saturating_sub(emitted_before);
        if total_new > allowed {
            out.truncate(start_len + allowed as usize);
        }
        engine.emitted = expected;
        Ok(())
    }
}

impl Engine {
    /// Processes `pending[pos..pos + need]`. `partial` = number of valid frames if the block is
    /// incomplete (the rest is read as silence).
    fn run(
        &mut self,
        pos: usize,
        need: usize,
        partial: Option<usize>,
        out: &mut Vec<f32>,
    ) -> Result<(), Error> {
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: partial,
            active_channels_mask: None,
        };
        let input = InterleavedSlice::new(&self.pending[pos..pos + need], 1, need).map_err(fail)?;
        let capacity = self.out_buf.len();
        let mut output = InterleavedSlice::new_mut(&mut self.out_buf, 1, capacity).map_err(fail)?;
        let (_read, written) = self
            .resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(fail)?;
        let skipped = self.skip.min(written);
        self.skip -= skipped;
        let usable = &self.out_buf[skipped..written];
        self.emitted += usable.len() as u64;
        out.extend_from_slice(usable);
        Ok(())
    }
}

/// Resamples a complete signal at once (utility for tests and files).
pub fn resample_all(input: &[f32], from_hz: u32, to_hz: u32) -> Result<Vec<f32>, Error> {
    let mut resampler = StreamResampler::new(from_hz, to_hz)?;
    let mut out = Vec::with_capacity(
        (input.len() as f64 * f64::from(to_hz) / f64::from(from_hz)) as usize + 16,
    );
    resampler.push(input, &mut out)?;
    resampler.finish(&mut out)?;
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tone(freq: f64, amp: f64, rate: u32, secs: f64) -> Vec<f32> {
        let n = (f64::from(rate) * secs) as usize;
        (0..n)
            .map(|i| {
                (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin())
                    as f32
            })
            .collect()
    }

    /// Power of the component at `freq` (Goertzel), normalised: a sinusoid of amplitude A
    /// gives A^2 / 4.
    fn power_at(signal: &[f32], freq: f64, rate: u32) -> f64 {
        let n = signal.len() as f64;
        let w = 2.0 * std::f64::consts::PI * freq / f64::from(rate);
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in signal {
            let s0 = f64::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let re = s1 - s2 * w.cos();
        let im = s2 * w.sin();
        (re * re + im * im) / (n * n)
    }

    fn energy(signal: &[f32]) -> f64 {
        signal.iter().map(|s| f64::from(*s) * f64::from(*s)).sum()
    }

    fn db(ratio: f64) -> f64 {
        10.0 * ratio.log10()
    }

    #[test]
    fn frame_count_48k() {
        for secs in [0.5, 1.0, 2.37] {
            let input = tone(440.0, 0.5, 48_000, secs);
            let out = resample_all(&input, 48_000, 16_000).expect("resample");
            let expected = (input.len() as f64 / 3.0).round() as i64;
            assert!(
                (out.len() as i64 - expected).abs() <= 1,
                "{} vs {expected}",
                out.len()
            );
        }
    }

    #[test]
    fn frame_count_44k1() {
        for secs in [0.5, 1.0, 3.3] {
            let input = tone(440.0, 0.5, 44_100, secs);
            let out = resample_all(&input, 44_100, 16_000).expect("resample");
            let expected = (input.len() as f64 * 16_000.0 / 44_100.0).round() as i64;
            assert!(
                (out.len() as i64 - expected).abs() <= 1,
                "{} vs {expected}",
                out.len()
            );
        }
    }

    #[test]
    fn short_inputs_do_not_panic() {
        for len in [0usize, 1, 10, 1023, 1024, 1025] {
            let input = vec![0.1f32; len];
            let out = resample_all(&input, 48_000, 16_000).expect("resample");
            let expected = (len as f64 / 3.0).round() as i64;
            assert!(
                (out.len() as i64 - expected).abs() <= 1,
                "len {len}: {}",
                out.len()
            );
        }
    }

    #[test]
    fn energy_is_preserved_in_band() {
        for rate in [48_000u32, 44_100] {
            let input = tone(1_000.0, 0.5, rate, 1.0);
            let out = resample_all(&input, rate, 16_000).expect("resample");
            // Mean energy per sample (sinusoid of amplitude 0.5: 0.125).
            let e_in = energy(&input) / input.len() as f64;
            let e_out = energy(&out) / out.len() as f64;
            assert!((db(e_out / e_in)).abs() < 0.3, "{rate}: {e_in} -> {e_out}");
        }
    }

    #[test]
    fn no_audible_aliasing() {
        // 1 kHz (useful) + an out-of-band tone that would fold into the voice band if it got through.
        // 48 kHz: 10 kHz would fold to 6 kHz, 13 kHz to 3 kHz.
        // 44.1 kHz: 11 kHz would fold to 5 kHz, 14 kHz to 2 kHz.
        let cases: [(u32, f64, f64, f64); 4] = [
            (48_000, 10_000.0, 6_000.0, 0.0),
            (48_000, 13_000.0, 3_000.0, 0.0),
            (44_100, 11_000.0, 5_000.0, 0.0),
            (44_100, 14_000.0, 2_000.0, 0.0),
        ];
        for (rate, out_of_band, alias, _) in cases {
            let a = tone(1_000.0, 0.4, rate, 1.0);
            let b = tone(out_of_band, 0.4, rate, 1.0);
            let input: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
            let out = resample_all(&input, rate, 16_000).expect("resample");
            // Ignores the edges (filter transients).
            let mid = &out[800..out.len() - 800];
            let wanted = power_at(mid, 1_000.0, 16_000);
            let aliased = power_at(mid, alias, 16_000);
            let attenuation = db(aliased / wanted);
            assert!(
                attenuation < -60.0,
                "{rate} Hz: {out_of_band} Hz folds to {alias} Hz at {attenuation:.1} dB"
            );
            // The useful tone keeps its amplitude (0.4^2/4 = 0.04).
            assert!(
                (db(wanted / 0.04)).abs() < 0.3,
                "ton utile : {:.2} dB",
                db(wanted / 0.04)
            );
        }
    }

    #[test]
    fn voice_band_is_flat() {
        // Gain close to 0 dB over the whole useful voice band (300 Hz to 6 kHz).
        for rate in [48_000u32, 44_100] {
            for freq in [300.0, 1_000.0, 3_000.0, 6_000.0] {
                let input = tone(freq, 0.5, rate, 1.0);
                let out = resample_all(&input, rate, 16_000).expect("resample");
                let mid = &out[800..out.len() - 800];
                let gain = db(power_at(mid, freq, 16_000) / (0.5 * 0.5 / 4.0));
                assert!(gain.abs() < 0.5, "{rate} Hz, {freq} Hz : {gain:.2} dB");
            }
        }
    }

    #[test]
    fn streaming_matches_one_shot() {
        let input = tone(700.0, 0.5, 48_000, 1.0);
        let whole = resample_all(&input, 48_000, 16_000).expect("resample");
        let mut resampler = StreamResampler::new(48_000, 16_000).expect("new");
        let mut out = Vec::new();
        for chunk in input.chunks(333) {
            resampler.push(chunk, &mut out).expect("push");
        }
        resampler.finish(&mut out).expect("finish");
        assert_eq!(out.len(), whole.len());
        for (a, b) in out.iter().zip(&whole) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn same_rate_is_passthrough() {
        let input = tone(440.0, 0.5, 16_000, 0.1);
        let out = resample_all(&input, 16_000, 16_000).expect("resample");
        assert_eq!(out, input);
    }

    #[test]
    fn signal_is_time_aligned() {
        // An impulse must stay at the same instant (filter delay compensated).
        let mut input = vec![0.0f32; 48_000];
        input[24_000] = 1.0;
        let out = resample_all(&input, 48_000, 16_000).expect("resample");
        let (peak_index, _) = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .expect("non vide");
        assert!(
            (peak_index as i64 - 8_000).abs() <= 1,
            "peak at {peak_index}"
        );
    }

    #[test]
    fn invalid_rate_is_error() {
        assert!(StreamResampler::new(0, 16_000).is_err());
    }
}
