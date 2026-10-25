//! Hub widget tests. GTK requires the main thread: custom harness (`harness = false`).
//!
//! No window is presented: the widgets stay unmapped. If GTK cannot
//! initialise (no display), the tests are skipped (exit code 0).

#[path = "../examples/support/fake_controller.rs"]
mod fake_controller;

use fake_controller::FakeController;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use linuxflow_core::domain::{ModelState, Phase};
use linuxflow_core::events::{Event, Page};
use linuxflow_core::traits::Controller;
use linuxflow_gtk::hub::{HubView, MemoryStateStore, PageId, StateStore};
use linuxflow_gtk::shortcut_recorder::{Modifier, Press};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let ctx = glib::MainContext::default();
    while Instant::now() < end {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn setup() -> (Arc<FakeController>, Rc<HubView>) {
    let c = FakeController::new();
    let v = HubView::new(c.clone());
    pump(60);
    (c, v)
}

use std::sync::Arc;

fn check(name: &str, ok: bool, failures: &mut Vec<String>) {
    if ok {
        println!("test {name} ... ok");
    } else {
        println!("test {name} ... FAILED");
        failures.push(name.to_string());
    }
}

fn main() {
    if gtk::init().is_err() || libadwaita::init().is_err() {
        println!("GTK unavailable (no display): widget tests skipped");
        return;
    }
    let mut f = Vec::new();

    {
        // Weak microphone: automatic at first; the user can turn it off (`micBoost`).
        let (c, v) = setup();
        check(
            "mic_boost_starts_automatic",
            v.settings().mic_boost_selected() == 1,
            &mut f,
        );
        v.settings().select_mic_boost(0);
        pump(100);
        check(
            "mic_boost_can_be_turned_off",
            c.calls()
                .iter()
                .any(|x| x.starts_with("set_settings") && x.contains("\"micBoost\":\"off\"")),
            &mut f,
        );
    }

    {
        // Privacy: the history is turned off from the Settings (`historyEnabled` setting).
        let (c, v) = setup();
        check(
            "history_switch_starts_on",
            v.settings().history_enabled(),
            &mut f,
        );
        v.settings().set_history_enabled(false);
        pump(100);
        check(
            "history_switch_sends_the_setting",
            c.calls()
                .iter()
                .any(|x| x.starts_with("set_settings") && x.contains("\"historyEnabled\":false")),
            &mut f,
        );
    }

    {
        let (_, v) = setup();
        check(
            "initial_state_fills_pages",
            v.history().len() == 4 && v.dictionary().len() == 3 && v.snippets().len() == 2,
            &mut f,
        );
        let (kind, text) = v.pill();
        check(
            "pill_ready",
            kind.is_some() && text.contains("small") && text.contains("ready"),
            &mut f,
        );
        check("no_wizard_when_first_run_done", !v.wizard_visible(), &mut f);
    }

    {
        let (c, v) = setup();
        let mut s = c.state();
        s.history.remove(0);
        s.history[0].text = "Changed".into();
        v.handle_event(Event::State(Box::new(s)));
        check("state_diff_updates_history", v.history().len() == 3, &mut f);
    }

    {
        let (_, v) = setup();
        v.handle_event(Event::InsertText("Hello".into()));
        v.handle_event(Event::InsertText("world".into()));
        check(
            "insert_text_adds_separator",
            v.home().try_text() == "Hello world",
            &mut f,
        );
        check(
            "placeholder_hidden_after_insert",
            !v.home().placeholder_visible(),
            &mut f,
        );
    }

    {
        let (c, v) = setup();
        v.home().mic_button().emit_clicked();
        check(
            "mic_toggles_dictation_to_hub",
            c.calls().iter().any(|x| x == "dictation_toggle true"),
            &mut f,
        );
        check(
            "phase_listening_after_click",
            v.home().phase() == Phase::Listening,
            &mut f,
        );
        v.handle_event(Event::Phase {
            phase: Phase::Idle,
            message: None,
        });
        check("phase_idle_resets", v.home().phase() == Phase::Idle, &mut f);
    }

    {
        let (_, v) = setup();
        v.handle_event(Event::Navigate(Page::Models));
        check("navigate_event", v.current_page() == PageId::Models, &mut f);
        let store = Rc::new(MemoryStateStore::default());
        v.set_state_store(store.clone());
        v.show_page(PageId::History);
        check(
            "page_is_saved",
            store.load_page().as_deref() == Some("history"),
            &mut f,
        );
        v.show_page(PageId::Troubleshooting);
        check(
            "troubleshooting_page_is_saved",
            store.load_page().as_deref() == Some("troubleshooting"),
            &mut f,
        );
        let (_, v2) = setup();
        v2.set_state_store(store);
        check(
            "page_is_restored",
            v2.current_page() == PageId::Troubleshooting,
            &mut f,
        );
    }

    {
        let (_, v) = setup();
        v.handle_event(Event::Toast("Saved".into()));
        check(
            "toast_event",
            v.toast().last_message().as_deref() == Some("Saved"),
            &mut f,
        );
        v.handle_event(Event::Phase {
            phase: Phase::Error,
            message: Some("Boom".into()),
        });
        check(
            "error_phase_toasts",
            v.toast().last_message().as_deref() == Some("Boom"),
            &mut f,
        );
    }

    {
        let (c, v) = setup();
        v.dictionary().set_test_text("I use cooper nettys");
        pump(40);
        let early = c
            .calls()
            .iter()
            .filter(|x| x.starts_with("format_preview"))
            .count();
        pump(300);
        let late = c
            .calls()
            .iter()
            .filter(|x| x.starts_with("format_preview"))
            .count();
        check("preview_is_debounced", early == 0 && late == 1, &mut f);
        check(
            "preview_text",
            v.dictionary().preview_text() == "I use Kubernetes",
            &mut f,
        );
    }

    {
        let (c, v) = setup();
        let entry = c.state().dictionary[0].clone();
        v.dictionary().start_edit(&entry);
        check(
            "edit_mode",
            matches!(
                v.dictionary().mode(),
                linuxflow_gtk::hub::pages::dictionary::FormMode::Edit(_)
            ),
            &mut f,
        );
        v.dictionary().submit();
        check(
            "edit_submits_update",
            c.calls()
                .iter()
                .any(|x| x.starts_with("dictionary_update Docker|Docker|")),
            &mut f,
        );
        check(
            "form_reset_after_submit",
            v.dictionary().mode() == linuxflow_gtk::hub::pages::dictionary::FormMode::Add,
            &mut f,
        );
    }

    {
        let (c, v) = setup();
        let rec = v.settings().recorder().clone();
        rec.start_recording();
        check(
            "recording_suspends_shortcut",
            c.calls().iter().any(|x| x == "shortcut_suspend true"),
            &mut f,
        );
        let none = BTreeSet::new();
        rec.key_pressed(37, Press::Modifier(Modifier::Ctrl), &none);
        rec.key_pressed(
            40,
            Press::Key("K".into()),
            &BTreeSet::from([Modifier::Ctrl]),
        );
        check(
            "recorder_pending",
            rec.pending().as_deref() == Some("Ctrl+K"),
            &mut f,
        );
        rec.confirm();
        check(
            "confirm_tests_then_saves",
            c.calls().iter().any(|x| x == "shortcut_test Ctrl+K")
                && c.calls()
                    .iter()
                    .any(|x| x.starts_with("set_settings") && x.contains("Ctrl+K")),
            &mut f,
        );
        check(
            "recording_resumes_shortcut",
            c.calls().iter().any(|x| x == "shortcut_suspend false"),
            &mut f,
        );
        // Shortcut refused by the controller.
        rec.start_recording();
        rec.key_pressed(37, Press::Modifier(Modifier::Ctrl), &none);
        rec.key_pressed(
            24,
            Press::Key("Q".into()),
            &BTreeSet::from([Modifier::Ctrl]),
        );
        rec.confirm();
        check(
            "rejected_shortcut_shows_error",
            rec.view().hint.contains("already used") && rec.pending().is_some(),
            &mut f,
        );
        rec.cancel();
        check("cancel_clears_pending", rec.pending().is_none(), &mut f);
    }

    {
        let (c, v) = setup();
        v.show_page(PageId::Settings);
        let rec = v.settings().recorder().clone();
        rec.start_recording();
        v.show_page(PageId::Home);
        check(
            "leaving_settings_cancels_recording",
            !rec.is_recording(),
            &mut f,
        );
        let n = c
            .calls()
            .iter()
            .filter(|x| *x == "shortcut_suspend false")
            .count();
        check("leaving_settings_resumes_shortcut", n == 1, &mut f);
    }

    {
        let (c, v) = setup();
        let mut s = c.state();
        s.ptt_available = false;
        v.handle_event(Event::State(Box::new(s)));
        check(
            "state_applies_without_panic_when_ptt_unavailable",
            true,
            &mut f,
        );
    }

    {
        let (_, v) = setup();
        v.show_page(PageId::Models);
        pump(150);
        check("models_listed", v.models().models().len() == 6, &mut f);
        let mut m = v.models().models();
        if let Some(x) = m.iter_mut().find(|x| x.id == "large-v3-q5_0") {
            x.state = ModelState::Downloading {
                done: 900_000_000,
                total: 1_081_140_203,
            };
        }
        v.models().set_models(m);
        check(
            "progress_update_keeps_rows",
            v.models().models().len() == 6,
            &mut f,
        );
    }

    {
        let (_, v) = setup();
        v.show_page(PageId::Troubleshooting);
        pump(150);
        check("compat_rows", v.compat().row_count() == 14, &mut f);
        check(
            "troubleshooting_is_highlighted_in_the_sidebar",
            v.selected_nav_index() == Some(PageId::NAV.len() - 1),
            &mut f,
        );
    }

    {
        let (c, v) = setup();
        let mut s = c.state();
        s.settings.first_run_done = false;
        v.handle_event(Event::State(Box::new(s)));
        pump(150);
        check("wizard_shown_on_first_run", v.wizard_visible(), &mut f);
        check(
            "wizard_preselects_recommended",
            v.wizard().choices().model.as_deref() == Some("small"),
            &mut f,
        );
        v.wizard().next_step();
        v.wizard().next_step();
        v.wizard().next_step();
        check(
            "wizard_reaches_summary",
            v.wizard().choices().step == linuxflow_gtk::wizard::logic::Step::Summary,
            &mut f,
        );
        // Finish: calls complete_first_run then returns to the application.
        let mut s2 = c.state();
        s2.settings.first_run_done = false;
        v.handle_event(Event::State(Box::new(s2)));
        check(
            "wizard_not_reset_by_state",
            v.wizard().choices().step == linuxflow_gtk::wizard::logic::Step::Summary,
            &mut f,
        );
    }

    french(&mut f);

    if f.is_empty() {
        println!("\ntest result: ok.");
    } else {
        println!("\ntest result: FAILED. {} failed: {f:?}", f.len());
        std::process::exit(1);
    }
}

/// Text of all the `Label`s of a widget tree.
fn all_text(w: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        out.push(l.text().to_string());
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        all_text(&child, out);
        c = child.next_sibling();
    }
}

