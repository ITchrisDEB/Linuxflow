//! Boost of a weak microphone before transcription: "Weak microphone" setting (off, automatic
//! or fixed gain).
//!
//! A microphone set too low (voice at −40 dB) makes Whisper lose words. The whole recording
//! is available before transcription: ONE constant gain is therefore applied (no pumping and no
//! variation inside a sentence), only to raise a weak signal. A signal that is already strong
//! enough is left alone, and so is near-silence (the background noise is not amplified).

use crate::domain::MicBoost;

/// Percentile of |x| used as the "peak level" (ignores isolated clicks).
const PEAK_PERCENTILE: f32 = 0.995;
/// Below this: silence or background noise, nothing to amplify.
const FLOOR: f32 = 0.0015;
/// Above this: sufficient level, no gain.
const ENOUGH: f32 = 0.1;
/// Target peak level (about −10 dBFS: headroom before clipping).
const TARGET: f32 = 0.3;
/// Maximum gain (26 dB).
const MAX_GAIN: f32 = 20.0;

/// Gain to apply to `samples` (1.0 = none).
pub fn auto_gain_factor(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 1.0;
    }
    let mut mags: Vec<f32> = samples.iter().map(|s| s.abs()).collect();
    let k = (((mags.len() - 1) as f32) * PEAK_PERCENTILE) as usize;
    let (_, level, _) = mags.select_nth_unstable_by(k, f32::total_cmp);
    let level = *level;
    if !level.is_finite() || !(FLOOR..ENOUGH).contains(&level) {
        return 1.0;
    }
    (TARGET / level).min(MAX_GAIN)
}

/// Applies the automatic gain (output bounded to ±1). Returns the gain used.
pub fn apply_auto_gain(samples: &mut [f32]) -> f32 {
    let g = auto_gain_factor(samples);
    if g != 1.0 {
        for s in samples.iter_mut() {
            *s = (*s * g).clamp(-1.0, 1.0);
        }
    }
    g
}

/// Applies the boost chosen by the user. Returns the factor used (1.0 = none).
pub fn apply_mic_boost(samples: &mut [f32], mode: MicBoost) -> f32 {
    match mode {
        MicBoost::Off => 1.0,
        MicBoost::Auto => apply_auto_gain(samples),
        fixed => {
            let g = fixed.fixed_factor().unwrap_or(1.0);
            for s in samples.iter_mut() {
                *s = if s.is_finite() {
                    (*s * g).clamp(-1.0, 1.0)
                } else {
                    0.0
                };
            }
            g
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (i as f32 * 0.05).sin())
            .collect::<Vec<_>>()
    }

    #[test]
    fn a_quiet_voice_is_raised_to_the_target() {
        let mut v = tone(0.01, 16_000);
        let g = apply_auto_gain(&mut v);
        assert!((g - MAX_GAIN).abs() < 1e-3, "{g}");
        let peak = v.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.2).abs() < 0.01, "{peak}");
        let mut w = tone(0.05, 16_000);
        let g = apply_auto_gain(&mut w);
        assert!(g > 5.0 && g < 7.0, "{g}");
    }

    #[test]
    fn a_normal_or_loud_signal_is_left_alone() {
        for amp in [0.12, 0.4, 0.9] {
            let mut v = tone(amp, 16_000);
            let before = v.clone();
            assert_eq!(apply_auto_gain(&mut v), 1.0);
            assert_eq!(v, before);
        }
    }

    #[test]
    fn silence_and_background_noise_are_not_amplified() {
        let mut silence = vec![0.0_f32; 16_000];
        assert_eq!(apply_auto_gain(&mut silence), 1.0);
        let mut hiss = tone(0.001, 16_000);
        assert_eq!(apply_auto_gain(&mut hiss), 1.0);
        let mut empty: Vec<f32> = Vec::new();
        assert_eq!(apply_auto_gain(&mut empty), 1.0);
    }

    #[test]
    fn an_isolated_click_does_not_prevent_the_boost() {
        let mut v = tone(0.01, 16_000);
        v[8_000] = 0.9;
        let g = auto_gain_factor(&v);
        assert!(g > 15.0, "{g}");
        apply_auto_gain(&mut v);
        assert!(v.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn non_finite_samples_never_panic() {
        let mut v = vec![f32::NAN, 0.01, f32::INFINITY, -0.01];
        let _ = apply_auto_gain(&mut v);
    }

    #[test]
    fn off_changes_nothing_and_fixed_levels_apply_their_gain() {
        let base = tone(0.01, 1_000);
        let mut off = base.clone();
        assert_eq!(apply_mic_boost(&mut off, MicBoost::Off), 1.0);
        assert_eq!(off, base);
        for (mode, g) in [
            (MicBoost::Low, 2.0),
            (MicBoost::Medium, 4.0),
            (MicBoost::High, 10.0),
        ] {
            let mut v = base.clone();
            assert_eq!(apply_mic_boost(&mut v, mode), g);
            assert!((v[100] - base[100] * g).abs() < 1e-6, "{mode:?}");
        }
        let mut loud = tone(0.5, 1_000);
        apply_mic_boost(&mut loud, MicBoost::High);
        assert!(loud.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn auto_mode_matches_the_automatic_gain() {
        let mut a = tone(0.01, 16_000);
        let mut b = a.clone();
        assert_eq!(
            apply_mic_boost(&mut a, MicBoost::Auto),
            apply_auto_gain(&mut b)
        );
        assert_eq!(a, b);
    }
}
