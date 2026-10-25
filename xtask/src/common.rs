//! Shared utilities: repository root, running Cargo commands, version sorting.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub type Result<T> = std::result::Result<T, String>;

/// Converts any displayable error into a message.
pub trait Ctx<T> {
    fn ctx(self, what: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display> Ctx<T> for std::result::Result<T, E> {
    fn ctx(self, what: &str) -> Result<T> {
        self.map_err(|e| format!("{what}: {e}"))
    }
}

/// Root of the LinuxFlow repository (the folder that contains `xtask/`).
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Runs a command and returns its standard output (error if the exit code is not 0).
pub fn capture(dir: &Path, program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ctx(&format!("cannot run {program}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            err.lines().next().unwrap_or("")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a command leaving its output on screen; returns `true` if it succeeds.
pub fn run(dir: &Path, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Is the program found in the PATH?
pub fn have(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|d| {
            let p = d.join(program);
            p.is_file()
        })
    })
}

/// Natural "version" sort (equivalent of `sort -V` for Cargo version numbers):
/// digit runs compare as numbers, the rest byte by byte.
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (da, db) = (trim_zeros(&a[..na]), trim_zeros(&b[..nb]));
                let ord = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[na..];
                b = &b[nb..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|c| **c == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_sort_naturally() {
        let mut v = vec!["0.10.1", "0.9.0", "0.9.10", "0.9.2", "1.0.0"];
        v.sort_by(|a, b| version_cmp(a, b));
        assert_eq!(v, ["0.9.0", "0.9.2", "0.9.10", "0.10.1", "1.0.0"]);
        assert_eq!(version_cmp("1.0", "1.0"), Ordering::Equal);
        assert_eq!(version_cmp("1.00", "1.0"), Ordering::Equal);
        assert_eq!(version_cmp("1.0", "1.0.1"), Ordering::Less);
    }
}
