//! "Complete" AppImage: the application, its engines AND the graphical stack (GTK 4, libadwaita and
//! their dependencies), to try LinuxFlow without installing anything.
//!
//! Portability: the bundled libraries do NOT come from the build machine (a recent
//! distribution would make them require a very recent glibc) but from **Ubuntu 24.04**: the official
//! packages are downloaded (SHA-256 verified) and unpacked here, without Docker or
//! root rights. The LinuxFlow binaries only require glibc 2.39, which is the one of Ubuntu 24.04;
//! the AppImage therefore has the same requirement as the archive. What stays the system's: glibc, the
//! graphics drivers (OpenGL, Vulkan), ALSA/PipeWire and the fonts.

use crate::common::{capture, Ctx, Result};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const MIRROR: &str = "https://archive.ubuntu.com/ubuntu";
const SUITES: [&str; 3] = ["noble", "noble-updates", "noble-security"];
const APPIMAGETOOL_URL: &str =
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage";
const RUNTIME_URL: &str =
    "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64";
const LIBDIR: &str = "usr/lib/x86_64-linux-gnu";

/// Starting packages: the graphical stack, the image loaders (PNG, SVG), the icons and the schemas.
const ROOTS: [&str; 9] = [
    "libgtk-4-1",
    "libadwaita-1-0",
    "libgdk-pixbuf-2.0-0",
    "libgdk-pixbuf2.0-bin",
    "librsvg2-common",
    "adwaita-icon-theme",
    "gsettings-desktop-schemas",
    "libstdc++6",
    "libgcc-s1",
];

/// Packages never bundled: they must be the system's (glibc, graphics drivers, audio,
/// security, packaging tools).
fn is_excluded(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "libc6",
        "libc-bin",
        "libc-dev-bin",
        "debconf",
        "dpkg",
        "perl-base",
        "ca-certificates",
        "tzdata",
        "base-files",
        "util-linux",
        "libpam0g",
        "libcap2-bin",
        "libseccomp2",
        "libudev1",
        "dconf-gsettings-backend",
        "dconf-service",
        "gtk-update-icon-cache",
        "hicolor-icon-theme",
        "shared-mime-info",
    ];
    const PREFIX: &[&str] = &[
        "libgl1",
        "libglx",
        "libegl",
        "libgles",
        "libopengl",
        "libglvnd",
        "libgbm",
        "libdrm",
        "libvulkan",
        "mesa-",
        "libasound",
        "libwayland-egl",
        "libllvm",
        "libxcb-dri",
        "libglapi",
        "xkb-data",
        "fonts-",
    ];
    EXACT.contains(&name) || PREFIX.iter().any(|p| name.starts_with(p))
}

#[derive(Debug, Clone, Default)]
struct Pkg {
    name: String,
    version: String,
    depends: Vec<Vec<String>>,
    provides: Vec<String>,
    filename: String,
    sha256: String,
}

fn strip_dep(s: &str) -> String {
    let s = s
        .split('(')
        .next()
        .unwrap_or("")
        .split('[')
        .next()
        .unwrap_or("")
        .trim();
    s.split(':').next().unwrap_or("").trim().to_string()
}

