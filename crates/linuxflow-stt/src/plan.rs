//! Loading plan (fallback chain), initial prompt formatting and text assembly.
//! The whole module is pure: no native call, fully testable.

use std::path::{Path, PathBuf};

use linuxflow_core::constants::HINTS_MAX;
use serde::{Deserialize, Serialize};

use crate::hardware::DeviceChoice;

/// Device requested for a loading step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Device {
    Cpu,
    /// Vulkan GPU, ggml number (see `GpuInfo::ggml_index`).
    Gpu(usize),
}

/// A step of the fallback chain: the first one that succeeds wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadPlan {
    pub model_path: PathBuf,
    pub device: Device,
    /// Readable name of the GPU, for display only.
    #[serde(default)]
    pub device_name: Option<String>,
}

impl LoadPlan {
    pub fn cpu(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            device: Device::Cpu,
            device_name: None,
        }
    }

    pub fn gpu(model_path: impl Into<PathBuf>, index: usize) -> Self {
        Self {
            model_path: model_path.into(),
            device: Device::Gpu(index),
            device_name: None,
        }
    }

    pub fn with_device_name(mut self, name: impl Into<String>) -> Self {
        self.device_name = Some(name.into());
        self
    }
}

/// Builds the fallback chain:
/// each GPU model present on disk on the chosen GPU (from the heaviest to the lightest, order
/// given by the caller), then the CPU model. Without a chosen GPU: the CPU model alone, and never a
/// GPU model on the CPU (the large model is too slow). Absent files are discarded.
pub fn build_fallback_chain(
    choice: &DeviceChoice,
    gpu_models: &[PathBuf],
    cpu_model: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<LoadPlan> {
    let mut chain = Vec::new();
    if let DeviceChoice::Gpu {
        ggml_index, name, ..
    } = choice
    {
        for model in gpu_models.iter().filter(|m| exists(m)) {
            let mut plan = LoadPlan::gpu(model, *ggml_index);
            plan.device_name = Some(name.clone());
            chain.push(plan);
        }
    }
    if let Some(model) = cpu_model.filter(|m| exists(m)) {
        // Avoids an exact duplicate (same file already planned on the CPU).
        let plan = LoadPlan::cpu(model);
        if !chain.contains(&plan) {
            chain.push(plan);
        }
    }
    chain
}

/// Cleans the hints: spaces removed, empty ones and null bytes discarded, at most `HINTS_MAX`.
pub fn clean_hints(hints: &[String]) -> Vec<String> {
    hints
        .iter()
        .map(|h| h.replace('\0', "").trim().to_string())
        .filter(|h| !h.is_empty())
        .take(HINTS_MAX)
        .collect()
}

/// Initial prompt: cleaned hints joined with ", "; `None` if there are none.
pub fn build_initial_prompt(hints: &[String]) -> Option<String> {
    let cleaned = clean_hints(hints);
    (!cleaned.is_empty()).then(|| cleaned.join(", "))
}

/// Non-empty segments (after trim) joined with a space.
pub fn join_segments<S: AsRef<str>>(segments: &[S]) -> String {
    segments
        .iter()
        .map(|s| s.as_ref().trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Requested language: `None`, empty or "auto" = automatic detection.
pub fn normalize_language(language: Option<&str>) -> Option<String> {
    let raw = language?.trim().to_lowercase();
    // `fr-FR`, `fr_CA`, `pt-BR` → main subtag; `auto` or empty → automatic detection.
    let primary = raw.split(['-', '_']).next().unwrap_or("");
    if primary.is_empty() || primary == "auto" {
        return None;
    }
    // A language code unknown to Whisper would make every dictation fail: fall back on detection.
    WHISPER_LANGUAGES
        .contains(&primary)
        .then(|| primary.to_string())
}

/// Language codes accepted by Whisper (whisper.cpp `g_lang`).
pub const WHISPER_LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su", "yue",
];

/// Short name of the model and precision from the GGML file name
/// (`ggml-large-v3-q5_0.bin` → `large-v3`, `q5_0`; without a quantisation suffix: `f16`).
pub fn describe_model(path: &Path) -> (String, Option<String>) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = stem.strip_prefix("ggml-").unwrap_or(&stem).to_string();
    if let Some(pos) = stem.rfind("-q") {
        let suffix = &stem[pos + 1..];
        let quant = suffix.len() >= 2
            && suffix.as_bytes()[0] == b'q'
            && suffix.as_bytes()[1].is_ascii_digit();
        if quant {
            return (stem[..pos].to_string(), Some(suffix.to_string()));
        }
    }
    (stem, Some("f16".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    fn gpu() -> DeviceChoice {
        DeviceChoice::Gpu {
            ggml_index: 1,
            name: "X".into(),
            vram_bytes: None,
        }
    }

    fn all_exist(_: &Path) -> bool {
        true
    }

    #[test]
    fn language_is_normalized_and_validated() {
        assert_eq!(normalize_language(Some("fr-FR")).as_deref(), Some("fr"));
        assert_eq!(normalize_language(Some(" PT_br ")).as_deref(), Some("pt"));
        assert_eq!(normalize_language(Some("en")).as_deref(), Some("en"));
        assert_eq!(normalize_language(Some("yue")).as_deref(), Some("yue"));
        assert_eq!(normalize_language(Some("auto")), None);
        assert_eq!(normalize_language(Some("")), None);
        assert_eq!(normalize_language(None), None);
        // unknown code: automatic detection rather than a failure of every dictation
        assert_eq!(normalize_language(Some("xx-YY")), None);
        assert_eq!(normalize_language(Some("klingon")), None);
    }

    #[test]
    fn prompt_joins_with_comma_and_skips_empty() {
        assert_eq!(
            build_initial_prompt(&s(&["  Alice ", "", "  ", "Bob, Jr."])),
            Some("Alice, Bob, Jr.".to_string())
        );
        assert_eq!(build_initial_prompt(&[]), None);
        assert_eq!(build_initial_prompt(&s(&[" ", ""])), None);
    }

    #[test]
    fn hints_are_limited_after_cleaning() {
        let many: Vec<String> = std::iter::once(String::new())
            .chain((0..200).map(|i| format!("mot{i}")))
            .collect();
        let cleaned = clean_hints(&many);
        assert_eq!(cleaned.len(), HINTS_MAX);
        assert_eq!(cleaned[0], "mot0");
    }

    #[test]
    fn nul_bytes_never_reach_the_native_layer() {
        assert_eq!(build_initial_prompt(&s(&["a\0b"])), Some("ab".to_string()));
    }

    #[test]
    fn segments_are_trimmed_and_joined() {
        assert_eq!(
            join_segments(&[" Hello ", "", "  ", "world."]),
            "Hello world."
        );
        assert_eq!(join_segments::<&str>(&[]), "");
    }

    #[test]
    fn language_auto_variants() {
        assert_eq!(normalize_language(None), None);
        assert_eq!(normalize_language(Some("")), None);
        assert_eq!(normalize_language(Some(" Auto ")), None);
        assert_eq!(normalize_language(Some("FR")), Some("fr".to_string()));
    }

    #[test]
    fn chain_gpu_models_then_cpu() {
        let chain = build_fallback_chain(
            &gpu(),
            &[
                PathBuf::from("large-f16.bin"),
                PathBuf::from("large-q5_0.bin"),
            ],
            Some(Path::new("small.bin")),
            &all_exist,
        );
        assert_eq!(
            chain,
            vec![
                LoadPlan::gpu("large-f16.bin", 1).with_device_name("X"),
                LoadPlan::gpu("large-q5_0.bin", 1).with_device_name("X"),
                LoadPlan::cpu("small.bin"),
            ]
        );
    }

    #[test]
    fn chain_skips_missing_files() {
        let exists = |p: &Path| p != Path::new("large-f16.bin");
        let chain = build_fallback_chain(
            &gpu(),
            &[
                PathBuf::from("large-f16.bin"),
                PathBuf::from("large-q5_0.bin"),
            ],
            Some(Path::new("small.bin")),
            &exists,
        );
        assert_eq!(chain.len(), 2);
        assert_eq!(
            chain[0],
            LoadPlan::gpu("large-q5_0.bin", 1).with_device_name("X")
        );
    }

    #[test]
    fn chain_without_gpu_never_uses_the_big_model() {
        let chain = build_fallback_chain(
            &DeviceChoice::Cpu,
            &[PathBuf::from("large-q5_0.bin")],
            Some(Path::new("small.bin")),
            &all_exist,
        );
        assert_eq!(chain, vec![LoadPlan::cpu("small.bin")]);
    }

    #[test]
    fn chain_can_be_empty() {
        assert!(build_fallback_chain(&DeviceChoice::Cpu, &[], None, &all_exist).is_empty());
    }

    #[test]
    fn model_description() {
        let d = |p: &str| describe_model(Path::new(p));
        assert_eq!(
            d("/m/ggml-large-v3-q5_0.bin"),
            ("large-v3".into(), Some("q5_0".into()))
        );
        assert_eq!(d("ggml-small.bin"), ("small".into(), Some("f16".into())));
        assert_eq!(
            d("ggml-small.en-q8_0.bin"),
            ("small.en".into(), Some("q8_0".into()))
        );
        assert_eq!(d("perso.bin"), ("perso".into(), Some("f16".into())));
    }
}
