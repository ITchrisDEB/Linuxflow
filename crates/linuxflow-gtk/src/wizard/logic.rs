//! Pure logic of the first-launch wizard.

use crate::hub::labels::format_size;
use linuxflow_core::domain::{Capabilities, ModelStatus};
use linuxflow_i18n::{tr, tr_fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Model,
    Shortcut,
    Summary,
}

impl Step {
    pub const COUNT: usize = 4;

    pub fn index(self) -> usize {
        match self {
            Step::Welcome => 0,
            Step::Model => 1,
            Step::Shortcut => 2,
            Step::Summary => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::Model => "model",
            Step::Shortcut => "shortcut",
            Step::Summary => "summary",
        }
    }

    pub fn next(self) -> Option<Step> {
        match self {
            Step::Welcome => Some(Step::Model),
            Step::Model => Some(Step::Shortcut),
            Step::Shortcut => Some(Step::Summary),
            Step::Summary => None,
        }
    }

    pub fn back(self) -> Option<Step> {
        match self {
            Step::Welcome => None,
            Step::Model => Some(Step::Welcome),
            Step::Shortcut => Some(Step::Model),
            Step::Summary => Some(Step::Shortcut),
        }
    }
}

/// User choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices {
    pub step: Step,
    pub model: Option<String>,
    /// Download nothing for now.
    pub skip_model: bool,
    /// A local file has already been given.
    pub local_file: bool,
}

impl Default for Choices {
    fn default() -> Self {
        Self {
            step: Step::Welcome,
            model: None,
            skip_model: false,
            local_file: false,
        }
    }
}

impl Choices {
    pub fn can_next(&self) -> bool {
        match self.step {
            Step::Model => self.skip_model || self.local_file || self.model.is_some(),
            Step::Summary => false,
            _ => true,
        }
    }

    pub fn go_next(&mut self) {
        if self.can_next() {
            if let Some(n) = self.step.next() {
                self.step = n;
            }
        }
    }

    pub fn go_back(&mut self) {
        if let Some(b) = self.step.back() {
            self.step = b;
        }
    }

    /// Model to pass to `complete_first_run`.
    pub fn model_for_finish(&self) -> Option<&str> {
        if self.skip_model || self.local_file {
            None
        } else {
            self.model.as_deref()
        }
    }
}

