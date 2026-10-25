//! linuxflow-i18n: translation and localised formats. The msgids are in ENGLISH (source language).
//!
//! Stable API used by all the crates: [`tr`], [`trn`], [`tr_fmt`].
//!
//! # Implementation choice
//! Reader of GNU gettext `.mo` catalogues **in pure Rust** (`mo` and `plural` modules), rather than
//! `gettext-rs`: the latter relies on the libc's libintl, whose state is global to the process,
//! which ignores `LANGUAGE` when the system locale is `C` or not generated, which offers no
//! reliable live language switching, and which requires a `setlocale` (not portable, `unsafe`
//! upstream); `gettext-sys` (C) would also have to be linked outside glibc. Here: no C dependency, no
//! system locale required, instant language switching, complete `Plural-Forms` plurals, and
//! deterministic tests. The catalogues remain standard `.mo` files (`msgfmt`, Weblate, Poedit).
//!
//! # Catalogues
//! `<folder>/<language>/LC_MESSAGES/<domain>.mo`. The folder is resolved by [`init`] without a hard-coded
//! path: explicit argument, `LINUXFLOW_LOCALE_DIR`, `<executable prefix>/share/locale`,
//! then `$XDG_DATA_DIRS/locale`. Missing or unreadable catalogue => English, never a panic.

mod format;
mod locale;
mod mo;
mod plural;

pub use locale::LOCALE_DIR_ENV;
pub use mo::Catalog;
pub use plural::PluralRule;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Default gettext domain.
pub const DEFAULT_DOMAIN: &str = "linuxflow";

/// Translator for a given language (usable independently of the global state).
#[derive(Debug, Clone, Default)]
pub struct Translator {
    lang: String,
    catalog: Option<Arc<Catalog>>,
}

impl Translator {
    /// Translator of the source language (English): returns the msgids.
    pub fn source() -> Self {
        Translator {
            lang: "en".into(),
            catalog: None,
        }
    }

    /// Translator from an already loaded catalogue.
    pub fn with_catalog(lang: &str, catalog: Catalog) -> Self {
        Translator {
            lang: lang.to_string(),
            catalog: Some(Arc::new(catalog)),
        }
    }

    /// Looks for the first available catalogue among `candidates` (in order) and `dirs`.
    pub fn load(domain: &str, candidates: &[String], dirs: &[PathBuf]) -> Self {
        for lang in candidates {
            for dir in dirs {
                let path = locale::mo_path(dir, lang, domain);
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                match Catalog::parse(&bytes) {
                    Ok(c) => return Translator::with_catalog(lang, c),
                    Err(e) => eprintln!("linuxflow-i18n: {} ignored ({e})", path.display()),
                }
            }
        }
        Translator::source()
    }

    /// Language actually used (`en` if no catalogue was found).
    pub fn language(&self) -> &str {
        &self.lang
    }

    /// Translates a message.
    pub fn tr(&self, msgid: &str) -> String {
        self.catalog
            .as_ref()
            .and_then(|c| c.get(msgid))
            .unwrap_or(msgid)
            .to_string()
    }

    /// Translates with a plural.
    pub fn trn(&self, singular: &str, plural: &str, n: u64) -> String {
        self.catalog
            .as_ref()
            .and_then(|c| c.get_plural(singular, n))
            .unwrap_or(if n == 1 { singular } else { plural })
            .to_string()
    }

    /// Translates then substitutes successive `{}` or `{0}`, `{1}`... Never a panic: a
    /// missing argument leaves the marker as is, an extra argument is ignored.
    pub fn tr_fmt(&self, msgid: &str, args: &[&str]) -> String {
        substitute(&self.tr(msgid), args)
    }

    fn style(&self) -> format::NumberStyle {
        format::style_for(&self.lang)
    }

    /// Number with the separators of the language (fr: U+202F for thousands, decimal comma).
    pub fn format_number(&self, value: f64, decimals: usize) -> String {
        format::number(self.style(), value, decimals)
    }

