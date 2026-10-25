#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Tests with real models. Guarded by `LINUXFLOW_TEST_MODEL_DIR` (ggml-small.bin,
//! ggml-large-v3-q5_0.bin, ggml-silero-v6.2.0.bin) and `LINUXFLOW_TEST_SAMPLES_DIR` (dix.wav,
//! phrase.wav): without these variables, the tests skip themselves. `LINUXFLOW_TEST_LARGE=1` adds the
//! large model (slow on CPU). Run with `--nocapture --release` to see the measurements.

use std::path::{Path, PathBuf};
use std::time::Instant;

use linuxflow_core::domain::{EnginePhase, Recording, TranscribeRequest};
use linuxflow_core::traits::SpeechToText;
use linuxflow_stt::{EngineConfig, LoadPlan, WhisperEngine};

fn dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

fn init_logs() {
    // whisper.cpp logs visible with LINUXFLOW_TEST_LOG=1 (on the test's stderr only).
    if std::env::var_os("LINUXFLOW_TEST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
    }
}

fn model_dir() -> Option<PathBuf> {
    init_logs();
    let d = dir("LINUXFLOW_TEST_MODEL_DIR");
    if d.is_none() {
        eprintln!("SKIP : LINUXFLOW_TEST_MODEL_DIR absent");
    }
    d
}

