//! Static checks and complete quality gate.
//!
//! Exceptions (test fixtures): `scripts/static-checks.allow`, one per line:
//!     CHECK|relative path glob|line regex (.* = the whole file)|reason
//! The regex applies to the `path:number:text` line (as with `grep -n`).

use crate::common::{capture, have, run, Ctx, Result};
use crate::notices;
use regex::Regex;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

struct Allow {
    check: String,
    glob: Regex,
    line: Regex,
}

struct Report {
    fails: usize,
    warns: usize,
}

impl Report {
    fn pass(&self, msg: impl AsRef<str>) {
        println!("OK     {}", msg.as_ref());
    }
    fn fail(&mut self, msg: impl AsRef<str>) {
        self.fails += 1;
        println!("FAILURE  {}", msg.as_ref());
    }
    fn warn(&mut self, msg: impl AsRef<str>) {
        self.warns += 1;
        println!("WARN.  {}", msg.as_ref());
    }
}

/// Shell `case` glob (`*` also covers `/`), converted into an anchored regex.
fn glob_to_regex(glob: &str) -> Result<Regex> {
    let mut re = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                re.push('[');
                for c in chars.by_ref() {
                    re.push(c);
                    if c == ']' {
                        break;
                    }
                }
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).ctx(&format!("bad glob {glob}"))
}

fn load_allow(path: &Path) -> Result<Vec<Allow>> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for l in text.lines() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let mut f = l.splitn(4, '|');
        let (Some(check), Some(glob), Some(re)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        out.push(Allow {
            check: check.to_string(),
            glob: glob_to_regex(glob)?,
            line: Regex::new(re).ctx(&format!("bad allow regex {re}"))?,
        });
    }
    Ok(out)
}

/// All the text files under the given paths (excluding `target` and `.git`), sorted.
fn text_files(root: &Path, paths: &[&str]) -> Vec<(String, String)> {
    fn walk(p: &Path, out: &mut Vec<PathBuf>) {
        let Ok(meta) = fs::symlink_metadata(p) else {
            return;
        };
        if meta.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" || name == ".git" {
                return;
            }
            if let Ok(rd) = fs::read_dir(p) {
                for e in rd.flatten() {
                    walk(&e.path(), out);
                }
            }
        } else if meta.is_file() {
            out.push(p.to_path_buf());
        }
    }
    let mut files = Vec::new();
    for p in paths {
        let full = root.join(p);
        if full.exists() {
            walk(&full, &mut files);
        }
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .filter_map(|f| {
            let bytes = fs::read(&f).ok()?;
            if bytes.contains(&0) {
                return None; // binary: ignored (like `grep -I`)
            }
            let rel = f.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            Some((rel, String::from_utf8_lossy(&bytes).into_owned()))
        })
        .collect()
}

/// `grep -rEnI` + exception list: returns (lines kept, total number of lines found).
fn scan(
    files: &[(String, String)],
    allow: &[Allow],
    chk: &str,
    re: &Regex,
) -> (Vec<String>, usize) {
    let mut kept = Vec::new();
    let mut all = 0;
    for (rel, text) in files {
        for (i, line) in text.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            all += 1;
            let hit = format!("{rel}:{}:{line}", i + 1);
            let skipped = allow
                .iter()
                .any(|a| a.check == chk && a.glob.is_match(rel) && a.line.is_match(&hit));
            if !skipped {
                kept.push(hit);
            }
        }
    }
    (kept, all)
}

struct Scanner<'a> {
    root: &'a Path,
    allow: &'a [Allow],
}