fn parse_packages(text: &str) -> Vec<Pkg> {
    let mut out = Vec::new();
    for stanza in text.split("\n\n") {
        let mut p = Pkg::default();
        let mut depends = String::new();
        for line in stanza.lines() {
            if line.starts_with(' ') || line.starts_with('\t') {
                continue;
            }
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let v = v.trim();
            match k {
                "Package" => p.name = v.to_string(),
                "Version" => p.version = v.to_string(),
                "Filename" => p.filename = v.to_string(),
                "SHA256" => p.sha256 = v.to_string(),
                "Depends" | "Pre-Depends" => {
                    if !depends.is_empty() {
                        depends.push_str(", ");
                    }
                    depends.push_str(v);
                }
                "Provides" => p.provides = v.split(',').map(strip_dep).collect(),
                _ => {}
            }
        }
        p.depends = depends
            .split(',')
            .map(|g| {
                g.split('|')
                    .map(strip_dep)
                    .filter(|n| !n.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|g: &Vec<String>| !g.is_empty())
            .collect();
        if !p.name.is_empty() && !p.filename.is_empty() {
            out.push(p);
        }
    }
    out
}

/// Debian version comparison (`[epoch:]upstream[-revision]`, `~` sorts before everything).
fn deb_version_cmp(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> (u64, &str, &str) {
        let (epoch, rest) = match v.split_once(':') {
            Some((e, r)) if e.chars().all(|c| c.is_ascii_digit()) => (e.parse().unwrap_or(0), r),
            _ => (0, v),
        };
        match rest.rsplit_once('-') {
            Some((u, r)) => (epoch, u, r),
            None => (epoch, rest, ""),
        }
    }
    fn order(c: Option<u8>) -> i32 {
        match c {
            None => 0,
            Some(b'~') => -1,
            Some(c) if c.is_ascii_alphabetic() => c as i32,
            Some(c) => c as i32 + 256,
        }
    }
    fn part(mut a: &[u8], mut b: &[u8]) -> Ordering {
        while !a.is_empty() || !b.is_empty() {
            let mut first = Ordering::Equal;
            while (!a.is_empty() && !a[0].is_ascii_digit())
                || (!b.is_empty() && !b[0].is_ascii_digit())
            {
                let (x, y) = (order(a.first().copied()), order(b.first().copied()));
                if x != y {
                    return x.cmp(&y);
                }
                a = a.get(1..).unwrap_or(&[]);
                b = b.get(1..).unwrap_or(&[]);
            }
            let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
            let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
            let (da, db) = (strip0(&a[..na]), strip0(&b[..nb]));
            first = da.len().cmp(&db.len()).then_with(|| da.cmp(db)).then(first);
            if first != Ordering::Equal {
                return first;
            }
            a = &a[na..];
            b = &b[nb..];
        }
        Ordering::Equal
    }
    fn strip0(d: &[u8]) -> &[u8] {
        let n = d.iter().take_while(|c| **c == b'0').count();
        &d[n..]
    }
    let (ea, ua, ra) = split(a);
    let (eb, ub, rb) = split(b);
    ea.cmp(&eb)
        .then_with(|| part(ua.as_bytes(), ub.as_bytes()))
        .then_with(|| part(ra.as_bytes(), rb.as_bytes()))
}

/// Closes the set of dependencies from the roots. Returns (packages kept, names not found).
fn resolve(index: &[Pkg]) -> (Vec<Pkg>, Vec<String>) {
    let mut best: BTreeMap<&str, &Pkg> = BTreeMap::new();
    for p in index {
        match best.get(p.name.as_str()) {
            Some(cur) if deb_version_cmp(&cur.version, &p.version) != Ordering::Less => {}
            _ => {
                best.insert(&p.name, p);
            }
        }
    }
    let mut providers: BTreeMap<&str, &Pkg> = BTreeMap::new();
    for p in best.values() {
        for v in &p.provides {
            providers.entry(v.as_str()).or_insert(p);
        }
    }
    let find = |n: &str| best.get(n).copied().or_else(|| providers.get(n).copied());
    let mut chosen: BTreeMap<String, Pkg> = BTreeMap::new();
    let mut missing = Vec::new();
    let mut queue: VecDeque<String> = ROOTS.iter().map(|s| s.to_string()).collect();
    while let Some(name) = queue.pop_front() {
        if chosen.contains_key(&name) || is_excluded(&name) {
            continue;
        }
        let Some(p) = find(&name) else {
            missing.push(name);
            continue;
        };
        if chosen.contains_key(&p.name) {
            continue;
        }
        chosen.insert(p.name.clone(), p.clone());
        for group in &p.depends {
            // first usable alternative; if all are excluded, the dependency is provided by the system
            if group.iter().any(|n| is_excluded(n)) {
                continue;
            }
            if let Some(n) = group.iter().find(|n| find(n).is_some()) {
                queue.push_back(n.clone());
            } else if let Some(n) = group.first() {
                missing.push(n.clone());
            }
        }
    }
    (chosen.into_values().collect(), missing)
}

/// Where a package file goes in the AppDir (`None` = ignored).
fn map_path(raw: &str) -> Option<PathBuf> {
    let p = raw.trim_start_matches("./").trim_end_matches('/');
    let p = p
        .strip_prefix("lib/x86_64-linux-gnu")
        .map_or(p.to_string(), |r| format!("usr/lib/x86_64-linux-gnu{r}"));
    if p.is_empty() || p.ends_with(".a") || p.ends_with(".la") {
        return None;
    }
    let skip_lib = [
        "/dri/",
        "/gconv/",
        "/audit/",
        "/perl",
        "/python",
        "/systemd/",
        "/security/",
        "/sasl2/",
        "/pkgconfig",
        "/cmake",
        "/girepository",
        "/glib-2.0/include",
        "/debug/",
    ];
    if let Some(rest) = p.strip_prefix(LIBDIR) {
        let probe = format!("{rest}/");
        return (!skip_lib.iter().any(|s| probe.contains(s))).then(|| PathBuf::from(&p));
    }
    if let Some(rest) = p.strip_prefix("usr/share/glib-2.0/schemas") {
        return (rest.is_empty() || rest.ends_with(".xml") || rest.ends_with(".override"))
            .then(|| PathBuf::from(&p));
    }
    if p.starts_with("usr/share/icons/Adwaita") {
        return (!p.contains("/cursors")).then(|| PathBuf::from(&p));
    }
    if let Some(rest) = p.strip_prefix("usr/share/locale/") {
        let lang = rest.split('/').next().unwrap_or("");
        let wanted =
            (lang == "fr" || lang == "en" || lang.starts_with("fr_") || lang.starts_with("en_"))
                && (rest.contains("gtk40")
                    || rest.contains("libadwaita")
                    || rest.contains("glib20")
                    || rest.ends_with(lang)
                    || rest.ends_with("LC_MESSAGES"));
        return wanted.then(|| PathBuf::from(&p));
    }
    None
}

// ---------------------------------------------------------------- reading the .deb files

fn read_ar(data: &[u8]) -> Result<Vec<(String, &[u8])>> {
    if !data.starts_with(b"!<arch>\n") {
        return Err("not an ar archive".into());
    }
    let mut pos = 8;
    let mut out = Vec::new();
    while pos + 60 <= data.len() {
        let h = &data[pos..pos + 60];
        let name = String::from_utf8_lossy(&h[..16])
            .trim()
            .trim_end_matches('/')
            .to_string();
        let size: usize = String::from_utf8_lossy(&h[48..58])
            .trim()
            .parse()
            .ctx("ar size")?;
        let start = pos + 60;
        let body = data.get(start..start + size).ok_or("truncated ar member")?;
        out.push((name, body));
        pos = start + size + (size % 2);
    }
    Ok(out)
}

fn decompress(name: &str, body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if name.ends_with(".xz") {
        lzma_rs::xz_decompress(&mut std::io::Cursor::new(body), &mut out)
            .map_err(|e| format!("xz: {e}"))?;
    } else if name.ends_with(".zst") {
        let mut d = ruzstd::StreamingDecoder::new(body).map_err(|e| format!("zstd: {e}"))?;
        d.read_to_end(&mut out).ctx("zstd")?;
    } else if name.ends_with(".gz") {
        flate2::read::GzDecoder::new(body)
            .read_to_end(&mut out)
            .ctx("gzip")?;
    } else {
        out.extend_from_slice(body);
    }
    Ok(out)
}

fn unpack_deb(deb: &[u8], appdir: &Path) -> Result<usize> {
    let members = read_ar(deb)?;
    let (name, body) = members
        .iter()
        .find(|(n, _)| n.starts_with("data.tar"))
        .ok_or("no data.tar in the package")?;
    let tar_bytes = decompress(name, body)?;
    let mut archive = tar::Archive::new(tar_bytes.as_slice());
    let mut n = 0;
    for entry in archive.entries().ctx("tar")? {
        let mut entry = entry.ctx("tar entry")?;
        let raw = entry.path().ctx("tar path")?.to_string_lossy().into_owned();
        let Some(rel) = map_path(&raw) else { continue };
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("unsafe path in package: {raw}"));
        }
        let dest = appdir.join(&rel);
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir).ctx("mkdir")?;
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&dest).ctx("mkdir")?;
        } else if kind.is_symlink() {
            if let Some(target) = entry.link_name().ctx("link")? {
                let _ = fs::remove_file(&dest);
                symlink(target.as_ref(), &dest).ctx("symlink")?;
                n += 1;
            }
        } else if kind.is_file() {
            let mode = entry.header().mode().unwrap_or(0o644);
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).ctx("read")?;
            let _ = fs::remove_file(&dest);
            fs::write(&dest, buf).ctx("write")?;
            fs::set_permissions(
                &dest,
                fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
            )
            .ctx("chmod")?;
            n += 1;
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------- network (curl) and cache

