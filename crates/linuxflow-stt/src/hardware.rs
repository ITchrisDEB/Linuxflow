//! Hardware detection without a hard link to libvulkan: `libvulkan.so.1` is loaded at
//! run time (`ash` crate, `loaded` feature). Its absence, a broken driver or a panic
//! simply give an empty GPU list and a reason, never a start-up crash.

use std::collections::HashSet;
use std::fmt;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ash::vk;
use serde::{Deserialize, Serialize};

use crate::cpu_guard;

/// Kind of a Vulkan device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuKind {
    Discrete,
    Integrated,
    Virtual,
    /// Software driver (llvmpipe, lavapipe, SwiftShader): treated as a CPU.
    Software,
    Other,
}

/// A Vulkan device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// Position in the raw Vulkan enumeration.
    pub vulkan_index: usize,
    /// Number seen by ggml (`gpu_device` of whisper.cpp): rank among the real GPUs. `None` for
    /// a software driver or a duplicate. This is the number the user chooses (`gpu:N`).
    pub ggml_index: Option<usize>,
    pub name: String,
    pub vendor_id: u32,
    pub kind: GpuKind,
    /// Estimate: sum of the memory heaps local to the device (shared with the system on
    /// an integrated GPU). `None` if Vulkan does not say.
    pub vram_bytes: Option<u64>,
    /// Vulkan API version of the driver, `major.minor.patch`.
    pub api_version: String,
}

impl GpuInfo {
    /// True for a real GPU (not a software driver) usable by the engine.
    pub fn is_real(&self) -> bool {
        self.kind != GpuKind::Software && self.ggml_index.is_some()
    }
}

/// Snapshot of the hardware useful for choosing the model and the device.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Hardware {
    pub gpus: Vec<GpuInfo>,
    /// x86 features of the processor useful to ggml (empty outside x86-64).
    pub cpu_features: Vec<String>,
    /// Total RAM, if known.
    pub ram_bytes: Option<u64>,
    /// Why the GPU list is empty (Vulkan absent, driver failing…), for the log and
    /// the compatibility page.
    pub vulkan_error: Option<String>,
}

impl Hardware {
    /// Detection in the current process. Never panics. A faulty Vulkan driver can
    /// still crash the process: see [`Hardware::detect_isolated`].
    pub fn detect() -> Self {
        let (gpus, vulkan_error) = match std::panic::catch_unwind(enumerate_vulkan) {
            Ok(Ok(gpus)) => (gpus, None),
            Ok(Err(err)) => (Vec::new(), Some(err)),
            Err(_) => (
                Vec::new(),
                Some("panic during Vulkan detection".to_string()),
            ),
        };
        Hardware {
            gpus,
            cpu_features: cpu_guard::host_features(),
            ram_bytes: read_meminfo_total(),
            vulkan_error,
        }
    }

    /// Detection in a child process (`<exe> --hardware-probe`): a driver that crashes does not
    /// kill the application. Fallback: hardware without a GPU and the reason in `vulkan_error`.
    pub fn detect_isolated(timeout: Duration) -> Self {
        match run_probe(timeout) {
            Ok(hw) => hw,
            Err(err) => Hardware {
                gpus: Vec::new(),
                cpu_features: cpu_guard::host_features(),
                ram_bytes: read_meminfo_total(),
                vulkan_error: Some(err),
            },
        }
    }

    /// Real GPUs (excluding software drivers), in ggml order.
    pub fn real_gpus(&self) -> Vec<&GpuInfo> {
        let mut gpus: Vec<&GpuInfo> = self.gpus.iter().filter(|g| g.is_real()).collect();
        gpus.sort_by_key(|g| g.ggml_index);
        gpus
    }

    pub fn has_real_gpu(&self) -> bool {
        self.gpus.iter().any(GpuInfo::is_real)
    }

    /// Largest estimated video memory among the real GPUs.
    pub fn best_vram_bytes(&self) -> Option<u64> {
        self.gpus
            .iter()
            .filter(|g| g.is_real())
            .filter_map(|g| g.vram_bytes)
            .max()
    }
}