impl Scanner<'_> {
    fn run(
        &self,
        rep: &mut Report,
        chk: &str,
        desc: &str,
        pattern: &str,
        case_insensitive: bool,
        paths: &[&str],
    ) {
        let present: Vec<&str> = paths
            .iter()
            .copied()
            .filter(|p| self.root.join(p).exists())
            .collect();
        if present.is_empty() {
            rep.pass(format!("{chk} {desc} (nothing to scan)"));
            return;
        }
        let pat = if case_insensitive {
            format!("(?i){pattern}")
        } else {
            pattern.to_string()
        };
        let re = match Regex::new(&pat) {
            Ok(r) => r,
            Err(e) => {
                rep.fail(format!("{chk} {desc} : motif invalide ({e})"));
                return;
            }
        };
        let files = text_files(self.root, &present);
        let (kept, all) = scan(&files, self.allow, chk, &re);
        if kept.is_empty() {
            rep.pass(format!(
                "{chk} {desc} (0 results; {all} documented exception(s))"
            ));
        } else {
            rep.fail(format!("{chk} {desc}: {} result(s)", kept.len()));
            for l in kept.iter().take(20) {
                println!("         {l}");
            }
            if kept.len() > 20 {
                println!("         ... ({} de plus)", kept.len() - 20);
            }
        }
    }
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap_or_default()
}

