//! Command line (clap): control options, directories, test tools.

use clap::{CommandFactory, FromArgMatches, Parser, ValueEnum};
use linuxflow_core::events::Page;
use linuxflow_i18n::{tr, tr_fmt};
use std::ffi::OsString;
use std::path::PathBuf;

/// Pages that can be opened with `--page`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PageArg {
    Home,
    History,
    Dictionary,
    Snippets,
    Models,
    Settings,
}

impl PageArg {
    pub fn to_page(self) -> Page {
        match self {
            PageArg::Home => Page::Home,
            PageArg::History => Page::History,
            PageArg::Dictionary => Page::Dictionary,
            PageArg::Snippets => Page::Snippets,
            PageArg::Models => Page::Models,
            PageArg::Settings => Page::Settings,
        }
    }
}

#[derive(Debug, Clone, Default, Parser)]
#[command(name = "linuxflow", version, about = "Local voice dictation")]
pub struct Cli {
    /// Toggles dictation (starts or stops it) in the running instance.
    #[arg(long)]
    pub toggle: bool,
    /// Starts dictation.
    #[arg(long)]
    pub start: bool,
    /// Stops dictation and transcribes.
    #[arg(long)]
    pub stop: bool,
    /// Cancels the current dictation.
    #[arg(long)]
    pub cancel: bool,
    /// Opens the hub on this page.
    #[arg(long, value_enum)]
    pub page: Option<PageArg>,
    /// Quits the running instance.
    #[arg(long)]
    pub quit: bool,
    /// Starts without showing the hub.
    #[arg(long)]
    pub background: bool,
    /// First name shown in the greeting (never read from the system).
    #[arg(long, value_name = "NAME")]
    pub display_name: Option<String>,
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    #[arg(long, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    #[arg(long, value_name = "DIR")]
    pub models_dir: Option<PathBuf>,
    /// Interface-only mode: no transcription engine is started.
    #[arg(long)]
    pub no_engine: bool,
    /// Test tool: transcribes this WAV without pasting or touching the clipboard.
    #[arg(long, value_name = "WAV")]
    pub test_pipeline: Option<PathBuf>,
    /// With `--test-pipeline`: exits after the result (always the case here).
    #[arg(long)]
    pub exit: bool,
    /// Development tool: renders the hub pages to PNG (Broadway only).
    #[arg(long, value_name = "DIR", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    pub screenshots: Option<String>,
}

/// Early exit of the command line (help, version or error), already translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliExit {
    pub code: i32,
    pub text: String,
    pub to_stderr: bool,
}

/// clap command whose visible texts all go through `tr()`.
pub fn localized_command() -> clap::Command {
    let usage_heading = tr("Usage:");
    let options_heading = tr("Options:");
    let template =
        format!("{{about}}\n\n{usage_heading} {{usage}}\n\n{options_heading}\n{{options}}");
    let mut cmd = Cli::command()
        .about(tr("Local voice dictation"))
        .help_template(template)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(
            clap::Arg::new("help")
                .short('h')
                .long("help")
                .action(clap::ArgAction::Help)
                .help(tr("Print help")),
        )
        .arg(
            clap::Arg::new("version")
                .short('V')
                .long("version")
                .action(clap::ArgAction::Version)
                .help(tr("Print version")),
        );
    let helps: [(&str, String); 17] = [
        (
            "toggle",
            tr("Start or stop dictation in the running instance"),
        ),
        ("start", tr("Start dictation")),
        ("stop", tr("Stop dictation and transcribe")),
        ("cancel", tr("Cancel the current dictation")),
        ("page", tr("Open the main window on this page")),
        ("quit", tr("Quit the running instance")),
        ("background", tr("Start without showing the main window")),
        ("display_name", tr("First name shown in the greeting")),
        ("config_dir", tr("Configuration folder")),
        ("data_dir", tr("Data folder")),
        ("cache_dir", tr("Cache folder")),
        ("state_dir", tr("State and logs folder")),
        ("models_dir", tr("Models folder")),
        (
            "no_engine",
            tr("Interface-only mode: no transcription engine"),
        ),
        (
            "test_pipeline",
            tr("Test tool: transcribe a WAV file without pasting"),
        ),
        ("exit", tr("With --test-pipeline: exit after the result")),
        (
            "screenshots",
            tr("Development tool: render the pages to PNG files (Broadway only)"),
        ),
    ];
    for (id, help) in helps {
        cmd = cmd.mut_arg(id, move |a| a.help(help));
    }
    // clap displays the value names and the list of possible values in English:
    // they are replaced by texts that go through `tr()`.
    let values = [
        ("page", tr("PAGE")),
        ("display_name", tr("NAME")),
        ("config_dir", tr("DIR")),
        ("data_dir", tr("DIR")),
        ("cache_dir", tr("DIR")),
        ("state_dir", tr("DIR")),
        ("models_dir", tr("DIR")),
        ("test_pipeline", tr("WAV")),
        ("screenshots", tr("DIR")),
    ];
    for (id, name) in values {
        // clap wants a `&'static str`: the command is built only once per process.
        let name: &'static str = Box::leak(name.into_boxed_str());
        cmd = cmd.mut_arg(id, move |a| a.value_name(name));
    }
    let pages = tr_fmt(
        "Open the main window on this page [possible values: {}]",
        &["home, history, dictionary, snippets, models, settings"],
    );
    cmd = cmd.mut_arg("page", move |a| a.help(pages).hide_possible_values(true));
    // The rendering tool only exists in development builds: it does not appear in the help.
    cmd = cmd.mut_arg("screenshots", |a| a.hide(!cfg!(feature = "dev-tools")));
    cmd
}

