//! Build of the `linuxflow-<version>-<arch>.tar.gz` archive (reproducible, without Docker).
//!
//! No local path: everything starts from the repository root. Reproducibility: `SOURCE_DATE_EPOCH`
//! (otherwise the date of the last git commit) sets the dates; entries sorted by name, owner 0/0,
//! gzip without name or date.

use crate::common::{capture, have, Ctx, Result};
use crate::{licenses, mo};
use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HELP: &str = "\
cargo xtask dist [options]

  --fake-binary FILE   uses FILE as bin/linuxflow (no compilation): to test the packaging
  --out DIR            output folder (default: <root>/target/dist)
  --version V          version (default: [workspace.package] version of Cargo.toml)
  --arch A             architecture in the name (default: the machine's)
  --features F         Cargo features of the application (e.g. dev-tools)
  --no-vulkan          does not build the GPU engine (CPU-only archive; otherwise the Vulkan headers and glslc are required)

Reproducibility: SOURCE_DATE_EPOCH (otherwise the date of the last git commit) sets the dates.
";

#[derive(Default)]
struct Options {
    fake: Option<PathBuf>,
    out: Option<PathBuf>,
    version: Option<String>,
    arch: Option<String>,
    features: Option<String>,
    no_vulkan: bool,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--fake-binary" => o.fake = Some(PathBuf::from(value("--fake-binary")?)),
            "--out" => o.out = Some(PathBuf::from(value("--out")?)),
            "--version" => o.version = Some(value("--version")?),
            "--arch" => o.arch = Some(value("--arch")?),
            "--features" => o.features = Some(value("--features")?),
            "--no-vulkan" => o.no_vulkan = true,
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(o)
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }
    let o = parse(args)?;
    let pkg = root.join("packaging");

    // ---------------------------------------------------------------- metadata
    let constants = fs::read_to_string(root.join("crates/linuxflow-core/src/constants.rs"))
        .ctx("cannot read constants.rs")?;
    let app_id = constants
        .lines()
        .find_map(|l| {
            l.strip_prefix("pub const APP_ID: &str = \"")
                .and_then(|r| r.split('"').next())
        })
        .ok_or("APP_ID not found in constants.rs")?
        .to_string();
    let version = match o.version.clone() {
        Some(v) => v,
        None => workspace_version(&fs::read_to_string(root.join("Cargo.toml")).ctx("Cargo.toml")?)
            .ok_or("cannot read the version from Cargo.toml")?,
    };
    let arch = o
        .arch
        .clone()
        .unwrap_or_else(|| std::env::consts::ARCH.to_string());
    let epoch: u64 = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(v) if !v.is_empty() => v.parse().ctx("SOURCE_DATE_EPOCH")?,
        _ => capture(root, "git", &["log", "-1", "--format=%ct"])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or("set SOURCE_DATE_EPOCH (no git history to take it from)")?,
    };
    let name = format!("linuxflow-{version}-{arch}");
    println!("Building {name} (app id: {app_id}, SOURCE_DATE_EPOCH={epoch})");

    let work = std::env::temp_dir().join(format!("linuxflow-dist-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let stage = work.join(&name);
    for d in [
        "bin",
        "share/applications",
        "share/doc/linuxflow",
        "share/linuxflow",
        "contrib",
    ] {
        fs::create_dir_all(stage.join(d)).ctx("cannot create the staging folder")?;
    }
    let result = build(root, &pkg, &stage, &o, &app_id);
    let result = result.and_then(|()| archive(&work, &name, epoch, &o));
    let _ = fs::remove_dir_all(&work);
    result
}

fn workspace_version(cargo_toml: &str) -> Option<String> {
    let mut in_section = false;
    for l in cargo_toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_section = t == "[workspace.package]";
        } else if in_section && t.starts_with("version") {
            return t.split('"').nth(1).map(str::to_string);
        }
    }
    None
}