pub fn static_checks(root: &Path) -> Result<bool> {
    let allow = load_allow(&root.join("scripts/static-checks.allow"))?;
    let sc = Scanner {
        root,
        allow: &allow,
    };
    let mut rep = Report { fails: 0, warns: 0 };

    println!("### T-HARD : valeurs locales en dur");
    sc.run(
        &mut rep,
        "T-HARD",
        "local paths/ports/identities in crates/ apps/ packaging/",
        r"(/home/|/Users/|C:\\\\|%APPDATA%|%LOCALAPPDATA%|127\.0\.0\.1:[0-9]+|48123)",
        false,
        &["crates", "apps", "packaging"],
    );
    // .desktop: no absolute path, Exec = @BINDIR@/linuxflow
    let desktop = root.join("packaging/linuxflow.desktop.in");
    if desktop.is_file() {
        let t = read(root, "packaging/linuxflow.desktop.in");
        let exec_ok = t.lines().any(|l| l == "Exec=@BINDIR@/linuxflow");
        let abs = Regex::new(r"^(Exec|Icon|TryExec|Path)=/").map_err(|e| e.to_string())?;
        if exec_ok && !t.lines().any(|l| abs.is_match(l)) {
            rep.pass("T-HARD .desktop template: Exec=@BINDIR@/linuxflow, no absolute path");
        } else {
            rep.fail("T-HARD .desktop template: Exec must be @BINDIR@/linuxflow and no absolute path is allowed");
        }
    } else {
        rep.fail("T-HARD packaging/linuxflow.desktop.in absent");
    }
    // install.sh / uninstall.sh: no hard-coded home or installation folder path
    // (/etc/os-release, /etc/udev/rules.d, /dev/* and ldconfig are documented system conventions)
    let abs =
        Regex::new(r#"(^|[ "=:(])/(home|Users|root|opt|usr|var|srv|mnt|media|tmp)(/|[ "]|$)"#)
            .map_err(|e| e.to_string())?;
    let mut bad = Vec::new();
    for f in ["packaging/install.sh", "packaging/uninstall.sh"] {
        for (i, l) in read(root, f).lines().enumerate() {
            // '/usr/sbin/ldconfig' is a tool lookup, not a destination
            if abs.is_match(l) && !l.contains("ldconfig") && !l.contains("/usr/local") {
                bad.push(format!("{f}:{}:{l}", i + 1));
            }
        }
    }
    if bad.is_empty() {
        rep.pass("T-HARD installation scripts: no absolute destination path");
    } else {
        rep.fail("T-HARD absolute paths in the installation scripts");
        for l in bad {
            println!("         {l}");
        }
    }
    if read(root, "packaging/install.sh").contains("HOME/.local") {
        rep.pass("T-HARD install.sh: default prefix through $HOME");
    } else {
        rep.fail("T-HARD install.sh: default prefix through $HOME not found");
    }

    println!("\n### T-ID: identity and internal names");
    let id_dirs = [
        "crates",
        "apps",
        "packaging",
        "po",
        "README.md",
        "BUILD.md",
        "CHANGELOG.md",
        "docs",
    ];
    sc.run(
        &mut rep,
        "T-ID",
        "identity terms (exact case)",
        r"(Al[a]n[E]CE|com\.al[a]n|iAl[a]n|PasteOutcome|WindowsShortcutMaskingV2|Free[F]low|free[f]low)",
        false,
        &id_dirs,
    );
    sc.run(
        &mut rep,
        "T-ID",
        "third-party product names (case-insensitive)",
        "([w]ispr)",
        true,
        &id_dirs,
    );
    let constants = read(root, "crates/linuxflow-core/src/constants.rs");
    let app_id = constants.lines().find_map(|l| {
        l.strip_prefix("pub const APP_ID: &str = \"")
            .and_then(|r| r.split('"').next())
    });
    match app_id {
        Some(id) => sc.run(
            &mut rep,
            "T-ID",
            "APP_ID written outside constants.rs",
            &regex::escape(id),
            false,
            &["crates", "apps", "packaging"],
        ),
        None => rep.fail("T-ID APP_ID not found in constants.rs"),
    }

    println!("\n### T-PRIV: privacy (static part)");
    sc.run(
        &mut rep,
        "T-PRIV",
        "telemetry libraries/names",
        r"(sentry|posthog|mixpanel|google-analytics|segment\.io|datadog)",
        true,
        &["crates", "apps", "packaging"],
    );
    sc.run(
        &mut rep,
        "T-PRIV",
        "literal secrets/tokens",
        r#"(api[_-]?key|secret|token|password)[A-Za-z_]*[[:space:]]*[:=][[:space:]]*"[A-Za-z0-9_./+=-]{8,}""#,
        true,
        &["crates", "apps"],
    );
    sc.run(
        &mut rep,
        "T-PRIV",
        "environment passed wholesale to a child process",
        r"envs?\([[:space:]]*(std::)?env::vars\(\)[[:space:]]*\)",
        false,
        &["crates", "apps"],
    );
    let perm = Regex::new("0o?600").map_err(|e| e.to_string())?;
    let rust_files: Vec<_> = text_files(root, &["crates", "apps"])
        .into_iter()
        .filter(|(p, _)| p.ends_with(".rs"))
        .collect();
    if rust_files.iter().any(|(_, t)| perm.is_match(t)) {
        rep.pass("T-PRIV 0600 permissions: present in the code (the real check is a run-time test of the data/log crates)");
    } else {
        rep.warn(
            "T-PRIV 0600 permissions: no occurrence in the code (history/logs not implemented?)",
        );
    }
    println!("         note: 'no network request outside the download' and 'logs without a personal path' are run-time tests (T-PRIV with the network cut), not covered here.");

    println!("\n### T-NET: network");
    sc.run(
        &mut rep,
        "T-NET",
        "ressources externes de polices/CDN",
        r"(fonts\.gstatic\.com|fonts\.googleapis\.com|googletagmanager|cdn\.jsdelivr|cdnjs\.|unpkg\.com)",
        true,
        &["crates", "apps", "packaging"],
    );
    url_check(&mut rep, root, &allow)?;
    graph_check(&mut rep, root)?;

    println!("\n### T-LIC : licences");
    licence_checks(&mut rep, root)?;

    println!("\n### T-VER addition: interface baseline");
    let ver =
        Regex::new(r"v4_(1[3-9]|[2-9][0-9])|v1_([5-9]|[1-9][0-9])").map_err(|e| e.to_string())?;
    let mut hits = Vec::new();
    for (rel, text) in text_files(root, &["crates", "apps", "Cargo.toml"]) {
        if !rel.ends_with("Cargo.toml") {
            continue;
        }
        for (i, l) in text.lines().enumerate() {
            if ver.is_match(l) {
                hits.push(format!("{rel}:{}:{l}", i + 1));
            }
        }
    }
    if hits.is_empty() {
        rep.pass("no v4_13+ / v1_5+ feature in the Cargo.toml files");
    } else {
        rep.fail("GTK/libadwaita features above the 4.12 / 1.4 baseline");
        for l in hits {
            println!("         {l}");
        }
    }

    println!(
        "\nResult: {} failure(s), {} warning(s).",
        rep.fails, rep.warns
    );
    Ok(rep.fails == 0)
}

/// http(s) URLs: only certain hosts are allowed (namespaces, model repositories, documentation).
fn url_check(rep: &mut Report, root: &Path, allow: &[Allow]) -> Result<()> {
    let url = Regex::new(r#"https?://[^ "')>`<\]]+"#).map_err(|e| e.to_string())?;
    let ok = Regex::new(
        r"(?i)^https?://(www\.w3\.org|specifications\.freedesktop\.org|www\.freedesktop\.org|freedesktop\.org|huggingface\.co|hf-mirror\.com|github\.com|docs\.rs|crates\.io|gitlab\.gnome\.org|www\.gnu\.org|gnu\.org|creativecommons\.org|spdx\.org|choosealicense\.com|doc\.rust-lang\.org|www\.gtk\.org|gnome\.pages\.gitlab\.gnome\.org|www\.mozilla\.org|opensource\.org|unicode\.org|www\.unicode\.org|localhost|example\.(com|org)|127\.0\.0\.1)",
    )
    .map_err(|e| e.to_string())?;
    let mut bad = Vec::new();
    for (rel, text) in text_files(root, &["crates", "apps", "packaging"]) {
        for (i, line) in text.lines().enumerate() {
            for m in url.find_iter(line) {
                if ok.is_match(m.as_str()) {
                    continue;
                }
                let hit = format!("{rel}:{}:{}", i + 1, m.as_str());
                let skipped = allow
                    .iter()
                    .any(|a| a.check == "T-NET" && a.glob.is_match(&rel) && a.line.is_match(&hit));
                if !skipped {
                    bad.push(hit);
                }
            }
        }
    }
    if bad.is_empty() {
        rep.pass("T-NET aucune URL externe hors liste admise (w3.org, freedesktop, huggingface, github, docs...)");
    } else {
        rep.fail(format!(
            "T-NET URLs externes hors liste admise : {}",
            bad.len()
        ));
        for l in bad.iter().take(20) {
            println!("         {l}");
        }
    }
    Ok(())
}

/// Dependency graph: no HTTP server, no system TLS stack, network limited to the chain
/// `linuxflow-models`.
fn graph_check(rep: &mut Report, root: &Path) -> Result<()> {
    if !have("cargo") || !root.join("Cargo.lock").is_file() {
        rep.warn("T-NET cargo or Cargo.lock missing: graph not examined");
        return Ok(());
    }
    let servers = Regex::new(r"^(axum|warp|tiny_http|actix-web|rouille|hyper-server|tide|rocket|poem|salvo|gotham|iron|nickel)$").map_err(|e| e.to_string())?;
    // openssl-probe (rustls-native-certs) does not link OpenSSL
    let tls = Regex::new(r"^(openssl|openssl-sys|native-tls)$").map_err(|e| e.to_string())?;
    let clients = Regex::new(r"^(reqwest|hyper|hyper-util|hyper-rustls|h2|ureq|isahc|surf|attohttpc|curl|curl-sys|tungstenite|tokio-tungstenite|rustls|webpki-roots|rustls-webpki)$").map_err(|e| e.to_string())?;
    let meta = match capture(
        root,
        "cargo",
        &["metadata", "--locked", "--format-version", "1", "--no-deps"],
    ) {
        Ok(m) => m,
        Err(e) => {
            rep.warn(format!(
                "T-NET cargo metadata failed (Cargo.lock up to date?): {e}"
            ));
            return Ok(());
        }
    };
    let meta: serde_json::Value = serde_json::from_str(&meta).ctx("cargo metadata")?;
    let mut members: BTreeSet<String> = BTreeSet::new();
    for p in meta["packages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if let Some(n) = p["name"].as_str().filter(|n| n.starts_with("linuxflow")) {
            members.insert(n.to_string());
        }
    }
    let mut bad = false;
    for m in &members {
        let tree = match capture(
            root,
            "cargo",
            &[
                "tree", "--locked", "-e", "normal", "--prefix", "none", "-p", m, "-f", "{p}",
            ],
        ) {
            Ok(t) => t,
            Err(e) => {
                rep.warn(format!("T-NET cargo tree -p {m} failed: {e}"));
                continue;
            }
        };
        let closure: BTreeSet<&str> = tree
            .lines()
            .filter_map(|l| l.split(' ').next())
            .filter(|n| !n.is_empty())
            .collect();
        if closure.is_empty() {
            rep.warn(format!("T-NET cargo tree -p {m} : sortie vide"));
            continue;
        }
        let pick = |re: &Regex| {
            closure
                .iter()
                .filter(|n| re.is_match(n))
                .copied()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let (s, t) = (pick(&servers), pick(&tls));
        if !s.is_empty() {
            rep.fail(format!("T-NET {m} depends on an HTTP server crate: {s}"));
            bad = true;
        }
        if !t.is_empty() {
            rep.fail(format!(
                "T-NET {m} depends on a system TLS stack: {t} (rustls only)"
            ));
            bad = true;
        }
        if !closure.contains("linuxflow-models") && m != "linuxflow-models" {
            let c = pick(&clients);
            if !c.is_empty() {
                rep.fail(format!(
                    "T-NET {m} (outside the linuxflow-models chain) depends on network crates: {c}"
                ));
                bad = true;
            }
        }
    }
    if !bad {
        rep.pass(format!(
            "T-NET dependency graph: no HTTP server, no system TLS stack, network limited to the linuxflow-models chain ({} members examined)",
            members.len()
        ));
    }
    Ok(())
}

fn licence_checks(rep: &mut Report, root: &Path) -> Result<()> {
    if have("cargo") && capture(root, "cargo", &["deny", "--version"]).is_ok() {
        match capture(root, "cargo", &["deny", "check", "licenses"]) {
            Ok(_) => rep.pass("T-LIC cargo deny check licenses"),
            Err(e) => rep.fail(format!("T-LIC cargo deny check licenses : {e}")),
        }
    } else {
        rep.warn("T-LIC cargo-deny is not installed: 'cargo deny check licenses' NOT run (fallback check below)");
    }
    if have("cargo") && root.join("Cargo.lock").is_file() {
        match capture(
            root,
            "cargo",
            &[
                "tree",
                "--locked",
                "-e",
                "normal,build",
                "--prefix",
                "none",
                "--no-dedupe",
                "-f",
                "{p}|{l}",
            ],
        ) {
            Ok(tree) if !tree.trim().is_empty() => {
                let lines: BTreeSet<&str> = tree.lines().collect();
                let (mut copyleft, mut review, mut empty) = (Vec::new(), Vec::new(), Vec::new());
                for l in &lines {
                    let (pkg, lic) = l.split_once('|').unwrap_or((l, ""));
                    if pkg.is_empty() || pkg.contains(" (/") {
                        continue;
                    }
                    if lic.is_empty() {
                        empty.push(pkg.to_string());
                        continue;
                    }
                    let alts: Vec<&str> = lic.split(" OR ").collect();
                    let some = alts.iter().any(|a| a.contains("GPL"));
                    let all = alts.iter().all(|a| a.contains("GPL"));
                    if some && all {
                        copyleft.push(format!("{pkg} ({lic})"));
                    } else if some {
                        review.push(format!("{pkg} ({lic})"));
                    }
                }
                if copyleft.is_empty() {
                    rep.pass(format!(
                        "T-LIC fallback: no Rust dependency under GPL/AGPL/LGPL only ({} crates examined)",
                        lines.len()
                    ));
                } else {
                    rep.fail("T-LIC dependency(ies) under (A/L)GPL only");
                    for c in copyleft {
                        println!("         {c}");
                    }
                }
                if !review.is_empty() {
                    rep.warn("T-LIC GPL licence offered as an alternative (to check):");
                    for c in review {
                        println!("         {c}");
                    }
                }
                if !empty.is_empty() {
                    rep.warn("T-LIC crates without a licence field (license-file):");
                    for c in empty {
                        println!("         {c}");
                    }
                }
            }
            Ok(_) => rep.warn("T-LIC fallback: cargo tree returned an empty output"),
            Err(e) => rep.warn(format!("T-LIC fallback: cargo tree failed ({e})")),
        }
    }
    match notices::render(root) {
        Ok((text, _))
            if fs::read_to_string(root.join("THIRD_PARTY_NOTICES.md"))
                .ok()
                .as_deref()
                == Some(text.as_str()) =>
        {
            rep.pass("T-LIC THIRD_PARTY_NOTICES.md up to date");
        }
        Ok(_) => {
            rep.fail("T-LIC THIRD_PARTY_NOTICES.md : missing or stale: run `cargo xtask notices`")
        }
        Err(e) => rep.fail(format!("T-LIC THIRD_PARTY_NOTICES.md : {e}")),
    }
    // NVIDIA / DLL files (excluding target and .git)
    let nv = Regex::new(r"(?i)(cublas|cudnn|cudart|nvrtc)|^libcuda|^nvcuda|\.dll$")
        .map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    fn walk(base: &Path, dir: &Path, nv: &Regex, found: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(base)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if ["target", "proto", ".git", "plan", "xtask/target"].contains(&rel.as_str()) {
                    continue;
                }
                walk(base, &p, nv, found);
            } else if nv.is_match(&e.file_name().to_string_lossy()) {
                found.push(format!("./{rel}"));
            }
        }
    }
    walk(root, root, &nv, &mut found);
    if found.is_empty() {
        rep.pass("T-LIC no NVIDIA/CUDA file or DLL in the repository");
    } else {
        rep.fail("T-LIC NVIDIA/DLL files in the repository");
        for f in found {
            println!("         {f}");
        }
    }
    let license = read(root, "LICENSE");
    if license.contains("Copyright (c) 2026 ITchrisDEB") {
        rep.pass("T-LIC LICENSE: LinuxFlow author");
    } else {
        rep.fail("T-LIC LICENSE: expected copyright lines missing");
    }
    let mit = Regex::new(r#"(?m)^license(\.workspace *= *true| *= *"MIT")"#)
        .map_err(|e| e.to_string())?;
    let mut missing = Vec::new();
    for dir in ["crates", "apps"] {
        if let Ok(rd) = fs::read_dir(root.join(dir)) {
            let mut v: Vec<_> = rd.flatten().map(|e| e.path().join("Cargo.toml")).collect();
            v.sort();
            for c in v.into_iter().filter(|c| c.is_file()) {
                if !mit.is_match(&fs::read_to_string(&c).unwrap_or_default()) {
                    missing.push(
                        c.strip_prefix(root)
                            .unwrap_or(&c)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
    }
    if missing.is_empty() {
        rep.pass(
            "T-LIC every Cargo.toml declares the MIT licence (directly or through the workspace)",
        );
    } else {
        rep.fail(format!(
            "T-LIC MIT licence not declared in: {}",
            missing.join(" ")
        ));
    }
    if read(root, "Cargo.toml")
        .lines()
        .any(|l| l == "license = \"MIT\"")
    {
        rep.pass("T-LIC [workspace.package] license = \"MIT\"");
    } else {
        rep.fail("T-LIC [workspace.package] license missing or different from MIT");
    }
    Ok(())
}

/// Local quality gate (the equivalent of a CI without Docker): fmt, clippy, tests, cargo deny,
/// up-to-date notices, static checks, script syntax, installer tests.
pub fn full_check(root: &Path) -> Result<bool> {
    let jobs = std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| "4".into());
    std::env::set_var("CARGO_BUILD_JOBS", jobs);
    let (mut failed, mut warned) = (0, 0);
    let mut step = |name: &str, ok: bool| {
        println!("\n== {name}");
        if !ok {
            failed += 1;
            eprintln!("FAILURE: {name}");
        }
    };
    let xtask_manifest = root.join("xtask/Cargo.toml");
    let xtask_manifest = xtask_manifest.to_string_lossy().into_owned();
    step(
        "cargo fmt --check",
        run(root, "cargo", &["fmt", "--all", "--", "--check"]),
    );
    step(
        "cargo clippy -D warnings",
        run(
            root,
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        ),
    );
    step(
        "cargo test --workspace",
        run(root, "cargo", &["test", "--workspace", "--locked"]),
    );
    step(
        "xtask : tests et clippy",
        run(root, "cargo", &["test", "--manifest-path", &xtask_manifest])
            && run(
                root,
                "cargo",
                &[
                    "clippy",
                    "--manifest-path",
                    &xtask_manifest,
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            ),
    );
    println!("\n== cargo deny check");
    if capture(root, "cargo", &["deny", "--version"]).is_ok() {
        if !run(root, "cargo", &["deny", "check"]) {
            failed += 1;
            eprintln!("FAILURE: cargo deny check");
        }
    } else {
        warned += 1;
        eprintln!("WARNING: cargo-deny is not installed, licences/security advisories NOT checked (cargo install cargo-deny --locked).");
    }
    println!("\n== THIRD_PARTY_NOTICES.md up to date");
    if let Err(e) = notices::run(root, true) {
        failed += 1;
        eprintln!("FAILURE: {e}");
    }
    println!("\n== garde-fous statiques (T-HARD/T-ID/T-PRIV/T-NET/T-LIC)");
    match static_checks(root) {
        Ok(true) => {}
        Ok(false) => {
            failed += 1;
            eprintln!("FAILURE: garde-fous statiques");
        }
        Err(e) => {
            failed += 1;
            eprintln!("FAILURE: {e}");
        }
    }
    println!("\n== script syntax");
    let mut scripts: Vec<String> = vec![
        "packaging/install.sh".into(),
        "packaging/uninstall.sh".into(),
    ];
    for dir in ["scripts", "scripts/tests"] {
        if let Ok(rd) = fs::read_dir(root.join(dir)) {
            let mut v: Vec<_> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "sh"))
                .collect();
            v.sort();
            scripts.extend(
                v.iter()
                    .filter_map(|p| p.strip_prefix(root).ok())
                    .map(|p| p.to_string_lossy().into_owned()),
            );
        }
    }
    for s in &scripts {
        // update-po.sh uses bash; the others are POSIX sh
        let shell = if s.ends_with("update-po.sh") {
            "bash"
        } else {
            "sh"
        };
        if !run(root, shell, &["-n", s]) {
            failed += 1;
            eprintln!("FAILURE: {shell} -n {s}");
        }
    }
    if have("shellcheck") {
        if !run(
            root,
            "shellcheck",
            &["packaging/install.sh", "packaging/uninstall.sh"],
        ) {
            failed += 1;
            eprintln!("FAILURE: shellcheck");
        }
    } else {
        warned += 1;
        eprintln!("WARNING: shellcheck is not installed, not run.");
    }
    println!("\n== installation script tests");
    if !run(root, "sh", &["scripts/tests/test-packaging.sh"]) {
        failed += 1;
        eprintln!("FAILURE: installation script tests");
    }
    println!("\n== Result: {failed} failure(s), {warned} warning(s)");
    Ok(failed == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_globs_cross_directory_separators() {
        let g = glob_to_regex("crates/*/tests/fixtures/*").unwrap();
        assert!(g.is_match("crates/linuxflow-core/tests/fixtures/a/b.json"));
        assert!(!g.is_match("crates/linuxflow-core/src/a.rs"));
        let g = glob_to_regex("docs/example.md").unwrap();
        assert!(g.is_match("docs/example.md"));
        assert!(!g.is_match("docs/exampleXmd"));
    }

    #[test]
    fn allow_entries_use_the_first_three_fields_and_skip_comments() {
        let d = tempdir();
        let f = d.join("allow");
        fs::write(
            &f,
            "# comment\n\nT-ID|a/*|.*|reason|extra\nT-HARD|b|48123|r\nmalformed\n",
        )
        .unwrap();
        let a = load_allow(&f).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].check, "T-ID");
        assert!(a[1].line.is_match("b:3:port 48123"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_hit_is_dropped_only_by_a_matching_allow_entry() {
        let files = vec![
            (
                "crates/x/tests/fixtures/f.json".to_string(),
                "Quarkfield\n".to_string(),
            ),
            (
                "crates/x/src/a.rs".to_string(),
                "Quarkfield\nok\n".to_string(),
            ),
        ];
        let allow = vec![Allow {
            check: "T-ID".into(),
            glob: glob_to_regex("crates/*/tests/fixtures/*").unwrap(),
            line: Regex::new(".*").unwrap(),
        }];
        let re = Regex::new("Quarkfield").unwrap();
        let (kept, all) = scan(&files, &allow, "T-ID", &re);
        assert_eq!(all, 2);
        assert_eq!(kept, ["crates/x/src/a.rs:1:Quarkfield"]);
        // an exception of another check does not apply
        let (kept, _) = scan(&files, &allow, "T-HARD", &re);
        assert_eq!(kept.len(), 2);
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("xtask-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
}
