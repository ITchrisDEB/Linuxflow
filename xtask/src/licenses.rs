//! COMPLETE text of the licences of the shipped components (copyright notices + licence text),
//! required by MIT / BSD / ISC / Apache-2.0 for a binary redistribution.
//!
//! - Rust dependencies: real graph (normal, Linux target); for each crate the
//!   LICENSE*, COPYING*, NOTICE*, UNLICENSE* files present at the root of its sources (Cargo
//!   registry) are taken. Identical texts are grouped (Apache-2.0 is written only once).
//! - Components outside Cargo: whisper.cpp / ggml (text taken from the whisper-rs-sys sources),
//!   models, system libraries (notices and origin of the sources).
//!
//! No hard-coded path: the Cargo registry is found through `cargo metadata`.

use crate::common::{capture, Ctx, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const HEAD: &str = include_str!("licenses_head.txt");

struct Krate {
    name: String,
    version: String,
    spdx: String,
    dir: PathBuf,
}

/// File content, number of crates, number of distinct texts.
pub fn render(root: &Path) -> Result<(String, usize, usize)> {
    let wanted = wanted_crates(root)?;
    let meta = capture(
        root,
        "cargo",
        &["metadata", "--locked", "--format-version", "1"],
    )?;
    let meta: serde_json::Value = serde_json::from_str(&meta).ctx("cargo metadata output")?;
    let mut crates: Vec<Krate> = Vec::new();
    for p in meta["packages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let name = p["name"].as_str().unwrap_or("").to_string();
        let version = p["version"].as_str().unwrap_or("").to_string();
        if !wanted.contains(&(name.clone(), version.clone())) {
            continue;
        }
        let manifest = p["manifest_path"].as_str().unwrap_or("");
        let dir = Path::new(manifest)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let spdx = p["license"]
            .as_str()
            .unwrap_or("see the crate sources")
            .to_string();
        crates.push(Krate {
            name,
            version,
            spdx,
            dir,
        });
    }
    crates.sort_by(|a, b| {
        (a.name.as_bytes(), a.version.as_bytes()).cmp(&(b.name.as_bytes(), b.version.as_bytes()))
    });
    crates.dedup_by(|a, b| a.name == b.name && a.version == b.version);

    // text hash -> (files per crate, first path encountered)
    let mut groups: BTreeMap<String, (BTreeSet<String>, PathBuf)> = BTreeMap::new();
    let mut no_license: BTreeSet<String> = BTreeSet::new();
    for c in &crates {
        let files = license_files(&c.dir);
        if files.is_empty() {
            no_license.insert(format!("{} {} [{}]", c.name, c.version, c.spdx));
            continue;
        }
        for f in files {
            let bytes = std::fs::read(&f).ctx(&format!("cannot read {}", f.display()))?;
            let hash = hex(&Sha256::digest(&bytes));
            let entry = groups
                .entry(hash)
                .or_insert_with(|| (BTreeSet::new(), f.clone()));
            entry
                .0
                .insert(format!("{} {} [{}]", c.name, c.version, c.spdx));
        }
    }

    let mut out = String::from(HEAD);
    out.push_str("-----------------------------------------------------------------\n");
    out.push_str("B.1 whisper.cpp / ggml\n");
    out.push_str("-----------------------------------------------------------------\n");
    let whisper = crates
        .iter()
        .find(|c| c.name == "whisper-rs-sys")
        .map(|c| c.dir.join("whisper.cpp").join("LICENSE"));
    match whisper.and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(t) => out.push_str(&t),
        None => out.push_str("(text not found in the sources: see https://github.com/ggml-org/whisper.cpp/blob/master/LICENSE)\n"),
    }
    out.push('\n');
    out.push_str("-----------------------------------------------------------------\n");
    out.push_str("C. Rust dependencies\n");
    out.push_str("-----------------------------------------------------------------\n\n");
    for (names, first) in groups.values() {
        out.push_str("=================================================================\n");
        out.push_str("Crates :\n");
        for n in names {
            out.push_str(&format!("  - {n}\n"));
        }
        out.push_str("-----------------------------------------------------------------\n");
        let text = std::fs::read_to_string(first)
            .or_else(|_| std::fs::read(first).map(|b| String::from_utf8_lossy(&b).into_owned()))
            .ctx(&format!("cannot read {}", first.display()))?;
        out.push_str(&text);
        out.push('\n');
    }
    if !no_license.is_empty() {
        out.push_str("=================================================================\n");
        out.push_str(
            "Crates without a licence file at the root of their sources (declared licence):\n",
        );
        for n in &no_license {
            out.push_str(&format!("  - {n}\n"));
        }
    }
    Ok((out, crates.len(), groups.len()))
}

/// Crates really shipped: "normal" dependencies of the Linux target, excluding workspace members.
fn wanted_crates(root: &Path) -> Result<BTreeSet<(String, String)>> {
    let tree = capture(
        root,
        "cargo",
        &[
            "tree",
            "--locked",
            "-e",
            "normal",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--prefix",
            "none",
            "--no-dedupe",
            "-f",
            "{p}",
        ],
    )?;
    let mut set = BTreeSet::new();
    for line in tree.lines() {
        if line.is_empty() || line.contains(" (/") {
            continue;
        }
        let mut p = line.split(' ');
        let name = p.next().unwrap_or("");
        let version = p.next().unwrap_or("").trim_start_matches('v');
        set.insert((name.to_string(), version.to_string()));
    }
    Ok(set)
}

/// LICENSE*, LICENCE*, COPYING*, NOTICE*, UNLICENSE* files at the root of a crate folder.
fn license_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            ["license", "licence", "copying", "notice", "unlicense"]
                .iter()
                .any(|p| n.starts_with(p))
        })
        .map(|e| e.path())
        .collect();
    files.sort_by(|a, b| {
        a.as_os_str()
            .as_encoded_bytes()
            .cmp(b.as_os_str().as_encoded_bytes())
    });
    files
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `cargo xtask licenses [FILE]`
pub fn run(root: &Path, out: Option<&Path>) -> Result<()> {
    let target = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join("target").join("THIRD_PARTY_LICENSES.txt"));
    let (text, n_crates, n_texts) = render(root)?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).ctx("cannot create the output folder")?;
    }
    std::fs::write(&target, &text).ctx("cannot write the licence texts")?;
    println!(
        "Wrote {} ({n_crates} crates, {n_texts} distinct license texts, {} bytes)",
        target.display(),
        text.len()
    );
    Ok(())
}
