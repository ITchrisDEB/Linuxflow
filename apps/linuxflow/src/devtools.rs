//! Development tools: `--test-pipeline` (no GTK) and `--screenshots` (Broadway).

use crate::adapters::{
    AppFormatter, MemoryClipboard, NoHistory, NoopWindow, StoreHints, UnavailablePaster,
};
use crate::engine::{EngineHost, WorkerLauncher};
use crate::engine_ctl::EngineController;
use crate::library::{DictionaryStore, SnippetStore};
use crate::models::ModelsService;
use crate::runtime::SystemClock;
use crate::wiring::{exe_dir, warn_logger};
use linuxflow_core::domain::{EngineInfo, Recording, Settings};
use linuxflow_core::paths::AppPaths;
use linuxflow_core::pipeline::{OutcomeKind, Pipeline, PipelineDeps, PipelineInput};
use linuxflow_core::settings::SettingsStore;
use linuxflow_core::traits::{Clipboard, SpeechToText};

use linuxflow_models::{Catalog, Config};
use linuxflow_platform::per_app::PerAppTable;
use linuxflow_stt::Hardware;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Maximum wait for the engine in test mode (240 s).
pub const ENGINE_WAIT: Duration = Duration::from_secs(240);

/// Reads a 16-bit PCM or float WAV and converts it to mono 16 kHz.
pub fn load_wav(path: &Path) -> Result<Recording, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| f32::from(v) / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        (f, b) => return Err(format!("unsupported WAV format ({f:?}, {b} bits)")),
    };
    let mono: Vec<f32> = samples
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect();
    let samples = if spec.sample_rate == linuxflow_core::constants::SAMPLE_RATE_HZ {
        mono
    } else {
        linuxflow_audio::resample::resample_all(
            &mono,
            spec.sample_rate,
            linuxflow_core::constants::SAMPLE_RATE_HZ,
        )
        .map_err(|e| e.to_string())?
    };
    Ok(Recording { samples })
}

/// `TEST_SIDECAR` line: engine state as JSON.
pub fn sidecar_json(info: &EngineInfo) -> String {
    json!({
        "ready": info.ready,
        "phase": format!("{:?}", info.phase).to_lowercase(),
        "device": format!("{:?}", info.device).to_lowercase(),
        "deviceName": info.device_name,
        "model": info.model,
        "precision": info.precision,
        "loadMs": info.load_ms,
        "error": info.error,
    })
    .to_string()
}

/// `--test-pipeline`: transcribes the WAV with the real engine, without pasting or touching the real
/// clipboard (in-memory clipboard, history not written). Returns the exit code.
pub fn test_pipeline(wav: &Path, paths: &AppPaths) -> i32 {
    match run_test_pipeline(wav, paths) {
        Ok(()) => 0,
        Err(e) => {
            println!("DEV_ERROR {e}");
            1
        }
    }
}

fn run_test_pipeline(wav: &Path, paths: &AppPaths) -> Result<(), String> {
    let audio = load_wav(wav)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let warn = warn_logger();
    let settings = Arc::new(SettingsStore::open_silent(paths.settings_file()));
    let current: Settings = settings.get();
    let host = Arc::new(EngineHost::new());
    let models_dir = std::env::var_os("LINUXFLOW_TEST_MODEL_DIR")
        .filter(|_| std::env::var_os(linuxflow_core::paths::ENV_MODEL_DIR).is_none())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths.models_dir.clone());
    let models = ModelsService::new(
        runtime.handle().clone(),
        Catalog::load_from_env(),
        Config::from_env(),
        models_dir,
        host.clone(),
    );
    let s2 = settings.clone();
    let engine = EngineController::new(
        host.clone(),
        models,
        Arc::new(WorkerLauncher),
        Arc::new(move || s2.get()),
        Arc::new(|| Hardware::detect_isolated(Duration::from_secs(10))),
        false,
        exe_dir(),
    );
    engine.reload();
    engine.wait_settled(ENGINE_WAIT)?;
    println!("TEST_SIDECAR {}", sidecar_json(&host.info()));

    let dictionary = Arc::new(DictionaryStore::open(paths.dictionary_file(), warn.clone()));
    let snippets = Arc::new(SnippetStore::open(paths.snippets_file(), warn));
    let clipboard = Arc::new(MemoryClipboard::default());
    let pipeline = Pipeline::new(PipelineDeps {
        stt: host.clone(),
        clipboard: clipboard.clone(),
        paster: Arc::new(UnavailablePaster("test mode".into())),
        window: Arc::new(NoopWindow),
        clock: Arc::new(SystemClock),
        formatter: Arc::new(AppFormatter {
            dictionary: dictionary.clone(),
            snippets,
        }),
        history: Arc::new(NoHistory),
        paste_modes: Arc::new(PerAppTable::embedded()),
        hints: Arc::new(StoreHints(dictionary)),
    });
    let mut input = PipelineInput::from_settings(audio, &current, false, true, None);
    input.record_history = false;
    let outcome = pipeline.run(input).map_err(|e| e.to_string());
    let outcome = outcome?;
    let result = json!({
        "input": wav.file_name().map(|n| n.to_string_lossy().into_owned()),
        "kind": match outcome.kind {
            OutcomeKind::Empty => "empty",
            OutcomeKind::Done => "done",
            OutcomeKind::PasteBlocked => "blocked",
        },
        "text": outcome.text,
        "raw": outcome.raw,
        "clipboard": clipboard.get_text().ok().flatten(),
        "audioMs": outcome.audio_ms,
        "sttMs": outcome.stt_ms,
        "latencyMs": outcome.latency_ms,
    });
    println!("TEST_PIPELINE_RESULT {result}");
    host.shutdown();
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, rate: u32, channels: u16, frames: usize) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..frames {
            for _ in 0..channels {
                w.write_sample(((i % 100) as i16 - 50) * 200).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    #[test]
    fn wav_16k_mono_is_read_as_is() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        write_wav(&p, 16_000, 1, 16_000);
        let r = load_wav(&p).unwrap();
        assert_eq!(r.samples.len(), 16_000);
        assert_eq!(r.duration_ms(), 1000);
    }

    #[test]
    fn stereo_48k_is_downmixed_and_resampled() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("b.wav");
        write_wav(&p, 48_000, 2, 48_000);
        let r = load_wav(&p).unwrap();
        let ms = r.duration_ms();
        assert!((990..=1010).contains(&ms), "duration {ms} ms");
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(load_wav(Path::new("/definitely/missing.wav")).is_err());
    }

    #[test]
    fn sidecar_line_is_json() {
        let info = EngineInfo {
            ready: true,
            model: Some("small".into()),
            ..EngineInfo::default()
        };
        let v: serde_json::Value = serde_json::from_str(&sidecar_json(&info)).unwrap();
        assert_eq!(v["ready"], true);
        assert_eq!(v["model"], "small");
    }
}
