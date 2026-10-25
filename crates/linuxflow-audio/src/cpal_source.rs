//! Real source: cpal (ALSA, which covers PipeWire and PulseAudio through their
//! compatibility layer).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat};
use linuxflow_core::domain::MicrophoneInfo;
use linuxflow_core::Error;

use crate::capture::{AudioSource, CaptureWriter, ErrorReporter, OpenedSource, Severity};
use crate::microphones::{
    build_microphone_list, choose_device, default_entry, DeviceChoice, RawDevice,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct CpalSource;

/// Severity of a cpal stream error (pure function, tested).
pub fn classify_stream_error(kind: ErrorKind) -> Severity {
    match kind {
        // The route follows the default microphone by itself; real-time priority is optional.
        ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => Severity::Ignore,
        // Possible gap in the audio, but the stream continues.
        ErrorKind::Xrun => Severity::Warn,
        // Device lost, busy, permission denied, stream invalidated... : capture lost.
        _ => Severity::Fatal,
    }
}

fn tr_error(prefix: &str, err: &cpal::Error) -> Error {
    Error::msg(linuxflow_i18n::tr_fmt(prefix, &[&err.to_string()]))
}

/// Translates an open error into a clear message according to its nature.
fn open_error(err: &cpal::Error) -> Error {
    match err.kind() {
        ErrorKind::DeviceNotAvailable => tr_error("The microphone is not available: {}", err),
        ErrorKind::DeviceBusy => tr_error(
            "The microphone is busy (used by another application): {}",
            err,
        ),
        ErrorKind::PermissionDenied => tr_error("Access to the microphone was denied: {}", err),
        _ => tr_error("Could not open the microphone: {}", err),
    }
}

fn raw_devices(host: &cpal::Host) -> Vec<RawDevice> {
    let Ok(devices) = host.devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|device| {
            let id = device.id().ok()?.id().to_string();
            let name = device
                .description()
                .map(|d| d.name().to_string())
                .unwrap_or_default();
            Some(RawDevice {
                id,
                name,
                input: device.supports_input(),
            })
        })
        .collect()
}

fn find_device(host: &cpal::Host, id: &str) -> Option<cpal::Device> {
    host.devices()
        .ok()?
        .find(|d| d.supports_input() && d.id().ok().is_some_and(|i| i.id() == id))
}

impl AudioSource for CpalSource {
    fn open(
        &self,
        device: Option<&str>,
        mut writer: CaptureWriter,
        errors: ErrorReporter,
    ) -> Result<OpenedSource, Error> {
        let host = cpal::default_host();
        let choice = if matches!(device.map(str::trim), None | Some("") | Some("default")) {
            DeviceChoice::Default
        } else {
            choose_device(device, &raw_devices(&host))
        };
        let selected = match &choice {
            DeviceChoice::Named(id) => find_device(&host, id),
            DeviceChoice::FallbackDefault => {
                tracing::warn!(
                    "requested microphone not found, falling back to the default microphone"
                );
                None
            }
            DeviceChoice::Default => None,
        };
        let device = match selected.or_else(|| host.default_input_device()) {
            Some(device) => device,
            None => return Err(Error::msg(linuxflow_i18n::tr("No microphone found"))),
        };
        let supported = device.default_input_config().map_err(|e| open_error(&e))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = usize::from(config.channels);
        let input_rate = config.sample_rate;
        let on_error = move |err: cpal::Error| {
            errors.report(classify_stream_error(err.kind()), &err.to_string());
        };
        macro_rules! build {
            ($t:ty) => {
                device.build_input_stream(
                    config,
                    move |data: &[$t], _| {
                        // Real-time path: writing to the ring, nothing else.
                        writer.push_interleaved(data, channels);
                    },
                    on_error,
                    None,
                )
            };
        }
        let stream = match format {
            SampleFormat::F32 => build!(f32),
            SampleFormat::I16 => build!(i16),
            SampleFormat::U16 => build!(u16),
            SampleFormat::I32 => build!(i32),
            SampleFormat::I8 => build!(i8),
            SampleFormat::U8 => build!(u8),
            other => {
                return Err(Error::msg(linuxflow_i18n::tr_fmt(
                    "Unsupported microphone sample format: {}",
                    &[&format!("{other:?}")],
                )))
            }
        }
        .map_err(|e| open_error(&e))?;
        stream.play().map_err(|e| open_error(&e))?;
        Ok(OpenedSource {
            input_rate,
            guard: Box::new(stream),
        })
    }

    fn microphones(&self) -> Vec<MicrophoneInfo> {
        let host = cpal::default_host();
        let list = build_microphone_list(&raw_devices(&host));
        if list.is_empty() {
            vec![default_entry()]
        } else {
            list
        }
    }
}

/// Available microphones, with the `default` entry always first.
pub fn list_microphones() -> Vec<MicrophoneInfo> {
    CpalSource.microphones()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn stream_error_classification() {
        assert_eq!(
            classify_stream_error(ErrorKind::DeviceNotAvailable),
            Severity::Fatal
        );
        assert_eq!(
            classify_stream_error(ErrorKind::StreamInvalidated),
            Severity::Fatal
        );
        assert_eq!(
            classify_stream_error(ErrorKind::PermissionDenied),
            Severity::Fatal
        );
        assert_eq!(
            classify_stream_error(ErrorKind::BackendError),
            Severity::Fatal
        );
        assert_eq!(classify_stream_error(ErrorKind::Xrun), Severity::Warn);
        assert_eq!(
            classify_stream_error(ErrorKind::DeviceChanged),
            Severity::Ignore
        );
    }

    #[test]
    fn open_errors_have_clear_messages() {
        let e = open_error(&cpal::Error::new(ErrorKind::DeviceNotAvailable)).to_string();
        assert!(e.contains("not available"), "{e}");
        let e = open_error(&cpal::Error::new(ErrorKind::PermissionDenied)).to_string();
        assert!(e.contains("denied"), "{e}");
    }

    /// Enumeration only: cpal opens no device at this step, no capture.
    #[test]
    fn real_enumeration_always_has_default() {
        let list = list_microphones();
        assert_eq!(list[0].id, "default");
        eprintln!("micros : {list:?}");
    }
}
