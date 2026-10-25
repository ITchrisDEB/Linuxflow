//! whisper.cpp backend (through `whisper-rs`). The only module that touches the native calls.
//!
//! Decoding parameters: beam 5, Silero VAD (min. silence 300 ms, margin 200 ms),
//! no previous context, automatic language if not specified, initial prompt = hints.
//!
//! VAD: `WhisperState::full` calls `whisper_full_with_state`, which IGNORES `params.vad` (only
//! `whisper_full` on the context handles it, and whisper-rs 0.16 does not expose it). The Silero VAD is
//! therefore applied here before the transcription, with `WhisperVadContext`: only the
//! speech segments are kept (same splitting as whisper.cpp), and without speech the text is empty.
//! The whisper.cpp/ggml logs are redirected to `tracing` (never stdout/stderr).

use std::path::Path;
use std::sync::Once;

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::cpu_guard;
use crate::engine::VadMode;
use crate::engine::{Backend, DecodeParams, EngineConfig, LoadFailure, LoadedModel, RawTranscript};
use crate::hardware::GPU_COMPILED;
use crate::plan::{Device, LoadPlan};

/// Beam width (beam search).
const BEAM_SIZE: i32 = 5;
/// Minimum silence and speech margin of the VAD, in milliseconds.
const VAD_MIN_SILENCE_MS: i32 = 300;
const VAD_SPEECH_PAD_MS: i32 = 200;
/// Duration of the warm-up silence.
/// Margin kept before the first and after the last speech segment (`Trim` mode), in
/// centiseconds, on top of the VAD's `speech_pad`.
const TRIM_MARGIN_CS: f32 = 20.0;
const WARMUP_SAMPLES: usize = 16_000;
const MAX_DEFAULT_THREADS: usize = 8;

static LOG_HOOKS: Once = Once::new();

/// Redirects whisper.cpp and ggml to `tracing` (idempotent).
pub fn install_log_hooks() {
    LOG_HOOKS.call_once(whisper_rs::install_logging_hooks);
}

pub struct WhisperBackend {
    config: EngineConfig,
}

impl WhisperBackend {
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    fn n_threads(&self) -> i32 {
        let n = self.config.n_threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(4)
                .min(MAX_DEFAULT_THREADS)
        });
        n.clamp(1, 64) as i32
    }
}

impl Backend for WhisperBackend {
    fn load(&self, plan: &LoadPlan) -> Result<Box<dyn LoadedModel>, LoadFailure> {
        // Before any native call: an incompatible processor would crash the process (SIGILL).
        cpu_guard::ensure_cpu_supported().map_err(|e| LoadFailure::Fatal(e.to_string()))?;
        install_log_hooks();

        let mut ctx_params = WhisperContextParameters::default();
        match plan.device {
            Device::Cpu => {
                ctx_params.use_gpu(false);
            }
            Device::Gpu(index) => {
                if !GPU_COMPILED {
                    return Err(LoadFailure::Recoverable(
                        "prise en charge Vulkan absente de cette compilation".into(),
                    ));
                }
                ctx_params
                    .use_gpu(true)
                    .gpu_device(index as std::os::raw::c_int);
            }
        }
        if !plan.model_path.is_file() {
            return Err(LoadFailure::Recoverable(format!(
                "model file missing: {}",
                plan.model_path.display()
            )));
        }
        let path = plan
            .model_path
            .to_str()
            .ok_or_else(|| LoadFailure::Recoverable("model path is not UTF-8".into()))?;
        // `new_with_params` returns an error if the file is corrupt or if the allocation of the
        // model fails (whisper_init_* returns NULL): the next step is then attempted.
        let ctx = WhisperContext::new_with_params(path, ctx_params)
            .map_err(|e| LoadFailure::Recoverable(format!("model loading: {e}")))?;
        let state = ctx
            .create_state()
            .map_err(|e| LoadFailure::Recoverable(format!("whisper state: {e}")))?;

        let vad = self
            .config
            .vad_model_path
            .as_deref()
            .filter(|p| p.is_file())
            .and_then(Path::to_str)
            .filter(|p| !p.contains('\0'))
            .and_then(|path| {
                let mut params = WhisperVadContextParams::default();
                params.set_n_threads(1);
                params.set_use_gpu(false);
                WhisperVadContext::new(path, params)
                    .map_err(|e| tracing::warn!("could not load the VAD: {e}"))
                    .ok()
            });
        if vad.is_none() {
            tracing::warn!("VAD model missing or unusable: voice detection disabled");
        }
        Ok(Box::new(WhisperLoaded {
            ctx,
            state: Some(state),
            vad,
            vad_mode: self.config.vad_mode,
            n_threads: self.n_threads(),
        }))
    }
}