    /// Percentage from a ratio (0.425 => "42,5 %").
    pub fn format_percent(&self, ratio: f64, decimals: usize) -> String {
        format::percent(self.style(), ratio, decimals)
    }

    /// Size in bytes: o/Ko/Mo/Go/To in French, B/KB/MB/GB/TB in English (decimal base).
    pub fn format_bytes(&self, bytes: u64) -> String {
        let (idx, value, decimals) = format::byte_unit(bytes);
        let n = self.format_number(value, decimals);
        let n = n.as_str();
        // Closures named like the public API: xgettext only extracts the `tr("...")` calls
        // (not the `self.tr(...)` method calls).
        let tr_fmt = |m: &str, a: &[&str]| self.tr_fmt(m, a);
        // TRANSLATORS: size in bytes; {} = already formatted number. Decimal units (1 GB = 10^9 B).
        match idx {
            0 => tr_fmt("{} B", &[n]),
            1 => tr_fmt("{} KB", &[n]),
            2 => tr_fmt("{} MB", &[n]),
            3 => tr_fmt("{} GB", &[n]),
            _ => tr_fmt("{} TB", &[n]),
        }
    }

    fn month_abbrev(&self, month: u8) -> String {
        // Closures named like the public API: xgettext only extracts the `tr("...")` calls
        // (not the `self.tr(...)` method calls).
        let tr = |m: &str| self.tr(m);
        match month {
            1 => tr("Jan"),
            2 => tr("Feb"),
            3 => tr("Mar"),
            4 => tr("Apr"),
            5 => tr("May"),
            6 => tr("Jun"),
            7 => tr("Jul"),
            8 => tr("Aug"),
            9 => tr("Sep"),
            10 => tr("Oct"),
            11 => tr("Nov"),
            _ => tr("Dec"),
        }
    }

    /// Relative time: "just now", "N min ago", "today at HH:MM", otherwise short date + time.
    /// `ts` and `now` in Unix seconds, `offset_secs` = local UTC offset.
    pub fn format_relative_at(&self, ts: i64, now: i64, offset_secs: i32) -> String {
        let diff = now.saturating_sub(ts);
        // Closures named like the public API: xgettext only extracts the `tr("...")` calls
        // (not the `self.tr(...)` method calls).
        let tr = |m: &str| self.tr(m);
        let tr_fmt = |m: &str, a: &[&str]| self.tr_fmt(m, a);
        if diff < 60 {
            return tr("just now");
        }
        if diff < 3600 {
            let minutes = (diff / 60) as u64;
            // "min" is invariable: no plural form.
            return tr_fmt("{} min ago", &[&minutes.to_string()]);
        }
        let Some(t) = format::local_time(ts, offset_secs) else {
            return tr("just now");
        };
        let hm = format!("{:02}:{:02}", t.hour, t.minute);
        let today = format::local_time(now, offset_secs).map(|n| n.ordinal_day);
        if today == Some(t.ordinal_day) {
            return tr_fmt("today at {}", &[&hm]);
        }
        let month = self.month_abbrev(t.month);
        let day = t.day.to_string();
        // Year shown only if different from the current year.
        let date = if format::local_time(now, offset_secs).is_some_and(|n| n.year != t.year) {
            let year = t.year.to_string();
            // TRANSLATORS: short date with year. {0} = abbreviated month, {1} = day, {2} = year.
            // Reorder freely (e.g. fr: "{1} {0} {2}").
            tr_fmt("{0} {1} {2}", &[&month, &day, &year])
        } else {
            // TRANSLATORS: short date. {0} = abbreviated month, {1} = day of the month (e.g. "Oct 24").
            tr_fmt("{0} {1}", &[&month, &day])
        };
        // TRANSLATORS: date then time. {0} = short date, {1} = time HH:MM.
        tr_fmt("{0}, {1}", &[&date, &hm])
    }
}