fn fetch(url: &str, dest: &Path) -> Result<()> {
    if dest.is_file() {
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).ctx("cache folder")?;
    }
    let tmp = dest.with_extension("part");
    let ok = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .ctx("cannot run curl")?
        .success();
    if !ok {
        let _ = fs::remove_file(&tmp);
        return Err(format!("download failed: {url}"));
    }
    fs::rename(&tmp, dest).ctx("cache")
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------- assemblage

const APPRUN: &str = r#"#!/bin/sh
# Launcher of the LinuxFlow AppImage: points GTK to the bundled libraries and data.
HERE=$(dirname "$(readlink -f "$0")")
export APPDIR=$HERE
LIB=$HERE/usr/lib/x86_64-linux-gnu
export LD_LIBRARY_PATH="$LIB${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export XDG_DATA_DIRS="$HERE/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export GSETTINGS_SCHEMA_DIR="$HERE/usr/share/glib-2.0/schemas"
export GTK_PATH="$LIB/gtk-4.0"
# The image loader paths depend on the mount point: the cache is rebuilt at every launch.
export GDK_PIXBUF_MODULEDIR="$LIB/gdk-pixbuf-2.0/2.10.0/loaders"
CACHE_DIR=${XDG_RUNTIME_DIR:-/tmp}
GDK_PIXBUF_MODULE_FILE="$CACHE_DIR/linuxflow-pixbuf-$$.cache"
if [ -x "$LIB/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders" ]; then
    "$LIB/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders" >"$GDK_PIXBUF_MODULE_FILE" 2>/dev/null || true
    export GDK_PIXBUF_MODULE_FILE
    trap 'rm -f "$GDK_PIXBUF_MODULE_FILE"' EXIT
