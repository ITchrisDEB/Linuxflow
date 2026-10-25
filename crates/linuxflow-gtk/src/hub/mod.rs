//! Main window (hub) of LinuxFlow: sidebar, pages, first-launch wizard.
//!
//! `HubView` holds the whole interface (rebuildable on the fly to change language) (windowless: useful for offscreen rendering and tests);
//! `HubWindow` wraps it in an `adw::ApplicationWindow` whose close button hides the window.

pub mod ctx;
pub mod dialog;
pub mod keyed;
pub mod labels;
pub mod pages;
pub mod style;
pub mod widgets;

use crate::toast::ToastHandle;
use crate::wizard::Wizard;
use adw::prelude::*;
use ctx::Ctx;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_core::constants::{APP_ID, APP_NAME};
use linuxflow_core::domain::{AppState, Phase};
use linuxflow_core::events::{Event, Page};
use linuxflow_core::traits::Controller;
use linuxflow_i18n::tr;
use pages::{
    compat::CompatPage, dictionary::DictionaryPage, history::HistoryPage, home::HomePage,
    settings::SettingsPage, snippets::SnippetsPage,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::models_page::ModelsPage;
use labels::{engine_pill, PillKind};

/// Width of the sidebar.
pub const SIDEBAR_WIDTH: i32 = 232;

/// Hub pages, in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageId {
    Home,
    History,
    Dictionary,
    Snippets,
    Models,
    Settings,
    Troubleshooting,
}

impl PageId {
    pub const NAV: [PageId; 7] = [
        PageId::Home,
        PageId::History,
        PageId::Dictionary,
        PageId::Snippets,
        PageId::Models,
        PageId::Settings,
        PageId::Troubleshooting,
    ];

    pub fn id(self) -> &'static str {
        match self {
            PageId::Home => "home",
            PageId::History => "history",
            PageId::Dictionary => "dictionary",
            PageId::Snippets => "snippets",
            PageId::Models => "models",
            PageId::Settings => "settings",
            PageId::Troubleshooting => "troubleshooting",
        }
    }

    pub fn from_id(id: &str) -> Option<PageId> {
        [
            PageId::Home,
            PageId::History,
            PageId::Dictionary,
            PageId::Snippets,
            PageId::Models,
            PageId::Settings,
            PageId::Troubleshooting,
        ]
        .into_iter()
        // "compat": old name of the page, remembered by previous versions.
        .find(|p| p.id() == id || (id == "compat" && *p == PageId::Troubleshooting))
    }

    pub fn from_event(p: Page) -> PageId {
        match p {
            Page::Home => PageId::Home,
            Page::History => PageId::History,
            Page::Dictionary => PageId::Dictionary,
            Page::Snippets => PageId::Snippets,
            Page::Settings => PageId::Settings,
            Page::Models => PageId::Models,
        }
    }

    pub fn title(self) -> String {
        match self {
            PageId::Home => tr("Home"),
            PageId::History => tr("History"),
            PageId::Dictionary => tr("Dictionary"),
            PageId::Snippets => tr("Snippets"),
            PageId::Models => tr("Models"),
            PageId::Settings => tr("Settings"),
            PageId::Troubleshooting => tr("Troubleshooting"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            PageId::Home => "go-home-symbolic",
            PageId::History => "document-open-recent-symbolic",
            PageId::Dictionary => "accessories-dictionary-symbolic",
            PageId::Snippets => "insert-text-symbolic",
            PageId::Models => "folder-download-symbolic",
            PageId::Settings => "emblem-system-symbolic",
            PageId::Troubleshooting => "help-browser-symbolic",
        }
    }

    /// Sidebar entry that represents this page.
    fn nav_index(self) -> usize {
        Self::NAV.iter().position(|x| *x == self).unwrap_or(0)
    }
}

/// Memorisation of the current page, provided by the application (state file...).
pub trait StateStore {
    fn load_page(&self) -> Option<String>;
    fn save_page(&self, page: &str);
}