/// Model recommended for the machine (otherwise the lightest).
pub fn recommended(models: &[ModelStatus]) -> Option<&ModelStatus> {
    models
        .iter()
        .find(|m| m.recommended)
        .or_else(|| models.iter().min_by_key(|m| m.download_bytes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Recommended,
    Lighter,
    MoreAccurate,
}

impl Tag {
    pub fn label(self) -> String {
        match self {
            Tag::Recommended => tr("Recommended"),
            Tag::Lighter => tr("Lighter"),
            Tag::MoreAccurate => tr("More accurate"),
        }
    }
}

/// Label of a model relative to the recommended one.
pub fn tag_for(m: &ModelStatus, rec: Option<&ModelStatus>) -> Option<Tag> {
    let rec = rec?;
    if m.id == rec.id {
        Some(Tag::Recommended)
    } else if m.download_bytes < rec.download_bytes {
        Some(Tag::Lighter)
    } else if m.download_bytes > rec.download_bytes {
        Some(Tag::MoreAccurate)
    } else {
        None
    }
}

/// Models sorted from the lightest to the heaviest.
pub fn sorted(models: &[ModelStatus]) -> Vec<ModelStatus> {
    let mut v = models.to_vec();
    v.sort_by_key(|m| m.download_bytes);
    v
}

/// Text of the detected hardware.
pub fn hardware_text(c: &Capabilities) -> String {
    if c.vulkan_available && !c.vulkan_devices.is_empty() {
        tr_fmt(
            "Graphics card detected: {}. A more accurate model can run on it.",
            &[&c.vulkan_devices.join(", ")],
        )
    } else {
        tr("No usable graphics card detected. A lighter model that runs on the processor is recommended.")
    }
}

/// Disk space: text and sufficiency (`None` = available space unknown).
pub fn disk_text(required: u64, available: Option<u64>) -> (String, bool) {
    match available {
        Some(a) => (
            tr_fmt(
                "Space required: {} · available: {}",
                &[&format_size(required), &format_size(a)],
            ),
            a >= required.saturating_add(required / 10),
        ),
        None => (
            tr_fmt("Space required: {}", &[&format_size(required)]),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxflow_core::domain::ModelState;

    fn model(id: &str, size: u64, rec: bool) -> ModelStatus {
        ModelStatus {
            id: id.into(),
            name: id.into(),
            download_bytes: size,
            disk_bytes: size,
            memory_bytes: size * 2,
            state: ModelState::NotInstalled,
            recommended: rec,
            in_use: false,
        }
    }

    #[test]
    fn navigation_requires_a_model_choice() {
        let mut c = Choices::default();
        c.go_next();
        assert_eq!(c.step, Step::Model);
        c.go_next();
        assert_eq!(c.step, Step::Model, "no model chosen");
        c.model = Some("small".into());
        c.go_next();
        assert_eq!(c.step, Step::Shortcut);
        c.go_next();
        assert_eq!(c.step, Step::Summary);
        assert!(!c.can_next());
        c.go_back();
        assert_eq!(c.step, Step::Shortcut);
    }

    #[test]
    fn skipping_or_local_file_unblocks_and_sends_no_model() {
        let mut c = Choices {
            step: Step::Model,
            model: Some("small".into()),
            ..Choices::default()
        };
        assert_eq!(c.model_for_finish(), Some("small"));
        c.skip_model = true;
        assert_eq!(c.model_for_finish(), None);
        let c2 = Choices {
            step: Step::Model,
            local_file: true,
            ..Choices::default()
        };
        assert!(c2.can_next());
        assert_eq!(c2.model_for_finish(), None);
    }

    #[test]
    fn recommendation_and_tags() {
        let v = vec![
            model("big", 3000, false),
            model("small", 500, true),
            model("mid", 1000, false),
        ];
        let rec = recommended(&v);
        assert_eq!(rec.map(|m| m.id.as_str()), Some("small"));
        assert_eq!(tag_for(&v[1], rec), Some(Tag::Recommended));
        assert_eq!(tag_for(&v[0], rec), Some(Tag::MoreAccurate));
        let rec2 = Some(&v[2]);
        assert_eq!(tag_for(&v[1], rec2), Some(Tag::Lighter));
        assert_eq!(
            sorted(&v).iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["small", "mid", "big"]
        );
        // Without a recommendation: the lightest.
        let none = vec![model("a", 900, false), model("b", 100, false)];
        assert_eq!(recommended(&none).map(|m| m.id.as_str()), Some("b"));
        assert!(recommended(&[]).is_none());
    }

    #[test]
    fn disk_and_hardware_texts() {
        let (t, ok) = disk_text(1_000_000_000, Some(5_000_000_000));
        assert!(ok && t.contains("available"), "{t}");
        assert!(
            !disk_text(1_000_000_000, Some(1_000_000_000)).1,
            "10 % margin required"
        );
        assert!(disk_text(1, None).1);
        let gpu = Capabilities {
            vulkan_available: true,
            vulkan_devices: vec!["Card X".into()],
            ..Capabilities::default()
        };
        assert!(hardware_text(&gpu).contains("Card X"));
        assert!(hardware_text(&Capabilities::default()).contains("No usable"));
    }

    #[test]
    fn step_indices_are_consistent() {
        let mut s = Step::Welcome;
        let mut n = 1;
        while let Some(x) = s.next() {
            assert_eq!(x.index(), n);
            s = x;
            n += 1;
        }
        assert_eq!(n, Step::COUNT);
    }
}