/// Parses the command line; the help, the version and the errors are translated.
pub fn parse_from<I, T>(args: I) -> Result<Cli, CliExit>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    use clap::error::ErrorKind;
    let matches = localized_command()
        .try_get_matches_from(args)
        .map_err(|e| match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => CliExit {
                code: 0,
                text: e.render().to_string(),
                to_stderr: false,
            },
            _ => {
                let detail = e.render().to_string();
                let first = detail.lines().next().unwrap_or("").trim().to_string();
                CliExit {
                    code: 2,
                    text: format!(
                        "{}\n{}\n",
                        tr_fmt("Invalid command line: {}", &[&first]),
                        tr("Try --help for the list of options.")
                    ),
                    to_stderr: true,
                }
            }
        })?;
    Cli::from_arg_matches(&matches).map_err(|e| CliExit {
        code: 2,
        text: format!(
            "{}\n",
            tr_fmt("Invalid command line: {}", &[&e.to_string()])
        ),
        to_stderr: true,
    })
}

/// Action passed on to the primary instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAction {
    Toggle,
    Start,
    Stop,
    Cancel,
    Quit,
    ShowHub(Option<Page>),
}

impl Cli {
    /// `LINUXFLOW_*` environment variables to set before the paths are resolved.
    pub fn env_overrides(&self) -> Vec<(&'static str, PathBuf)> {
        use linuxflow_core::paths::*;
        let mut v = Vec::new();
        for (key, val) in [
            (ENV_CONFIG_DIR, &self.config_dir),
            (ENV_DATA_DIR, &self.data_dir),
            (ENV_CACHE_DIR, &self.cache_dir),
            (ENV_STATE_DIR, &self.state_dir),
            (ENV_MODEL_DIR, &self.models_dir),
        ] {
            if let Some(p) = val {
                v.push((key, p.clone()));
            }
        }
        v
    }

    /// True for the development tools that do not use the single instance.
    pub fn is_devtool(&self) -> bool {
        self.test_pipeline.is_some() || self.screenshots.is_some()
    }

    /// Requested actions, in order: dictation control, page, quit. With no control option,
    /// opens the hub.
    pub fn actions(&self) -> Vec<RemoteAction> {
        let mut out = Vec::new();
        if self.cancel {
            out.push(RemoteAction::Cancel);
        }
        if self.start {
            out.push(RemoteAction::Start);
        }
        if self.stop {
            out.push(RemoteAction::Stop);
        }
        if self.toggle {
            out.push(RemoteAction::Toggle);
        }
        if let Some(p) = self.page {
            out.push(RemoteAction::ShowHub(Some(p.to_page())));
        }
        if self.quit {
            out.push(RemoteAction::Quit);
        }
        if out.is_empty() && !self.background {
            out.push(RemoteAction::ShowHub(None));
        }
        out
    }