fi
"$HERE/usr/bin/linuxflow" "$@"
"#;

pub const HELP: &str = "\
cargo xtask appimage [options]

  --out DIR     output folder (default: <root>/target/dist)
  --tool PATH   appimagetool already downloaded (otherwise downloaded into target/appimage-cache)

Prerequisite: the archive from `cargo xtask dist` must exist (target/dist/linuxflow-<version>-x86_64.tar.gz).
Network: Ubuntu 24.04 packages (SHA-256 verified), appimagetool and its runtime (GitHub), through curl.
";

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }
    let mut out_dir = root.join("target/dist");
    let mut tool: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out_dir = PathBuf::from(it.next().ok_or("--out needs a value")?),
            "--tool" => tool = Some(PathBuf::from(it.next().ok_or("--tool needs a value")?)),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let cache = root.join("target/appimage-cache");

    // 1. distribution archive
    let tarball = find_tarball(&root.join("target/dist"))?;
    let version = tarball
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("linuxflow-"))
        .and_then(|n| n.strip_suffix("-x86_64.tar.gz"))
        .ok_or("unexpected archive name")?
        .to_string();
    println!("LinuxFlow {version} AppImage from {}", tarball.display());

    let appdir = std::env::temp_dir().join(format!("linuxflow-appdir-{}", std::process::id()));
    let _ = fs::remove_dir_all(&appdir);
    fs::create_dir_all(&appdir).ctx("appdir")?;
    let result = assemble(root, &cache, &appdir, &tarball, &version, &out_dir, tool);
    let _ = fs::remove_dir_all(&appdir);
    result
}

