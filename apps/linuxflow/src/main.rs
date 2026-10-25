#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! LinuxFlow: 100 % local voice dictation.

pub mod adapters;
pub mod app;
pub mod cli;
pub mod clipboard_gdk;
pub mod devtools;
pub mod engine;
pub mod engine_ctl;
pub mod hotkeys;
#[cfg(test)]
pub mod i18n_tests;
pub mod library;
pub mod logging;
pub mod models;
pub mod runtime;
#[cfg(feature = "dev-tools")]
pub mod screenshots;
pub mod service;
pub mod signals;
pub mod system;
#[cfg(test)]
pub mod testkit;
pub mod ui_text;
pub mod wiring;

use linuxflow_core::paths::{AppPaths, SystemEnv};
use std::ffi::OsString;
use std::process::ExitCode;

/// Gettext domain of the application.
const I18N_DOMAIN: &str = "linuxflow";

fn main() -> ExitCode {
    // 1. "Child process" modes: before GTK, tokio, the configuration or the log.
    if let Some(code) = linuxflow_stt::dispatch_subprocess_mode() {
        return exit_code(code);
    }
    if std::env::args().nth(1).as_deref() == Some(linuxflow_gtk::overlay::WORKER_ARG) {
        return linuxflow_gtk::overlay::worker_main();
    }

    // 2. Local time offset: before any thread.
    linuxflow_core::stats::init_local_offset();

    // 3. Translation (system language), then the translated command line.
    linuxflow_i18n::init(I18N_DOMAIN, None);
    let args: Vec<OsString> = std::env::args_os().collect();
    let cli = match cli::parse_from(args.clone()) {
        Ok(c) => c,
        Err(exit) => {
            if exit.to_stderr {
                eprint!("{}", exit.text);
            } else {
                print!("{}", exit.text);
            }
            return exit_code(exit.code);
        }
    };

    // 4. Directories (the options set the variables before resolution).
    for (key, dir) in cli.env_overrides() {
        std::env::set_var(key, dir);
    }
    let paths = match AppPaths::resolve(&SystemEnv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "{}",
                linuxflow_i18n::tr_fmt(
                    "LinuxFlow could not find its folders: {}",
                    &[&e.to_string()]
                )
            );
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = paths.ensure_dirs() {
        eprintln!(
            "{}",
            linuxflow_i18n::tr_fmt(
                "LinuxFlow could not create its folders: {}",
                &[&e.to_string()]
            )
        );
        return ExitCode::FAILURE;
    }

    // 5. Log, then the interface language chosen in the settings.
    let _log = logging::init(&paths.logs_dir);
    let ui_language = linuxflow_core::settings::SettingsStore::open_silent(paths.settings_file())
        .get()
        .ui_language;
    linuxflow_i18n::set_language(ui_language.as_deref());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    // 6. Development tools, otherwise the application.
    if let Some(wav) = &cli.test_pipeline {
        return exit_code(devtools::test_pipeline(wav, &paths));
    }
    if let Some(dir) = &cli.screenshots {
        #[cfg(feature = "dev-tools")]
        return exit_code(screenshots::run(dir, &paths));
        #[cfg(not(feature = "dev-tools"))]
        {
            let _ = dir;
            eprintln!(
                "{}",
                linuxflow_i18n::tr(
                    "This option is only available in development builds (feature dev-tools)."
                )
            );
            return std::process::ExitCode::from(2);
        }
    }
    let code = app::run(cli, paths, args);
    // glib::ExitCode does not expose its value: the conversion goes through `From`.
    code.into_raw_exit()
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(code.clamp(0, 255) as u8)
}

trait IntoRaw {
    fn into_raw_exit(self) -> ExitCode;
}

impl IntoRaw for gtk4::glib::ExitCode {
    fn into_raw_exit(self) -> ExitCode {
        ExitCode::from(self.get())
    }
}