struct WhisperLoaded {
    ctx: WhisperContext,
    /// State reused from one call to the next; dropped after an error then recreated.
    state: Option<WhisperState>,
    vad: Option<WhisperVadContext>,
    vad_mode: VadMode,
    n_threads: i32,
}

impl WhisperLoaded {
    fn run(
        &mut self,
        audio: &[f32],
        language: Option<&str>,
        prompt: Option<&str>,
        use_vad: bool,
        beam: i32,
    ) -> Result<RawTranscript, String> {
        // VAD: only keep the speech; without speech, no transcription (avoids
        // hallucinations of Whisper on silence).
        let filtered;
        let mode = self.vad_mode;
        let audio = match self
            .vad
            .as_mut()
            .filter(|_| use_vad && mode != VadMode::Off)
        {
            Some(vad) => {
                let mut vad_params = WhisperVadParams::new();
                vad_params.set_min_silence_duration(VAD_MIN_SILENCE_MS);
                vad_params.set_speech_pad(VAD_SPEECH_PAD_MS);
                match vad.segments_from_samples(vad_params, audio) {
                    Ok(segments) => {
                        let spans: Vec<(f32, f32)> = segments.map(|s| (s.start, s.end)).collect();
                        if mode == VadMode::Trim {
                            match vad_trim_range(audio.len(), &spans, TRIM_MARGIN_CS) {
                                Some((a, b)) => &audio[a..b],
                                None => return Ok(RawTranscript::default()),
                            }
                        } else {
                            match vad_filter_audio(audio, &spans) {
                                Some(samples) => {
                                    filtered = samples;
                                    filtered.as_slice()
                                }
                                None => return Ok(RawTranscript::default()),
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("VAD failed, audio not filtered: {e}");
                        audio
                    }
                }
            }
            None => audio,
        };
        let mut state = match self.state.take() {
            Some(state) => state,
            None => self
                .ctx
                .create_state()
                .map_err(|e| format!("whisper state: {e}"))?,
        };
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: beam,
            patience: -1.0,
        });
        // `None` = "auto": whisper.cpp detects the language (nullptr would also be accepted).
        params.set_language(Some(language.unwrap_or("auto")));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_n_threads(self.n_threads);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(prompt) = prompt.filter(|p| !p.contains('\0')) {
            params.set_initial_prompt(prompt);
        }
        state
            .full(params, audio)
            .map_err(|e| format!("transcription : {e}"))?;
        let segments: Vec<String> = state
            .as_iter()
            .map(|s| s.to_str_lossy().map(|t| t.into_owned()).unwrap_or_default())
            .collect();
        let detected =
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_string);
        self.state = Some(state);
        Ok(RawTranscript {
            segments,
            language: detected,
        })
    }
}

impl LoadedModel for WhisperLoaded {
    fn transcribe(
        &mut self,
        audio: &[f32],
        params: &DecodeParams,
    ) -> Result<RawTranscript, String> {
        self.run(
            audio,
            params.language.as_deref(),
            params.prompt.as_deref(),
            true,
            BEAM_SIZE,
        )
    }

    fn warmup(&mut self) -> Result<(), String> {
        // 1 s of silence, beam 1, no VAD, language fixed.
        self.run(&vec![0.0; WARMUP_SAMPLES], Some("fr"), None, false, 1)
            .map(|_| ())
    }
}

