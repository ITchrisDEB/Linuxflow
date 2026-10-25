//! Microphone list and device selection. Pure logic (testable without hardware);
//! the real enumeration is in [`crate::cpal_source`].

use linuxflow_core::domain::MicrophoneInfo;

/// Identifier of the system default microphone.
pub const DEFAULT_ID: &str = "default";

/// Raw device as reported by the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDevice {
    /// Driver identifier (ALSA PCM name: `pipewire`, `sysdefault:CARD=PCH`...).
    pub id: String,
    /// Readable name (first line of the driver description).
    pub name: String,
    /// True if the device can capture.
    pub input: bool,
}

/// "Default microphone" entry, always present and always first.
pub fn default_entry() -> MicrophoneInfo {
    MicrophoneInfo {
        id: DEFAULT_ID.to_string(),
        label: linuxflow_i18n::tr("System default microphone"),
    }
}

/// ALSA plugins of no interest to the user (conversion, mixing, output, raw access).
fn is_noise(id: &str) -> bool {
    let base = id.split([':', '=']).next().unwrap_or(id);
    matches!(
        base,
        "null"
            | "lavrate"
            | "samplerate"
            | "speexrate"
            | "speex"
            | "jack"
            | "oss"
            | "upmix"
            | "vdownmix"
            | "dmix"
            | "dsnoop"
            | "front"
            | "surround21"
            | "surround40"
            | "surround41"
            | "surround50"
            | "surround51"
            | "surround71"
            | "iec958"
            | "spdif"
            | "hdmi"
            | "a52"
            | "usbstream"
            | "hw"
            | "plughw"
            | "plug"
    )
}

/// Builds the list offered to the user: `default` first, input devices
/// only, technical plugins discarded, unique labels.
pub fn build_microphone_list(raw: &[RawDevice]) -> Vec<MicrophoneInfo> {
    let mut list = vec![default_entry()];
    for device in raw {
        if !device.input || device.id == DEFAULT_ID || is_noise(&device.id) {
            continue;
        }
        let label = if device.name.trim().is_empty() {
            device.id.clone()
        } else {
            device.name.trim().to_string()
        };
        if list.iter().any(|m| m.id == device.id) {
            continue;
        }
        // Two devices with the same name: the identifier is appended to tell them apart.
        let label = if list.iter().any(|m| m.label == label) {
            format!("{label} ({})", device.id)
        } else {
            label
        };
        list.push(MicrophoneInfo {
            id: device.id.clone(),
            label,
        });
    }
    list
}

/// Result of choosing a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceChoice {
    /// Default microphone requested.
    Default,
    /// Device found (driver identifier).
    Named(String),
    /// Device requested but absent: fall back to the default microphone.
    FallbackDefault,
}

/// Chooses the device to open. Match by exact identifier, then by readable name
/// (case-insensitive). An unknown identifier falls back to the default.
pub fn choose_device(requested: Option<&str>, available: &[RawDevice]) -> DeviceChoice {
    let wanted = requested.map(str::trim).unwrap_or("");
    if wanted.is_empty() || wanted.eq_ignore_ascii_case(DEFAULT_ID) {
        return DeviceChoice::Default;
    }
    let inputs = || available.iter().filter(|d| d.input);
    if let Some(found) = inputs().find(|d| d.id == wanted) {
        return DeviceChoice::Named(found.id.clone());
    }
    let lower = wanted.to_lowercase();
    if let Some(found) = inputs().find(|d| d.name.to_lowercase() == lower) {
        return DeviceChoice::Named(found.id.clone());
    }
    DeviceChoice::FallbackDefault
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str, input: bool) -> RawDevice {
        RawDevice {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    fn sample() -> Vec<RawDevice> {
        vec![
            dev("null", "Discard all samples", true),
            dev("pipewire", "PipeWire Sound Server", true),
            dev("default", "Default ALSA Output", true),
            dev("sysdefault:CARD=PCH", "HDA Intel PCH", true),
            dev("hw:CARD=PCH,DEV=0", "HDA Intel PCH", true),
            dev("plughw:CARD=PCH,DEV=0", "HDA Intel PCH", true),
            dev("front:CARD=PCH,DEV=0", "HDA Intel PCH", false),
            dev("sysdefault:CARD=Out", "Speakers only", false),
            dev("dmix", "Direct sample mixing", true),
            dev("usb", "", true),
        ]
    }

    #[test]
    fn default_is_always_first() {
        assert_eq!(build_microphone_list(&[])[0].id, "default");
        assert_eq!(build_microphone_list(&sample())[0].id, "default");
        assert!(!default_entry().label.is_empty());
    }

    #[test]
    fn only_inputs_and_no_technical_plugins() {
        let ids: Vec<_> = build_microphone_list(&sample())
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            ids,
            vec!["default", "pipewire", "sysdefault:CARD=PCH", "usb"]
        );
    }

    #[test]
    fn labels_are_readable_and_unique() {
        let list = build_microphone_list(&sample());
        assert_eq!(list[1].label, "PipeWire Sound Server");
        assert_eq!(list[3].label, "usb"); // nom vide -> identifiant
        let mut labels: Vec<_> = list.iter().map(|m| m.label.clone()).collect();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), list.len());
        let twin = build_microphone_list(&[dev("a", "Mic", true), dev("b", "Mic", true)]);
        assert_eq!(twin[2].label, "Mic (b)");
    }

    #[test]
    fn choose_default_variants() {
        assert_eq!(choose_device(None, &sample()), DeviceChoice::Default);
        assert_eq!(choose_device(Some(""), &sample()), DeviceChoice::Default);
        assert_eq!(
            choose_device(Some("default"), &sample()),
            DeviceChoice::Default
        );
        assert_eq!(choose_device(Some("Default"), &[]), DeviceChoice::Default);
    }

    #[test]
    fn choose_by_id_then_by_name() {
        assert_eq!(
            choose_device(Some("sysdefault:CARD=PCH"), &sample()),
            DeviceChoice::Named("sysdefault:CARD=PCH".into())
        );
        assert_eq!(
            choose_device(Some("pipewire sound server"), &sample()),
            DeviceChoice::Named("pipewire".into())
        );
    }

    #[test]
    fn missing_device_falls_back_to_default() {
        assert_eq!(
            choose_device(Some("usb-gone"), &sample()),
            DeviceChoice::FallbackDefault
        );
        // An output-only device is not a microphone.
        assert_eq!(
            choose_device(Some("sysdefault:CARD=Out"), &sample()),
            DeviceChoice::FallbackDefault
        );
    }
}
