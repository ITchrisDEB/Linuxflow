//! `cargo xtask install`: the equivalent of `make && make install`. Builds the
//! distribution archive (like `cargo xtask dist`), extracts it, then runs its `install.sh`. The installation
//! goes through the same code path as a published release.

use crate::common::{Ctx, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HELP: &str = "\
cargo xtask install [--system] [dist options] [-- install.sh options]

  (no option)     builds, then installs for the current user under ~/.local
  --system        builds with your account, then installs for all users under
                  /usr/local with sudo (the build never runs as root)

The options before `--` are those of `cargo xtask dist` (--no-vulkan, --out DIR...), those after
`--` are passed to install.sh (--prefix DIR, --dry-run, --with-uinput-rule...).

Examples:
  cargo xtask install
  cargo xtask install --system
  cargo xtask install --no-vulkan -- --dry-run
";

/// Most recent `linuxflow-*.tar.gz` of the folder.
fn newest_archive(dir: &Path) -> Result<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ctx("cannot read the output folder")?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("linuxflow-") && n.ends_with(".tar.gz"))
        })
        .collect();
    found.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
    found.pop().ok_or_else(|| "no archive was produced".into())
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let system = args[..split].iter().any(|a| a == "--system");
    let mut dist_args: Vec<String> = args[..split]
        .iter()
        .filter(|a| *a != "--system")
        .cloned()
        .collect();
    let installer_args: Vec<String> = args.get(split + 1..).unwrap_or(&[]).to_vec();

    // output folder: the requested one, otherwise the default folder of `dist`
    let out = dist_args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| dist_args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/dist"));
    if !dist_args.iter().any(|a| a == "--out") {
        dist_args.extend(["--out".into(), out.display().to_string()]);
    }
    crate::dist::run(root, &dist_args)?;

    let archive = newest_archive(&out)?;
    let stage = out.join("install-stage");
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage).ctx("cannot create the staging folder")?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&stage)
        .status()
        .ctx("cannot run tar")?;
    if !status.success() {
        return Err("could not extract the archive".into());
    }
    let dir = fs::read_dir(&stage)
        .ctx("cannot read the staging folder")?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or("the archive is empty")?;

    let mut cmd = if system {
        let mut c = Command::new("sudo");
        c.arg("./install.sh");
        c
    } else {
        Command::new("./install.sh")
    };
    cmd.current_dir(&dir).args(&installer_args);
    println!(
        "\n=> {}",
        if system {
            "sudo ./install.sh"
        } else {
            "./install.sh"
        }
    );
    let status = cmd.status().ctx("cannot run install.sh")?;
    let _ = fs::remove_dir_all(&stage);
    if status.success() {
        Ok(())
    } else {
        Err("install.sh failed".into())
    }
}
