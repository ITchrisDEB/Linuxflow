//! Settings page: shortcut, mode, transcription (language, microphone, switches), local engine.

use crate::hub::ctx::{run_blocking, Ctx};
use crate::hub::labels::{engine_device_text, engine_state_text, format_latency_ms};
use crate::hub::widgets::{button, card, label, page_box, page_head, scrolled_page, set_label};
use crate::hub::PageId;
use crate::shortcut_recorder::ShortcutRecorderWidget;
use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_core::constants::APP_NAME;
use linuxflow_core::domain::{AppState, MicBoost, MicrophoneInfo, ShortcutMode};
use linuxflow_i18n::{tr, tr_fmt};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Common Whisper languages: code then translated label. `auto` = automatic detection.
pub fn languages() -> Vec<(&'static str, String)> {
    vec![
        ("auto", tr("Automatic detection")),
        ("en", tr("English")),
        ("fr", tr("French")),
        ("de", tr("German")),
        ("es", tr("Spanish")),
        ("it", tr("Italian")),
        ("pt", tr("Portuguese")),
        ("nl", tr("Dutch")),
        ("pl", tr("Polish")),
        ("ru", tr("Russian")),
        ("uk", tr("Ukrainian")),
        ("tr", tr("Turkish")),
        ("ar", tr("Arabic")),
        ("he", tr("Hebrew")),
        ("hi", tr("Hindi")),
        ("zh", tr("Chinese")),
        ("ja", tr("Japanese")),
        ("ko", tr("Korean")),
        ("vi", tr("Vietnamese")),
        ("th", tr("Thai")),
        ("id", tr("Indonesian")),
        ("sv", tr("Swedish")),
        ("da", tr("Danish")),
        ("no", tr("Norwegian")),
        ("fi", tr("Finnish")),
        ("cs", tr("Czech")),
        ("el", tr("Greek")),
        ("hu", tr("Hungarian")),
        ("ro", tr("Romanian")),
    ]
}

/// Offered interface languages: setting code (`None` = automatic) and position.
pub const UI_LANGUAGES: [Option<&str>; 3] = [None, Some("en"), Some("fr")];

/// Position in the selector for the value of the `ui_language` setting (`None`/`auto` = automatic;
/// `fr_FR` → French; unknown value → automatic).
pub fn ui_language_index(setting: Option<&str>) -> u32 {
    let code = setting.map(|s| s.trim().to_lowercase()).unwrap_or_default();
    let base = code.split(['_', '-', '.']).next().unwrap_or("");
    UI_LANGUAGES
        .iter()
        .position(|l| l.is_some_and(|l| l == base))
        .unwrap_or(0) as u32
}

/// Value of the setting for a position of the selector (`auto`, `en`, `fr`).
pub fn ui_language_setting(index: u32) -> &'static str {
    UI_LANGUAGES
        .get(index as usize)
        .copied()
        .flatten()
        .unwrap_or("auto")
}

/// Position in the "Weak microphone" selector for the value of the setting.
pub fn mic_boost_index(mode: MicBoost) -> u32 {
    MicBoost::ALL.iter().position(|m| *m == mode).unwrap_or(0) as u32
}

/// Value of the setting for a position of the selector (`off`, `auto`, `low`, `medium`, `high`).
pub fn mic_boost_setting(index: u32) -> &'static str {
    MicBoost::ALL
        .get(index as usize)
        .copied()
        .unwrap_or(MicBoost::Off)
        .setting()
}

/// Entries of the microphone selector: "default microphone" first, no duplicate.
pub fn microphone_entries(mics: &[MicrophoneInfo]) -> Vec<(String, String)> {
    let mut out = vec![("default".to_string(), tr("Default microphone"))];
    for m in mics {
        if m.id != "default" && !out.iter().any(|(id, _)| *id == m.id) {
            out.push((
                m.id.clone(),
                if m.label.is_empty() {
                    m.id.clone()
                } else {
                    m.label.clone()
                },
            ));
        }
    }
    out
}

struct EngineCells {
    state: gtk::Label,
    device: gtk::Label,
    model: gtk::Label,
    precision: gtk::Label,
    latency: gtk::Label,
    load: gtk::Label,
}

