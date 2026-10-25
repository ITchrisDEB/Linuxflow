//! Notification area icon (StatusNotifierItem + dbusmenu) through `ksni` 0.3.
//!
//! - Complete menu built by the pure function [`model::view`]; here only the ksni glue.
//! - Icon: the application logo (coloured theme name, fallback on ARGB pixmaps that
//!   draw the same logo). Its bars turn green while listening. The status stays
//!   `Active` (never `Passive`: KDE would hide the icon; never `NeedsAttention`: blinking).
//! - Left click: open the hub; middle click: start/stop dictation; wheel: nothing.
//! - Robustness: `Tray::spawn` does not block. ksni is started with `assume_sni_available`: if
//!   the StatusNotifierWatcher is absent (GNOME extension enabled later, shell or Plasma
//!   restarted), the icon registers (again) by itself when it appears. The state is visible through
//!   [`TrayHandle::availability`] and [`TrayHandle::subscribe`].
//! - Without a D-Bus session bus: [`TrayAvailability::Unavailable`], no error or panic.

pub mod advice;
pub mod icons;
pub mod model;

pub use advice::{advice, advice_for, detect_distro, Distro, TrayAdvice};
pub use model::{
    download_percent, engine_label, format_gb, language_name, loading_label, pretty_shortcut, view,
    MenuNode, TrayCommand, TrayIconState, TrayModel, TrayView, LANGUAGES,
};

use icons::IconChoice;
use ksni::TrayMethods;
use linuxflow_core::constants::{APP_DIR_NAME, APP_NAME};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Availability of the icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAvailability {
    /// Registered with a StatusNotifierWatcher.
    Available,
    /// No watcher for now: the icon will appear as soon as there is one.
    WaitingForWatcher,
    /// Unusable (no session bus, shutdown); technical text, not translated, for the logs.
    Unavailable(String),
}

/// State shared between the ksni thread, the callbacks and the handle.
struct Shared {
    current: Mutex<TrayAvailability>,
    /// False as long as no watcher callback has happened (initial state not confirmed).
    decided: Mutex<bool>,
    events: async_channel::Sender<TrayAvailability>,
}

impl Shared {
    fn set(&self, value: TrayAvailability) {
        if let Ok(mut cur) = self.current.lock() {
            if *cur != value {
                *cur = value.clone();
                // Unbounded channel: sending only fails if nobody listens any more.
                let _ = self.events.try_send(value);
            }
        }
    }

    fn decide(&self, value: TrayAvailability) {
        if let Ok(mut d) = self.decided.lock() {
            *d = true;
        }
        self.set(value);
    }

    fn get(&self) -> TrayAvailability {
        self.current
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|_| TrayAvailability::Unavailable("poisoned".into()))
    }
}

/// Maximum wait for the tray thread to stop.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// After this number of close attempts the state becomes `Unavailable`, but we NEVER give up:
/// a very early launch (autostart) may precede the appearance of the session bus.
const CONNECT_ATTEMPTS: u32 = 3;

/// Delay before the next attempt: 2 s (3 times), then 10 s up to about 2 minutes, then 60 s.
fn retry_delay(attempt: u32) -> Duration {
    match attempt {
        0..=3 => Duration::from_secs(2),
        4..=15 => Duration::from_secs(10),
        _ => Duration::from_secs(60),
    }
}

/// Entry point: see [`Tray::spawn`].
pub struct Tray;

impl Tray {
    /// Starts the icon without blocking. The menu actions arrive on `commands_tx`.
    ///
    /// The handle is first in [`TrayAvailability::WaitingForWatcher`]; it moves to `Available`
    /// as soon as registration happens, or to `Unavailable` if there is no session bus.
    pub fn spawn(commands_tx: async_channel::Sender<TrayCommand>) -> TrayHandle {
        let (events_tx, events_rx) = async_channel::unbounded();
        let shared = Arc::new(Shared {
            current: Mutex::new(TrayAvailability::WaitingForWatcher),
            decided: Mutex::new(false),
            events: events_tx,
        });
        let (msg_tx, msg_rx) = tokio::sync::mpsc::unbounded_channel();

        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("linuxflow-tray".into())
            .spawn(move || run(commands_tx, thread_shared, msg_rx));
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                shared.set(TrayAvailability::Unavailable(format!("thread: {e}")));
                None
            }
        };
        TrayHandle {
            shared,
            events_rx,
            tx: thread.is_some().then_some(msg_tx),
            thread,
        }
    }
}

enum Msg {
    Update(Box<TrayModel>),
    Shutdown,
}