/// Entry point of the `--hardware-probe` mode: writes the JSON of [`Hardware::detect`] on stdout.
pub fn probe_main() -> i32 {
    match serde_json::to_string(&Hardware::detect()) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(_) => 1,
    }
}

fn run_probe(timeout: Duration) -> Result<Hardware, String> {
    let exe = std::env::current_exe().map_err(|e| format!("executable not found: {e}"))?;
    let mut child = Command::new(exe)
        .arg("--hardware-probe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the probe: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "probe output unavailable".to_string())?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Vulkan detection timed out".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("waiting for the probe: {e}")),
        }
    };
    let output = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "the Vulkan probe stopped ({status}): driver probably faulty"
        ));
    }
    let line = output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    serde_json::from_str(line).map_err(|e| format!("unreadable probe answer: {e}"))
}

// ---- Vulkan enumeration -------------------------------------------------------------------------

/// Raw data of a device, before classification (separates the Vulkan API from the pure logic).
struct RawDevice {
    name: String,
    vendor_id: u32,
    device_type: i32,
    heap_local_bytes: u64,
    api_version: u32,
    uuid: Option<[u8; 16]>,
}

fn enumerate_vulkan() -> Result<Vec<GpuInfo>, String> {
    // SAFETY: loads libvulkan.so.1 with dlopen; fails cleanly if it is absent.
    let entry = unsafe { ash::Entry::load() }
        .map_err(|e| format!("libvulkan.so.1 introuvable ou inutilisable : {e}"))?;
    // SAFETY: loader entry call with no pointer supplied by us.
    let instance_version = unsafe { entry.try_enumerate_instance_version() }
        .ok()
        .flatten()
        .unwrap_or(vk::API_VERSION_1_0);
    let api = if instance_version >= vk::API_VERSION_1_1 {
        vk::API_VERSION_1_1
    } else {
        vk::API_VERSION_1_0
    };
    let app_info = vk::ApplicationInfo::default().api_version(api);
    let create_info = vk::InstanceCreateInfo::default().application_info(&app_info);
    // SAFETY: `create_info` and `app_info` live during the call; the instance is destroyed further down.
    let instance = unsafe { entry.create_instance(&create_info, None) }
        .map_err(|e| format!("could not create the Vulkan instance: {e}"))?;

    // SAFETY: `instance` is valid until the `destroy_instance` below.
    let raws: Result<Vec<RawDevice>, String> = unsafe {
        instance
            .enumerate_physical_devices()
            .map_err(|e| format!("could not enumerate the GPUs: {e}"))
            .map(|devices| {
                devices
                    .into_iter()
                    .map(|device| read_device(&instance, device, api))
                    .collect()
            })
    };
    // SAFETY: no object derived from the instance is used after this point.
    unsafe { instance.destroy_instance(None) };
    Ok(classify_devices(raws?))
}

/// # Safety
/// `device` must come from `instance`, which is still valid.
unsafe fn read_device(instance: &ash::Instance, device: vk::PhysicalDevice, api: u32) -> RawDevice {
    // SAFETY: guaranteed by the caller for the next three calls.
    let props = unsafe { instance.get_physical_device_properties(device) };
    let mem = unsafe { instance.get_physical_device_memory_properties(device) };
    let uuid = if api >= vk::API_VERSION_1_1 {
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        // SAFETY: 1.1 instance, `props2` only chains `id`, both local.
        unsafe { instance.get_physical_device_properties2(device, &mut props2) };
        Some(id.device_uuid)
    } else {
        None
    };
    let name = props
        .device_name_as_c_str()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "GPU inconnu".to_string());
    let heap_local_bytes = mem.memory_heaps[..mem.memory_heap_count as usize]
        .iter()
        .filter(|h| h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .map(|h| h.size)
        .sum();
    RawDevice {
        name,
        vendor_id: props.vendor_id,
        device_type: props.device_type.as_raw(),
        heap_local_bytes,
        api_version: props.api_version,
        uuid,
    }
}

