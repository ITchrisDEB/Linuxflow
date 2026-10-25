//! GNOME: shortcut set by the application itself as a GNOME "custom shortcut"
//! (schema `org.gnome.settings-daemon.plugins.media-keys.custom-keybinding`, the one of
//! Settings > Keyboard > Custom Shortcuts).
//!
//! GNOME's GlobalShortcuts portal lets the user decide the key; here the application
//! chooses it, after checking that no GNOME action already uses it. On press, GNOME runs
//! `linuxflow --toggle`, which passes the command to the running instance. Limit: GNOME does not send the
//! key release, hence no push-to-talk this way (toggle only).

use super::backend::{BackendError, BackendErrorKind, BackendReport, HotkeyBackend, StartRequest};
use super::native::{canonical_gnome_binding, gnome_binding, gnome_command};
use super::select::BackendKind;
use gio::glib;
use gio::prelude::*;
use linuxflow_core::traits::HotkeySink;
use linuxflow_i18n::{tr, tr_fmt};
use std::sync::Arc;

const PARENT_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const LIST_KEY: &str = "custom-keybindings";
const BASE_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/";
/// Our entry: fixed path, found again at every launch.
const OUR_NAME: &str = "linuxflow";
/// GNOME schemas that define shortcuts; conflicts are looked for there.
const SYSTEM_SCHEMAS: [&str; 5] = [
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.shell.keybindings",
    "org.gnome.mutter.keybindings",
    "org.gnome.mutter.wayland.keybindings",
    PARENT_SCHEMA,
];

fn our_path() -> String {
    format!("{BASE_PATH}{OUR_NAME}/")
}

fn schema_exists(id: &str) -> bool {
    gio::SettingsSchemaSource::default().is_some_and(|s| s.lookup(id, true).is_some())
}

/// Bindings of a setting value (`s` or `as`).
fn bindings_of(value: &glib::Variant) -> Vec<String> {
    value
        .get::<Vec<String>>()
        .or_else(|| value.get::<String>().map(|s| vec![s]))
        .unwrap_or_default()
}

/// GNOME action that already uses `wanted` (canonical form), if there is one.
fn find_conflict(wanted: &str) -> Option<String> {
    let source = gio::SettingsSchemaSource::default()?;
    for id in SYSTEM_SCHEMAS {
        let Some(schema) = source.lookup(id, true) else {
            continue;
        };
        let settings = gio::Settings::new(id);
        for key in schema.list_keys() {
            let value = settings.value(&key);
            if bindings_of(&value)
                .iter()
                .any(|b| !b.is_empty() && canonical_gnome_binding(b) == wanted)
            {
                return Some(key.to_string());
            }
        }
    }
    // custom shortcuts of other applications
    let list = gio::Settings::new(PARENT_SCHEMA).strv(LIST_KEY);
    for path in list
        .iter()
        .map(|p| p.to_string())
        .filter(|p| *p != our_path())
    {
        let custom = gio::Settings::with_path(CUSTOM_SCHEMA, &path);
        if canonical_gnome_binding(custom.string("binding").as_str()) == wanted {
            let name = custom.string("name").to_string();
            return Some(if name.is_empty() { path } else { name });
        }
    }
    None
}

fn remove_ours() {
    let parent = gio::Settings::new(PARENT_SCHEMA);
    let path = our_path();
    let kept: Vec<String> = parent
        .strv(LIST_KEY)
        .iter()
        .map(|p| p.to_string())
        .filter(|p| *p != path)
        .collect();
    let refs: Vec<&str> = kept.iter().map(String::as_str).collect();
    let _ = parent.set_strv(LIST_KEY, refs.as_slice());
    let ours = gio::Settings::with_path(CUSTOM_SCHEMA, &path);
    for key in ["name", "command", "binding"] {
        ours.reset(key);
    }
    gio::Settings::sync();
}