pub struct SettingsPage {
    root: gtk::ScrolledWindow,
    ctx: Ctx,
    recorder: Rc<ShortcutRecorderWidget>,
    toggle: gtk::CheckButton,
    ptt: gtk::CheckButton,
    ptt_hint: gtk::Label,
    language: gtk::DropDown,
    languages: RefCell<Vec<(String, String)>>,
    mic: gtk::DropDown,
    mics: RefCell<Vec<(String, String)>>,
    autopaste: adw::SwitchRow,
    login: adw::SwitchRow,
    history: adw::SwitchRow,
    mic_boost: adw::ComboRow,
    ui_language: adw::ComboRow,
    engine: EngineCells,
    engine_error: gtk::Label,
    cache: gtk::Label,
    version: gtk::Label,
    updating: Cell<bool>,
    wanted_mic: RefCell<String>,
}

fn field(title: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.append(&label(title, &["lf-field-label"]));
    b.append(widget);
    b
}

fn engine_cell(title: &str) -> (gtk::Box, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_hexpand(true);
    b.append(&label(title, &["lf-k"]));
    let v = label("—", &["lf-v"]);
    b.append(&v);
    (b, v)
}

fn radio_text(title: &str, sub: &str) -> (gtk::Box, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.append(&label(title, &["lf-radio-title"]));
    let s = label(sub, &["lf-radio-sub"]);
    b.append(&s);
    (b, s)
}

impl SettingsPage {
    pub fn new(ctx: &Ctx, navigate: Rc<dyn Fn(PageId)>, request_rebuild: Rc<dyn Fn()>) -> Rc<Self> {
        let page = page_box();
        let (head, _, _) = page_head(
            &tr("Settings"),
            &tr("Shortcut, mode, language, pasting and microphone."),
        );
        page.append(&head);

        // --- Raccourci
        let sc = card();
        sc.append(&label(&tr("Shortcut"), &["lf-h2"]));
        let recorder = ShortcutRecorderWidget::new(ctx);
        sc.append(recorder.widget());
        let modes = gtk::Box::new(gtk::Orientation::Vertical, 6);
        modes.set_margin_top(14);
        let toggle = gtk::CheckButton::new();
        let (tb, _) = radio_text(
            &tr("Press to start / stop"),
            &tr("Hands-free: one press starts listening, another ends it."),
        );
        toggle.set_child(Some(&tb));
        let ptt = gtk::CheckButton::new();
        ptt.set_group(Some(&toggle));
        let (pb, ptt_hint) = radio_text(
            &tr("Hold to talk"),
            &tr("Keep the key held while you speak; a short press switches to hands-free."),
        );
        ptt.set_child(Some(&pb));
        modes.append(&toggle);
        modes.append(&ptt);
        sc.append(&modes);
        page.append(&sc);

        // --- Transcription
        let tc = card();
        tc.append(&label(&tr("Transcription"), &["lf-h2"]));
        let langs = languages();
        let names: Vec<&str> = langs.iter().map(|(_, n)| n.as_str()).collect();
        let language = gtk::DropDown::from_strings(&names);
        language.set_hexpand(true);
        let mic = gtk::DropDown::from_strings(&[&tr("Default microphone")]);
        mic.set_hexpand(true);
        let grid = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        grid.set_homogeneous(true);
        grid.set_margin_top(10);
        grid.append(&field(&tr("Language"), &language));
        grid.append(&field(&tr("Microphone"), &mic));
        tc.append(&grid);
        let autopaste = adw::SwitchRow::new();
        autopaste.set_title(&tr(
            "Paste the text automatically into the active application",
        ));
        let login = adw::SwitchRow::new();
        login.set_title(&tr_fmt("Launch {} when I log in", &[APP_NAME]));
        let history = adw::SwitchRow::new();
        history.set_title(&tr("Keep a history of my dictations"));
        history.set_subtitle(&tr(
            "Saved only on this computer. Turn off to stop saving new dictations.",
        ));
        let sw = gtk::ListBox::new();
        sw.set_selection_mode(gtk::SelectionMode::None);
        sw.add_css_class("boxed-list");
        sw.add_css_class("lf-switches");
        sw.set_margin_top(14);
        sw.append(&autopaste);
        sw.append(&login);
        sw.append(&history);
        let mic_boost = adw::ComboRow::new();
        mic_boost.set_title(&tr("Weak microphone"));
        mic_boost.set_subtitle(&tr(
            "Raises the volume of a voice that is too quiet before transcription. Automatic adapts the boost to each recording.",
        ));
        let (off, automatic) = (tr("Off"), tr("Automatic"));
        let (low, medium, high) = (
            tr("Low (+6 dB)"),
            tr("Medium (+12 dB)"),
            tr("High (+20 dB)"),
        );
        mic_boost.set_model(Some(&gtk::StringList::new(&[
            off.as_str(),
            automatic.as_str(),
            low.as_str(),
            medium.as_str(),
            high.as_str(),
        ])));
        sw.append(&mic_boost);
        tc.append(&sw);
        page.append(&tc);

        // --- Interface language (language names are proper nouns: never translated)
        let ic = card();
        ic.append(&label(&tr("Interface"), &["lf-h2"]));
        let ui_language = adw::ComboRow::new();
        ui_language.set_title(&tr("Interface language"));
        let automatic = tr("Automatic");
        ui_language.set_model(Some(&gtk::StringList::new(&[
            automatic.as_str(),
            "English",
            "Français",
        ])));
        let ui_list = gtk::ListBox::new();
        ui_list.set_selection_mode(gtk::SelectionMode::None);
        ui_list.add_css_class("boxed-list");
        ui_list.append(&ui_language);
        ic.append(&ui_list);
        page.append(&ic);

        // --- Moteur local
        let ec = card();
        ec.append(&label(&tr("Local engine"), &["lf-h2"]));
        let g = gtk::Grid::new();
        g.set_column_spacing(18);
        g.set_row_spacing(12);
        g.set_column_homogeneous(true);
        let (c1, state) = engine_cell(&tr("State"));
        let (c2, device) = engine_cell(&tr("Hardware"));
        let (c3, model) = engine_cell(&tr("Model"));
        let (c4, precision) = engine_cell(&tr("Precision"));
        let (c5, latency) = engine_cell(&tr("Average latency"));
        let (c6, load) = engine_cell(&tr("Load time"));
        for (i, c) in [c1, c2, c3, c4, c5, c6].iter().enumerate() {
            g.attach(c, (i % 3) as i32, (i / 3) as i32, 1, 1);
        }
        ec.append(&g);
        let engine_error = label("", &["lf-hint", "bad"]);
        ec.append(&engine_error);
        let cache = label("", &["lf-hint"]);
        ec.append(&cache);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_margin_top(12);
        let models_b = button(&tr("Manage models"), &["lf-btn"]);
        row.append(&models_b);
        ec.append(&row);
        page.append(&ec);
        let version = label("", &["lf-hint"]);
        page.append(&version);

        let this = Rc::new(Self {
            root: scrolled_page(&page),
            ctx: ctx.clone(),
            recorder,
            toggle,
            ptt,
            ptt_hint,
            language,
            languages: RefCell::new(
                langs
                    .iter()
                    .map(|(c, n)| (c.to_string(), n.clone()))
                    .collect(),
            ),
            mic,
            mics: RefCell::new(vec![("default".into(), tr("Default microphone"))]),
            autopaste,
            login,
            history,
            mic_boost,
            ui_language,
            engine: EngineCells {
                state,
                device,
                model,
                precision,
                latency,
                load,
            },
            engine_error,
            cache,
            version,
            updating: Cell::new(false),
            wanted_mic: RefCell::new("default".into()),
        });
        this.wire(&models_b, navigate, request_rebuild);
        this
    }