    /// True if, with no running instance, nothing must be started (quit, cancel, stop).
    pub fn is_noop_without_instance(&self) -> bool {
        (self.quit || self.cancel || self.stop)
            && !self.toggle
            && !self.start
            && self.page.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let mut v = vec!["linuxflow"];
        v.extend_from_slice(args);
        Cli::try_parse_from(v).expect("arguments valides")
    }

    #[test]
    fn no_args_opens_hub() {
        assert_eq!(parse(&[]).actions(), vec![RemoteAction::ShowHub(None)]);
    }

    #[test]
    fn background_does_nothing_remote() {
        assert!(parse(&["--background"]).actions().is_empty());
    }

    #[test]
    fn dictation_controls() {
        assert_eq!(parse(&["--toggle"]).actions(), vec![RemoteAction::Toggle]);
        assert_eq!(parse(&["--start"]).actions(), vec![RemoteAction::Start]);
        assert_eq!(parse(&["--stop"]).actions(), vec![RemoteAction::Stop]);
        assert_eq!(parse(&["--cancel"]).actions(), vec![RemoteAction::Cancel]);
        assert_eq!(parse(&["--quit"]).actions(), vec![RemoteAction::Quit]);
    }

    #[test]
    fn pages() {
        for (name, page) in [
            ("home", Page::Home),
            ("history", Page::History),
            ("dictionary", Page::Dictionary),
            ("snippets", Page::Snippets),
            ("models", Page::Models),
            ("settings", Page::Settings),
        ] {
            assert_eq!(
                parse(&["--page", name]).actions(),
                vec![RemoteAction::ShowHub(Some(page))]
            );
        }
        assert!(Cli::try_parse_from(["linuxflow", "--page", "nope"]).is_err());
    }

    #[test]
    fn directories_become_env() {
        let c = parse(&[
            "--config-dir=/a",
            "--data-dir=/b",
            "--cache-dir=/c",
            "--state-dir=/d",
            "--models-dir=/e",
        ]);
        let env = c.env_overrides();
        assert_eq!(env.len(), 5);
        assert!(env.contains(&("LINUXFLOW_CONFIG_DIR", PathBuf::from("/a"))));
        assert!(env.contains(&("LINUXFLOW_MODEL_DIR", PathBuf::from("/e"))));
    }

    #[test]
    fn misc_options() {
        let c = parse(&["--display-name", "Sam", "--no-engine"]);
        assert_eq!(c.display_name.as_deref(), Some("Sam"));
        assert!(c.no_engine);
        let t = parse(&["--test-pipeline", "x.wav", "--exit"]);
        assert!(t.is_devtool() && t.exit);
        assert_eq!(parse(&["--screenshots"]).screenshots.as_deref(), Some(""));
        assert_eq!(
            parse(&["--screenshots=out"]).screenshots.as_deref(),
            Some("out")
        );
    }

    #[test]
    fn noop_without_instance() {
        assert!(parse(&["--quit"]).is_noop_without_instance());
        assert!(parse(&["--cancel"]).is_noop_without_instance());
        assert!(!parse(&["--toggle"]).is_noop_without_instance());
        assert!(!parse(&[]).is_noop_without_instance());
    }

    #[test]
    fn localized_parse_handles_help_version_and_errors() {
        let v = parse_from(["linuxflow", "--version"]).expect_err("version");
        assert_eq!(v.code, 0);
        assert!(v.text.contains(env!("CARGO_PKG_VERSION")));
        let h = parse_from(["linuxflow", "--help"]).expect_err("help");
        assert_eq!(h.code, 0);
        assert!(h.text.contains("Usage:"));
        assert!(h.text.contains("--toggle"));
        assert!(h.text.contains("Quit the running instance"));
        let e = parse_from(["linuxflow", "--bogus"]).expect_err("error");
        assert_eq!(e.code, 2);
        assert!(e.to_stderr);
        assert!(e.text.starts_with("Invalid command line:"));
        assert!(parse_from(["linuxflow", "--toggle"]).unwrap().toggle);
    }

    #[test]
    fn version_flag_is_handled_by_clap() {
        let e = Cli::try_parse_from(["linuxflow", "--version"]).expect_err("version");
        assert_eq!(e.kind(), clap::error::ErrorKind::DisplayVersion);
    }
}
