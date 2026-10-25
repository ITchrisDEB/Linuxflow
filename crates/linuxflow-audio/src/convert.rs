//! Sample format conversion and mono mixing. Pure functions, no allocation:
//! they are called from the audio callback.

/// PCM sample convertible to `f32` in [-1, 1].
pub trait PcmSample: Copy {
    fn to_f32(self) -> f32;
}

impl PcmSample for f32 {
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
}

impl PcmSample for i16 {
    #[inline]
    fn to_f32(self) -> f32 {
        f32::from(self) / 32768.0
    }
}

impl PcmSample for u16 {
    #[inline]
    fn to_f32(self) -> f32 {
        (f32::from(self) - 32768.0) / 32768.0
    }
}

impl PcmSample for i32 {
    #[inline]
    fn to_f32(self) -> f32 {
        (f64::from(self) / 2_147_483_648.0) as f32
    }
}

impl PcmSample for i8 {
    #[inline]
    fn to_f32(self) -> f32 {
        f32::from(self) / 128.0
    }
}

impl PcmSample for u8 {
    #[inline]
    fn to_f32(self) -> f32 {
        (f32::from(self) - 128.0) / 128.0
    }
}

/// Clamps a sample to [-1, 1]; a non-finite value becomes 0 (never a NaN downstream).
#[inline]
pub fn sanitize(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Mixes a frame (one sample per channel) to mono: average of the channels, clamped to [-1, 1].
#[inline]
pub fn mix_frame<T: PcmSample>(frame: &[T]) -> f32 {
    match frame {
        [] => 0.0,
        [single] => sanitize(single.to_f32()),
        _ => {
            let sum: f32 = frame.iter().map(|s| s.to_f32()).sum();
            sanitize(sum / frame.len() as f32)
        }
    }
}

/// Mixes interleaved data to mono into `out` (appends). `channels` = 0 is treated
/// as 1. A possible incomplete remainder at the end of the buffer is ignored.
pub fn mix_to_mono<T: PcmSample>(data: &[T], channels: usize, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    out.extend(data.chunks_exact(channels).map(mix_frame));
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn i16_conversion() {
        assert_eq!(0i16.to_f32(), 0.0);
        assert_eq!(i16::MIN.to_f32(), -1.0);
        assert!((i16::MAX.to_f32() - 0.999_969_5).abs() < 1e-6);
        assert_eq!(16384i16.to_f32(), 0.5);
    }

    #[test]
    fn u16_conversion() {
        assert_eq!(32768u16.to_f32(), 0.0);
        assert_eq!(0u16.to_f32(), -1.0);
        assert!((u16::MAX.to_f32() - 0.999_969_5).abs() < 1e-6);
        assert_eq!(49152u16.to_f32(), 0.5);
    }

    #[test]
    fn f32_i32_u8_i8_conversion() {
        assert_eq!(0.25f32.to_f32(), 0.25);
        assert_eq!(i32::MIN.to_f32(), -1.0);
        assert_eq!(0i32.to_f32(), 0.0);
        assert_eq!(128u8.to_f32(), 0.0);
        assert_eq!(0u8.to_f32(), -1.0);
        assert_eq!((-128i8).to_f32(), -1.0);
    }

    #[test]
    fn stereo_is_averaged() {
        let mut out = Vec::new();
        mix_to_mono(&[0.2f32, 0.6, -0.5, 0.5, 1.0, 0.0], 2, &mut out);
        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.4).abs() < 1e-6);
        assert!(out[1].abs() < 1e-6);
        assert!((out[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mono_passthrough_and_odd_tail_ignored() {
        let mut out = Vec::new();
        mix_to_mono(&[0.1f32, 0.2, 0.3], 1, &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
        out.clear();
        mix_to_mono(&[0.1f32, 0.3, 0.9], 2, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn six_channels_and_i16_mix() {
        let mut out = Vec::new();
        mix_to_mono(&[16384i16, 16384, 16384, 0, 0, 0], 6, &mut out);
        assert!((out[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn saturation_and_non_finite() {
        assert_eq!(sanitize(3.0), 1.0);
        assert_eq!(sanitize(-3.0), -1.0);
        assert_eq!(sanitize(f32::NAN), 0.0);
        assert_eq!(sanitize(f32::INFINITY), 0.0);
        let mut out = Vec::new();
        mix_to_mono(&[1.5f32, 1.5, -2.0, -2.0, f32::NAN, 0.0], 2, &mut out);
        assert_eq!(out, vec![1.0, -1.0, 0.0]);
    }
}
