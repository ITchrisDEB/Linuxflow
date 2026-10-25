// Fragment included by both `build.rs` and `src/cpu_guard.rs`: a single table of
// correspondence between feature names and `is_x86_feature_detected!`.

/// True if the current processor (build host or run-time host) has the feature.
/// An unknown name is considered absent. Always false outside x86-64.
#[allow(unused_variables)]
fn host_has(feature: &str) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        match feature {
            "sse4.2" => std::is_x86_feature_detected!("sse4.2"),
            "avx" => std::is_x86_feature_detected!("avx"),
            "avx2" => std::is_x86_feature_detected!("avx2"),
            "fma" => std::is_x86_feature_detected!("fma"),
            "f16c" => std::is_x86_feature_detected!("f16c"),
            "bmi2" => std::is_x86_feature_detected!("bmi2"),
            "avx512f" => std::is_x86_feature_detected!("avx512f"),
            "avx512vbmi" => std::is_x86_feature_detected!("avx512vbmi"),
            "avx512vnni" => std::is_x86_feature_detected!("avx512vnni"),
            "avx512bf16" => std::is_x86_feature_detected!("avx512bf16"),
            "avxvnni" => std::is_x86_feature_detected!("avxvnni"),
            _ => false,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// x86 features that ggml can exploit (stable order).
const TRACKED_FEATURES: [&str; 11] = [
    "sse4.2",
    "avx",
    "avx2",
    "fma",
    "f16c",
    "bmi2",
    "avx512f",
    "avx512vbmi",
    "avx512vnni",
    "avx512bf16",
    "avxvnni",
];