/// `[start, end)` range of samples to keep: from the first segment minus `margin_cs` to the last
/// plus `margin_cs`, bounded to the audio. Internal pauses stay intact. `None` = no speech.
pub fn vad_trim_range(
    len: usize,
    segments_cs: &[(f32, f32)],
    margin_cs: f32,
) -> Option<(usize, usize)> {
    let first = segments_cs.first()?;
    let last = segments_cs.last()?;
    let to_samples = |cs: f32| ((cs.max(0.0) / 100.0) * 16_000.0) as usize;
    let start = to_samples(first.0 - margin_cs).min(len);
    let end = to_samples(last.1 + margin_cs).min(len);
    (end > start).then_some((start, end))
}

/// Rebuilds the speech audio from the VAD segments `(start, end)` in centiseconds:
/// same rules as whisper.cpp (0.1 s overlap on all the segments except the last,
/// 0.1 s of silence between two segments). `None` if there is no speech.
pub fn vad_filter_audio(audio: &[f32], segments_cs: &[(f32, f32)]) -> Option<Vec<f32>> {
    const OVERLAP: usize = 1_600;
    const GAP: usize = 1_600;
    if segments_cs.is_empty() || audio.is_empty() {
        return None;
    }
    let last_index = audio.len() - 1;
    let to_samples = |cs: f32| ((cs.max(0.0) / 100.0) * 16_000.0) as usize;
    let mut out = Vec::new();
    for (i, (start, end)) in segments_cs.iter().enumerate() {
        let mut end_s = to_samples(*end);
        if i + 1 < segments_cs.len() {
            end_s += OVERLAP;
        }
        let start_s = to_samples(*start).min(last_index);
        let end_s = end_s.min(last_index);
        if end_s <= start_s {
            continue;
        }
        if !out.is_empty() {
            out.resize(out.len() + GAP, 0.0);
        }
        out.extend_from_slice(&audio[start_s..end_s]);
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_range_keeps_internal_pauses_and_adds_margins() {
        // Speech from 2 s to 3 s and from 5 s to 6 s in 10 s of audio, margin 0.2 s.
        let r = vad_trim_range(160_000, &[(200.0, 300.0), (500.0, 600.0)], 20.0);
        assert_eq!(r, Some((28_800, 99_200)));
        // Audio bounds respected.
        assert_eq!(
            vad_trim_range(16_000, &[(5.0, 95.0)], 20.0),
            Some((0, 16_000))
        );
        assert_eq!(vad_trim_range(16_000, &[], 20.0), None);
    }

    #[test]
    fn no_segment_means_no_speech() {
        assert_eq!(vad_filter_audio(&[0.1; 16_000], &[]), None);
        assert_eq!(vad_filter_audio(&[], &[(0.0, 50.0)]), None);
    }

    #[test]
    fn single_segment_is_cut_without_overlap() {
        let audio: Vec<f32> = (0..32_000).map(|i| i as f32).collect();
        // 0.5 s to 1.0 s = samples 8000..16000.
        let out = vad_filter_audio(&audio, &[(50.0, 100.0)]).unwrap_or_default();
        assert_eq!(out.len(), 8_000);
        assert_eq!(out[0], 8_000.0);
    }

    #[test]
    fn two_segments_get_overlap_and_gap() {
        let audio = vec![1.0; 64_000];
        // First: 0..1 s (+0.1 s of overlap), second: 2..3 s.
        let out = vad_filter_audio(&audio, &[(0.0, 100.0), (200.0, 300.0)]).unwrap_or_default();
        assert_eq!(out.len(), 17_600 + 1_600 + 16_000);
        // The interleaved silence is indeed zero.
        assert_eq!(out[17_600], 0.0);
    }

    #[test]
    fn segments_beyond_the_end_are_clamped() {
        let audio = vec![1.0; 16_000];
        let out = vad_filter_audio(&audio, &[(50.0, 900.0)]).unwrap_or_default();
        assert_eq!(out.len(), 15_999 - 8_000);
    }
}