fn texts(v: &HubView) -> Vec<String> {
    let mut out = Vec::new();
    all_text(v.widget(), &mut out);
    out
}

/// Compiles `po/fr.po` in a temporary folder (without msgfmt: test skipped).
fn french_catalog() -> Option<tempfile::TempDir> {
    let po = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../po/fr.po");
    let dir = tempfile::tempdir().ok()?;
    let out = dir.path().join("fr/LC_MESSAGES");
    std::fs::create_dir_all(&out).ok()?;
    let status = std::process::Command::new("msgfmt")
        .arg("-o")
        .arg(out.join("linuxflow.mo"))
        .arg(&po)
        .status()
        .ok()?;
    status.success().then_some(dir)
}

fn french(f: &mut Vec<String>) {
    let Some(dir) = french_catalog() else {
        println!("test french_* ... skipped (msgfmt ou po/fr.po indisponible)");
        return;
    };
    linuxflow_i18n::init("linuxflow", Some(dir.path()));
    linuxflow_i18n::set_language(Some("en"));
    let (c, v) = setup();
    let english = texts(&v);
    check(
        "english_labels_before_switch",
        english.iter().any(|t| t == "History") && english.iter().any(|t| t == "Settings"),
        f,
    );

    // Inputs and page to keep during the rebuild.
    v.show_page(PageId::Dictionary);
    v.dictionary()
        .restore_form(&linuxflow_gtk::hub::pages::dictionary::FormValues {
            word: "Docker".into(),
            sounds: "dock her".into(),
            test: "abc".into(),
            mode: linuxflow_gtk::hub::pages::dictionary::FormMode::Edit("Docker".into()),
        });
    v.home().set_try_text("kept text");

    // The interface language selector calls set_settings then rebuilds (deferred).
    v.settings().select_ui_language(2);
    pump(150);
    check(
        "ui_language_saved_in_settings",
        c.calls()
            .iter()
            .any(|x| x.starts_with("set_settings") && x.contains("\"uiLanguage\":\"fr\"")),
        f,
    );
    check(
        "language_is_french",
        linuxflow_i18n::current_language() == "fr",
        f,
    );
    let fr = texts(&v);
    for label in ["Historique", "Paramètres", "Dictionnaire", "Modèles"] {
        check(
            &format!("french_label_{label}"),
            fr.iter().any(|t| t == label),
            f,
        );
    }
    check("english_nav_gone", !fr.iter().any(|t| t == "History"), f);
    check(
        "page_kept_after_rebuild",
        v.current_page() == PageId::Dictionary,
        f,
    );
    check(
        "input_kept_after_rebuild",
        v.home().try_text() == "kept text"
            && v.dictionary().form_values().word == "Docker"
            && v.dictionary().form_values().test == "abc",
        f,
    );
    check(
        "edit_mode_kept_after_rebuild",
        matches!(
            v.dictionary().mode(),
            linuxflow_gtk::hub::pages::dictionary::FormMode::Edit(_)
        ),
        f,
    );
    check(
        "rebuilt_pages_are_live",
        v.history().len() == 4 && v.pill().1.contains("prêt"),
        f,
    );

    // Back to English: selector set back on "English".
    v.settings().select_ui_language(1);
    pump(150);
    check(
        "back_to_english",
        texts(&v).iter().any(|t| t == "History"),
        f,
    );
    linuxflow_i18n::set_language(Some("en"));
}
