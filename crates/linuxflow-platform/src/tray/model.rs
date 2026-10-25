//! Model of the notification area: PURE functions (menu tree, labels, states).
//!
//! Nothing here depends on ksni or D-Bus: [`view`] builds the complete tree of entries, and
//! each entry carries the [`TrayCommand`] it emits. `mod.rs` only translates this tree
//! into dbusmenu entries. All the texts go through `linuxflow_i18n`.

use linuxflow_core::constants::APP_NAME;
use linuxflow_core::domain::{
    AppState, DeviceKind, EngineInfo, EnginePhase, MicrophoneInfo, Phase,
};
use linuxflow_core::events::Page;
use linuxflow_i18n::{format_number, format_percent, tr, tr_fmt};

/// Commands emitted by the icon towards the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    OpenHub,
    OpenPage(Page),
    ToggleDictation,
    CancelDictation,
    /// `auto` or Whisper language code.
    SetLanguage(String),
    /// Microphone identifier (`default` = system microphone).
    SetMicrophone(String),
    SetAutoPaste(bool),
    SetLaunchAtLogin(bool),
    About,
    Quit,
}

/// Everything the icon needs to know, pushed at every change (`TrayHandle::update`).
#[derive(Debug, Clone, PartialEq)]
pub struct TrayModel {
    pub phase: Phase,
    /// Engine; the download progress is in `engine.download`.
    pub engine: EngineInfo,
    /// Shortcut in the internal format (`Ctrl+Alt+Space`).
    pub shortcut: String,
    /// False if the registration of the shortcut failed.
    pub shortcut_ok: bool,
    /// Key assigned by the desktop (portal), shown in place of the setting.
    pub desktop_trigger: Option<String>,
    /// `language` setting: `auto` or Whisper code.
    pub language: String,
    /// `microphoneId` setting: `default` or device name.
    pub microphone_id: String,
    pub auto_paste: bool,
    pub launch_at_login: bool,
    pub microphones: Vec<MicrophoneInfo>,
}

impl Default for TrayModel {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            engine: EngineInfo {
                phase: EnginePhase::Starting,
                loading: true,
                ..EngineInfo::default()
            },
            shortcut: linuxflow_core::constants::DEFAULT_SHORTCUT.to_string(),
            shortcut_ok: true,
            desktop_trigger: None,
            language: "auto".into(),
            microphone_id: "default".into(),
            auto_paste: true,
            launch_at_login: false,
            microphones: Vec::new(),
        }
    }
}

impl TrayModel {
    /// Builds the model from the application state and the microphone list.
    pub fn from_app_state(state: &AppState, microphones: Vec<MicrophoneInfo>) -> Self {
        Self {
            phase: state.phase,
            engine: state.engine.clone(),
            shortcut: state.settings.shortcut.clone(),
            shortcut_ok: state.shortcut_ok,
            desktop_trigger: state.shortcut_desktop_trigger.clone(),
            language: state.settings.language.clone(),
            microphone_id: state.settings.microphone_id.clone(),
            auto_paste: state.settings.auto_paste,
            launch_at_login: state.settings.launch_at_login,
            microphones,
        }
    }
}

/// The three visual states of the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIconState {
    Loading,
    Ready,
    Recording,
}
/// Menu node, independent of ksni.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuNode {
    /// Clickable entry.
    Action {
        label: String,
        enabled: bool,
        command: TrayCommand,
    },
    /// Greyed-out informational label.
    Info(String),
    /// Check box; `command` is emitted on click (inverse value of `checked`).
    Check {
        label: String,
        checked: bool,
        enabled: bool,
        command: TrayCommand,
    },
    /// Group of radio buttons; `options[i].1` is emitted when option `i` is chosen.
    Radio {
        selected: usize,
        options: Vec<(String, TrayCommand)>,
    },
    SubMenu {
        label: String,
        enabled: bool,
        children: Vec<MenuNode>,
    },
    Separator,
}

/// Complete rendering computed from a [`TrayModel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    pub icon: TrayIconState,
    pub tooltip_title: String,
    pub tooltip_description: String,
    pub menu: Vec<MenuNode>,
}

/// Whisper languages offered in the menu (code, display order).
pub const LANGUAGES: [&str; 18] = [
    "en", "fr", "es", "de", "it", "pt", "nl", "ru", "ja", "zh", "ar", "pl", "tr", "uk", "sv", "cs",
    "ko", "hi",
];

