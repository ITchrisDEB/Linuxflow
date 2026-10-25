//! Determines which x86 instructions the build of whisper.cpp (ggml) requires, from the
//! `GGML_*` variables passed to CMake by `whisper-rs-sys`, and exposes them to the code through
//! `LINUXFLOW_GGML_LEVEL` and `LINUXFLOW_GGML_REQUIRED`. The run-time guard (`cpu_guard`)
//! compares them with the real processor before any native call (a SIGILL cannot be caught).
//!
//! Rules taken from `ggml/CMakeLists.txt` (whisper.cpp 1.8.3):
//! - `GGML_NATIVE` absent or ON: `-march=native`, hence everything the build host has;
//! - `GGML_NATIVE=OFF`: SSE4.2, AVX, AVX2, BMI2, FMA, F16C enabled by default (fixed level close to
//!   x86-64-v3), the rest (AVX-512, VNNI) disabled by default; each option is set on its own.

use std::env;

include!("src/cpu_detect.inc.rs");

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_uppercase().as_str(),
        "ON" | "1" | "TRUE" | "YES" | "Y"
    )
}

/// Value of a CMake option `GGML_<name>` with its default.
fn option(name: &str, default: bool) -> bool {
    match env::var(format!("GGML_{name}")) {
        Ok(value) if !value.trim().is_empty() => truthy(&value),
        _ => default,
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/cpu_detect.inc.rs");
    for name in [
        "NATIVE",
        "SSE42",
        "AVX",
        "AVX2",
        "BMI2",
        "FMA",
        "F16C",
        "AVX512",
        "AVX512_VBMI",
        "AVX512_VNNI",
        "AVX512_BF16",
        "AVX_VNNI",
    ] {
        println!("cargo:rerun-if-env-changed=GGML_{name}");
    }

    let is_x86_64 = env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64");
    let (level, required): (&str, Vec<&str>) = if !is_x86_64 {
        ("none", Vec::new())
    } else if option("NATIVE", true) {
        // -march=native: the build host sets the level (development build only).
        let required = TRACKED_FEATURES
            .iter()
            .copied()
            .filter(|f| host_has(f))
            .collect();
        ("native", required)
    } else {
        let table = [
            ("sse4.2", option("SSE42", true)),
            ("avx", option("AVX", true)),
            ("avx2", option("AVX2", true)),
            ("bmi2", option("BMI2", true)),
            ("fma", option("FMA", true)),
            ("f16c", option("F16C", true)),
            ("avx512f", option("AVX512", false)),
            ("avx512vbmi", option("AVX512_VBMI", false)),
            ("avx512vnni", option("AVX512_VNNI", false)),
            ("avx512bf16", option("AVX512_BF16", false)),
            ("avxvnni", option("AVX_VNNI", false)),
        ];
        let required: Vec<&str> = table
            .iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| *name)
            .collect();
        let level = if required.contains(&"avx2") {
            "v3"
        } else {
            "v2"
        };
        (level, required)
    };
    println!("cargo:rustc-env=LINUXFLOW_GGML_LEVEL={level}");
    println!(
        "cargo:rustc-env=LINUXFLOW_GGML_REQUIRED={}",
        required.join(",")
    );
}
