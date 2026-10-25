#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! linuxflow-stt: transcription engine (whisper.cpp through `whisper-rs`).
//!
//! - [`WhisperEngine`] / [`InProcessEngine`]: engine in the current process;
//! - [`WorkerClient`]: same `SpeechToText` contract, engine in a child process of the same
//!   binary (`--engine-worker`) so that a driver crash does not kill the application;
//! - [`Hardware`]: detection of the Vulkan GPUs (dynamic loading) and choice of the device;
//! - [`cpu_guard`]: clean refusal on a processor without the build's instructions.
//!
//! The host binary must call [`dispatch_subprocess_mode`] at the start of `main`.

pub mod cpu_guard;
pub mod engine;
pub mod hardware;
pub mod plan;
pub mod whisper_backend;
pub mod worker;

pub use engine::{
    Backend, DecodeParams, EngineConfig, InProcessEngine, LoadFailure, LoadedModel, RawTranscript,
    VadMode, WhisperEngine,
};
pub use hardware::{
    device_setting_from_env, parse_device_setting, resolve_device_setting, select_device,
    DeviceChoice, DeviceError, DeviceSetting, GpuInfo, GpuKind, Hardware, GPU_COMPILED,
};
pub use plan::{build_fallback_chain, Device, LoadPlan};
pub use worker::{dispatch_subprocess_mode, run_worker, worker_main, WorkerClient, WorkerConfig};