/// Kind of the device from the raw Vulkan type and its name (pure function).
pub fn classify_kind(device_type_raw: i32, name: &str) -> GpuKind {
    let lower = name.to_lowercase();
    let software_name = [
        "llvmpipe",
        "lavapipe",
        "swiftshader",
        "softpipe",
        "software rasterizer",
    ]
    .iter()
    .any(|s| lower.contains(s));
    if software_name || device_type_raw == vk::PhysicalDeviceType::CPU.as_raw() {
        return GpuKind::Software;
    }
    match device_type_raw {
        x if x == vk::PhysicalDeviceType::DISCRETE_GPU.as_raw() => GpuKind::Discrete,
        x if x == vk::PhysicalDeviceType::INTEGRATED_GPU.as_raw() => GpuKind::Integrated,
        x if x == vk::PhysicalDeviceType::VIRTUAL_GPU.as_raw() => GpuKind::Virtual,
        _ => GpuKind::Other,
    }
}

fn classify_devices(raws: Vec<RawDevice>) -> Vec<GpuInfo> {
    let mut seen = HashSet::new();
    let mut gpus = Vec::new();
    for (vulkan_index, raw) in raws.into_iter().enumerate() {
        let kind = classify_kind(raw.device_type, &raw.name);
        // Same GPU exposed by two drivers: the first one is kept (ggml makes a finer choice).
        let duplicate = kind != GpuKind::Software
            && raw.uuid.is_some_and(|u| u != [0u8; 16] && !seen.insert(u));
        gpus.push(GpuInfo {
            vulkan_index,
            ggml_index: None,
            name: raw.name,
            vendor_id: raw.vendor_id,
            kind: if duplicate { GpuKind::Other } else { kind },
            vram_bytes: (raw.heap_local_bytes > 0).then_some(raw.heap_local_bytes),
            api_version: format!(
                "{}.{}.{}",
                vk::api_version_major(raw.api_version),
                vk::api_version_minor(raw.api_version),
                vk::api_version_patch(raw.api_version)
            ),
        });
        if duplicate {
            if let Some(last) = gpus.last_mut() {
                last.ggml_index = None;
            }
        }
    }
    assign_ggml_indices(&mut gpus);
    gpus
}

/// Reproduces the numbering of ggml-vulkan: only discrete and integrated GPUs count, in
/// enumeration order; without such GPUs, the first non-software device is number 0.
/// Duplicates (marked `Other` before the call by UUID) stay without a number.
pub fn assign_ggml_indices(gpus: &mut [GpuInfo]) {
    let mut next = 0;
    for gpu in gpus.iter_mut() {
        gpu.ggml_index = None;
        if matches!(gpu.kind, GpuKind::Discrete | GpuKind::Integrated) {
            gpu.ggml_index = Some(next);
            next += 1;
        }
    }
    if next == 0 {
        if let Some(first) = gpus.iter_mut().find(|g| g.kind != GpuKind::Software) {
            first.ggml_index = Some(0);
        }
    }
}

/// `MemTotal` of `/proc/meminfo` in bytes (pure function on the text).
pub fn parse_meminfo_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

fn read_meminfo_total() -> Option<u64> {
    parse_meminfo_total(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

// ---- Device choice ----------------------------------------------------------------------

/// User setting for the device (`device` / `LINUXFLOW_DEVICE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceSetting {
    /// Best real GPU if there is one, otherwise CPU.
    Auto,
    Cpu,
    /// A GPU: the first real one (`None`) or the given ggml number.
    Gpu(Option<usize>),
}

/// Invalid setting or impossible to satisfy. The messages are explicit: never a silent
/// fallback when the user forced a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    Unsupported(String),
    Invalid(String),
    NoGpu,
    GpuNotFound(usize),
    NotCompiled,
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::Unsupported(v) => {
                write!(
                    f,
                    "\"{v}\" is not supported, use \"gpu\" (Vulkan) or \"cpu\""
                )
            }
            DeviceError::Invalid(v) => write!(
                f,
                "invalid device \"{v}\": expected auto, cpu, gpu, gpu:N or a number"
            ),
            DeviceError::NoGpu => write!(f, "no usable Vulkan GPU (software drivers are ignored)"),
            DeviceError::GpuNotFound(n) => write!(f, "GPU #{n} does not exist"),
            DeviceError::NotCompiled => {
                write!(f, "this build was not compiled with Vulkan support")
            }
        }
    }
}