    fn patch(&self, patch: serde_json::Value) {
        if self.updating.get() {
            return;
        }
        if let Err(e) = self.ctx.controller.set_settings(patch) {
            self.ctx.toast.show(&e.to_string());
        }
    }

    fn wire(
        self: &Rc<Self>,
        models_b: &gtk::Button,
        navigate: Rc<dyn Fn(PageId)>,
        request_rebuild: Rc<dyn Fn()>,
    ) {
        let weak = Rc::downgrade(self);
        macro_rules! with {
            ($t:ident => $body:expr) => {{
                let weak = weak.clone();
                move |_| {
                    if let Some($t) = weak.upgrade() {
                        $body
                    }
                }
            }};
        }
        self.toggle.connect_toggled(
            with!(t => if t.toggle.is_active() { t.patch(serde_json::json!({"mode": "toggle"})) }),
        );
        self.ptt.connect_toggled(
            with!(t => if t.ptt.is_active() { t.patch(serde_json::json!({"mode": "ptt"})) }),
        );
        self.language.connect_selected_notify(with!(t => {
            let idx = t.language.selected() as usize;
            let code = t.languages.borrow().get(idx).map(|(c, _)| c.clone());
            if let Some(code) = code {
                t.patch(serde_json::json!({"language": code}));
            }
        }));
        self.mic.connect_selected_notify(with!(t => {
            let idx = t.mic.selected() as usize;
            let id = t.mics.borrow().get(idx).map(|(i, _)| i.clone());
            if let Some(id) = id {
                *t.wanted_mic.borrow_mut() = id.clone();
                t.patch(serde_json::json!({"microphoneId": id}));
            }
        }));
        self.autopaste.connect_active_notify(
            with!(t => t.patch(serde_json::json!({"autoPaste": t.autopaste.is_active()}))),
        );
        self.login.connect_active_notify(
            with!(t => t.patch(serde_json::json!({"launchAtLogin": t.login.is_active()}))),
        );
        self.history.connect_active_notify(
            with!(t => t.patch(serde_json::json!({"historyEnabled": t.history.is_active()}))),
        );
        self.mic_boost.connect_selected_notify(with!(t => {
            if t.updating.get() {
                return;
            }
            let setting = mic_boost_setting(t.mic_boost.selected());
            t.patch(serde_json::json!({"micBoost": setting}));
        }));
        self.ui_language.connect_selected_notify(with!(t => {
            if t.updating.get() {
                return;
            }
            let setting = ui_language_setting(t.ui_language.selected());
            t.patch(serde_json::json!({"uiLanguage": setting}));
            linuxflow_i18n::set_language(if setting == "auto" { None } else { Some(setting) });
            request_rebuild();
        }));
        models_b.connect_clicked(move |_| navigate(PageId::Models));
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn recorder(&self) -> &Rc<ShortcutRecorderWidget> {
        &self.recorder
    }

    /// Reloads the microphone list (off the GTK thread).
    pub fn refresh_microphones(self: &Rc<Self>) {
        let controller = self.ctx.controller.clone();
        let weak = Rc::downgrade(self);
        run_blocking(
            move || controller.list_microphones(),
            move |mics| {
                if let Some(t) = weak.upgrade() {
                    t.set_microphones(&mics);
                }
            },
        );
    }

    pub fn set_microphones(&self, mics: &[MicrophoneInfo]) {
        let entries = microphone_entries(mics);
        if *self.mics.borrow() == entries {
            return;
        }
        self.updating.set(true);
        let names: Vec<&str> = entries.iter().map(|(_, l)| l.as_str()).collect();
        self.mic.set_model(Some(&gtk::StringList::new(&names)));
        *self.mics.borrow_mut() = entries;
        self.select_mic();
        self.updating.set(false);
    }

    fn select_mic(&self) {
        let wanted = self.wanted_mic.borrow().clone();
        let idx = self
            .mics
            .borrow()
            .iter()
            .position(|(i, _)| *i == wanted)
            .unwrap_or(0);
        self.mic.set_selected(idx as u32);
    }

    /// Selects an interface language (like a user click; tests).
    pub fn select_ui_language(&self, index: u32) {
        self.ui_language.set_selected(index);
    }

    /// Chooses a weak-microphone boost (like a user click; tests).
    pub fn select_mic_boost(&self, index: u32) {
        self.mic_boost.set_selected(index);
    }

    /// Displayed position of the "Weak microphone" selector.
    pub fn mic_boost_selected(&self) -> u32 {
        self.mic_boost.selected()
    }

    /// Toggles the "keep a history" switch (like a user click; tests).
    pub fn set_history_enabled(&self, on: bool) {
        self.history.set_active(on);
    }

    /// Displayed state of the "keep a history" switch.
    pub fn history_enabled(&self) -> bool {
        self.history.is_active()
    }

    /// Leaves the page or hides the hub: the shortcut recording is cancelled.
    pub fn deactivate(&self) {
        self.recorder.cancel();
    }

    pub fn update(&self, state: &AppState) {
        self.updating.set(true);
        let s = &state.settings;
        self.recorder
            .set_desktop_trigger(state.shortcut_desktop_trigger.as_deref());
        self.recorder.set_failure(state.shortcut_message.as_deref());
        self.recorder.set_current(&s.shortcut, state.shortcut_ok);
        match s.mode {
            ShortcutMode::Toggle => self.toggle.set_active(true),
            ShortcutMode::Ptt => self.ptt.set_active(true),
        }
        self.ptt.set_sensitive(state.ptt_available);
        set_label(
            &self.ptt_hint,
            &if state.ptt_available {
                tr("Keep the key held while you speak; a short press switches to hands-free.")
            } else {
                tr("Unavailable: key releases cannot be read on this system (see the Troubleshooting page).")
            },
        );
        // Language: a language unknown to the list is added as is.
        let pos = self
            .languages
            .borrow()
            .iter()
            .position(|(c, _)| *c == s.language);
        let idx = match pos {
            Some(i) => i,
            None => {
                let mut l = self.languages.borrow_mut();
                l.push((s.language.clone(), s.language.clone()));
                let names: Vec<&str> = l.iter().map(|(_, n)| n.as_str()).collect();
                self.language.set_model(Some(&gtk::StringList::new(&names)));
                l.len() - 1
            }
        };
        if self.language.selected() as usize != idx {
            self.language.set_selected(idx as u32);
        }
        *self.wanted_mic.borrow_mut() = s.microphone_id.clone();
        if !self
            .mics
            .borrow()
            .iter()
            .any(|(i, _)| *i == s.microphone_id)
        {
            let mut m = self.mics.borrow().clone();
            m.push((s.microphone_id.clone(), s.microphone_id.clone()));
            let names: Vec<&str> = m.iter().map(|(_, l)| l.as_str()).collect();
            self.mic.set_model(Some(&gtk::StringList::new(&names)));
            *self.mics.borrow_mut() = m;
        }
        self.select_mic();
        if self.autopaste.is_active() != s.auto_paste {
            self.autopaste.set_active(s.auto_paste);
        }
        if self.login.is_active() != s.launch_at_login {
            self.login.set_active(s.launch_at_login);
        }
        if self.history.is_active() != s.history_enabled {
            self.history.set_active(s.history_enabled);
        }

        let boost_idx = mic_boost_index(s.mic_boost);
        if self.mic_boost.selected() != boost_idx {
            self.mic_boost.set_selected(boost_idx);
        }

        let ui_idx = ui_language_index(s.ui_language.as_deref());
        if self.ui_language.selected() != ui_idx {
            self.ui_language.set_selected(ui_idx);
        }

        let eng = &state.engine;
        set_label(&self.engine.state, &engine_state_text(eng));
        set_label(&self.engine.device, &engine_device_text(eng));
        set_label(&self.engine.model, eng.model.as_deref().unwrap_or("—"));
        set_label(
            &self.engine.precision,
            eng.precision.as_deref().unwrap_or("—"),
        );
        set_label(
            &self.engine.latency,
            &format_latency_ms(state.stats.avg_latency_ms),
        );
        set_label(
            &self.engine.load,
            &eng.load_ms
                .map(|ms| format_latency_ms(ms as f64))
                .unwrap_or_else(|| "—".to_string()),
        );
        let err = eng.error.as_deref().filter(|_| !eng.ready).unwrap_or("");
        set_label(&self.engine_error, err);
        self.engine_error.set_visible(!err.is_empty());
        let dir = s
            .model_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| tr("default location"));
        set_label(&self.cache, &tr_fmt("Models folder: {}", &[&dir]));
        set_label(
            &self.version,
            &tr_fmt("{} version {}", &[APP_NAME, &state.version]),
        );
        self.updating.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_boost_mapping_round_trips() {
        for (i, m) in MicBoost::ALL.iter().enumerate() {
            assert_eq!(mic_boost_index(*m), i as u32);
            assert_eq!(mic_boost_setting(i as u32), m.setting());
        }
        assert_eq!(mic_boost_setting(99), "off");
    }

    #[test]
    fn ui_language_mapping() {
        assert_eq!(ui_language_index(None), 0);
        assert_eq!(ui_language_index(Some("auto")), 0);
        assert_eq!(ui_language_index(Some("en")), 1);
        assert_eq!(ui_language_index(Some("fr")), 2);
        assert_eq!(ui_language_index(Some("fr_FR.UTF-8")), 2);
        assert_eq!(ui_language_index(Some("FR-ca")), 2);
        assert_eq!(ui_language_index(Some("de")), 0);
        assert_eq!(ui_language_setting(0), "auto");
        assert_eq!(ui_language_setting(1), "en");
        assert_eq!(ui_language_setting(2), "fr");
        assert_eq!(ui_language_setting(9), "auto");
    }

    #[test]
    fn microphone_list_starts_with_default_and_has_no_duplicates() {
        let mics = vec![
            MicrophoneInfo {
                id: "default".into(),
                label: "x".into(),
            },
            MicrophoneInfo {
                id: "usb".into(),
                label: String::new(),
            },
            MicrophoneInfo {
                id: "usb".into(),
                label: "dup".into(),
            },
        ];
        let e = microphone_entries(&mics);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].0, "default");
        assert_eq!(e[1], ("usb".to_string(), "usb".to_string()));
    }
}
