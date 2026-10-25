//! Sound level for the pill's waveform.
//!
//! Formula: RMS over one sample out of 4, then
//! `level = min(1, (rms * 6)^0.7)`.
//!
//! Rate choice: the level is emitted in blocks of [`LEVEL_BLOCK_SAMPLES`] samples at
//! 16 kHz, i.e. 50 ms (20 Hz). 20 Hz stays within the 33 to 100 ms range of the contract
//! (`Recorder::set_level_sink`) and makes the animation smoother. Any smoothing is left
//! to the interface.

/// Size of a level block: 800 samples at 16 kHz = 50 ms.
pub const LEVEL_BLOCK_SAMPLES: usize = 800;

/// Converts an RMS into a level [0, 1].
pub fn level_from_rms(rms: f32) -> f32 {
    // `f32::min` ignores NaNs: they are discarded beforehand (NaN or negative -> 0).
    if rms.is_nan() || rms <= 0.0 {
        return 0.0;
    }
    (rms * 6.0).powf(0.7).min(1.0)
}

/// Level of a block: RMS over one sample out of 4, then the formula above.
pub fn level_from_block(block: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for sample in block.iter().step_by(4) {
        sum += sample * sample;
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    level_from_rms((sum / count as f32).sqrt())
}

/// Splits a 16 kHz stream into blocks and computes one level per complete block.
#[derive(Debug, Default)]
pub struct LevelMeter {
    block: Vec<f32>,
}

impl LevelMeter {
    pub fn new() -> Self {
        Self {
            block: Vec::with_capacity(LEVEL_BLOCK_SAMPLES),
        }
    }

    /// Adds samples; calls `emit` for each complete block. A partial block stays
    /// pending (it is never emitted: a level computed on too few samples would be wrong).
    pub fn push(&mut self, samples: &[f32], mut emit: impl FnMut(f32)) {
        let mut rest = samples;
        while !rest.is_empty() {
            let room = LEVEL_BLOCK_SAMPLES - self.block.len();
            let take = room.min(rest.len());
            self.block.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.block.len() == LEVEL_BLOCK_SAMPLES {
                emit(level_from_block(&self.block));
                self.block.clear();
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sine(amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin())
            .collect()
    }

    #[test]
    fn silence_is_zero() {
        assert_eq!(level_from_block(&[0.0; 800]), 0.0);
        assert_eq!(level_from_block(&[]), 0.0);
    }

    #[test]
    fn full_scale_sine_is_one() {
        assert_eq!(level_from_block(&sine(1.0, 800)), 1.0);
    }

    #[test]
    fn formula_intermediate_values() {
        // rms 0.05 -> (0.3)^0.7
        let expected = 0.3f32.powf(0.7);
        assert!((level_from_rms(0.05) - expected).abs() < 1e-6);
        assert!((level_from_rms(0.05) - 0.4304).abs() < 1e-3);
        // rms 0.01 -> (0.06)^0.7
        assert!((level_from_rms(0.01) - 0.06f32.powf(0.7)).abs() < 1e-6);
        // Constant signal of 0.1: rms 0.1 -> 0.6^0.7
        assert!((level_from_block(&[0.1; 800]) - 0.6f32.powf(0.7)).abs() < 1e-5);
    }

    #[test]
    fn saturates_and_is_bounded() {
        assert_eq!(level_from_rms(10.0), 1.0);
        assert_eq!(level_from_rms(0.0), 0.0);
        assert_eq!(level_from_rms(f32::NAN), 0.0);
        assert_eq!(level_from_rms(f32::INFINITY), 1.0);
        assert_eq!(level_from_rms(-1.0), 0.0);
    }

    #[test]
    fn monotonic() {
        let mut previous = -1.0;
        for step in 0..=100 {
            let level = level_from_rms(step as f32 / 100.0);
            assert!(level >= previous);
            previous = level;
        }
    }

    #[test]
    fn only_one_sample_in_four_is_used() {
        // The samples at indexes 1, 2, 3 (mod 4) are ignored.
        let mut block = vec![0.0f32; 800];
        for (i, s) in block.iter_mut().enumerate() {
            if i % 4 != 0 {
                *s = 1.0;
            }
        }
        assert_eq!(level_from_block(&block), 0.0);
    }

    #[test]
    fn meter_emits_per_full_block() {
        let mut meter = LevelMeter::new();
        let mut levels = Vec::new();
        meter.push(&sine(0.5, 1000), |l| levels.push(l));
        assert_eq!(levels.len(), 1);
        meter.push(&sine(0.5, 600), |l| levels.push(l));
        assert_eq!(levels.len(), 2); // 1600 accumulated samples = 2 blocks
        assert!(levels.iter().all(|l| (0.0..=1.0).contains(l) && *l > 0.5));
        meter.push(&[0.0; 799], |l| levels.push(l));
        assert_eq!(levels.len(), 2); // 1599: partial block not emitted
    }
}