/// Handle of the icon. All the methods have no effect if the tray is unusable.
pub struct TrayHandle {
    shared: Arc<Shared>,
    events_rx: async_channel::Receiver<TrayAvailability>,
    tx: Option<tokio::sync::mpsc::UnboundedSender<Msg>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TrayHandle {
    pub fn availability(&self) -> TrayAvailability {
        self.shared.get()
    }

    /// True if the icon is registered with a watcher.
    pub fn available(&self) -> bool {
        self.availability() == TrayAvailability::Available
    }

    /// Stream of availability changes (a single consumer: the clones share the
    /// messages). Closed when the handle is destroyed.
    pub fn subscribe(&self) -> async_channel::Receiver<TrayAvailability> {
        self.events_rx.clone()
    }

    /// Pushes a new state (icon, tooltip, menu).
    pub fn update(&self, model: TrayModel) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Update(Box::new(model)));
        }
    }

    /// Removes the icon and waits for the thread to stop, [`SHUTDOWN_WAIT`] at most: if ksni stays blocked
    /// on D-Bus, the thread is abandoned (it ends by itself) rather than freezing the shutdown.
    pub fn shutdown(mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let deadline = std::time::Instant::now() + SHUTDOWN_WAIT;
            while !thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                tracing::warn!("tray thread did not stop in time: abandoned");
            }
        }
    }

    fn stop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Shutdown);
        }
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    commands_tx: async_channel::Sender<TrayCommand>,
    shared: Arc<Shared>,
    mut msg_rx: tokio::sync::mpsc::UnboundedReceiver<Msg>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("tray runtime unavailable: {e}");
            shared.set(TrayAvailability::Unavailable(format!("runtime: {e}")));
            return;
        }
    };
    runtime.block_on(async move {
        let data_dirs = icons::without_appdir(
            icons::data_dirs(&|k| std::env::var(k).ok()),
            std::env::var_os("APPDIR").as_deref(),
        );
        let installed: HashSet<String> = icons::all_icon_names()
            .into_iter()
            .filter(|n| icons::icon_installed(n, &data_dirs))
            .collect();

        // Updates received before ksni is ready: the last one is kept.
        let mut model = TrayModel::default();
        let mut attempt = 0;
        let handle = loop {
            attempt += 1;
            let tray = SniTray {
                model: model.clone(),
                commands_tx: commands_tx.clone(),
                shared: Arc::clone(&shared),
                installed: installed.clone(),
            };
            // `assume_sni_available`: the absence of a watcher is not an error, ksni waits
            // its appearance (GNOME extension enabled later, shell restart).
            match tray.assume_sni_available(true).spawn().await {
                Ok(h) => break h,
                Err(e) => {
                    tracing::info!("tray: cannot reach the session bus ({attempt}): {e}");
                    if attempt == CONNECT_ATTEMPTS {
                        shared.set(TrayAvailability::Unavailable(e.to_string()));
                    }
                    // Interruptible wait: an update is remembered, a stop leaves at once.
                    let wait = tokio::time::sleep(retry_delay(attempt));
                    tokio::pin!(wait);
                    loop {
                        tokio::select! {
                            _ = &mut wait => break,
                            msg = msg_rx.recv() => match msg {
                                Some(Msg::Update(m)) => model = *m,
                                Some(Msg::Shutdown) | None => return,
                            },
                        }
                    }
                }
            }
        };
        // No watcher callback during start-up: the registration succeeded.
        let undecided = shared.decided.lock().map(|d| !*d).unwrap_or(false);
        if undecided {
            shared.set(TrayAvailability::Available);
        }
        while let Some(msg) = msg_rx.recv().await {
            match msg {
                Msg::Update(m) => {
                    handle.update(|t| t.model = *m).await;
                }
                Msg::Shutdown => break,
            }
        }
        handle.shutdown().await;
        shared.set(TrayAvailability::Unavailable("shut down".into()));
    });
}

/// ksni implementation; glue only, the logic is in [`model`] and [`icons`].
struct SniTray {
    model: TrayModel,
    commands_tx: async_channel::Sender<TrayCommand>,
    shared: Arc<Shared>,
    /// Icon names present in the theme.
    installed: HashSet<String>,
}

impl SniTray {
    fn send(&self, command: TrayCommand) {
        // Channel full or closed: the command is lost, never blocking.
        let _ = self.commands_tx.try_send(command);
    }

    fn is_installed(&self, name: &str) -> bool {
        self.installed.contains(name)
    }
}