/// Safe substitution of `{}` / `{N}` in an already translated template.
fn substitute(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0usize;
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let close = after.find('}');
        let token = close.map(|c| &after[..c]);
        let arg = match token {
            Some("") => {
                let a = args.get(next);
                next += 1;
                Some(a)
            }
            Some(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => {
                Some(t.parse::<usize>().ok().and_then(|n| args.get(n)))
            }
            _ => None,
        };
        match (arg, close) {
            (Some(Some(a)), Some(c)) => {
                out.push_str(a);
                rest = &after[c + 1..];
            }
            (Some(None), Some(c)) => {
                out.push_str(&rest[i..i + c + 2]);
                rest = &after[c + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// --- Global state -----------------------------------------------------------------------------

struct State {
    domain: String,
    explicit_dir: Option<PathBuf>,
    translator: Arc<Translator>,
}

static STATE: RwLock<Option<State>> = RwLock::new(None);

fn env_get(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn current() -> Arc<Translator> {
    let guard = STATE.read().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(s) => Arc::clone(&s.translator),
        None => Arc::new(Translator::source()),
    }
}

fn reload(domain: &str, explicit_dir: Option<&Path>, language: Option<&str>) -> Translator {
    let exe = std::env::current_exe().ok();
    let dirs = locale::locale_dirs(explicit_dir, &env_get, exe.as_deref());
    let candidates = locale::from_setting(language, &env_get);
    Translator::load(domain, &candidates, &dirs)
}

/// Initialises the translation: resolves the catalogue folder and detects the language
/// (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`). Idempotent; never panics.
/// Then call [`set_language`] with `Settings.ui_language` to honour the setting.
pub fn init(domain: &str, locale_dir_override: Option<&Path>) {
    let translator = Arc::new(reload(domain, locale_dir_override, None));
    let mut guard = STATE.write().unwrap_or_else(|e| e.into_inner());
    *guard = Some(State {
        domain: domain.to_string(),
        explicit_dir: locale_dir_override.map(Path::to_path_buf),
        translator,
    });
}

/// Changes the language live. `None` (or `""`, `"auto"`) = system language; otherwise a code
/// (`"fr"`, `"fr_FR"`, `"en"`). Without a matching catalogue, falls back to English.
pub fn set_language(lang: Option<&str>) {
    let (domain, dir) = {
        let guard = STATE.read().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(s) => (s.domain.clone(), s.explicit_dir.clone()),
            None => (DEFAULT_DOMAIN.to_string(), None),
        }
    };
    let translator = Arc::new(reload(&domain, dir.as_deref(), lang));
    let mut guard = STATE.write().unwrap_or_else(|e| e.into_inner());
    *guard = Some(State {
        domain,
        explicit_dir: dir,
        translator,
    });
}

/// Language currently active (`"en"` if no catalogue is loaded).
pub fn current_language() -> String {
    current().language().to_string()
}

/// Translates a message (English msgid).
pub fn tr(msgid: &str) -> String {
    current().tr(msgid)
}

/// Translates with a plural: `n` chooses the form (`ngettext`).
pub fn trn(singular: &str, plural: &str, n: u64) -> String {
    current().trn(singular, plural, n)
}

/// Replaces successive `{}` (or `{0}`, `{1}`...) with the arguments, after translating the template.
pub fn tr_fmt(msgid: &str, args: &[&str]) -> String {
    current().tr_fmt(msgid, args)
}

/// Number according to the active language.
pub fn format_number(value: f64, decimals: usize) -> String {
    current().format_number(value, decimals)
}

/// Size in bytes according to the active language (GB/MB in French).
pub fn format_bytes(bytes: u64) -> String {
    current().format_bytes(bytes)
}

/// Percentage (ratio 0..1) according to the active language.
pub fn format_percent(ratio: f64, decimals: usize) -> String {
    current().format_percent(ratio, decimals)
}

/// Relative time of a Unix timestamp (seconds) compared to now, in local time.
pub fn format_relative(unix_secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix_secs);
    current().format_relative_at(unix_secs, now, format::local_offset_secs(now))
}
