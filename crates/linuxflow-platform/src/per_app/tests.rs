#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn win(app: &str) -> WindowInfo {
    WindowInfo {
        app_id: Some(app.to_string()),
        ..WindowInfo::default()
    }
}

#[test]
fn embedded_table_parses_and_is_not_empty() {
    assert_eq!(
        parse_layer(EMBEDDED_TABLE).map(|m| m.len()),
        Ok(PerAppTable::embedded().len())
    );
    assert!(PerAppTable::embedded().len() >= 20);
}

#[test]
fn terminals_use_ctrl_shift_v() {
    let table = PerAppTable::embedded();
    for app in [
        "kitty",
        "alacritty",
        "foot",
        "org.wezfurlong.wezterm",
        "org.gnome.Terminal",
        "org.gnome.Console",
        "org.gnome.Ptyxis",
        "konsole",
        "xterm",
        "URxvt",
        "tilix",
        "terminator",
        "com.mitchellh.ghostty",
        "xfce4-terminal",
        "st",
    ] {
        assert_eq!(
            table.resolve(Some(&win(app)), 60),
            (PasteMode::CtrlShiftV, 60),
            "{app}"
        );
    }
}

#[test]
fn chat_apps_have_their_delay() {
    let table = PerAppTable::embedded();
    for (app, delay) in [
        ("slack", 120),
        ("teams-for-linux", 120),
        ("org.telegram.desktop", 100),
        ("discord", 100),
        ("signal-desktop", 100),
    ] {
        assert_eq!(
            table.resolve(Some(&win(app)), 60),
            (PasteMode::CtrlV, delay),
            "{app}"
        );
    }
}

#[test]
fn unknown_or_missing_window_is_ctrl_v_with_default_delay() {
    let table = PerAppTable::embedded();
    assert_eq!(
        table.resolve(Some(&win("firefox")), 75),
        (PasteMode::CtrlV, 75)
    );
    assert_eq!(table.resolve(None, 60), (PasteMode::CtrlV, 60));
    assert_eq!(
        table.resolve(Some(&WindowInfo::default()), 60),
        (PasteMode::CtrlV, 60)
    );
}

#[test]
fn lookup_ignores_case_and_desktop_suffix() {
    let table = PerAppTable::embedded();
    assert_eq!(
        table.resolve(Some(&win("KITTY")), 60).0,
        PasteMode::CtrlShiftV
    );
    assert_eq!(
        table.resolve(Some(&win("kitty.desktop")), 60).0,
        PasteMode::CtrlShiftV
    );
}

#[test]
fn user_file_overrides_field_by_field() {
    let user = r#"
        [apps.kitty]
        mode = "shift_insert"
        [apps.slack]
        delay_ms = 200
        [apps.Firefox]
        mode = "ctrl_v"
        delay_ms = 10
        [apps.alacritty]
        delay_ms = 90
    "#;
    let table = PerAppTable::from_user_toml(user).unwrap();
    assert_eq!(
        table.resolve(Some(&win("kitty")), 60),
        (PasteMode::ShiftInsert, 60)
    );
    assert_eq!(
        table.resolve(Some(&win("slack")), 60),
        (PasteMode::CtrlV, 200)
    );
    assert_eq!(
        table.resolve(Some(&win("firefox")), 60),
        (PasteMode::CtrlV, 10)
    );
    // Only the delay is overridden: the embedded mode stays.
    assert_eq!(
        table.resolve(Some(&win("alacritty")), 60),
        (PasteMode::CtrlShiftV, 90)
    );
    // The rest is intact.
    assert_eq!(
        table.resolve(Some(&win("foot")), 60).0,
        PasteMode::CtrlShiftV
    );
}

#[test]
fn invalid_user_toml_is_rejected() {
    assert!(matches!(
        PerAppTable::from_user_toml("apps = ["),
        Err(PerAppError::Parse(_))
    ));
    assert!(matches!(
        PerAppTable::from_user_toml("[apps.x]\nmode = \"ctrl_w\""),
        Err(PerAppError::UnknownMode { .. })
    ));
    assert!(matches!(
        PerAppTable::from_user_toml("[apps.x]\nbogus = 1"),
        Err(PerAppError::Parse(_))
    ));
}

#[test]
fn load_reads_user_file_and_survives_bad_or_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.toml");
    std::fs::write(&good, "[apps.foo]\nmode = \"shift_insert\"\n").unwrap();
    let table = PerAppTable::load(Some(&good));
    assert_eq!(
        table.resolve(Some(&win("foo")), 60).0,
        PasteMode::ShiftInsert
    );

    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "not = [valid").unwrap();
    assert_eq!(PerAppTable::load(Some(&bad)), PerAppTable::embedded());
    assert_eq!(
        PerAppTable::load(Some(&dir.path().join("absent.toml"))),
        PerAppTable::embedded()
    );
    assert_eq!(PerAppTable::load(None), PerAppTable::embedded());
}

#[test]
fn mode_names_round_trip() {
    for mode in [
        PasteMode::CtrlV,
        PasteMode::CtrlShiftV,
        PasteMode::ShiftInsert,
    ] {
        assert_eq!(parse_mode(mode_to_str(mode)), Some(mode));
    }
    assert_eq!(parse_mode("nope"), None);
}