fn build(root: &Path, pkg: &Path, stage: &Path, o: &Options, app_id: &str) -> Result<()> {
    // ---------------------------------------------------------------- binaires
    let bin = stage.join("bin");
    if let Some(fake) = &o.fake {
        if !fake.is_file() {
            return Err(format!("fake binary not found: {}", fake.display()));
        }
        for n in [
            "linuxflow",
            "linuxflow-engine",
            "linuxflow-engine-v2",
            "linuxflow-overlay",
        ] {
            fs::copy(fake, bin.join(n)).ctx("cannot copy the fake binary")?;
        }
        if !o.no_vulkan {
            fs::copy(fake, bin.join("linuxflow-engine-vulkan"))
                .ctx("cannot copy the fake binary")?;
        }
        println!("Using the supplied binary (no compilation).");
    } else {
        compile(root, &bin, o)?;
    }
    for e in fs::read_dir(&bin).ctx("bin")?.flatten() {
        fs::set_permissions(e.path(), fs::Permissions::from_mode(0o755)).ctx("chmod")?;
    }

    // ---------------------------------------------------------------- .desktop and icons (names derived from APP_ID)
    let desktop =
        fs::read_to_string(pkg.join("linuxflow.desktop.in")).ctx("linuxflow.desktop.in")?;
    fs::write(
        stage.join(format!("share/applications/{app_id}.desktop")),
        desktop.replace("@APP_ID@", app_id),
    )
    .ctx("desktop file")?;
    let icons = stage.join("share/icons/hicolor");
    let copy_icon = |from: &str, to_dir: &str, to: String| -> Result<()> {
        fs::create_dir_all(icons.join(to_dir)).ctx("icons")?;
        fs::copy(pkg.join("icons").join(from), icons.join(to_dir).join(to))
            .map(|_| ())
            .ctx(&format!("missing icon packaging/icons/{from}"))
    };
    copy_icon("app.svg", "scalable/apps", format!("{app_id}.svg"))?;
    copy_icon("app-rec.svg", "scalable/apps", format!("{app_id}-rec.svg"))?;
    copy_icon(
        "app-loading.svg",
        "scalable/apps",
        format!("{app_id}-loading.svg"),
    )?;
    for n in [16, 24, 32, 48, 64, 128, 256, 512] {
        copy_icon(
            &format!("png/{n}x{n}.png"),
            &format!("{n}x{n}/apps"),
            format!("{app_id}.png"),
        )?;
    }

    // ---------------------------------------------------------------- data provided by the other crates
    let css = root.join("crates/linuxflow-gtk/data/style.css");
    if css.is_file() {
        fs::copy(&css, stage.join("share/linuxflow/style.css")).ctx("style.css")?;
    } else {
        println!("note: crates/linuxflow-gtk/data/style.css absent, not shipped");
    }
    // any file placed in packaging/share/ is shipped as is in share/
    if pkg.join("share").is_dir() {
        copy_tree(&pkg.join("share"), &stage.join("share"))?;
    }

    // ---------------------------------------------------------------- translation catalogues
    let mut pos: Vec<PathBuf> = fs::read_dir(root.join("po"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "po"))
                .collect()
        })
        .unwrap_or_default();
    pos.sort();
    if pos.is_empty() {
        println!("note: no po/*.po found, no catalogue shipped");
    }
    for po in pos {
        let lang = po.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if lang.contains('.') {
            continue; // fr.compendium.po: working file, not a language
        }
        let text = fs::read_to_string(&po).ctx(&format!("cannot read {}", po.display()))?;
        let bytes = mo::compile(&text).map_err(|e| format!("{}: {e}", po.display()))?;
        let dir = stage.join(format!("share/locale/{lang}/LC_MESSAGES"));
        fs::create_dir_all(&dir).ctx("locale folder")?;
        fs::write(dir.join("linuxflow.mo"), bytes).ctx("cannot write the .mo catalogue")?;
    }

    // ---------------------------------------------------------------- documentation, udev rule, scripts
    // COMPLETE text of the third-party licences (required to redistribute a binary: MIT/BSD/ISC/Apache).
    let doc = stage.join("share/doc/linuxflow");
    let (lic, _, _) = licenses::render(root)
        .map_err(|e| format!("could not generate THIRD_PARTY_LICENSES.txt: {e}"))?;
    fs::write(doc.join("THIRD_PARTY_LICENSES.txt"), lic).ctx("THIRD_PARTY_LICENSES.txt")?;
    for f in [
        "LICENSE",
        "THIRD_PARTY_NOTICES.md",
        "README.md",
        "DEPENDENCIES.md",
    ] {
        if root.join(f).is_file() {
            fs::copy(root.join(f), doc.join(f)).ctx(f)?;
        } else {
            eprintln!("warning: {f} missing, not shipped");
        }
    }
    fs::copy(
        pkg.join("contrib/99-linuxflow-uinput.rules"),
        stage.join("contrib/99-linuxflow-uinput.rules"),
    )
    .ctx("udev rule")?;
    for f in ["install.sh", "uninstall.sh", "MIN_VERSIONS"] {
        fs::copy(pkg.join(f), stage.join(f)).ctx(f)?;
    }
    for f in ["install.sh", "uninstall.sh"] {
        fs::set_permissions(stage.join(f), fs::Permissions::from_mode(0o755)).ctx("chmod")?;
    }

    // ---------------------------------------------------------------- normalisation then SHA256SUMS
    normalise(stage)?;
    let mut files = Vec::new();
    list_files(stage, stage, &mut files)?;
    files.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut sums = String::new();
    for f in &files {
        if f.chars().any(char::is_whitespace) {
            return Err("a file name contains whitespace".into());
        }
        let data = fs::read(stage.join(f)).ctx(f)?;
        sums.push_str(&format!("{}  {f}\n", hex(&Sha256::digest(&data))));
    }
    fs::write(stage.join("SHA256SUMS"), sums).ctx("SHA256SUMS")?;
    fs::set_permissions(stage.join("SHA256SUMS"), fs::Permissions::from_mode(0o644))
        .ctx("chmod")?;
    Ok(())
}

