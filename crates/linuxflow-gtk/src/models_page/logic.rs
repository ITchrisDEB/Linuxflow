//! Pure logic of the Models page: actions available per state, labels, speed and remaining time.

use crate::hub::labels::{download_percent, format_size};
use linuxflow_core::domain::{ModelState, ModelStatus};
use linuxflow_i18n::{format_percent, tr, tr_fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelAction {
    Download,
    Pause,
    Resume,
    Cancel,
    Delete,
    Verify,
    Use,
}

impl ModelAction {
    pub fn label(self) -> String {
        match self {
            ModelAction::Download => tr("Download"),
            ModelAction::Pause => tr("Pause"),
            ModelAction::Resume => tr("Resume"),
            ModelAction::Cancel => tr("Cancel"),
            ModelAction::Delete => tr("Delete"),
            ModelAction::Verify => tr("Verify integrity"),
            ModelAction::Use => tr("Use this model"),
        }
    }
}

/// Actions offered for a model.
pub fn actions_for(status: &ModelStatus) -> Vec<ModelAction> {
    use ModelAction::*;
    match &status.state {
        ModelState::NotInstalled => vec![Download],
        ModelState::Downloading { .. } => vec![Pause, Cancel],
        ModelState::Paused { .. } => vec![Resume, Cancel],
        ModelState::Installed | ModelState::Unverified => {
            let mut v = Vec::new();
            if !status.in_use {
                v.push(Use);
            }
            v.extend([Verify, Delete]);
            v
        }
        ModelState::Error(_) => vec![Download, Delete],
    }
}

/// State text (never carried by colour alone).
pub fn state_text(state: &ModelState) -> String {
    match state {
        ModelState::NotInstalled => tr("Not installed"),
        ModelState::Downloading { done, total } => {
            if *total == 0 {
                tr("Downloading…")
            } else {
                tr_fmt(
                    "Downloading · {}",
                    &[&format_percent(
                        f64::from(download_percent(*done, *total)) / 100.0,
                        0,
                    )],
                )
            }
        }
        ModelState::Paused { .. } => tr("Paused"),
        ModelState::Installed => tr("Installed"),
        ModelState::Unverified => tr("Installed, not verified (checksum unknown)"),
        ModelState::Error(msg) => tr_fmt("Error: {}", &[msg]),
    }
}

/// Progress fraction [0, 1] (`None` if no download or unknown size).
pub fn progress_fraction(state: &ModelState) -> Option<f64> {
    match state {
        ModelState::Downloading { done, total } | ModelState::Paused { done, total }
            if *total > 0 =>
        {
            Some((*done as f64 / *total as f64).clamp(0.0, 1.0))
        }
        _ => None,
    }
}

pub fn is_in_progress(state: &ModelState) -> bool {
    matches!(
        state,
        ModelState::Downloading { .. } | ModelState::Paused { .. }
    )
}

/// Bytes used on disk by the models present.
pub fn disk_used(models: &[ModelStatus]) -> u64 {
    models
        .iter()
        .map(|m| match m.state {
            ModelState::Installed | ModelState::Unverified => m.disk_bytes,
            ModelState::Downloading { done, .. } | ModelState::Paused { done, .. } => done,
            _ => 0,
        })
        .sum()
}

/// Speed estimate by exponential moving average on timestamped samples.
#[derive(Debug, Default, Clone)]
pub struct SpeedMeter {
    last: Option<(u64, u64)>, // (ms, octets)
    bytes_per_sec: f64,
}

impl SpeedMeter {
    pub fn sample(&mut self, now_ms: u64, done: u64) {
        if let Some((t0, d0)) = self.last {
            if now_ms > t0 && done >= d0 {
                let inst = (done - d0) as f64 * 1000.0 / (now_ms - t0) as f64;
                self.bytes_per_sec = if self.bytes_per_sec == 0.0 {
                    inst
                } else {
                    0.7 * self.bytes_per_sec + 0.3 * inst
                };
            } else if done < d0 {
                self.bytes_per_sec = 0.0; // restart from zero
            }
        }
        self.last = Some((now_ms, done));
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn bytes_per_sec(&self) -> f64 {
        self.bytes_per_sec
    }

    /// Remaining time in seconds, if the speed is known.
    pub fn eta_secs(&self, done: u64, total: u64) -> Option<u64> {
        (self.bytes_per_sec > 1.0 && total > done)
            .then(|| ((total - done) as f64 / self.bytes_per_sec).ceil() as u64)
    }
}

/// Readable short duration ("3 min", "45 s").
pub fn format_eta(secs: u64) -> String {
    if secs < 60 {
        tr_fmt("{} s", &[&secs.to_string()])
    } else if secs < 3600 {
        tr_fmt("{} min", &[&(secs.div_ceil(60)).to_string()])
    } else {
        tr_fmt(
            "{} h {} min",
            &[
                &(secs / 3600).to_string(),
                &((secs % 3600).div_ceil(60)).to_string(),
            ],
        )
    }
}

/// Progress text: "1.3 GB / 3.1 GB · 4.2 MB/s · 7 min left".
pub fn progress_text(done: u64, total: u64, meter: &SpeedMeter) -> String {
    if total == 0 {
        return format_size(done);
    }
    let base = tr_fmt("{} / {}", &[&format_size(done), &format_size(total)]);
    let speed = meter.bytes_per_sec();
    if speed < 1.0 {
        return base;
    }
    let rate = tr_fmt("{}/s", &[&format_size(speed as u64)]);
    match meter.eta_secs(done, total) {
        Some(eta) => tr_fmt("{} · {} · {} left", &[&base, &rate, &format_eta(eta)]),
        None => tr_fmt("{} · {}", &[&base, &rate]),
    }
}

/// Description of the sizes of a model.
pub fn sizes_text(m: &ModelStatus) -> String {
    tr_fmt(
        "Download {} · On disk {} · Memory ≈ {}",
        &[
            &format_size(m.download_bytes),
            &format_size(m.disk_bytes),
            &format_size(m.memory_bytes),
        ],
    )
}

/// Name of a model in the interface language. The catalogue (`models.json`) is in English;
/// known names go through `tr()`, a model added by the user keeps its own name.
pub fn display_name(m: &ModelStatus) -> String {
    match m.id.as_str() {
        "small" => tr("Whisper small (multilingual)"),
        "large-v3-q5_0" => tr("Whisper large-v3 (quantized q5_0)"),
        "large-v3" => tr("Whisper large-v3 (full precision f16)"),
        "silero-vad" => tr("Silero VAD v6.2.0 (voice activity detection)"),
        _ => m.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: ModelState, in_use: bool) -> ModelStatus {
        ModelStatus {
            id: "small".into(),
            name: "Small".into(),
            download_bytes: 487_601_967,
            disk_bytes: 487_601_967,
            memory_bytes: 1_000_000_000,
            state,
            recommended: false,
            in_use,
        }
    }

    #[test]
    fn actions_per_state() {
        use ModelAction::*;
        assert_eq!(
            actions_for(&status(ModelState::NotInstalled, false)),
            vec![Download]
        );
        assert_eq!(
            actions_for(&status(
                ModelState::Downloading { done: 1, total: 2 },
                false
            )),
            vec![Pause, Cancel]
        );
        assert_eq!(
            actions_for(&status(ModelState::Paused { done: 1, total: 2 }, false)),
            vec![Resume, Cancel]
        );
        assert_eq!(
            actions_for(&status(ModelState::Installed, false)),
            vec![Use, Verify, Delete]
        );
        assert_eq!(
            actions_for(&status(ModelState::Installed, true)),
            vec![Verify, Delete]
        );
        assert_eq!(
            actions_for(&status(ModelState::Unverified, false)),
            vec![Use, Verify, Delete]
        );
        assert_eq!(
            actions_for(&status(ModelState::Error("x".into()), false)),
            vec![Download, Delete]
        );
    }

    #[test]
    fn state_texts_and_fraction() {
        assert_eq!(state_text(&ModelState::NotInstalled), "Not installed");
        assert_eq!(
            state_text(&ModelState::Downloading {
                done: 50,
                total: 100
            }),
            "Downloading · 50%"
        );
        assert_eq!(
            state_text(&ModelState::Downloading { done: 5, total: 0 }),
            "Downloading…"
        );
        assert_eq!(
            state_text(&ModelState::Error("disk full".into())),
            "Error: disk full"
        );
        assert_eq!(
            progress_fraction(&ModelState::Paused {
                done: 25,
                total: 100
            }),
            Some(0.25)
        );
        assert_eq!(
            progress_fraction(&ModelState::Downloading { done: 5, total: 0 }),
            None
        );
        assert_eq!(progress_fraction(&ModelState::Installed), None);
        assert_eq!(
            progress_fraction(&ModelState::Downloading {
                done: 500,
                total: 100
            }),
            Some(1.0)
        );
    }

    #[test]
    fn disk_usage_counts_present_files() {
        let v = vec![
            status(ModelState::Installed, true),
            status(
                ModelState::Paused {
                    done: 10,
                    total: 100,
                },
                false,
            ),
            status(ModelState::NotInstalled, false),
        ];
        assert_eq!(disk_used(&v), 487_601_967 + 10);
    }

    #[test]
    fn speed_and_eta() {
        let mut m = SpeedMeter::default();
        m.sample(0, 0);
        m.sample(1000, 1_000_000);
        assert!((m.bytes_per_sec() - 1_000_000.0).abs() < 1.0);
        assert_eq!(m.eta_secs(1_000_000, 11_000_000), Some(10));
        m.sample(2000, 3_000_000);
        assert!(m.bytes_per_sec() > 1_000_000.0 && m.bytes_per_sec() < 2_000_000.0);
        m.sample(3000, 0); // restart from zero
        assert_eq!(m.bytes_per_sec(), 0.0);
        assert_eq!(m.eta_secs(0, 100), None);
    }

    #[test]
    fn eta_formats() {
        assert_eq!(format_eta(45), "45 s");
        assert_eq!(format_eta(61), "2 min");
        assert_eq!(format_eta(3700), "1 h 2 min");
    }

    #[test]
    fn progress_text_without_speed() {
        let m = SpeedMeter::default();
        let t = progress_text(500_000_000, 1_000_000_000, &m);
        assert!(t.contains(" / "), "{t}");
        assert!(!t.contains("left"));
        let mut fast = SpeedMeter::default();
        fast.sample(0, 0);
        fast.sample(1000, 10_000_000);
        assert!(progress_text(10_000_000, 1_000_000_000, &fast).contains("left"));
    }
}