fn find_tarball(dist: &Path) -> Result<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dist)
        .map_err(|_| format!("{} not found: run `cargo xtask dist` first", dist.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("linuxflow-") && n.ends_with("-x86_64.tar.gz"))
        })
        .collect();
    found.sort();
    found.pop().ok_or_else(|| {
        "no linuxflow-*-x86_64.tar.gz in target/dist: run `cargo xtask dist` first".to_string()
    })
}

fn assemble(
    root: &Path,
    cache: &Path,
    appdir: &Path,
    tarball: &Path,
    version: &str,
    out_dir: &Path,
    tool: Option<PathBuf>,
) -> Result<()> {
    // 2. archive content -> usr/
    let usr = appdir.join("usr");
    fs::create_dir_all(&usr).ctx("usr")?;
    let gz = flate2::read::GzDecoder::new(fs::File::open(tarball).ctx("archive")?);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries().ctx("archive")? {
        let mut entry = entry.ctx("archive entry")?;
        let path = entry.path().ctx("path")?.to_path_buf();
        let rel: PathBuf = path.components().skip(1).collect(); // retire linuxflow-<v>-x86_64/
        let top = rel
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or("");
        if top != "bin" && top != "share" {
            continue;
        }
        let dest = usr.join(&rel);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&dest).ctx("mkdir")?;
        } else {
            if let Some(d) = dest.parent() {
                fs::create_dir_all(d).ctx("mkdir")?;
            }
            entry.unpack(&dest).ctx("unpack")?;
        }
    }

    // 3. Ubuntu 24.04 graphical stack
    let mut index = Vec::new();
    for suite in SUITES {
        let file = cache.join(format!("Packages-{suite}-main.gz"));
        fetch(
            &format!("{MIRROR}/dists/{suite}/main/binary-amd64/Packages.gz"),
            &file,
        )?;
        let mut text = Vec::new();
        flate2::read::GzDecoder::new(fs::File::open(&file).ctx("index")?)
            .read_to_end(&mut text)
            .ctx("index gzip")?;
        index.extend(parse_packages(&String::from_utf8_lossy(&text)));
    }
    let (packages, missing) = resolve(&index);
    println!("{} Ubuntu 24.04 packages to bundle", packages.len());
    if !missing.is_empty() {
        println!(
            "  dependencies not found (ignored): {}",
            missing
                .iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let mut files = 0;
    for p in &packages {
        let deb_path = cache
            .join("debs")
            .join(p.filename.rsplit('/').next().unwrap_or(&p.filename));
        fetch(&format!("{MIRROR}/{}", p.filename), &deb_path)?;
        let data = fs::read(&deb_path).ctx("deb")?;
        if sha256_hex(&data) != p.sha256 {
            let _ = fs::remove_file(&deb_path);
            return Err(format!(
                "SHA-256 mismatch for {} (file removed, run again)",
                p.name
            ));
        }
        files += unpack_deb(&data, appdir).map_err(|e| format!("{}: {e}", p.name))?;
    }
    println!("{files} files unpacked");

    // layer-shell library: absent from Ubuntu 24.04, taken from the build machine if it exists
    let lib = appdir.join(LIBDIR);
    fs::create_dir_all(&lib).ctx("lib")?;
    match fs::canonicalize("/usr/lib/libgtk4-layer-shell.so.0") {
        Ok(real) => {
            fs::copy(&real, lib.join("libgtk4-layer-shell.so.0")).ctx("layer-shell")?;
        }
        Err(_) => {
            println!("  libgtk4-layer-shell missing from the machine: the pill will use XWayland")
        }
    }

    // compiled GSettings schemas
    let schemas = appdir.join("usr/share/glib-2.0/schemas");
    if schemas.is_dir() {
        let ok = Command::new("glib-compile-schemas")
            .arg(&schemas)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err(
                "glib-compile-schemas failed (package glib2 / libglib2.0-bin on the build machine)"
                    .into(),
            );
        }
    }

    // 4. AppRun, .desktop, icon
    fs::write(appdir.join("AppRun"), APPRUN).ctx("AppRun")?;
    fs::set_permissions(appdir.join("AppRun"), fs::Permissions::from_mode(0o755)).ctx("chmod")?;
    let app_id = read_app_id(root)?;
    let desktop = fs::read_to_string(usr.join(format!("share/applications/{app_id}.desktop")))
        .ctx("desktop file")?;
    let desktop = desktop.replace("@BINDIR@/linuxflow", "linuxflow");
    fs::write(appdir.join(format!("{app_id}.desktop")), desktop).ctx("desktop file")?;
    let icon = usr.join(format!("share/icons/hicolor/256x256/apps/{app_id}.png"));
    fs::copy(&icon, appdir.join(format!("{app_id}.png"))).ctx("icon")?;
    let _ = symlink(format!("{app_id}.png"), appdir.join(".DirIcon"));

    // 5. appimagetool + runtime
    let tool = match tool {
        Some(t) => t,
        None => {
            let t = cache.join("appimagetool-x86_64.AppImage");
            fetch(APPIMAGETOOL_URL, &t)?;
            fs::set_permissions(&t, fs::Permissions::from_mode(0o755)).ctx("chmod")?;
            t
        }
    };
    let runtime = cache.join("runtime-x86_64");
    fetch(RUNTIME_URL, &runtime)?;
    fs::create_dir_all(out_dir).ctx("output folder")?;
    let out = out_dir.join(format!("LinuxFlow-{version}-x86_64.AppImage"));
    let _ = fs::remove_file(&out);
    let status = Command::new(&tool)
        .args([
            "--appimage-extract-and-run",
            "--no-appstream",
            "--runtime-file",
        ])
        .arg(&runtime)
        .arg(appdir)
        .arg(&out)
        .env("ARCH", "x86_64")
        .status()
        .ctx("cannot run appimagetool")?;
    if !status.success() {
        return Err("appimagetool failed".into());
    }
    let data = fs::read(&out).ctx("AppImage")?;
    let line = format!(
        "{}  {}\n",
        sha256_hex(&data),
        out.file_name().and_then(|n| n.to_str()).unwrap_or("")
    );
    fs::write(out.with_extension("AppImage.sha256"), &line).ctx("sha256")?;
    println!(
        "AppImage : {} ({} Mo)",
        out.display(),
        data.len() / 1_000_000
    );
    print!("{line}");
    let _ = capture; // (shared utility, unused here)
    Ok(())
}

