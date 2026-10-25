//! Language detection and resolution of the catalogue folders (no hard-coded path).

use std::path::{Path, PathBuf};

/// Environment variable that forces the catalogue folder.
pub const LOCALE_DIR_ENV: &str = "LINUXFLOW_LOCALE_DIR";

/// Normalises a locale identifier: `fr_FR.UTF-8@euro` => `fr_FR`, `fr-CA` => `fr_CA`.
/// `None` for neutral locales (`C`, `POSIX`, empty), which designate the source language.
pub fn normalize(raw: &str) -> Option<String> {
    let base = raw
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .trim()
        .replace('-', "_");
    let valid = !base.is_empty()
        && base.len() <= 32
        && base.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (valid && base != "C" && base != "POSIX").then_some(base)
}

/// Ordered list of candidate languages (`fr_CA` then `fr`), from a raw value
/// (or an `a:b:c` list for `LANGUAGE`).
pub fn expand(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw.split(':') {
        let Some(n) = normalize(item) else {
            // Explicit `C` in a list: the source language stops the search.
            if matches!(item.trim(), "C" | "POSIX") {
                break;
            }
            continue;
        };
        let mut push = |s: String| {
            if !out.contains(&s) {
                out.push(s);
            }
        };
        let primary = n.split('_').next().unwrap_or(&n).to_string();
        push(n);
        push(primary);
    }
    out
}

/// Candidate languages according to the environment, in gettext priority order:
/// `LANGUAGE`, then the first defined of `LC_ALL`, `LC_MESSAGES`, `LANG`.
pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let non_empty = |k: &str| get(k).filter(|v| !v.trim().is_empty());
    let mut out = Vec::new();
    if let Some(l) = non_empty("LANGUAGE") {
        out = expand(&l);
        // `LANGUAGE=C` explicitly asks for the source language.
        if out.is_empty() && matches!(l.split(':').next().map(str::trim), Some("C" | "POSIX")) {
            return out;
        }
    }
    if out.is_empty() {
        if let Some(l) = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| non_empty(k))
        {
            out = expand(&l);
        }
    }
    out
}

/// Candidates for an explicitly requested language (`ui_language` setting).
pub fn from_setting(lang: Option<&str>, get: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    match lang.map(str::trim) {
        None | Some("") | Some("auto") | Some("system") => from_env(get),
        Some(l) => expand(l),
    }
}

/// Possible catalogue folders, in priority order:
/// 1. the explicit argument, 2. `LINUXFLOW_LOCALE_DIR`,
/// 3. `<executable prefix>/share/locale` (executable in `<prefix>/bin`),
/// 4. `$XDG_DATA_DIRS/locale` (XDG default of the specification if not defined).
pub fn locale_dirs(
    explicit: Option<&Path>,
    get: &dyn Fn(&str) -> Option<String>,
    exe: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    };
    if let Some(p) = explicit {
        push(p.to_path_buf());
    }
    if let Some(v) = get(LOCALE_DIR_ENV).filter(|v| !v.is_empty()) {
        push(PathBuf::from(v));
    }
    if let Some(prefix) = exe.and_then(Path::parent).and_then(Path::parent) {
        push(prefix.join("share").join("locale"));
    }
    // Default value imposed by the XDG Base Directory specification.
    let xdg = get("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for d in xdg.split(':').filter(|d| !d.is_empty()) {
        push(Path::new(d).join("locale"));
    }
    dirs
}

/// Path of a catalogue: `<dir>/<lang>/LC_MESSAGES/<domain>.mo`.
pub fn mo_path(dir: &Path, lang: &str, domain: &str) -> PathBuf {
    dir.join(lang)
        .join("LC_MESSAGES")
        .join(format!("{domain}.mo"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize("fr_FR.UTF-8@euro").as_deref(), Some("fr_FR"));
        assert_eq!(normalize("fr-CA").as_deref(), Some("fr_CA"));
        assert_eq!(normalize("C"), None);
        assert_eq!(normalize("POSIX"), None);
        assert_eq!(normalize("../etc"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn env_priority() {
        let e = env(&[
            ("LANG", "de_DE.UTF-8"),
            ("LC_ALL", "fr_FR.UTF-8"),
            ("LANGUAGE", "es:fr"),
        ]);
        assert_eq!(from_env(&e), ["es", "fr"]);
        let e = env(&[("LANG", "de_DE.UTF-8"), ("LC_ALL", "fr_FR.UTF-8")]);
        assert_eq!(from_env(&e), ["fr_FR", "fr"]);
        let e = env(&[("LANG", "de_DE.UTF-8"), ("LC_MESSAGES", "")]);
        assert_eq!(from_env(&e), ["de_DE", "de"]);
        assert!(from_env(&env(&[("LANG", "C")])).is_empty());
        assert!(from_env(&env(&[])).is_empty());
        // `C` in LANGUAGE cuts the list.
        assert_eq!(from_env(&env(&[("LANGUAGE", "fr:C:de")])), ["fr"]);
        assert!(from_env(&env(&[("LANGUAGE", "C"), ("LANG", "fr_FR.UTF-8")])).is_empty());
    }

    #[test]
    fn setting_overrides_env() {
        let e = env(&[("LANG", "de_DE.UTF-8")]);
        assert_eq!(from_setting(Some("fr"), &e), ["fr"]);
        assert_eq!(from_setting(None, &e), ["de_DE", "de"]);
        assert_eq!(from_setting(Some(""), &e), ["de_DE", "de"]);
        assert_eq!(from_setting(Some("en"), &e), ["en"]);
    }

    #[test]
    fn dirs_order() {
        let e = env(&[(LOCALE_DIR_ENV, "/a"), ("XDG_DATA_DIRS", "/x:/y")]);
        let d = locale_dirs(
            Some(Path::new("/z")),
            &e,
            Some(Path::new("/opt/ff/bin/linuxflow")),
        );
        let want: Vec<PathBuf> = ["/z", "/a", "/opt/ff/share/locale", "/x/locale", "/y/locale"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(d, want);
        let d = locale_dirs(None, &env(&[]), None);
        assert_eq!(
            d,
            [
                PathBuf::from("/usr/local/share/locale"),
                PathBuf::from("/usr/share/locale")
            ]
        );
    }
}