/// In-memory memorisation (default).
#[derive(Default)]
pub struct MemoryStateStore {
    page: RefCell<Option<String>>,
}

impl StateStore for MemoryStateStore {
    fn load_page(&self) -> Option<String> {
        self.page.borrow().clone()
    }

    fn save_page(&self, page: &str) {
        *self.page.borrow_mut() = Some(page.to_string());
    }
}

/// Memorisation through two functions provided by the application.
pub struct CallbackStateStore {
    pub load: Box<dyn Fn() -> Option<String>>,
    pub save: Box<dyn Fn(&str)>,
}

impl StateStore for CallbackStateStore {
    fn load_page(&self) -> Option<String> {
        (self.load)()
    }

    fn save_page(&self, page: &str) {
        (self.save)(page)
    }
}

pub struct HubInner {
    root: adw::ToastOverlay,
    ctx: Ctx,
    outer: gtk::Stack,
    content: gtk::Stack,
    nav: gtk::ListBox,
    pill_dot: gtk::Box,
    pill_text: gtk::Label,
    blink: adw::TimedAnimation,
    home: Rc<HomePage>,
    history: Rc<HistoryPage>,
    dictionary: Rc<DictionaryPage>,
    snippets: Rc<SnippetsPage>,
    models: Rc<ModelsPage>,
    settings: Rc<SettingsPage>,
    compat: Rc<CompatPage>,
    wizard: Rc<Wizard>,
    current: Cell<PageId>,
    store: RefCell<Rc<dyn StateStore>>,
    last_state: RefCell<Option<AppState>>,
    first_run_seen: Cell<Option<bool>>,
    wizard_finished: Cell<bool>,
    selecting: Cell<bool>,
    pill_kind: Cell<Option<PillKind>>,
}