/// dbusmenu entry of a model node.
fn to_item(node: MenuNode) -> ksni::MenuItem<SniTray> {
    use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
    match node {
        MenuNode::Separator => ksni::MenuItem::Separator,
        MenuNode::Info(label) => StandardItem {
            label,
            enabled: false,
            ..StandardItem::default()
        }
        .into(),
        MenuNode::Action {
            label,
            enabled,
            command,
        } => StandardItem {
            label,
            enabled,
            activate: Box::new(move |t: &mut SniTray| t.send(command.clone())),
            ..StandardItem::default()
        }
        .into(),
        MenuNode::Check {
            label,
            checked,
            enabled,
            command,
        } => CheckmarkItem {
            label,
            enabled,
            checked,
            activate: Box::new(move |t: &mut SniTray| t.send(command.clone())),
            ..CheckmarkItem::default()
        }
        .into(),
        MenuNode::Radio { selected, options } => {
            let commands: Vec<TrayCommand> = options.iter().map(|(_, c)| c.clone()).collect();
            RadioGroup {
                selected,
                select: Box::new(move |t: &mut SniTray, index: usize| {
                    if let Some(c) = commands.get(index) {
                        t.send(c.clone());
                    }
                }),
                options: options
                    .into_iter()
                    .map(|(label, _)| RadioItem {
                        label,
                        ..RadioItem::default()
                    })
                    .collect(),
            }
            .into()
        }
        MenuNode::SubMenu {
            label,
            enabled,
            children,
        } => SubMenu {
            label,
            enabled,
            submenu: children.into_iter().map(to_item).collect(),
            ..SubMenu::default()
        }
        .into(),
    }
}

impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        APP_DIR_NAME.to_string()
    }

    fn title(&self) -> String {
        APP_NAME.to_string()
    }

    fn icon_name(&self) -> String {
        match icons::choose_icon(view(&self.model).icon, &|n| self.is_installed(n)) {
            IconChoice::Themed(name) => name,
            IconChoice::Pixmap => String::new(),
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let state = view(&self.model).icon;
        match icons::choose_icon(state, &|n| self.is_installed(n)) {
            IconChoice::Themed(_) => Vec::new(),
            IconChoice::Pixmap => icons::pixmaps(state),
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let v = view(&self.model);
        ksni::ToolTip {
            title: v.tooltip_title,
            description: v.tooltip_description,
            ..ksni::ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::OpenHub);
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::ToggleDictation);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        view(&self.model).menu.into_iter().map(to_item).collect()
    }

    fn watcher_online(&self) {
        self.shared.decide(TrayAvailability::Available);
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        // Never give up: the watcher may come back (extension enabled later, shell restarted).
        let state = match reason {
            ksni::OfflineReason::Error(ksni::Error::Dbus(e)) => {
                TrayAvailability::Unavailable(e.to_string())
            }
            _ => TrayAvailability::WaitingForWatcher,
        };
        self.shared.decide(state);
        true
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use linuxflow_core::domain::{EnginePhase, Phase};

    fn sni(installed: &[&str]) -> (SniTray, async_channel::Receiver<TrayCommand>) {
        let (tx, rx) = async_channel::unbounded();
        let (etx, _erx) = async_channel::unbounded();
        let t = SniTray {
            model: TrayModel::default(),
            commands_tx: tx,
            shared: Arc::new(Shared {
                current: Mutex::new(TrayAvailability::WaitingForWatcher),
                decided: Mutex::new(false),
                events: etx,
            }),
            installed: installed.iter().map(|s| s.to_string()).collect(),
        };
        (t, rx)
    }

    #[test]
    fn the_connection_is_retried_forever_with_growing_delays() {
        assert_eq!(retry_delay(1), Duration::from_secs(2));
        assert_eq!(retry_delay(3), Duration::from_secs(2));
        assert_eq!(retry_delay(4), Duration::from_secs(10));
        assert_eq!(retry_delay(15), Duration::from_secs(10));
        assert_eq!(retry_delay(16), Duration::from_secs(60));
        assert_eq!(retry_delay(10_000), Duration::from_secs(60));
    }

    #[test]
    fn inactive_handle_is_harmless() {
        let (etx, erx) = async_channel::unbounded();
        let shared = Arc::new(Shared {
            current: Mutex::new(TrayAvailability::Unavailable("test".into())),
            decided: Mutex::new(true),
            events: etx,
        });
        let handle = TrayHandle {
            shared,
            events_rx: erx,
            tx: None,
            thread: None,
        };
        assert!(!handle.available());
        assert_eq!(
            handle.availability(),
            TrayAvailability::Unavailable("test".into())
        );
        handle.update(TrayModel::default());
        handle.shutdown();
    }

    #[test]
    fn availability_changes_are_published_once() {
        let (etx, erx) = async_channel::unbounded();
        let shared = Shared {
            current: Mutex::new(TrayAvailability::WaitingForWatcher),
            decided: Mutex::new(false),
            events: etx,
        };
        shared.set(TrayAvailability::WaitingForWatcher); // unchanged: nothing
        shared.decide(TrayAvailability::Available);
        shared.decide(TrayAvailability::Available); // doublon : rien
        shared.decide(TrayAvailability::WaitingForWatcher);
        assert_eq!(erx.try_recv().ok(), Some(TrayAvailability::Available));
        assert_eq!(
            erx.try_recv().ok(),
            Some(TrayAvailability::WaitingForWatcher)
        );
        assert!(erx.try_recv().is_err());
    }

    #[test]
    fn watcher_callbacks_drive_availability() {
        use ksni::Tray as _;
        let (t, _rx) = sni(&[]);
        assert!(t.watcher_offline(ksni::OfflineReason::No));
        assert_eq!(t.shared.get(), TrayAvailability::WaitingForWatcher);
        t.watcher_online();
        assert_eq!(t.shared.get(), TrayAvailability::Available);
        assert!(
            t.watcher_offline(ksni::OfflineReason::No),
            "never stop the service"
        );
        assert_eq!(t.shared.get(), TrayAvailability::WaitingForWatcher);
    }

    #[test]
    fn icon_falls_back_from_colored_theme_icon_to_pixmap() {
        use ksni::Tray as _;
        let (t, _) = sni(&[]);
        assert_eq!(t.icon_name(), "");
        assert_eq!(t.icon_pixmap().len(), icons::PIXMAP_SIZES.len());
        let loading = icons::icon_name(TrayIconState::Loading);
        let (t, _) = sni(&[loading.as_str()]);
        assert_eq!(t.icon_name(), loading);
        assert!(t.icon_pixmap().is_empty());
    }

    #[test]
    fn listening_shows_the_green_logo_without_attention_blinking() {
        use ksni::Tray as _;
        let (mut t, _) = sni(&[]);
        assert!(matches!(t.status(), ksni::Status::Active));
        let bytes =
            |t: &SniTray| -> Vec<Vec<u8>> { t.icon_pixmap().into_iter().map(|i| i.data).collect() };
        let idle = bytes(&t);
        t.model.phase = Phase::Listening;
        // Never "attention": the green icon replaces the normal icon, with no blinking.
        assert!(matches!(t.status(), ksni::Status::Active));
        assert_ne!(bytes(&t), idle);
        assert!(t.attention_icon_pixmap().is_empty());
    }

    #[test]
    fn clicks_map_to_commands() {
        use ksni::Tray as _;
        let (mut t, rx) = sni(&[]);
        t.activate(0, 0);
        t.secondary_activate(0, 0);
        t.scroll(1, ksni::Orientation::Vertical);
        assert_eq!(rx.try_recv().ok(), Some(TrayCommand::OpenHub));
        assert_eq!(rx.try_recv().ok(), Some(TrayCommand::ToggleDictation));
        assert!(rx.try_recv().is_err(), "the wheel does nothing");
    }

    #[test]
    fn menu_items_emit_their_commands() {
        use ksni::Tray as _;
        let (mut t, rx) = sni(&[]);
        t.model.engine.ready = true;
        t.model.engine.phase = EnginePhase::Ready;
        t.model.microphones = vec![linuxflow_core::domain::MicrophoneInfo {
            id: "usb".into(),
            label: "USB".into(),
        }];
        let mut expected = Vec::new();
        fn walk(items: Vec<ksni::MenuItem<SniTray>>, t: &mut SniTray, expected: &mut Vec<usize>) {
            for item in items {
                match item {
                    ksni::MenuItem::Standard(s) if s.enabled => {
                        (s.activate)(t);
                        expected.push(1);
                    }
                    ksni::MenuItem::Checkmark(c) if c.enabled => {
                        (c.activate)(t);
                        expected.push(1);
                    }
                    ksni::MenuItem::RadioGroup(r) => {
                        (r.select)(t, 1);
                        expected.push(1);
                    }
                    ksni::MenuItem::SubMenu(s) => walk(s.submenu, t, expected),
                    _ => {}
                }
            }
        }
        walk(t.menu(), &mut t, &mut expected);
        let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(got.len(), expected.len());
        assert!(got.contains(&TrayCommand::SetLanguage("en".into())));
        assert!(got.contains(&TrayCommand::SetAutoPaste(false)));
        assert!(got.contains(&TrayCommand::ToggleDictation));
        assert!(got.contains(&TrayCommand::SetMicrophone("usb".into())));
        assert!(got.contains(&TrayCommand::Quit));
    }

    /// Really registers the icon: `LINUXFLOW_TEST_DESKTOP=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_tray_registers() {
        if std::env::var("LINUXFLOW_TEST_DESKTOP").as_deref() != Ok("1") {
            return;
        }
        let (tx, _rx) = async_channel::unbounded::<TrayCommand>();
        let handle = Tray::spawn(tx);
        std::thread::sleep(Duration::from_secs(3));
        eprintln!("tray availability: {:?}", handle.availability());
        handle.update(TrayModel::default());
        std::thread::sleep(Duration::from_secs(30));
        handle.shutdown();
    }
}
