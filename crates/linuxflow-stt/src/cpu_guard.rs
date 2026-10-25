//! CPU guard: cleanly refuses to load whisper.cpp on a processor that lacks the
//! instructions used at build time (a SIGILL cannot be caught).
//!
//! Design: `whisper-rs-sys` 0.15 builds ggml
//! statically with a frozen instruction level (no run-time dispatch). `build.rs`
//! reads the `GGML_*` variables that will be passed to CMake and computes the exact list of
//! required features; this guard compares it with the real processor *before any native call*.
//! Recommended configuration for a distributable binary: `GGML_NATIVE=OFF` (level v3, see
//! `.cargo/config.toml`) or, for old processors, the documented v2 variant.

use std::fmt;
use std::sync::OnceLock;

include!("cpu_detect.inc.rs");

/// Instruction level of the native build: `native`, `v3`, `v2` or `none` (outside x86-64).
pub const BUILD_LEVEL: &str = env!("LINUXFLOW_GGML_LEVEL");
const BUILD_REQUIRED: &str = env!("LINUXFLOW_GGML_REQUIRED");

/// Features required by the current build of whisper.cpp.
pub fn build_required_features() -> Vec<String> {
    parse_feature_list(BUILD_REQUIRED)
}

/// Splits an `a,b,c` list (spaces tolerated, empty elements ignored).
pub fn parse_feature_list(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Features of the current processor (among those ggml knows how to use).
pub fn host_features() -> Vec<String> {
    TRACKED_FEATURES
        .iter()
        .filter(|f| host_has(f))
        .map(|f| (*f).to_string())
        .collect()
}

/// Features of the x86-64 levels that can be built (see `.cargo/config.toml`).
pub const LEVEL_V3_FEATURES: [&str; 6] = ["sse4.2", "avx", "avx2", "bmi2", "fma", "f16c"];
pub const LEVEL_V2_FEATURES: [&str; 1] = ["sse4.2"];

/// Best build level that the processor (`has`) can run: `v3`, `v2` or `None`.
/// Used to choose between two engine binaries (fast AVX2 / slow but universal).
pub fn best_supported_level(has: &dyn Fn(&str) -> bool) -> Option<&'static str> {
    if LEVEL_V3_FEATURES.iter().all(|f| has(f)) {
        Some("v3")
    } else if LEVEL_V2_FEATURES.iter().all(|f| has(f)) {
        Some("v2")
    } else {
        None
    }
}

/// Guard error: the processor cannot run this build of the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuGuardError {
    pub level: String,
    pub missing: Vec<String>,
}

impl fmt::Display for CpuGuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "this processor does not support the instructions required by the engine (level {}): {}",
            self.level,
            self.missing.join(", ")
        )
    }
}

impl std::error::Error for CpuGuardError {}

/// Features of `required` missing according to `has` (pure function, testable).
pub fn missing_features(required: &[String], has: &dyn Fn(&str) -> bool) -> Vec<String> {
    required.iter().filter(|f| !has(f)).cloned().collect()
}

/// Checks `required` against `has`; `Err` lists what is missing.
pub fn check_requirements(
    level: &str,
    required: &[String],
    has: &dyn Fn(&str) -> bool,
) -> Result<(), CpuGuardError> {
    let missing = missing_features(required, has);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CpuGuardError {
            level: level.to_string(),
            missing,
        })
    }
}

/// Checks the real processor against the current build. Result cached.
pub fn ensure_cpu_supported() -> Result<(), CpuGuardError> {
    static RESULT: OnceLock<Result<(), CpuGuardError>> = OnceLock::new();
    RESULT
        .get_or_init(|| check_requirements(BUILD_LEVEL, &build_required_features(), &host_has))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_ignores_empty_items_and_spaces() {
        assert_eq!(parse_feature_list(" avx2, ,fma,"), set(&["avx2", "fma"]));
        assert!(parse_feature_list("").is_empty());
    }

    #[test]
    fn cpu_without_avx2_is_refused_for_v3() {
        let required = set(&["sse4.2", "avx", "avx2", "fma"]);
        let cpu = |f: &str| matches!(f, "sse4.2" | "avx");
        let err = check_requirements("v3", &required, &cpu).unwrap_err();
        assert_eq!(err.missing, set(&["avx2", "fma"]));
        assert!(err.to_string().contains("avx2, fma"));
        assert!(err.to_string().contains("v3"));
    }

    #[test]
    fn cpu_with_everything_is_accepted() {
        let required = set(&["sse4.2", "avx2"]);
        assert!(check_requirements("v3", &required, &|_| true).is_ok());
    }

    #[test]
    fn empty_requirement_always_passes() {
        assert!(check_requirements("none", &[], &|_| false).is_ok());
    }

    #[test]
    fn best_level_selection() {
        assert_eq!(best_supported_level(&|_| true), Some("v3"));
        // Processor without AVX2 (case seen under qemu): fall back to level v2.
        let no_avx2 = |f: &str| matches!(f, "sse4.2" | "avx");
        assert_eq!(best_supported_level(&no_avx2), Some("v2"));
        assert_eq!(best_supported_level(&|_| false), None);
    }

    #[test]
    fn unknown_feature_counts_as_missing() {
        assert!(!host_has("not-a-feature"));
    }

    #[test]
    fn real_cpu_satisfies_its_own_build() {
        // The test build is compiled for this machine or for a level it handles.
        assert_eq!(ensure_cpu_supported(), Ok(()));
        assert!(["native", "v3", "v2", "none"].contains(&BUILD_LEVEL));
    }
}