impl HubInner {
    /// `request_rebuild`: request to rebuild the interface (language change).
    pub fn new(
        controller: Arc<dyn Controller>,
        toast: ToastHandle,
        request_rebuild: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        style::install();
        let ctx = Ctx::new(controller, toast.clone());

        let content = gtk::Stack::new();
        content.set_hexpand(true);
        content.set_vexpand(true);

        // The navigation callback is wired after construction (Rc cycle avoided with Weak).
        let nav_target: Rc<RefCell<Option<std::rc::Weak<HubInner>>>> = Rc::default();
        let navigate: Rc<dyn Fn(PageId)> = {
            let nav_target = nav_target.clone();
            Rc::new(move |p| {
                if let Some(v) = nav_target.borrow().as_ref().and_then(|w| w.upgrade()) {
                    v.show_page(p);
                }
            })
        };

        let home = HomePage::new(&ctx);
        let history = HistoryPage::new(&ctx);
        let dictionary = DictionaryPage::new(&ctx);
        let snippets = SnippetsPage::new(&ctx);
        let models = ModelsPage::new(&ctx);
        let settings = SettingsPage::new(&ctx, navigate.clone(), request_rebuild.clone());
        let compat = CompatPage::new(&ctx);
        for (id, w) in [
            (PageId::Home, home.widget()),
            (PageId::History, history.widget()),
            (PageId::Dictionary, dictionary.widget()),
            (PageId::Snippets, snippets.widget()),
            (PageId::Models, models.widget()),
            (PageId::Settings, settings.widget()),
            (PageId::Troubleshooting, compat.widget()),
        ] {
            content.add_named(w, Some(id.id()));
        }

        // --- sidebar
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("lf-sidebar");
        sidebar.set_width_request(SIDEBAR_WIDTH);
        sidebar.append(&Self::brand());
        let nav = gtk::ListBox::new();
        nav.set_selection_mode(gtk::SelectionMode::Single);
        nav.add_css_class("lf-nav");
        for p in PageId::NAV {
            let row = gtk::ListBoxRow::new();
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            b.append(&gtk::Image::from_icon_name(p.icon()));
            b.append(&gtk::Label::new(Some(&p.title())));
            row.set_child(Some(&b));
            nav.append(&row);
        }
        sidebar.append(&nav);
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        sidebar.append(&spacer);
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pill.add_css_class("lf-engine");
        let pill_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill_dot.add_css_class("lf-dot");
        pill_dot.set_valign(gtk::Align::Center);
        let pill_text = widgets::label("", &["lf-engine-text"]);
        pill.append(&pill_dot);
        pill.append(&pill_text);
        sidebar.append(&pill);

        let main = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        main.append(&sidebar);
        main.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        main.append(&content);

        let wizard = Wizard::new(
            &ctx,
            {
                let nav_target = nav_target.clone();
                Rc::new(move || {
                    if let Some(v) = nav_target.borrow().as_ref().and_then(|w| w.upgrade()) {
                        v.finish_wizard();
                    }
                })
            },
            request_rebuild,
        );
        let wizard_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wizard_page.set_hexpand(true);
        wizard_page.append(wizard.widget());
        wizard.widget().set_vexpand(true);

        let outer = gtk::Stack::new();
        outer.add_named(&main, Some("main"));
        outer.add_named(&wizard_page, Some("wizard"));
        outer.add_css_class("lf-hub");
        toast.overlay().set_child(Some(&outer));

        let blink = adw::TimedAnimation::builder()
            .widget(&pill_dot)
            .value_from(1.0)
            .value_to(0.3)
            .duration(900)
            .repeat_count(0)
            .alternate(true)
            .target(&adw::PropertyAnimationTarget::new(&pill_dot, "opacity"))
            .build();

        let view = Rc::new(Self {
            root: toast.overlay().clone(),
            ctx,
            outer,
            content,
            nav: nav.clone(),
            pill_dot,
            pill_text,
            blink,
            home,
            history,
            dictionary,
            snippets,
            models,
            settings,
            compat,
            wizard,
            current: Cell::new(PageId::Home),
            store: RefCell::new(Rc::new(MemoryStateStore::default())),
            last_state: RefCell::new(None),
            first_run_seen: Cell::new(None),
            wizard_finished: Cell::new(false),
            selecting: Cell::new(false),
            pill_kind: Cell::new(None),
        });
        *nav_target.borrow_mut() = Some(Rc::downgrade(&view));
        let weak = Rc::downgrade(&view);
        nav.connect_row_selected(move |_, row| {
            let (Some(v), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            if v.selecting.get() {
                return;
            }
            if let Some(p) = PageId::NAV.get(row.index() as usize) {
                v.show_page(*p);
            }
        });
        view.show_page(PageId::Home);
        // Initial state (short call, GTK thread).
        let state = view.ctx.controller.state();
        view.apply_state(&state);
        view
    }

    fn brand() -> gtk::Box {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        b.add_css_class("lf-brand");
        // The application icon (dark tile, cream waves) when the theme
        // knows it (installed archive); otherwise a generic microphone on the same dark tile.
        let themed = gtk::gdk::Display::default()
            .map(|d| gtk::IconTheme::for_display(&d).has_icon(APP_ID))
            .unwrap_or(false);
        let icon = if themed {
            let i = gtk::Image::from_icon_name(APP_ID);
            i.set_pixel_size(34);
            i
        } else {
            let i = gtk::Image::from_icon_name("audio-input-microphone-symbolic");
            i.set_pixel_size(20);
            i.add_css_class("lf-brand-icon");
            i
        };
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::Center);
        icon.set_size_request(34, 34);
        b.append(&icon);
        let t = gtk::Box::new(gtk::Orientation::Vertical, 0);
        t.append(&widgets::label(APP_NAME, &["lf-brand-name"]));
        t.append(&widgets::label(
            &tr("Voice dictation, 100% local"),
            &["lf-brand-tag"],
        ));
        b.append(&t);
        b
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn current_page(&self) -> PageId {
        self.current.get()
    }

    /// Position of the highlighted entry in the sidebar.
    pub fn selected_nav_index(&self) -> Option<usize> {
        self.nav.selected_row().map(|r| r.index() as usize)
    }

    pub fn home(&self) -> &Rc<HomePage> {
        &self.home
    }

    pub fn history(&self) -> &Rc<HistoryPage> {
        &self.history
    }

    pub fn dictionary(&self) -> &Rc<DictionaryPage> {
        &self.dictionary
    }

    pub fn snippets(&self) -> &Rc<SnippetsPage> {
        &self.snippets
    }

    pub fn models(&self) -> &Rc<ModelsPage> {
        &self.models
    }

    pub fn settings(&self) -> &Rc<SettingsPage> {
        &self.settings
    }

    pub fn compat(&self) -> &Rc<CompatPage> {
        &self.compat
    }

    pub fn wizard(&self) -> &Rc<Wizard> {
        &self.wizard
    }

    /// Current text and style of the engine pill (tests).
    pub fn pill(&self) -> (Option<PillKind>, String) {
        (self.pill_kind.get(), self.pill_text.text().to_string())
    }

    pub fn wizard_visible(&self) -> bool {
        self.outer.visible_child_name().as_deref() == Some("wizard")
    }

    /// Installs the memorisation of the current page and restores the memorised page.
    pub fn set_state_store(self: &Rc<Self>, store: Rc<dyn StateStore>) {
        let saved = store.load_page().and_then(|id| PageId::from_id(&id));
        *self.store.borrow_mut() = store;
        if let Some(p) = saved {
            self.show_page(p);
        }
    }

    pub fn show_page(self: &Rc<Self>, page: PageId) {
        let previous = self.current.replace(page);
        if previous == PageId::Settings && page != PageId::Settings {
            self.settings.deactivate();
        }
        self.content.set_visible_child_name(page.id());
        self.selecting.set(true);
        if let Some(row) = self.nav.row_at_index(page.nav_index() as i32) {
            self.nav.select_row(Some(&row));
        }
        self.selecting.set(false);
        self.models.set_visible_page(page == PageId::Models);
        match page {
            PageId::Settings => self.settings.refresh_microphones(),
            PageId::Troubleshooting => self.compat.refresh(),
            _ => {}
        }
        self.store.borrow().save_page(page.id());
    }

    /// Last state received (to rebuild the interface).
    pub fn last_state(&self) -> Option<AppState> {
        self.last_state.borrow().clone()
    }

    pub fn store(&self) -> Rc<dyn StateStore> {
        self.store.borrow().clone()
    }

    /// Inputs in progress in the forms and the test area.
    pub fn capture_input(&self) -> InputSnapshot {
        InputSnapshot {
            try_text: self.home.try_text(),
            dictionary: self.dictionary.form_values(),
            snippet: self.snippets.form_values(),
        }
    }

    pub fn restore_input(&self, input: &InputSnapshot) {
        self.home.set_try_text(&input.try_text);
        self.dictionary.restore_form(&input.dictionary);
        self.snippets.restore_form(&input.snippet);
    }

    /// Called when the window is hidden: cancels the shortcut recording.
    pub fn on_hidden(&self) {
        self.settings.deactivate();
    }

    fn finish_wizard(self: &Rc<Self>) {
        self.wizard_finished.set(true);
        self.outer.set_visible_child_name("main");
        self.show_page(PageId::Home);
    }

    pub fn handle_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::State(s) => self.apply_state(&s),
            Event::Phase { phase, message } => {
                self.home.set_phase(phase);
                if let Some(state) = self.last_state.borrow_mut().as_mut() {
                    state.phase = phase;
                    self.update_pill(state);
                }
                if phase == Phase::Error {
                    if let Some(m) = message.filter(|m| !m.is_empty()) {
                        self.ctx.toast.show(&m);
                    }
                }
            }
            Event::InsertText(text) => self.home.insert_text(&text),
            Event::Navigate(p) => self.show_page(PageId::from_event(p)),
            Event::Toast(text) => self.ctx.toast.show(&text),
            Event::Overlay(_) | Event::Level(_) => {}
        }
    }

    pub fn apply_state(self: &Rc<Self>, state: &AppState) {
        *self.last_state.borrow_mut() = Some(state.clone());
        self.home.update(state);
        self.history.update(state);
        self.dictionary.update(state);
        self.snippets.update(state);
        self.settings.update(state);
        self.wizard.update(state);
        self.models.on_state();
        self.update_pill(state);
        style::apply_theme(state.settings.theme);

        let done = state.settings.first_run_done;
        let previous = self.first_run_seen.replace(Some(done));
        if done {
            self.wizard_finished.set(false);
            if previous != Some(true) {
                self.outer.set_visible_child_name("main");
            }
        } else if previous != Some(false) && !self.wizard_finished.get() {
            self.outer.set_visible_child_name("wizard");
            self.wizard.load();
        }
    }

    fn update_pill(&self, state: &AppState) {
        let (kind, text) = engine_pill(state);
        labels_set(&self.pill_text, &text);
        if self.pill_kind.get() != Some(kind) {
            for k in [
                PillKind::Loading,
                PillKind::Ready,
                PillKind::Error,
                PillKind::Listening,
            ] {
                self.pill_dot.remove_css_class(k.css_class());
            }
            self.pill_dot.add_css_class(kind.css_class());
            if kind == PillKind::Listening {
                self.blink.play();
            } else {
                self.blink.reset();
                self.pill_dot.set_opacity(1.0);
            }
            self.pill_kind.set(Some(kind));
        }
    }
}