/// Attempt WITHOUT writing anything: does a GNOME action already use this key?
pub fn probe(spec: &super::spec::HotkeySpec) -> Result<(), String> {
    if !schema_exists(PARENT_SCHEMA) {
        return Ok(());
    }
    let Some(binding) = gnome_binding(spec) else {
        return Ok(());
    };
    match find_conflict(&canonical_gnome_binding(&binding)) {
        Some(action) => Err(tr_fmt(
            "{} is already used by GNOME (“{}”). Choose another shortcut.",
            &[&spec.display(), &action],
        )),
        None => Ok(()),
    }
}

pub struct GnomeKeysBackend {
    active: bool,
}

impl GnomeKeysBackend {
    pub fn new() -> Self {
        Self { active: false }
    }
}

impl Default for GnomeKeysBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn err(kind: BackendErrorKind, msg: impl Into<String>) -> BackendError {
    BackendError::new(kind, msg)
}

impl HotkeyBackend for GnomeKeysBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::GnomeKeys
    }

    fn start(
        &mut self,
        req: &StartRequest,
        _sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError> {
        self.stop();
        if !schema_exists(PARENT_SCHEMA) || !schema_exists(CUSTOM_SCHEMA) {
            return Err(err(
                BackendErrorKind::Unavailable,
                tr("GNOME's custom shortcut settings are not available."),
            ));
        }
        let binding = gnome_binding(&req.spec).ok_or_else(|| {
            err(
                BackendErrorKind::Unavailable,
                tr("This shortcut cannot be used with GNOME's keyboard settings."),
            )
        })?;
        if let Some(action) = find_conflict(&canonical_gnome_binding(&binding)) {
            return Err(err(
                BackendErrorKind::Conflict,
                tr_fmt(
                    "{} is already used by GNOME (“{}”). Choose another shortcut.",
                    &[&req.spec.display(), &action],
                ),
            ));
        }
        let exe = crate::autostart::launch_exe()
            .map_err(|e| err(BackendErrorKind::Failed, e.to_string()))?;
        let command = gnome_command(&exe.to_string_lossy());
        let path = our_path();
        let ours = gio::Settings::with_path(CUSTOM_SCHEMA, &path);
        let failed = |e: glib::BoolError| err(BackendErrorKind::Failed, e.to_string());
        ours.set_string("name", linuxflow_core::constants::APP_NAME)
            .map_err(failed)?;
        ours.set_string("command", &command).map_err(failed)?;
        ours.set_string("binding", &binding).map_err(failed)?;
        let parent = gio::Settings::new(PARENT_SCHEMA);
        let mut list: Vec<String> = parent
            .strv(LIST_KEY)
            .iter()
            .map(|p| p.to_string())
            .collect();
        if !list.contains(&path) {
            list.push(path);
            let refs: Vec<&str> = list.iter().map(String::as_str).collect();
            parent.set_strv(LIST_KEY, refs.as_slice()).map_err(failed)?;
        }
        gio::Settings::sync();
        self.active = true;
        Ok(BackendReport {
            // GNOME runs a command: neither release nor events towards this process.
            supports_release: false,
            supports_modifier_only: false,
            trigger_description: req.spec.display(),
            provides_hotkey_events: false,
            provides_raw_keys: false,
            note: Some(tr(
                "GNOME starts \"linuxflow --toggle\" when you press the shortcut: press once to start, again to stop.",
            )),
        })
    }

    fn stop(&mut self) {
        if self.active {
            self.active = false;
            remove_ours();
        }
    }
}

impl Drop for GnomeKeysBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_entry_lives_under_the_custom_keybindings_path() {
        assert_eq!(
            our_path(),
            "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/linuxflow/"
        );
    }

    #[test]
    fn a_setting_value_is_read_as_a_list_whether_it_is_a_string_or_an_array() {
        use gio::glib::prelude::ToVariant;
        assert_eq!(bindings_of(&"<Alt>F4".to_variant()), vec!["<Alt>F4"]);
        assert_eq!(
            bindings_of(&vec!["<Super>d".to_string(), "<Primary><Alt>d".to_string()].to_variant()),
            vec!["<Super>d", "<Primary><Alt>d"]
        );
        assert!(bindings_of(&true.to_variant()).is_empty());
    }
}
