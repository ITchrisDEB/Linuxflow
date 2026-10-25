//! `THIRD_PARTY_NOTICES.md`: Rust dependencies (name, version, licence, repository) taken from the real
//! Cargo graph (`cargo tree`), plus the components outside Cargo described in `notices_head.md`.
//! Only the "normal" dependencies (shipped in the binary) are listed.

use crate::common::{capture, version_cmp, Ctx, Result};
use std::collections::HashSet;
use std::path::Path;

const HEAD: &str = include_str!("notices_head.md");

struct Row {
    name: String,
    version: String,
    license: String,
    repo: String,
}

/// File content and number of crates.
pub fn render(root: &Path) -> Result<(String, usize)> {
    let tree = capture(
        root,
        "cargo",
        &[
            "tree",
            "--locked",
            "-e",
            "normal",
            "--prefix",
            "none",
            "--no-dedupe",
            "-f",
            "{p}|{l}|{r}",
        ],
    )
    .map_err(|e| format!("{e} (is Cargo.lock up to date? crates.io reachable?)"))?;
    let rows = parse_tree(&tree);
    let mut out = String::from(HEAD);
    for r in &rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            r.name, r.version, r.license, r.repo
        ));
    }
    out.push_str(&format!("\n{} Rust crates listed.\n", rows.len()));
    Ok((out, rows.len()))
}

fn parse_tree(tree: &str) -> Vec<Row> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for line in tree.lines() {
        let mut f = line.splitn(3, '|');
        let first = f.next().unwrap_or("");
        // workspace crates (local path) and empty lines: discarded
        if first.is_empty() || first.contains(" (/") {
            continue;
        }
        let mut p = first.split(' ');
        let name = p.next().unwrap_or("").to_string();
        let version = p.next().unwrap_or("").trim_start_matches('v').to_string();
        let license = match f.next().unwrap_or("") {
            "" => "see the crate sources (license-file)".to_string(),
            l => l.to_string(),
        };
        let repo = match f.next().unwrap_or("") {
            "" => "-".to_string(),
            r => r.to_string(),
        };
        if seen.insert(format!("{name} {version}")) {
            rows.push(Row {
                name,
                version,
                license,
                repo,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.name
            .as_bytes()
            .cmp(b.name.as_bytes())
            .then_with(|| version_cmp(&a.version, &b.version))
    });
    rows
}

/// `cargo xtask notices [--check]`
pub fn run(root: &Path, check: bool) -> Result<()> {
    let (text, n) = render(root)?;
    let file = root.join("THIRD_PARTY_NOTICES.md");
    if check {
        if std::fs::read_to_string(&file).ok().as_deref() == Some(text.as_str()) {
            println!("THIRD_PARTY_NOTICES.md is up to date.");
            return Ok(());
        }
        return Err("THIRD_PARTY_NOTICES.md is missing or stale: run `cargo xtask notices`".into());
    }
    std::fs::write(&file, text).ctx("cannot write THIRD_PARTY_NOTICES.md")?;
    println!("Wrote {} ({n} crates).", file.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_crates_and_blank_lines_are_skipped_and_duplicates_merged() {
        let tree = "\
linuxflow v0.1.0 (/home/x/linuxflow)|MIT|
serde v1.0.2|MIT OR Apache-2.0|https://github.com/serde-rs/serde
serde v1.0.2|MIT OR Apache-2.0|https://github.com/serde-rs/serde
|MIT|
syn v2.0.1 (proc-macro)||
hashbrown v0.9.0|MIT|
hashbrown v0.10.0|MIT|";
        let rows = parse_tree(tree);
        let names: Vec<_> = rows
            .iter()
            .map(|r| format!("{} {}", r.name, r.version))
            .collect();
        assert_eq!(
            names,
            [
                "hashbrown 0.9.0",
                "hashbrown 0.10.0",
                "serde 1.0.2",
                "syn 2.0.1"
            ]
        );
        assert_eq!(rows[3].license, "see the crate sources (license-file)");
        assert_eq!(rows[3].repo, "-");
    }
}