fn labels_set(l: &gtk::Label, text: &str) {
    widgets::set_label(l, text);
}

/// Inputs to keep when the interface is rebuilt.
#[derive(Debug, Clone, Default)]
pub struct InputSnapshot {
    pub try_text: String,
    pub dictionary: pages::dictionary::FormValues,
    pub snippet: (String, String),
}

/// Complete interface of the hub, rebuildable on the fly (interface language change)
/// while keeping the page, the inputs in progress and the page memorisation. The root `ToastOverlay`
/// stays the same widget: only its content is replaced.
pub struct HubView {
    controller: Arc<dyn Controller>,
    toast: ToastHandle,
    inner: RefCell<Rc<HubInner>>,
    this: std::rc::Weak<HubView>,
}

impl HubView {
    pub fn new(controller: Arc<dyn Controller>) -> Rc<Self> {
        Rc::new_cyclic(|weak: &std::rc::Weak<HubView>| {
            let toast = ToastHandle::new();
            let inner = HubInner::new(
                controller.clone(),
                toast.clone(),
                Self::rebuild_request(weak.clone()),
            );
            HubView {
                controller,
                toast,
                inner: RefCell::new(inner),
                this: weak.clone(),
            }
        })
    }

    /// Deferred request (outside the signal handler of the widget that triggers it).
    fn rebuild_request(weak: std::rc::Weak<HubView>) -> Rc<dyn Fn()> {
        Rc::new(move || {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(v) = weak.upgrade() {
                    v.rebuild_for_language();
                }
            });
        })
    }

    /// Rebuilds all the pages with the current language of `linuxflow_i18n`: the page, the
    /// inputs in progress and the page memorisation are kept. Keyboard focus goes back to
    /// the content of the page (the exact widget is not restored).
    pub fn rebuild_for_language(&self) {
        let old = self.inner.borrow().clone();
        old.on_hidden(); // annule un enregistrement de raccourci en cours
        let (page, input, store) = (old.current_page(), old.capture_input(), old.store());
        let new = HubInner::new(
            self.controller.clone(),
            self.toast.clone(),
            Self::rebuild_request(self.this.clone()),
        );
        new.set_state_store(store);
        new.show_page(page);
        new.restore_input(&input);
        *self.inner.borrow_mut() = new;
    }

    fn inner(&self) -> Rc<HubInner> {
        self.inner.borrow().clone()
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.toast.overlay().upcast_ref()
    }

    pub fn toast(&self) -> &ToastHandle {
        &self.toast
    }

    pub fn current_page(&self) -> PageId {
        self.inner().current_page()
    }

    pub fn selected_nav_index(&self) -> Option<usize> {
        self.inner().selected_nav_index()
    }

    pub fn home(&self) -> Rc<HomePage> {
        self.inner().home().clone()
    }

    pub fn history(&self) -> Rc<HistoryPage> {
        self.inner().history().clone()
    }

    pub fn dictionary(&self) -> Rc<DictionaryPage> {
        self.inner().dictionary().clone()
    }

    pub fn snippets(&self) -> Rc<SnippetsPage> {
        self.inner().snippets().clone()
    }

    pub fn models(&self) -> Rc<ModelsPage> {
        self.inner().models().clone()
    }

    pub fn settings(&self) -> Rc<SettingsPage> {
        self.inner().settings().clone()
    }

    pub fn compat(&self) -> Rc<CompatPage> {
        self.inner().compat().clone()
    }

    pub fn wizard(&self) -> Rc<Wizard> {
        self.inner().wizard().clone()
    }

    pub fn pill(&self) -> (Option<PillKind>, String) {
        self.inner().pill()
    }

    pub fn wizard_visible(&self) -> bool {
        self.inner().wizard_visible()
    }

    pub fn set_state_store(&self, store: Rc<dyn StateStore>) {
        self.inner().set_state_store(store);
    }

    pub fn show_page(&self, page: PageId) {
        self.inner().show_page(page);
    }

    pub fn on_hidden(&self) {
        self.inner().on_hidden();
    }

    pub fn handle_event(&self, event: Event) {
        self.inner().handle_event(event);
    }

    pub fn apply_state(&self, state: &AppState) {
        self.inner().apply_state(state);
    }
}