/// Translated name of a language; an unknown code is returned as is.
pub fn language_name(code: &str) -> String {
    match code {
        "auto" => tr("Automatic"),
        "en" => tr("English"),
        "fr" => tr("French"),
        "es" => tr("Spanish"),
        "de" => tr("German"),
        "it" => tr("Italian"),
        "pt" => tr("Portuguese"),
        "nl" => tr("Dutch"),
        "ru" => tr("Russian"),
        "ja" => tr("Japanese"),
        "zh" => tr("Chinese"),
        "ar" => tr("Arabic"),
        "pl" => tr("Polish"),
        "tr" => tr("Turkish"),
        "uk" => tr("Ukrainian"),
        "sv" => tr("Swedish"),
        "cs" => tr("Czech"),
        "ko" => tr("Korean"),
        "hi" => tr("Hindi"),
        other => other.to_string(),
    }
}

/// `Ctrl+Alt+Space` becomes `Ctrl+Alt+Space` with the key names translated.
pub fn pretty_shortcut(accelerator: &str) -> String {
    accelerator
        .split('+')
        .filter(|k| !k.is_empty())
        .map(|key| match key {
            "Space" => tr("Space"),
            "Shift" => tr("Shift"),
            other => linuxflow_core::accelerator::numpad_label(other)
                .unwrap_or_else(|| other.to_string()),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Decimal gigabytes, one decimal, according to the interface language.
pub fn format_gb(bytes: u64) -> String {
    // Integer rounding at the decimal (no binary error), then localised formatting.
    let tenths = (u128::from(bytes) + 50_000_000) / 100_000_000;
    format_number(tenths as f64 / 10.0, 1)
}

/// Integer percentage capped at 99 while the download is not finished.
pub fn download_percent(info: &EngineInfo) -> Option<u8> {
    let d = match (&info.phase, &info.download) {
        (EnginePhase::Downloading, Some(d)) if d.total > 0 => d,
        _ => return None,
    };
    let pct = (u128::from(d.done) * 100 / u128::from(d.total)).min(99);
    u8::try_from(pct).ok()
}

fn percent_text(pct: u8) -> String {
    format_percent(f64::from(pct) / 100.0, 0)
}

/// True during a model download.
pub fn is_downloading(info: &EngineInfo) -> bool {
    info.phase == EnginePhase::Downloading && info.download.is_some()
}

/// Loading label: download in % (capped at 99), otherwise "Loading the model…".
///
/// `short` gives the short variant for small spaces.
pub fn loading_label(info: &EngineInfo, short: bool) -> String {
    let download = match (&info.phase, &info.download) {
        (EnginePhase::Downloading, Some(d)) => d,
        _ => return tr("Loading the model…"),
    };
    let Some(pct) = download_percent(info) else {
        return if short {
            tr("Downloading…")
        } else {
            tr_fmt(
                "Downloading the model · {} GB",
                &[&format_gb(download.done)],
            )
        };
    };
    let pct = percent_text(pct);
    if short {
        tr_fmt("Downloading · {}", &[&pct])
    } else {
        tr_fmt(
            "Downloading the model · {} ({} / {} GB)",
            &[&pct, &format_gb(download.done), &format_gb(download.total)],
        )
    }
}

/// "GPU · model" / "CPU · model" when ready, loading, error or "…".
pub fn engine_label(info: &EngineInfo) -> String {
    if info.ready {
        let device = match info.device {
            DeviceKind::Gpu => "GPU",
            DeviceKind::Cpu => "CPU",
        };
        match info.model.as_deref() {
            Some(model) if !model.is_empty() => tr_fmt("{} · {}", &[device, model]),
            _ => device.to_string(),
        }
    } else if info.loading || info.phase == EnginePhase::Downloading {
        loading_label(info, false)
    } else if has_error(info) {
        tr("Engine unavailable")
    } else {
        "…".to_string()
    }
}

fn has_error(info: &EngineInfo) -> bool {
    info.error.is_some() || info.phase == EnginePhase::Error
}

fn is_busy(phase: Phase) -> bool {
    matches!(phase, Phase::Transcribing | Phase::Formatting)
}

fn action(label: String, command: TrayCommand) -> MenuNode {
    MenuNode::Action {
        label,
        enabled: true,
        command,
    }
}

fn check(label: String, checked: bool, command: TrayCommand) -> MenuNode {
    MenuNode::Check {
        label,
        checked,
        enabled: true,
        command,
    }
}

fn language_menu(model: &TrayModel) -> MenuNode {
    let mut options: Vec<(String, TrayCommand)> = Vec::with_capacity(LANGUAGES.len() + 2);
    options.push((
        language_name("auto"),
        TrayCommand::SetLanguage("auto".into()),
    ));
    for code in LANGUAGES {
        options.push((language_name(code), TrayCommand::SetLanguage(code.into())));
    }
    let mut selected = options
        .iter()
        .position(|(_, c)| *c == TrayCommand::SetLanguage(model.language.clone()));
    if selected.is_none() && !model.language.is_empty() {
        // Code absent from the list (hand-edited): show it rather than lie.
        options.push((
            model.language.clone(),
            TrayCommand::SetLanguage(model.language.clone()),
        ));
        selected = Some(options.len() - 1);
    }
    MenuNode::SubMenu {
        label: tr("Language"),
        enabled: true,
        children: vec![MenuNode::Radio {
            selected: selected.unwrap_or(0),
            options,
        }],
    }
}

fn microphone_menu(model: &TrayModel) -> MenuNode {
    let mut options: Vec<(String, TrayCommand)> = vec![(
        tr("System default"),
        TrayCommand::SetMicrophone("default".into()),
    )];
    for mic in model.microphones.iter().filter(|m| m.id != "default") {
        options.push((
            mic.label.clone(),
            TrayCommand::SetMicrophone(mic.id.clone()),
        ));
    }
    let mut selected = options
        .iter()
        .position(|(_, c)| *c == TrayCommand::SetMicrophone(model.microphone_id.clone()));
    if selected.is_none() && !model.microphone_id.is_empty() {
        // Microphone chosen but unplugged: it is shown, marked as absent.
        options.push((
            tr_fmt("{} (not connected)", &[&model.microphone_id]),
            TrayCommand::SetMicrophone(model.microphone_id.clone()),
        ));
        selected = Some(options.len() - 1);
    }
    let mut children = vec![MenuNode::Radio {
        selected: selected.unwrap_or(0),
        options,
    }];
    if model.microphones.iter().all(|m| m.id == "default") {
        children.push(MenuNode::Separator);
        children.push(MenuNode::Info(tr("No microphone detected")));
    }
    MenuNode::SubMenu {
        label: tr("Microphone"),
        enabled: true,
        children,
    }
}

/// Computes icon, status, tooltip and menu.
pub fn view(model: &TrayModel) -> TrayView {
    let listening = model.phase == Phase::Listening;
    let busy = is_busy(model.phase);
    let ready = model.engine.ready;

    let icon = if listening {
        TrayIconState::Recording
    } else if ready {
        TrayIconState::Ready
    } else {
        TrayIconState::Loading
    };
    let engine = engine_label(&model.engine);
    let shortcut = model
        .desktop_trigger
        .clone()
        .unwrap_or_else(|| pretty_shortcut(&model.shortcut));
    let shortcut_line = if model.shortcut_ok {
        tr_fmt("Shortcut: {}", &[&shortcut])
    } else {
        tr_fmt("Shortcut: {} (unavailable)", &[&shortcut])
    };
    let state_text = if listening {
        tr("Listening…")
    } else if busy {
        tr("Transcribing…")
    } else {
        engine.clone()
    };

    let mut tooltip = vec![state_text, shortcut_line.clone()];
    if let Some(pct) = download_percent(&model.engine) {
        tooltip.push(tr_fmt("Downloading model… {}", &[&percent_text(pct)]));
    }

    let mut menu = vec![MenuNode::Info(tr_fmt("{} — {}", &[APP_NAME, &engine]))];
    if is_downloading(&model.engine) {
        menu.push(MenuNode::Info(match download_percent(&model.engine) {
            Some(pct) => tr_fmt("Downloading model… {}", &[&percent_text(pct)]),
            None => tr("Downloading model…"),
        }));
        menu.push(action(
            tr("Open models page"),
            TrayCommand::OpenPage(Page::Models),
        ));
    }
    menu.push(MenuNode::Action {
        label: if listening {
            tr("Stop dictation")
        } else {
            tr("Start dictation (hands-free)")
        },
        enabled: (ready && !busy) || listening,
        command: TrayCommand::ToggleDictation,
    });
    if listening || busy {
        menu.push(action(tr("Cancel dictation"), TrayCommand::CancelDictation));
    }
    menu.push(MenuNode::Info(shortcut_line));
    menu.push(MenuNode::Separator);
    menu.push(action(tr_fmt("Open {}", &[APP_NAME]), TrayCommand::OpenHub));
    menu.push(MenuNode::Separator);
    menu.push(language_menu(model));
    menu.push(microphone_menu(model));
    menu.push(check(
        tr("Paste automatically"),
        model.auto_paste,
        TrayCommand::SetAutoPaste(!model.auto_paste),
    ));
    menu.push(check(
        tr("Launch at login"),
        model.launch_at_login,
        TrayCommand::SetLaunchAtLogin(!model.launch_at_login),
    ));
    menu.push(MenuNode::Separator);
    menu.push(action(tr_fmt("About {}", &[APP_NAME]), TrayCommand::About));
    menu.push(action(tr("Quit"), TrayCommand::Quit));

    TrayView {
        icon,
        tooltip_title: APP_NAME.to_string(),
        tooltip_description: tooltip.join("\n"),
        menu,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use linuxflow_core::domain::DownloadProgress;

    fn downloading(done: u64, total: u64) -> EngineInfo {
        EngineInfo {
            phase: EnginePhase::Downloading,
            loading: true,
            download: Some(DownloadProgress {
                model: "m".into(),
                done,
                total,
            }),
            ..EngineInfo::default()
        }
    }

    fn ready_engine() -> EngineInfo {
        EngineInfo {
            phase: EnginePhase::Ready,
            ready: true,
            device: DeviceKind::Gpu,
            model: Some("small".into()),
            precision: Some("q5_0".into()),
            ..EngineInfo::default()
        }
    }

    fn model(phase: Phase, engine: EngineInfo) -> TrayModel {
        TrayModel {
            phase,
            engine,
            microphones: vec![
                MicrophoneInfo {
                    id: "default".into(),
                    label: "Default".into(),
                },
                MicrophoneInfo {
                    id: "usb".into(),
                    label: "USB Mic".into(),
                },
            ],
            ..TrayModel::default()
        }
    }

    /// First node (depth-first) satisfying the predicate.
    fn find<'a>(nodes: &'a [MenuNode], pred: &dyn Fn(&MenuNode) -> bool) -> Option<&'a MenuNode> {
        for n in nodes {
            if pred(n) {
                return Some(n);
            }
            if let MenuNode::SubMenu { children, .. } = n {
                if let Some(f) = find(children, pred) {
                    return Some(f);
                }
            }
        }
        None
    }

    fn action_of<'a>(v: &'a TrayView, cmd: &TrayCommand) -> Option<(&'a str, bool)> {
        match find(
            &v.menu,
            &|n| matches!(n, MenuNode::Action { command, .. } if command == cmd),
        ) {
            Some(MenuNode::Action { label, enabled, .. }) => Some((label.as_str(), *enabled)),
            _ => None,
        }
    }

    fn top_labels(v: &TrayView) -> Vec<String> {
        v.menu
            .iter()
            .map(|n| match n {
                MenuNode::Action { label, .. } | MenuNode::Info(label) => label.clone(),
                MenuNode::Check { label, .. } | MenuNode::SubMenu { label, .. } => label.clone(),
                MenuNode::Radio { .. } => "<radio>".into(),
                MenuNode::Separator => "---".into(),
            })
            .collect()
    }

    #[test]
    fn loading_label_with_percentage() {
        let info = downloading(1_300_000_000, 3_100_000_000);
        assert_eq!(
            loading_label(&info, false),
            "Downloading the model · 41% (1.3 / 3.1 GB)"
        );
        assert_eq!(loading_label(&info, true), "Downloading · 41%");
    }

    #[test]
    fn percentage_is_clamped_to_99() {
        for (done, expect) in [
            (0, 0),
            (1_302_000_000, 42),
            (3_069_000_000, 99),
            (3_100_000_000, 99),
            (9_000_000_000, 99),
        ] {
            assert_eq!(
                download_percent(&downloading(done, 3_100_000_000)),
                Some(expect),
                "{done}"
            );
        }
        assert_eq!(download_percent(&downloading(5, 0)), None);
        assert_eq!(download_percent(&ready_engine()), None);
    }

    #[test]
    fn loading_label_with_unknown_total() {
        let info = downloading(500_000_000, 0);
        assert_eq!(
            loading_label(&info, false),
            "Downloading the model · 0.5 GB"
        );
        assert_eq!(loading_label(&info, true), "Downloading…");
    }

    #[test]
    fn loading_label_without_download() {
        let loading = EngineInfo {
            phase: EnginePhase::Loading,
            loading: true,
            ..EngineInfo::default()
        };
        assert_eq!(loading_label(&loading, false), "Loading the model…");
        let no_progress = EngineInfo {
            phase: EnginePhase::Downloading,
            ..EngineInfo::default()
        };
        assert_eq!(loading_label(&no_progress, true), "Loading the model…");
        let mut stale = downloading(1, 2);
        stale.phase = EnginePhase::Loading;
        assert_eq!(loading_label(&stale, false), "Loading the model…");
    }

    #[test]
    fn gigabytes_round_half_up() {
        assert_eq!(format_gb(1_300_000_000), "1.3");
        assert_eq!(format_gb(1_950_000_000), "2.0");
        assert_eq!(format_gb(0), "0.0");
    }

    #[test]
    fn pretty_shortcut_names_numpad_keys() {
        assert_eq!(pretty_shortcut("Ctrl+numadd"), "Ctrl+Num+");
    }

    #[test]
    fn pretty_shortcut_joins_keys() {
        assert_eq!(pretty_shortcut("Ctrl+Alt+Space"), "Ctrl+Alt+Space");
        assert_eq!(pretty_shortcut("Super+Shift+D"), "Super+Shift+D");
        assert_eq!(pretty_shortcut(""), "");
    }

    #[test]
    fn engine_labels() {
        assert_eq!(engine_label(&ready_engine()), "GPU · small");
        let cpu = EngineInfo {
            device: DeviceKind::Cpu,
            ..ready_engine()
        };
        assert_eq!(engine_label(&cpu), "CPU · small");
        let failed = EngineInfo {
            phase: EnginePhase::Error,
            error: Some("x".into()),
            ..EngineInfo::default()
        };
        assert_eq!(engine_label(&failed), "Engine unavailable");
        assert_eq!(engine_label(&EngineInfo::default()), "…");
    }

    #[test]
    fn ready_idle_menu_order_and_separators() {
        let v = view(&model(Phase::Idle, ready_engine()));
        assert_eq!(v.icon, TrayIconState::Ready);
        assert_eq!(
            top_labels(&v),
            [
                "LinuxFlow — GPU · small",
                "Start dictation (hands-free)",
                "Shortcut: Ctrl+Alt+Space",
                "---",
                "Open LinuxFlow",
                "---",
                "Language",
                "Microphone",
                "Paste automatically",
                "Launch at login",
                "---",
                "About LinuxFlow",
                "Quit",
            ]
        );
        assert_eq!(
            action_of(&v, &TrayCommand::ToggleDictation),
            Some(("Start dictation (hands-free)", true))
        );
        assert!(action_of(&v, &TrayCommand::CancelDictation).is_none());
    }

    #[test]
    fn listening_state() {
        let v = view(&model(Phase::Listening, ready_engine()));
        assert_eq!(v.icon, TrayIconState::Recording);
        assert_eq!(
            action_of(&v, &TrayCommand::ToggleDictation),
            Some(("Stop dictation", true))
        );
        assert_eq!(
            action_of(&v, &TrayCommand::CancelDictation),
            Some(("Cancel dictation", true))
        );
        assert!(v.tooltip_description.starts_with("Listening…\n"));
    }

    #[test]
    fn transcribing_state() {
        for phase in [Phase::Transcribing, Phase::Formatting] {
            let v = view(&model(phase, ready_engine()));
            assert_eq!(v.icon, TrayIconState::Ready);
            assert_eq!(
                action_of(&v, &TrayCommand::ToggleDictation),
                Some(("Start dictation (hands-free)", false))
            );
            assert!(action_of(&v, &TrayCommand::CancelDictation).is_some());
            assert!(v.tooltip_description.starts_with("Transcribing…\n"));
        }
    }

    #[test]
    fn engine_error_state() {
        let failed = EngineInfo {
            phase: EnginePhase::Error,
            error: Some("boom".into()),
            ..EngineInfo::default()
        };
        let v = view(&model(Phase::Idle, failed));
        assert_eq!(v.icon, TrayIconState::Loading);
        assert_eq!(top_labels(&v)[0], "LinuxFlow — Engine unavailable");
        assert_eq!(
            action_of(&v, &TrayCommand::ToggleDictation).map(|a| a.1),
            Some(false)
        );
    }

    #[test]
    fn loading_engine_state() {
        let loading = EngineInfo {
            phase: EnginePhase::Loading,
            loading: true,
            ..EngineInfo::default()
        };
        let v = view(&model(Phase::Idle, loading));
        assert_eq!(v.icon, TrayIconState::Loading);
        assert_eq!(top_labels(&v)[0], "LinuxFlow — Loading the model…");
        assert_eq!(
            action_of(&v, &TrayCommand::ToggleDictation).map(|a| a.1),
            Some(false)
        );
    }

    #[test]
    fn download_states() {
        for (done, text) in [(0, "0%"), (1_302_000_000, "42%"), (3_100_000_000, "99%")] {
            let v = view(&model(Phase::Idle, downloading(done, 3_100_000_000)));
            let labels = top_labels(&v);
            assert_eq!(labels[1], format!("Downloading model… {text}"));
            assert_eq!(labels[2], "Open models page");
            assert!(v
                .tooltip_description
                .contains(&format!("Downloading model… {text}")));
            assert_eq!(
                find(
                    &v.menu,
                    &|n| matches!(n, MenuNode::Action { label, .. } if label == "Open models page")
                ),
                Some(&MenuNode::Action {
                    label: "Open models page".into(),
                    enabled: true,
                    command: TrayCommand::OpenPage(Page::Models)
                })
            );
            assert_eq!(v.icon, TrayIconState::Loading);
        }
        // Unknown total: no percentage.
        let v = view(&model(Phase::Idle, downloading(10, 0)));
        assert_eq!(top_labels(&v)[1], "Downloading model…");
        // Outside a download: no dedicated line or entry.
        let v = view(&model(Phase::Idle, ready_engine()));
        assert!(!top_labels(&v).iter().any(|l| l == "Open models page"));
    }

    #[test]
    fn unavailable_shortcut_is_flagged() {
        let mut m = model(Phase::Idle, ready_engine());
        m.shortcut_ok = false;
        let v = view(&m);
        assert_eq!(top_labels(&v)[2], "Shortcut: Ctrl+Alt+Space (unavailable)");
        assert!(v.tooltip_description.contains("(unavailable)"));
    }

    #[test]
    fn tooltip_text() {
        let v = view(&model(Phase::Idle, ready_engine()));
        assert_eq!(v.tooltip_title, "LinuxFlow");
        assert_eq!(
            v.tooltip_description,
            "GPU · small\nShortcut: Ctrl+Alt+Space"
        );
        let v = view(&model(
            Phase::Idle,
            downloading(1_302_000_000, 3_100_000_000),
        ));
        assert_eq!(
            v.tooltip_description,
            "Downloading the model · 42% (1.3 / 3.1 GB)\nShortcut: Ctrl+Alt+Space\nDownloading model… 42%"
        );
    }

    fn radio(v: &TrayView, submenu: &str) -> (usize, Vec<(String, TrayCommand)>) {
        let sub = find(
            &v.menu,
            &|n| matches!(n, MenuNode::SubMenu { label, .. } if label == submenu),
        )
        .expect("sous-menu");
        let MenuNode::SubMenu { children, .. } = sub else {
            unreachable!()
        };
        match &children[0] {
            MenuNode::Radio { selected, options } => (*selected, options.clone()),
            other => panic!("radio expected, found {other:?}"),
        }
    }

    #[test]
    fn language_radio_follows_settings() {
        let mut m = model(Phase::Idle, ready_engine());
        let (sel, opts) = radio(&view(&m), "Language");
        assert_eq!((sel, opts.len()), (0, 19));
        assert_eq!(
            opts[0],
            ("Automatic".into(), TrayCommand::SetLanguage("auto".into()))
        );
        assert_eq!(
            opts[2],
            ("French".into(), TrayCommand::SetLanguage("fr".into()))
        );
        m.language = "fr".into();
        assert_eq!(radio(&view(&m), "Language").0, 2);
        m.language = "hi".into();
        assert_eq!(radio(&view(&m), "Language").0, 18);
        // Code outside the list: added and selected.
        m.language = "eu".into();
        let (sel, opts) = radio(&view(&m), "Language");
        assert_eq!((sel, opts.len(), opts[sel].0.as_str()), (19, 20, "eu"));
    }

    #[test]
    fn microphone_radio_follows_settings() {
        let mut m = model(Phase::Idle, ready_engine());
        let (sel, opts) = radio(&view(&m), "Microphone");
        assert_eq!(sel, 0);
        assert_eq!(
            opts.iter().map(|o| o.0.as_str()).collect::<Vec<_>>(),
            ["System default", "USB Mic"]
        );
        m.microphone_id = "usb".into();
        assert_eq!(radio(&view(&m), "Microphone").0, 1);
        m.microphone_id = "gone".into();
        let (sel, opts) = radio(&view(&m), "Microphone");
        assert_eq!(opts[sel].0, "gone (not connected)");
        assert_eq!(opts[sel].1, TrayCommand::SetMicrophone("gone".into()));
    }

    #[test]
    fn no_microphone_shows_hint() {
        let mut m = model(Phase::Idle, ready_engine());
        m.microphones.clear();
        let v = view(&m);
        let sub = find(
            &v.menu,
            &|n| matches!(n, MenuNode::SubMenu { label, .. } if label == "Microphone"),
        )
        .expect("sub");
        let MenuNode::SubMenu { children, .. } = sub else {
            unreachable!()
        };
        assert!(children.contains(&MenuNode::Info("No microphone detected".into())));
        assert_eq!(radio(&v, "Microphone").1.len(), 1);
    }

    #[test]
    fn checkboxes_emit_the_inverse_value() {
        let mut m = model(Phase::Idle, ready_engine());
        let v = view(&m);
        let check_of = |v: &TrayView, label: &str| match find(
            &v.menu,
            &|n| matches!(n, MenuNode::Check { label: l, .. } if l == label),
        ) {
            Some(MenuNode::Check {
                checked, command, ..
            }) => (*checked, command.clone()),
            _ => panic!("case absente"),
        };
        assert_eq!(
            check_of(&v, "Paste automatically"),
            (true, TrayCommand::SetAutoPaste(false))
        );
        assert_eq!(
            check_of(&v, "Launch at login"),
            (false, TrayCommand::SetLaunchAtLogin(true))
        );
        m.auto_paste = false;
        m.launch_at_login = true;
        let v = view(&m);
        assert_eq!(
            check_of(&v, "Paste automatically"),
            (false, TrayCommand::SetAutoPaste(true))
        );
        assert_eq!(
            check_of(&v, "Launch at login"),
            (true, TrayCommand::SetLaunchAtLogin(false))
        );
    }

    #[test]
    fn menu_entries_map_to_commands() {
        let v = view(&model(Phase::Idle, ready_engine()));
        for (label, cmd) in [
            ("Open LinuxFlow", TrayCommand::OpenHub),
            ("About LinuxFlow", TrayCommand::About),
            ("Quit", TrayCommand::Quit),
        ] {
            let found = find(
                &v.menu,
                &|n| matches!(n, MenuNode::Action { label: l, command, .. } if l == label && *command == cmd),
            );
            assert!(found.is_some(), "{label}");
        }
        // No "Go to", no troubleshooting, no shortcut switch: they live in the window.
        for gone in ["Go to", "Troubleshooting", "Shortcut enabled"] {
            assert!(
                find(&v.menu, &|n| match n {
                    MenuNode::SubMenu { label, .. }
                    | MenuNode::Action { label, .. }
                    | MenuNode::Check { label, .. } => label == gone,
                    _ => false,
                })
                .is_none(),
                "{gone}"
            );
        }
    }

    #[test]
    fn from_app_state_copies_settings() {
        let mut st = AppState::default();
        st.settings.language = "de".into();
        st.settings.microphone_id = "usb".into();
        st.settings.auto_paste = false;
        st.settings.launch_at_login = true;
        st.shortcut_ok = false;
        st.phase = Phase::Listening;
        let m = TrayModel::from_app_state(&st, vec![]);
        assert_eq!(
            (
                m.language.as_str(),
                m.microphone_id.as_str(),
                m.auto_paste,
                m.launch_at_login,
                m.shortcut_ok,
                m.phase
            ),
            ("de", "usb", false, true, false, Phase::Listening)
        );
    }

    #[test]
    fn default_model_is_loading() {
        let v = view(&TrayModel::default());
        assert_eq!(v.icon, TrayIconState::Loading);
        assert_eq!(top_labels(&v)[0], "LinuxFlow — Loading the model…");
    }
}
