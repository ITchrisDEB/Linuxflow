//! Development and distribution tools of LinuxFlow, in Rust (they replace shell scripts).
//!
//!   cargo xtask notices [--check]      THIRD_PARTY_NOTICES.md
//!   cargo xtask licenses [FILE]        complete text of the third-party licences
//!   cargo xtask dist [options]         distribution tar.gz archive
//!   cargo xtask install [options]      builds and installs (equivalent of `make install`)
//!   cargo xtask static-checks          static checks
//!   cargo xtask check                  complete quality gate
//!
//! See `cargo xtask help`.

mod appimage;
mod checks;
mod common;
mod dist;
mod install;
mod licenses;
mod mo;
mod notices;

use common::Result;
use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "\
cargo xtask <command>

  notices [--check]     writes (or checks) THIRD_PARTY_NOTICES.md
  licenses [FILE]       writes the complete text of the third-party licences
  static-checks         static checks (T-HARD, T-ID, T-PRIV, T-NET, T-LIC)
  check                 complete quality gate (fmt, clippy, tests, deny, notices, static checks, installer)
  appimage [options]    builds the complete AppImage (bundled GTK, Ubuntu 24.04 base)
  dist [options]        builds the tar.gz archive (see `cargo xtask dist --help`)
  install [options]     builds and installs (equivalent of `make && make install`, see --help)
  mo <po> <mo>          compiles a .po catalogue into a .mo
  help                  this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: &[String]) -> Result<()> {
    let root = common::root();
    match args.first().map(String::as_str) {
        Some("notices") => notices::run(&root, args.iter().any(|a| a == "--check")),
        Some("static-checks") => checks::static_checks(&root).and_then(|ok| {
            if ok {
                Ok(())
            } else {
                Err("static checks failed".into())
            }
        }),
        Some("check") => checks::full_check(&root).and_then(|ok| {
            if ok {
                Ok(())
            } else {
                Err("quality checks failed".into())
            }
        }),
        Some("appimage") => appimage::run(&root, &args[1..]),
        Some("dist") => dist::run(&root, &args[1..]),
        Some("install") => install::run(&root, &args[1..]),
        Some("mo") => match (args.get(1), args.get(2)) {
            (Some(po), Some(out)) => {
                let text = std::fs::read_to_string(po).map_err(|e| e.to_string())?;
                std::fs::write(out, mo::compile(&text)?).map_err(|e| e.to_string())
            }
            _ => Err("usage: cargo xtask mo <file.po> <file.mo>".into()),
        },
        Some("licenses") => licenses::run(&root, args.get(1).map(PathBuf::from).as_deref()),
        Some("help") | Some("-h") | Some("--help") | None => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}\n{HELP}")),
    }
}