/// Main window. The close button hides the window instead of quitting.
pub struct HubWindow {
    window: adw::ApplicationWindow,
    view: Rc<HubView>,
}

impl HubWindow {
    pub fn new(app: &adw::Application, controller: Arc<dyn Controller>) -> HubWindow {
        let view = HubView::new(controller);
        let header = adw::HeaderBar::new();
        header.add_css_class("flat");
        header.set_show_title(false);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(view.widget()));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(APP_NAME)
            .default_width(1100)
            .default_height(720)
            .content(&toolbar)
            .build();
        window.set_size_request(900, 600);
        let v = view.clone();
        window.connect_close_request(move |w| {
            v.on_hidden();
            w.set_visible(false);
            glib::Propagation::Stop
        });
        HubWindow { window, view }
    }

    /// Rebuilds the interface after a language change (`linuxflow_i18n::set_language`).
    pub fn rebuild_for_language(&self) {
        self.view.rebuild_for_language();
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn hide(&self) {
        self.view.on_hidden();
        self.window.set_visible(false);
    }

    pub fn handle_event(&self, event: Event) {
        self.view.handle_event(event);
    }

    pub fn set_state_store(&self, store: Rc<dyn StateStore>) {
        self.view.set_state_store(store);
    }

    pub fn view(&self) -> &Rc<HubView> {
        &self.view
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_ids_round_trip() {
        for p in [
            PageId::Home,
            PageId::History,
            PageId::Dictionary,
            PageId::Snippets,
            PageId::Models,
            PageId::Settings,
            PageId::Troubleshooting,
        ] {
            assert_eq!(PageId::from_id(p.id()), Some(p));
        }
        assert_eq!(PageId::from_id("compat"), Some(PageId::Troubleshooting));
        assert_eq!(PageId::from_id("nope"), None);
        assert_eq!(PageId::from_event(Page::Models), PageId::Models);
        assert_eq!(PageId::Troubleshooting.nav_index(), PageId::NAV.len() - 1);
    }

    #[test]
    fn memory_store_keeps_the_last_page() {
        let s = MemoryStateStore::default();
        assert_eq!(s.load_page(), None);
        s.save_page("history");
        assert_eq!(s.load_page().as_deref(), Some("history"));
    }
}