/// Reads a mono/stereo WAV as 16 kHz f32 (simple linear resampling, enough here).
fn load_wav(path: &Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("wav");
    let spec = reader.spec();
    let raw: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| f32::from(s.expect("sample")) / 32768.0)
        .collect();
    let mono: Vec<f32> = raw
        .chunks(spec.channels as usize)
        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
        .collect();
    if spec.sample_rate == 16_000 {
        return mono;
    }
    let ratio = f64::from(spec.sample_rate) / 16_000.0;
    let n = (mono.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let i0 = pos as usize;
            let frac = (pos - i0 as f64) as f32;
            let a = mono[i0];
            let b = mono.get(i0 + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

fn sample(name: &str) -> Option<Vec<f32>> {
    let d = dir("LINUXFLOW_TEST_SAMPLES_DIR")?;
    Some(load_wav(&d.join(name)))
}

fn vm_hwm_mib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .unwrap_or(0)
        / 1024
}

fn request(samples: Vec<f32>, lang: Option<&str>, hints: &[&str]) -> TranscribeRequest {
    TranscribeRequest {
        audio: Recording { samples },
        language: lang.map(str::to_string),
        hints: hints.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn config(models: &Path) -> EngineConfig {
    EngineConfig {
        vad_model_path: Some(models.join("ggml-silero-v6.2.0.bin")),
        ..EngineConfig::new()
    }
}

#[test]
fn small_cpu_transcribes_ten_seconds() {
    let (Some(models), Some(audio)) = (model_dir(), sample("dix.wav")) else {
        return;
    };
    let engine = WhisperEngine::start(
        config(&models),
        vec![LoadPlan::cpu(models.join("ggml-small.bin"))],
    );
    let info = engine.wait_loaded();
    assert_eq!(info.phase, EnginePhase::Ready, "{info:?}");
    println!(
        "MEASURE small CPU: loading+warm-up {} ms",
        info.load_ms.unwrap_or(0)
    );
    let t = Instant::now();
    let out = engine
        .transcribe(&request(audio, Some("fr"), &[]))
        .expect("transcription");
    println!(
        "MESURE small CPU : stt_ms={} (mur {} ms) audio_ms={} pic_RSS={} Mio texte={:?}",
        out.stt_ms,
        t.elapsed().as_millis(),
        out.audio_ms,
        vm_hwm_mib(),
        out.text
    );
    assert!(!out.text.is_empty());
    assert!(out.audio_ms >= 9_000);
}

#[test]
fn language_auto_is_detected() {
    let (Some(models), Some(audio)) = (model_dir(), sample("phrase.wav")) else {
        return;
    };
    let engine = WhisperEngine::start(
        config(&models),
        vec![LoadPlan::cpu(models.join("ggml-small.bin"))],
    );
    engine.wait_loaded();
    let out = engine
        .transcribe(&request(audio, None, &["LinuxFlow"]))
        .expect("transcription");
    println!("auto : langue={:?} texte={:?}", out.language, out.text);
    assert!(out.language.is_some());
    assert!(!out.text.is_empty());
}

#[test]
fn silence_gives_empty_text_with_vad() {
    let Some(models) = model_dir() else { return };
    let engine = WhisperEngine::start(
        config(&models),
        vec![LoadPlan::cpu(models.join("ggml-small.bin"))],
    );
    engine.wait_loaded();
    let out = engine
        .transcribe(&request(vec![0.0; 48_000], Some("fr"), &[]))
        .expect("transcription");
    println!("silence : {:?}", out.text);
    assert!(out.text.trim().is_empty());
}

#[test]
fn gpu_plan_without_vulkan_build_falls_back_to_cpu() {
    let Some(models) = model_dir() else { return };
    if cfg!(feature = "vulkan") {
        return;
    }
    let engine = WhisperEngine::start(
        config(&models),
        vec![
            LoadPlan::gpu(models.join("ggml-large-v3-q5_0.bin"), 0),
            LoadPlan::cpu(models.join("ggml-small.bin")),
        ],
    );
    let info = engine.wait_loaded();
    assert_eq!(info.phase, EnginePhase::Ready);
    assert_eq!(info.model.as_deref(), Some("small"));
}

#[test]
fn corrupt_and_truncated_models_are_recoverable_errors() {
    let Some(models) = model_dir() else { return };
    let tmp = std::env::temp_dir().join(format!("linuxflow-stt-corrupt-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let garbage = tmp.join("ggml-garbage.bin");
    std::fs::write(&garbage, vec![0x42u8; 4096]).expect("write");
    let empty = tmp.join("ggml-empty.bin");
    std::fs::write(&empty, b"").expect("write");
    // First 20 MiB of the real model: valid header, missing tensors.
    let truncated = tmp.join("ggml-truncated.bin");
    let head = {
        use std::io::Read;
        let mut buf = vec![0u8; 20 << 20];
        std::fs::File::open(models.join("ggml-small.bin"))
            .and_then(|mut f| f.read_exact(&mut buf))
            .expect("read");
        buf
    };
    std::fs::write(&truncated, head).expect("write");
    let engine = WhisperEngine::start(
        config(&models),
        vec![
            LoadPlan::cpu(&empty),
            LoadPlan::cpu(&garbage),
            LoadPlan::cpu(&truncated),
            LoadPlan::cpu(models.join("absent.bin")),
            LoadPlan::cpu(models.join("ggml-small.bin")),
        ],
    );
    let info = engine.wait_loaded();
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(info.phase, EnginePhase::Ready, "{info:?}");
    assert_eq!(info.model.as_deref(), Some("small"));
}

#[test]
fn large_v3_q5_0_cpu_ten_seconds() {
    let (Some(models), Some(audio)) = (model_dir(), sample("dix.wav")) else {
        return;
    };
    if std::env::var_os("LINUXFLOW_TEST_LARGE").is_none() {
        eprintln!("SKIP : LINUXFLOW_TEST_LARGE absent");
        return;
    }
    let engine = WhisperEngine::start(
        config(&models),
        vec![LoadPlan::cpu(models.join("ggml-large-v3-q5_0.bin"))],
    );
    let info = engine.wait_loaded();
    assert_eq!(info.phase, EnginePhase::Ready, "{info:?}");
    println!(
        "MEASURE large-v3 q5_0 CPU: loading+warm-up {} ms",
        info.load_ms.unwrap_or(0)
    );
    let out = engine
        .transcribe(&request(audio, Some("fr"), &[]))
        .expect("transcription");
    println!(
        "MESURE large-v3 q5_0 CPU : stt_ms={} pic_RSS={} Mio texte={:?}",
        out.stt_ms,
        vm_hwm_mib(),
        out.text
    );
    assert!(!out.text.is_empty());
}

// ---- Comparison of the VAD modes (test bench) -------------------------------------------------

use linuxflow_stt::VadMode;

fn engine_with(models: &Path, mode: VadMode) -> WhisperEngine {
    let mut cfg = config(models);
    cfg.vad_mode = mode;
    let engine = WhisperEngine::start(cfg, vec![LoadPlan::cpu(models.join("ggml-small.bin"))]);
    engine.wait_loaded();
    engine
}

fn text_of(engine: &WhisperEngine, audio: Vec<f32>, lang: Option<&str>) -> String {
    engine
        .transcribe(&request(audio, lang, &[]))
        .expect("transcription")
        .text
}

fn with_silence(audio: &[f32], before_s: usize, after_s: usize) -> Vec<f32> {
    let mut out = vec![0.0; before_s * 16_000];
    out.extend_from_slice(audio);
    out.resize(out.len() + after_s * 16_000, 0.0);
    out
}

/// Deterministic pseudo-random white noise, low amplitude.
fn noise(seconds: usize) -> Vec<f32> {
    let mut x: u32 = 12345;
    (0..seconds * 16_000)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((x >> 8) as f32 / 16_777_216.0 - 0.5) * 0.02
        })
        .collect()
}

#[test]
fn vad_modes_comparison() {
    let (Some(models), Some(dix), Some(phrase)) =
        (model_dir(), sample("dix.wav"), sample("phrase.wav"))
    else {
        return;
    };
    for (label, mode) in [
        ("(a) Off", VadMode::Off),
        ("(b) Segments", VadMode::Segments),
        ("(c) Trim", VadMode::Trim),
    ] {
        let engine = engine_with(&models, mode);
        println!(
            "VAD {label} dix    : {:?}",
            text_of(&engine, dix.clone(), Some("fr"))
        );
        println!(
            "VAD {label} phrase : {:?}",
            text_of(&engine, phrase.clone(), Some("fr"))
        );
        println!(
            "VAD {label} silence 3 s : {:?}",
            text_of(&engine, vec![0.0; 48_000], Some("fr"))
        );
        println!(
            "VAD {label} bruit 3 s   : {:?}",
            text_of(&engine, noise(3), Some("fr"))
        );
        println!(
            "VAD {label} ten + 3 s silence before/after: {:?}",
            text_of(&engine, with_silence(&dix, 3, 3), Some("fr"))
        );
    }
}

/// The default mode keeps all the text of a sentence preceded and followed by silence.
#[test]
fn default_mode_keeps_speech_surrounded_by_silence() {
    let (Some(models), Some(dix)) = (model_dir(), sample("dix.wav")) else {
        return;
    };
    let engine = engine_with(&models, VadMode::default());
    let plain = text_of(&engine, dix.clone(), Some("fr"));
    let padded = text_of(&engine, with_silence(&dix, 3, 3), Some("fr"));
    println!("sans silence : {plain:?}\navec silence : {padded:?}");
    // Identical start and end: the key words of the sentence are kept on both sides.
    for word in ["Bonjour", "essai", "dictée", "Linux", "français", "site"] {
        assert!(
            plain.contains(word),
            "« {word} » absent sans silence : {plain}"
        );
        assert!(
            padded.contains(word),
            "« {word} » absent avec silence : {padded}"
        );
    }
    // Silence alone or noise alone: no text.
    assert!(text_of(&engine, vec![0.0; 48_000], Some("fr"))
        .trim()
        .is_empty());
    assert!(text_of(&engine, noise(3), Some("fr")).trim().is_empty());
}