/// Builds the application and the three engines, then copies them into `bin/`.
fn compile(root: &Path, bin: &Path, o: &Options) -> Result<()> {
    if !have("cargo") {
        return Err("cargo not found".into());
    }
    let mut rustflags = std::env::var("RUSTFLAGS").unwrap_or_default();
    if rustflags.contains("target-cpu=native") {
        return Err(
            "RUSTFLAGS contains target-cpu=native: forbidden for distributed builds".into(),
        );
    }
    // local paths removed from the binary (reproducibility and privacy)
    // When several prefixes match, the last one wins: from the widest to the most precise.
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            rustflags.push_str(&format!(" --remap-path-prefix={h}=/home-redacted"));
        }
    }
    if let Ok(h) = std::env::var("CARGO_HOME") {
        if !h.is_empty() {
            rustflags.push_str(&format!(" --remap-path-prefix={h}=/cargo"));
        }
    }
    rustflags.push_str(&format!(" --remap-path-prefix={}=/build", root.display()));
    // Same removal for the C/C++ code (whisper.cpp): its source paths go through `__FILE__`.
    let mut c_prefix = String::new();
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            c_prefix.push_str(&format!(" -ffile-prefix-map={h}=/home-redacted"));
        }
    }
    c_prefix.push_str(&format!(" -ffile-prefix-map={}=/build", root.display()));
    let cflags = format!("{}{c_prefix}", std::env::var("CFLAGS").unwrap_or_default());
    let cxxflags = format!(
        "{}{c_prefix}",
        std::env::var("CXXFLAGS").unwrap_or_default()
    );
    // whisper.cpp: never -march=native in a distributed binary. CPU level fixed
    // by crates/linuxflow-stt/build.rs; the caller can override GGML_* explicitly.
    let ggml_native = std::env::var("GGML_NATIVE").unwrap_or_else(|_| "OFF".into());
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let build = |target: &Path, extra_env: &[(&str, &str)], args: &[&str]| -> Result<()> {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(root)
            .args(["build", "--release", "--locked"])
            .args(args)
            .env("RUSTFLAGS", &rustflags)
            .env("CFLAGS", &cflags)
            .env("CXXFLAGS", &cxxflags)
            .env("GGML_NATIVE", &ggml_native)
            .env("CARGO_TARGET_DIR", target);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        if cmd.status().ctx("cannot run cargo")?.success() {
            Ok(())
        } else {
            Err(format!("cargo build {} failed", args.join(" ")))
        }
    };
    let (mut app_features, mut engine_features) = (Vec::new(), Vec::new());
    for f in o
        .features
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|f| !f.is_empty())
    {
        app_features.push(format!("linuxflow/{f}"));
        engine_features.push(format!("linuxflow-engine/{f}"));
    }
    let take = |p: &Path, from: &str, to: &str| -> Result<()> {
        let src = p.join("release").join(from);
        if !src.is_file() {
            return Err(format!("binary not found at {}", src.display()));
        }
        fs::copy(&src, bin.join(to)).map(|_| ()).ctx(to)
    };

    // Application (x86-64-v3 CPU level) + light engine of the same level.
    let mut args = vec!["-p", "linuxflow", "-p", "linuxflow-engine"];
    let joined = [app_features.clone(), engine_features.clone()]
        .concat()
        .join(",");
    if !joined.is_empty() {
        args.extend(["--features", &joined]);
    }
    build(&target_dir, &[], &args)?;
    take(&target_dir, "linuxflow", "linuxflow")?;
    take(&target_dir, "linuxflow-engine", "linuxflow-engine")?;

    // Layer-shell pill: separate program, the only one linked to libgtk4-layer-shell (absent from some
    // repositories). Built in a separate Cargo call, so that the `layer-shell` feature is not
    // enabled by unification in the main application.
    build(&target_dir, &[], &["-p", "linuxflow-overlay"])?;
    take(&target_dir, "linuxflow-overlay", "linuxflow-overlay")?;

    // Fallback engine for CPUs without AVX2 (x86-64-v2): same code, whisper.cpp built without
    // AVX/AVX2/FMA/F16C/BMI2, in a separate build folder. About 6.5 times slower (measured).
    let v2 = std::env::var_os("LINUXFLOW_V2_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/dist-v2"));
    let engine_only = engine_features.join(",");
    let mut args = vec!["-p", "linuxflow-engine"];
    if !engine_only.is_empty() {
        args.extend(["--features", &engine_only]);
    }
    build(
        &v2,
        &[
            ("GGML_AVX", "OFF"),
            ("GGML_AVX2", "OFF"),
            ("GGML_FMA", "OFF"),
            ("GGML_F16C", "OFF"),
            ("GGML_BMI2", "OFF"),
        ],
        &args,
    )?;
    take(&v2, "linuxflow-engine", "linuxflow-engine-v2")?;

    // GPU engine: whisper.cpp built with Vulkan (hard-linked to libvulkan.so.1, hence shipped NEXT TO the
    // CPU engine, which starts everywhere). The application only chooses it if a Vulkan GPU is detected.
    if !o.no_vulkan {
        if !have("glslc") {
            return Err("glslc not found (package shaderc / glslc): it compiles the GPU shaders. Use --no-vulkan for a CPU-only archive.".into());
        }
        if !vulkan_headers_present() {
            return Err("Vulkan headers not found (package libvulkan-dev / vulkan-devel / vulkan-headers). Use --no-vulkan for a CPU-only archive.".into());
        }
        let vk = std::env::var_os("LINUXFLOW_VULKAN_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target/dist-vulkan"));
        build(
            &vk,
            &[],
            &["-p", "linuxflow-engine", "--features", "vulkan"],
        )?;
        take(&vk, "linuxflow-engine", "linuxflow-engine-vulkan")?;
    }
    Ok(())
}

