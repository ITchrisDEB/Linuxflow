//! Lightweight engine binary: contains no interface (no GTK).
//!
//! The main application launches it as a child process (`--engine-worker`) so that a crash
//! of the GPU driver or of whisper.cpp does not kill it. The archive ships two variants built
//! for different CPU levels (x86-64-v3 and x86-64-v2); the application picks one according to the CPU.

fn main() {
    if let Some(code) = linuxflow_stt::dispatch_subprocess_mode() {
        std::process::exit(code);
    }
    eprintln!("linuxflow-engine: internal binary, started by linuxflow (--engine-worker or --hardware-probe).");
    std::process::exit(2);
}