impl std::error::Error for DeviceError {}

/// Parses a setting (`auto`, `cpu`, `gpu`, `vulkan`, `gpu:N`, `N`). CUDA/ROCm: explicit error.
pub fn parse_device_setting(raw: &str) -> Result<DeviceSetting, DeviceError> {
    let value = raw.trim().to_lowercase();
    match value.as_str() {
        "" | "auto" => return Ok(DeviceSetting::Auto),
        "cpu" => return Ok(DeviceSetting::Cpu),
        "gpu" | "vulkan" => return Ok(DeviceSetting::Gpu(None)),
        _ => {}
    }
    if value.starts_with("cuda") || value.starts_with("rocm") || value.starts_with("hip") {
        return Err(DeviceError::Unsupported(raw.trim().to_string()));
    }
    let number = value
        .strip_prefix("gpu:")
        .or_else(|| value.strip_prefix("vulkan:"))
        .unwrap_or(&value);
    number
        .parse::<usize>()
        .map(|n| DeviceSetting::Gpu(Some(n)))
        .map_err(|_| DeviceError::Invalid(raw.trim().to_string()))
}

/// Combines the setting and the environment variable (which wins if it is non-empty).
pub fn resolve_device_setting(
    setting: Option<&str>,
    env_value: Option<&str>,
) -> Result<DeviceSetting, DeviceError> {
    let env_value = env_value.map(str::trim).filter(|v| !v.is_empty());
    parse_device_setting(env_value.or(setting).unwrap_or("auto"))
}

/// Reads `LINUXFLOW_DEVICE` then applies [`resolve_device_setting`].
pub fn device_setting_from_env(setting: Option<&str>) -> Result<DeviceSetting, DeviceError> {
    resolve_device_setting(setting, std::env::var("LINUXFLOW_DEVICE").ok().as_deref())
}

/// Device chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceChoice {
    Cpu,
    Gpu {
        ggml_index: usize,
        name: String,
        vram_bytes: Option<u64>,
    },
}

/// True if this build embeds the Vulkan backend.
pub const GPU_COMPILED: bool = cfg!(feature = "vulkan");