/// Does the C preprocessor find `<vulkan/vulkan.h>`? (honours CPATH, like the whisper.cpp build)
fn vulkan_headers_present() -> bool {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let Ok(mut child) = Command::new(cc)
        .args(["-E", "-x", "c", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"#include <vulkan/vulkan.h>\n");
    }
    child.wait().is_ok_and(|s| s.success())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for e in fs::read_dir(from).ctx("copy")?.flatten() {
        let dest = to.join(e.file_name());
        if e.file_type().ctx("copy")?.is_dir() {
            fs::create_dir_all(&dest).ctx("copy")?;
            copy_tree(&e.path(), &dest)?;
        } else {
            fs::copy(e.path(), &dest).ctx("copy")?;
        }
    }
    Ok(())
}

/// Folders at 0755, files at 0644 (0755 if executable).
fn normalise(dir: &Path) -> Result<()> {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).ctx("chmod")?;
    for e in fs::read_dir(dir).ctx("normalise")?.flatten() {
        let p = e.path();
        if e.file_type().ctx("normalise")?.is_dir() {
            normalise(&p)?;
        } else {
            let exec = fs::metadata(&p).ctx("stat")?.permissions().mode() & 0o100 != 0;
            fs::set_permissions(
                &p,
                fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }),
            )
            .ctx("chmod")?;
        }
    }
    Ok(())
}