fn read_app_id(root: &Path) -> Result<String> {
    let c = fs::read_to_string(root.join("crates/linuxflow-core/src/constants.rs"))
        .ctx("constants.rs")?;
    c.lines()
        .find_map(|l| {
            l.strip_prefix("pub const APP_ID: &str = \"")
                .and_then(|r| r.split('"').next())
        })
        .map(str::to_string)
        .ok_or_else(|| "APP_ID not found".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debian_versions_compare_like_dpkg() {
        use Ordering::*;
        assert_eq!(deb_version_cmp("1.0-1", "1.0-2"), Less);
        assert_eq!(deb_version_cmp("2.39-0ubuntu8.3", "2.39-0ubuntu8.10"), Less);
        assert_eq!(deb_version_cmp("1.0~rc1-1", "1.0-1"), Less);
        assert_eq!(deb_version_cmp("1:0.9-1", "2.0-1"), Greater);
        assert_eq!(
            deb_version_cmp("4.14.5+ds-0ubuntu0.1", "4.14.5+ds-0ubuntu0.1"),
            Equal
        );
        assert_eq!(deb_version_cmp("1.10", "1.9"), Greater);
    }

    #[test]
    fn packages_are_parsed_with_alternatives_and_provides() {
        let idx = "Package: libfoo1\nVersion: 1.2-1\nDepends: libc6 (>= 2.38), libbar1 | libbaz1, libqux:any\nProvides: libfoo-abi (= 1)\nFilename: pool/main/libfoo1.deb\nSHA256: abc\nDescription: x\n continuation\n\nPackage: broken\nVersion: 1\n";
        let p = parse_packages(idx);
        assert_eq!(p.len(), 1, "a stanza without Filename is ignored");
        assert_eq!(
            p[0].depends,
            vec![vec!["libc6"], vec!["libbar1", "libbaz1"], vec!["libqux"]]
        );
        assert_eq!(p[0].provides, vec!["libfoo-abi"]);
    }

    #[test]
    fn the_closure_stops_at_system_packages_and_prefers_the_newest_version() {
        let mk = |n: &str, v: &str, d: &str| Pkg {
            name: n.into(),
            version: v.into(),
            depends: d
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|g| g.split('|').map(|x| x.trim().to_string()).collect())
                .collect(),
            filename: format!("{n}.deb"),
            ..Pkg::default()
        };
        let index = vec![
            mk(
                "libgtk-4-1",
                "1",
                "libc6, libglib, libegl1, libwayland | other",
            ),
            mk("libglib", "2", ""),
            mk("libglib", "10", ""),
            mk("libwayland", "1", ""),
            mk("libadwaita-1-0", "1", "libgtk-4-1"),
        ];
        let (chosen, missing) = resolve(&index);
        let names: Vec<_> = chosen.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.contains(&"libgtk-4-1")
                && names.contains(&"libglib")
                && names.contains(&"libwayland")
        );
        assert!(!names.contains(&"libc6") && !names.contains(&"libegl1"));
        assert_eq!(
            chosen
                .iter()
                .find(|p| p.name == "libglib")
                .map(|p| p.version.as_str()),
            Some("10")
        );
        assert!(
            missing.iter().any(|m| m == "libgdk-pixbuf-2.0-0"),
            "{missing:?}"
        );
    }

    #[test]
    fn only_the_wanted_files_are_kept_and_the_usr_merge_is_applied() {
        let keep = |p: &str| map_path(p).map(|x| x.to_string_lossy().into_owned());
        assert_eq!(
            keep("./usr/lib/x86_64-linux-gnu/libgtk-4.so.1.1400.5").as_deref(),
            Some("usr/lib/x86_64-linux-gnu/libgtk-4.so.1.1400.5")
        );
        assert_eq!(
            keep("./lib/x86_64-linux-gnu/libz.so.1").as_deref(),
            Some("usr/lib/x86_64-linux-gnu/libz.so.1")
        );
        assert_eq!(keep("./usr/lib/x86_64-linux-gnu/dri/radeonsi_dri.so"), None);
        assert_eq!(keep("./usr/lib/x86_64-linux-gnu/libfoo.a"), None);
        assert_eq!(keep("./usr/share/doc/libgtk-4-1/copyright"), None);
        assert_eq!(keep("./usr/share/man/man1/x.1.gz"), None);
        assert!(
            keep("./usr/share/glib-2.0/schemas/org.gtk.gtk4.Settings.FileChooser.gschema.xml")
                .is_some()
        );
        assert!(
            keep("./usr/share/icons/Adwaita/symbolic/actions/edit-copy-symbolic.svg").is_some()
        );
        assert_eq!(keep("./usr/share/icons/Adwaita/cursors/left_ptr"), None);
        assert!(keep("./usr/share/locale/fr/LC_MESSAGES/gtk40.mo").is_some());
        assert_eq!(keep("./usr/share/locale/de/LC_MESSAGES/gtk40.mo"), None);
    }

    #[test]
    fn an_ar_archive_is_read_member_by_member() {
        let mut data = b"!<arch>\n".to_vec();
        for (name, body) in [
            ("debian-binary", b"2.0\n".as_slice()),
            ("data.tar.xz", b"abc".as_slice()),
        ] {
            data.extend_from_slice(
                format!(
                    "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                    format!("{name}/"),
                    0,
                    0,
                    0,
                    "100644",
                    body.len()
                )
                .as_bytes(),
            );
            data.extend_from_slice(body);
            if body.len() % 2 == 1 {
                data.push(b'\n');
            }
        }
        let m = read_ar(&data).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].0, "data.tar.xz");
        assert_eq!(m[1].1, b"abc");
        assert!(read_ar(b"nope").is_err());
    }
}