/// Chooses the device. `gpu_compiled` is [`GPU_COMPILED`] in production (parameter for
/// be able to test both cases).
pub fn select_device(
    hardware: &Hardware,
    setting: DeviceSetting,
    gpu_compiled: bool,
) -> Result<DeviceChoice, DeviceError> {
    let real = hardware.real_gpus();
    // Same preference for `auto` and for plain `gpu`: discrete before integrated, then largest memory.
    let best_gpu = || {
        real.iter()
            .copied()
            .max_by_key(|g| (g.kind == GpuKind::Discrete, g.vram_bytes.unwrap_or(0)))
    };
    let to_choice = |g: &GpuInfo| DeviceChoice::Gpu {
        ggml_index: g.ggml_index.unwrap_or(0),
        name: g.name.clone(),
        vram_bytes: g.vram_bytes,
    };
    match setting {
        DeviceSetting::Cpu => Ok(DeviceChoice::Cpu),
        DeviceSetting::Auto => {
            if !gpu_compiled {
                return Ok(DeviceChoice::Cpu);
            }
            Ok(best_gpu().map_or(DeviceChoice::Cpu, to_choice))
        }
        DeviceSetting::Gpu(wanted) => {
            if !gpu_compiled {
                return Err(DeviceError::NotCompiled);
            }
            match wanted {
                None => best_gpu().map(to_choice).ok_or(DeviceError::NoGpu),
                Some(n) => real
                    .iter()
                    .copied()
                    .find(|g| g.ggml_index == Some(n))
                    .map(to_choice)
                    .ok_or(if real.is_empty() {
                        DeviceError::NoGpu
                    } else {
                        DeviceError::GpuNotFound(n)
                    }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, ty: vk::PhysicalDeviceType, vram: u64, uuid: u8) -> RawDevice {
        RawDevice {
            name: name.to_string(),
            vendor_id: 0x1234,
            device_type: ty.as_raw(),
            heap_local_bytes: vram,
            api_version: vk::make_api_version(0, 1, 3, 280),
            uuid: Some([uuid; 16]),
        }
    }

    const GIB: u64 = 1 << 30;

    fn sample_hw() -> Hardware {
        let gpus = classify_devices(vec![
            raw("llvmpipe (LLVM 19)", vk::PhysicalDeviceType::CPU, 0, 9),
            raw(
                "AMD Radeon 610M",
                vk::PhysicalDeviceType::INTEGRATED_GPU,
                2 * GIB,
                1,
            ),
            raw(
                "NVIDIA GeForce RTX",
                vk::PhysicalDeviceType::DISCRETE_GPU,
                16 * GIB,
                2,
            ),
        ]);
        Hardware {
            gpus,
            ..Hardware::default()
        }
    }

    #[test]
    fn plain_gpu_prefers_the_discrete_gpu_like_auto() {
        let hw = Hardware {
            gpus: classify_devices(vec![
                raw("iGPU", vk::PhysicalDeviceType::INTEGRATED_GPU, 2 * GIB, 1),
                raw("RTX", vk::PhysicalDeviceType::DISCRETE_GPU, 16 * GIB, 2),
            ]),
            ..Hardware::default()
        };
        let auto = select_device(&hw, DeviceSetting::Auto, true).unwrap();
        let gpu = select_device(&hw, DeviceSetting::Gpu(None), true).unwrap();
        assert_eq!(auto, gpu);
        assert!(matches!(gpu, DeviceChoice::Gpu { ref name, .. } if name == "RTX"));
    }

    #[test]
    fn software_drivers_are_cpu() {
        assert_eq!(
            classify_kind(vk::PhysicalDeviceType::CPU.as_raw(), "x"),
            GpuKind::Software
        );
        // Even when announced as a GPU, llvmpipe/lavapipe stay software.
        let discrete = vk::PhysicalDeviceType::DISCRETE_GPU.as_raw();
        assert_eq!(
            classify_kind(discrete, "llvmpipe (LLVM 19.1)"),
            GpuKind::Software
        );
        assert_eq!(classify_kind(discrete, "Lavapipe"), GpuKind::Software);
        assert_eq!(classify_kind(discrete, "RTX 4070"), GpuKind::Discrete);
    }

    #[test]
    fn ggml_indices_skip_software_and_follow_enumeration() {
        let hw = sample_hw();
        assert_eq!(hw.gpus[0].ggml_index, None);
        assert_eq!(hw.gpus[1].ggml_index, Some(0));
        assert_eq!(hw.gpus[2].ggml_index, Some(1));
        assert_eq!(hw.real_gpus().len(), 2);
        assert_eq!(hw.best_vram_bytes(), Some(16 * GIB));
    }

    #[test]
    fn only_software_means_no_real_gpu() {
        let hw = Hardware {
            gpus: classify_devices(vec![raw("llvmpipe", vk::PhysicalDeviceType::CPU, 0, 1)]),
            ..Hardware::default()
        };
        assert!(!hw.has_real_gpu());
        assert_eq!(
            select_device(&hw, DeviceSetting::Auto, true),
            Ok(DeviceChoice::Cpu)
        );
        assert_eq!(
            select_device(&hw, DeviceSetting::Gpu(None), true),
            Err(DeviceError::NoGpu)
        );
    }

    #[test]
    fn duplicate_uuid_is_listed_once_as_usable() {
        let gpus = classify_devices(vec![
            raw("RTX", vk::PhysicalDeviceType::DISCRETE_GPU, GIB, 7),
            raw(
                "RTX (autre pilote)",
                vk::PhysicalDeviceType::DISCRETE_GPU,
                GIB,
                7,
            ),
        ]);
        assert_eq!(gpus.iter().filter(|g| g.ggml_index.is_some()).count(), 1);
    }

    #[test]
    fn auto_prefers_discrete_gpu() {
        match select_device(&sample_hw(), DeviceSetting::Auto, true) {
            Ok(DeviceChoice::Gpu {
                ggml_index, name, ..
            }) => {
                assert_eq!(ggml_index, 1);
                assert!(name.contains("NVIDIA"));
            }
            other => panic!("choix inattendu : {other:?}"),
        }
    }

    #[test]
    fn explicit_choices() {
        let hw = sample_hw();
        assert_eq!(
            select_device(&hw, DeviceSetting::Cpu, true),
            Ok(DeviceChoice::Cpu)
        );
        assert!(matches!(
            select_device(&hw, DeviceSetting::Gpu(Some(0)), true),
            Ok(DeviceChoice::Gpu { ggml_index: 0, .. })
        ));
        assert_eq!(
            select_device(&hw, DeviceSetting::Gpu(Some(5)), true),
            Err(DeviceError::GpuNotFound(5))
        );
    }

    #[test]
    fn without_vulkan_build_auto_is_cpu_and_forced_gpu_is_an_error() {
        let hw = sample_hw();
        assert_eq!(
            select_device(&hw, DeviceSetting::Auto, false),
            Ok(DeviceChoice::Cpu)
        );
        assert_eq!(
            select_device(&hw, DeviceSetting::Gpu(None), false),
            Err(DeviceError::NotCompiled)
        );
    }

    #[test]
    fn parse_setting() {
        assert_eq!(parse_device_setting(""), Ok(DeviceSetting::Auto));
        assert_eq!(parse_device_setting(" CPU "), Ok(DeviceSetting::Cpu));
        assert_eq!(parse_device_setting("gpu"), Ok(DeviceSetting::Gpu(None)));
        assert_eq!(
            parse_device_setting("vulkan:1"),
            Ok(DeviceSetting::Gpu(Some(1)))
        );
        assert_eq!(parse_device_setting("2"), Ok(DeviceSetting::Gpu(Some(2))));
        assert!(matches!(
            parse_device_setting("cuda"),
            Err(DeviceError::Unsupported(_))
        ));
        assert!(matches!(
            parse_device_setting("rocm:0"),
            Err(DeviceError::Unsupported(_))
        ));
        assert!(matches!(
            parse_device_setting("banane"),
            Err(DeviceError::Invalid(_))
        ));
    }

    #[test]
    fn env_overrides_setting() {
        assert_eq!(
            resolve_device_setting(Some("cpu"), Some("gpu:0")),
            Ok(DeviceSetting::Gpu(Some(0)))
        );
        assert_eq!(
            resolve_device_setting(Some("cpu"), Some("  ")),
            Ok(DeviceSetting::Cpu)
        );
        assert_eq!(resolve_device_setting(None, None), Ok(DeviceSetting::Auto));
    }

    #[test]
    fn meminfo_parsing() {
        let text = "MemTotal:       32768000 kB\nMemFree: 1 kB\n";
        assert_eq!(parse_meminfo_total(text), Some(32_768_000 * 1024));
        assert_eq!(parse_meminfo_total("rien"), None);
    }

    #[test]
    fn detect_never_panics_and_reports_cpu() {
        let hw = Hardware::detect();
        println!("hardware detected: {hw:?}");
        // Whatever Vulkan does, these fields are filled in or made explicit.
        assert!(hw.ram_bytes.is_some());
        assert!(hw.gpus.iter().all(|g| !g.name.is_empty()) || hw.gpus.is_empty());
    }

    #[test]
    fn hardware_roundtrips_json() {
        let hw = sample_hw();
        let json = serde_json::to_string(&hw).unwrap_or_default();
        assert_eq!(serde_json::from_str::<Hardware>(&json).ok(), Some(hw));
    }
}
