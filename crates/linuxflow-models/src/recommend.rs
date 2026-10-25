//! Model recommendation according to the hardware. Pure functions.
//!
//! Rule: never a large model without a usable GPU.

use crate::catalog::{Catalog, ModelEntry, Usage};

/// Summary of the hardware useful for the recommendation (filled in by the caller from the detection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HardwareSummary {
    /// A real Vulkan GPU is usable (software drivers such as llvmpipe do not count).
    pub gpu_vulkan: bool,
    /// VRAM of the best GPU, in bytes (`None` if unknown).
    pub vram_bytes: Option<u64>,
    /// Total RAM, in bytes.
    pub ram_bytes: u64,
}

/// Reason for the choice, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// GPU with enough VRAM for the chosen model.
    GpuFits,
    /// No usable GPU: light model on the CPU.
    NoUsableGpu,
    /// GPU present but VRAM insufficient or unknown for a large model.
    GpuTooSmall,
}

/// Result of the recommendation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    /// Identifier of the recommended model.
    pub model: String,
    /// "Lighter" alternative (lower rank), if it exists.
    pub lighter: Option<String>,
    /// "More accurate" alternative (higher rank) **usable on this machine**, if it exists.
    pub more_accurate: Option<String>,
    pub reason: Reason,
    /// The RAM is below the estimate of the recommended model: the interface must warn.
    pub low_memory: bool,
}

/// Can a model run on this hardware?
fn usable(e: &ModelEntry, hw: &HardwareSummary) -> bool {
    match e.usage {
        Usage::Gpu => hw.gpu_vulkan && hw.vram_bytes.is_some_and(|v| v >= e.vram_bytes),
        Usage::Cpu | Usage::Light => true,
    }
}

/// Recommends a transcription model from the catalogue. `None` if the catalogue contains none.
pub fn recommend_in(catalog: &Catalog, hw: &HardwareSummary) -> Option<Recommendation> {
    let ranked = catalog.stt_by_rank();
    // The most accurate of the usable models; failing that, the lightest of the catalogue.
    let (idx, chosen) = ranked
        .iter()
        .enumerate()
        .rev()
        .find(|(_, e)| usable(e, hw))
        .or_else(|| ranked.iter().enumerate().next())?;
    let reason = if chosen.usage == Usage::Gpu {
        Reason::GpuFits
    } else if hw.gpu_vulkan {
        Reason::GpuTooSmall
    } else {
        Reason::NoUsableGpu
    };
    let lighter = ranked[..idx].last().map(|e| e.id.clone());
    let more_accurate = ranked[idx + 1..]
        .iter()
        .find(|e| usable(e, hw))
        .map(|e| e.id.clone());
    Some(Recommendation {
        model: chosen.id.clone(),
        lighter,
        more_accurate,
        reason,
        low_memory: hw.ram_bytes != 0 && hw.ram_bytes < chosen.ram_bytes,
    })
}

/// `recommend_in` with the embedded catalogue.
pub fn recommend(hw: &HardwareSummary) -> Recommendation {
    let fallback = Recommendation {
        model: String::new(),
        lighter: None,
        more_accurate: None,
        reason: Reason::NoUsableGpu,
        low_memory: false,
    };
    recommend_in(&Catalog::embedded(), hw).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn hw(gpu: bool, vram_gib: Option<u64>, ram_gib: u64) -> HardwareSummary {
        HardwareSummary {
            gpu_vulkan: gpu,
            vram_bytes: vram_gib.map(|g| g * GIB),
            ram_bytes: ram_gib * GIB,
        }
    }

    #[test]
    fn big_gpu_gets_f16() {
        let r = recommend(&hw(true, Some(12), 32));
        assert_eq!(r.model, "large-v3");
        assert_eq!(r.reason, Reason::GpuFits);
        assert_eq!(r.lighter.as_deref(), Some("large-v3-q5_0"));
        assert_eq!(r.more_accurate, None);
        assert!(!r.low_memory);
    }

    #[test]
    fn medium_gpu_gets_q5() {
        let r = recommend(&hw(true, Some(4), 16));
        assert_eq!(r.model, "large-v3-q5_0");
        assert_eq!(r.lighter.as_deref(), Some("small"));
        assert_eq!(r.more_accurate, None, "f16 does not fit in 4 GiB");
    }

    #[test]
    fn small_gpu_gets_small() {
        let r = recommend(&hw(true, Some(2), 16));
        assert_eq!(r.model, "small");
        assert_eq!(r.reason, Reason::GpuTooSmall);
        assert_eq!(r.lighter, None);
        assert_eq!(r.more_accurate, None);
    }

    #[test]
    fn unknown_vram_never_gets_big_model() {
        let r = recommend(&hw(true, None, 16));
        assert_eq!(r.model, "small");
    }

    #[test]
    fn no_gpu_never_gets_big_model_whatever_the_ram() {
        for ram in [2, 8, 64, 512] {
            let r = recommend(&hw(false, None, ram));
            assert_eq!(r.model, "small");
            assert_eq!(r.reason, Reason::NoUsableGpu);
            assert_eq!(r.more_accurate, None, "no heavy alternative without a GPU");
        }
    }

    #[test]
    fn software_vulkan_only_counts_as_no_gpu() {
        // The caller sets gpu_vulkan = false for llvmpipe; even with an advertised "VRAM".
        let r = recommend(&hw(false, Some(64), 64));
        assert_eq!(r.model, "small");
    }

    #[test]
    fn low_ram_is_flagged() {
        let r = recommend(&HardwareSummary {
            gpu_vulkan: false,
            vram_bytes: None,
            ram_bytes: GIB / 2,
        });
        assert_eq!(r.model, "small");
        assert!(r.low_memory);
        let r = recommend(&hw(false, None, 0));
        assert!(!r.low_memory, "unknown RAM: no warning");
    }

    #[test]
    fn exact_vram_boundaries() {
        let c = Catalog::embedded();
        let q5 = c.get("large-v3-q5_0").unwrap().vram_bytes;
        let f16 = c.get("large-v3").unwrap().vram_bytes;
        let mk = |v| HardwareSummary {
            gpu_vulkan: true,
            vram_bytes: Some(v),
            ram_bytes: 16 * GIB,
        };
        assert_eq!(recommend(&mk(q5 - 1)).model, "small");
        assert_eq!(recommend(&mk(q5)).model, "large-v3-q5_0");
        assert_eq!(recommend(&mk(f16 - 1)).model, "large-v3-q5_0");
        assert_eq!(recommend(&mk(f16)).model, "large-v3");
    }

    #[test]
    fn vad_is_never_recommended() {
        for h in [hw(true, Some(24), 64), hw(false, None, 4)] {
            assert_ne!(recommend(&h).model, "silero-vad");
        }
    }

    #[test]
    fn empty_catalog_gives_none() {
        let c = Catalog::from_json(r#"{"models":[]}"#).unwrap();
        assert!(recommend_in(&c, &hw(true, Some(8), 8)).is_none());
    }
}
