//! WAV utilities: 44-byte RIFF wrapper around little-endian PCM16.

use crate::constants::SAMPLE_RATE_HZ;
use crate::domain::Recording;

/// Size of the fixed WAV header.
pub const WAV_HEADER_LEN: usize = 44;

/// Wraps PCM16 samples (little-endian bytes) in a RIFF/WAVE header.
pub fn pcm16_to_wav(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let block_align = channels.wrapping_mul(2);
    let mut out = Vec::with_capacity(WAV_HEADER_LEN + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32.wrapping_add(data_len)).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // taille du bloc fmt
    out.extend_from_slice(&1u16.to_le_bytes()); // format PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(
        &sample_rate
            .wrapping_mul(u32::from(channels))
            .wrapping_mul(2)
            .to_le_bytes(),
    );
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Duration in milliseconds of a WAV produced by `pcm16_to_wav` (0 if the header is incomplete).
pub fn wav_duration_ms(wav: &[u8]) -> u64 {
    if wav.len() < WAV_HEADER_LEN {
        return 0;
    }
    let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    let block = u16::from_le_bytes([wav[32], wav[33]]);
    let block = if block == 0 { 2 } else { block };
    if rate == 0 {
        return 0;
    }
    let frames = (wav.len() - WAV_HEADER_LEN) as f64 / f64::from(block);
    // Rounding half-values towards +infinity.
    (frames / f64::from(rate) * 1000.0 + 0.5).floor() as u64
}

/// f32 [-1, 1] to i16, with saturation (`s<0 ? s*32768 : s*32767`, truncation).
pub fn f32_to_i16(sample: f32) -> i16 {
    let s = sample.clamp(-1.0, 1.0);
    if s < 0.0 {
        (s * 32768.0) as i16
    } else {
        (s * 32767.0) as i16
    }
}

/// i16 to f32 in [-1, 1[ (`/ 32768.0`).
pub fn i16_to_f32(sample: i16) -> f32 {
    f32::from(sample) / 32768.0
}

pub fn f32_to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples.iter().copied().map(f32_to_i16).collect()
}

pub fn pcm16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().copied().map(i16_to_f32).collect()
}

/// PCM16 little-endian bytes of an array of f32 samples.
pub fn f32_to_pcm16_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&f32_to_i16(*s).to_le_bytes());
    }
    out
}

/// 16 kHz mono WAV of a recording.
pub fn recording_to_wav(recording: &Recording) -> Vec<u8> {
    pcm16_to_wav(&f32_to_pcm16_bytes(&recording.samples), SAMPLE_RATE_HZ, 1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn header_byte_for_byte_16k_mono_one_second() {
        let pcm = vec![0u8; 32_000];
        let wav = pcm16_to_wav(&pcm, 16_000, 1);
        assert_eq!(wav.len(), 32_044);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 32_036);
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wav[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 32_000);
        assert_eq!(wav_duration_ms(&wav), 1000);
    }

    #[test]
    fn stereo_header() {
        let wav = pcm16_to_wav(&[0u8; 8], 44_100, 2);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 176_400);
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 4);
    }

    #[test]
    fn duration_short_input_is_zero() {
        assert_eq!(wav_duration_ms(&[]), 0);
        assert_eq!(wav_duration_ms(&[0u8; 43]), 0);
        assert_eq!(wav_duration_ms(&pcm16_to_wav(&[], 16_000, 1)), 0);
    }

    #[test]
    fn duration_rounds_half_up() {
        // 8 samples at 16 kHz = 0.5 ms -> 1 ms (rounded half up).
        let wav = pcm16_to_wav(&[0u8; 16], 16_000, 1);
        assert_eq!(wav_duration_ms(&wav), 1);
        // 1 sample = 0.0625 ms -> 0.
        assert_eq!(wav_duration_ms(&pcm16_to_wav(&[0u8; 2], 16_000, 1)), 0);
        // 100 ms exactes.
        assert_eq!(
            wav_duration_ms(&pcm16_to_wav(&vec![0u8; 3200], 16_000, 1)),
            100
        );
    }

    #[test]
    fn duration_block_zero_defaults_to_two_and_rate_zero_is_zero() {
        let mut wav = pcm16_to_wav(&[0u8; 32_000], 16_000, 1);
        wav[32] = 0;
        wav[33] = 0;
        assert_eq!(wav_duration_ms(&wav), 1000);
        wav[24..28].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(wav_duration_ms(&wav), 0);
    }

    #[test]
    fn f32_i16_conversions() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32768);
        assert_eq!(f32_to_i16(2.5), 32767);
        assert_eq!(f32_to_i16(-7.0), -32768);
        assert_eq!(f32_to_i16(f32::NAN), 0);
        assert_eq!(f32_to_i16(0.5), 16383);
        assert_eq!(f32_to_i16(-0.5), -16384);
        assert_eq!(i16_to_f32(-32768), -1.0);
        assert_eq!(i16_to_f32(16384), 0.5);
        assert_eq!(pcm16_to_f32(&[0, 16384]), vec![0.0, 0.5]);
        assert_eq!(f32_to_pcm16(&[0.0, 1.0]), vec![0, 32767]);
    }

    #[test]
    fn recording_to_wav_is_mono_16k() {
        let rec = Recording {
            samples: vec![0.0, 1.0, -1.0],
        };
        let wav = recording_to_wav(&rec);
        assert_eq!(wav.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(&wav[44..], &[0, 0, 0xFF, 0x7F, 0x00, 0x80]);
    }
}