fn list_files(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for e in fs::read_dir(dir).ctx("list")?.flatten() {
        let p = e.path();
        if e.file_type().ctx("list")?.is_dir() {
            list_files(base, &p, out)?;
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

/// Reproducible archive: entries sorted by name, fixed dates, owner 0/0.
fn archive(work: &Path, name: &str, epoch: u64, o: &Options) -> Result<()> {
    let out_dir = o
        .out
        .clone()
        .unwrap_or_else(|| crate::common::root().join("target/dist"));
    fs::create_dir_all(&out_dir).ctx("cannot create the output folder")?;
    let target = out_dir.join(format!("{name}.tar.gz"));
    let tmp = out_dir.join(format!("{name}.tar.gz.tmp"));
    {
        let file = fs::File::create(&tmp).ctx("cannot create the archive")?;
        let gz = GzEncoder::new(file, Compression::best());
        let mut tar = tar::Builder::new(gz);
        tar.mode(tar::HeaderMode::Deterministic);
        append_dir(&mut tar, &work.join(name), name, epoch)?;
        tar.into_inner().ctx("tar")?.finish().ctx("gzip")?;
    }
    fs::rename(&tmp, &target).ctx("cannot move the archive")?;
    let data = fs::read(&target).ctx("archive")?;
    let line = format!("{}  {name}.tar.gz\n", hex(&Sha256::digest(&data)));
    fs::write(out_dir.join(format!("{name}.tar.gz.sha256")), &line).ctx("sha256 file")?;
    println!("Archive: {}", target.display());
    print!("{line}");
    Ok(())
}

fn append_dir<W: Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    rel: &str,
    epoch: u64,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(epoch);
    header.set_size(0);
    tar.append_data(&mut header, format!("{rel}/"), std::io::empty())
        .ctx("tar")?;
    let mut entries: Vec<_> = fs::read_dir(dir).ctx("tar")?.flatten().collect();
    entries.sort_by_key(|e| e.file_name().as_encoded_bytes().to_vec());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let rel_path = format!("{rel}/{name}");
        if e.file_type().ctx("tar")?.is_dir() {
            append_dir(tar, &path, &rel_path, epoch)?;
        } else {
            let data = fs::read(&path).ctx(&rel_path)?;
            let exec = fs::metadata(&path).ctx("stat")?.permissions().mode() & 0o100 != 0;
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Regular);
            h.set_mode(if exec { 0o755 } else { 0o644 });
            h.set_uid(0);
            h.set_gid(0);
            h.set_mtime(epoch);
            h.set_size(data.len() as u64);
            tar.append_data(&mut h, &rel_path, data.as_slice())
                .ctx("tar")?;
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_version_is_read_from_its_section() {
        let t = "[workspace]\nmembers=[]\n[workspace.package]\nedition = \"2021\"\nversion = \"0.3.1\"\n[other]\nversion = \"9\"\n";
        assert_eq!(workspace_version(t).as_deref(), Some("0.3.1"));
        assert_eq!(workspace_version("[package]\nversion = \"1\"\n"), None);
    }

    #[test]
    fn options_are_parsed_and_unknown_ones_refused() {
        let args: Vec<String> = ["--no-vulkan", "--out", "/o", "--version", "1.2.3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let o = parse(&args).unwrap();
        assert!(o.no_vulkan);
        assert_eq!(o.version.as_deref(), Some("1.2.3"));
        assert!(parse(&["--nope".to_string()]).is_err());
        assert!(parse(&["--out".to_string()]).is_err());
    }
}
