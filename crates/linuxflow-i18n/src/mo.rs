//! Reader of `.mo` catalogues (GNU gettext format) in pure Rust.

use crate::plural::PluralRule;
use std::collections::HashMap;

/// Catalogue loaded in memory.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    entries: HashMap<String, Vec<String>>,
    plural: PluralRule,
}

const MAGIC: u32 = 0x9504_12de;
/// Safeguard: a catalogue bigger than this is not a LinuxFlow catalogue.
const MAX_ENTRIES: usize = 200_000;

fn rd(data: &[u8], off: usize, big: bool) -> Option<u32> {
    let b: [u8; 4] = data.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(b)
    } else {
        u32::from_le_bytes(b)
    })
}

fn slice(data: &[u8], len: u32, off: u32) -> Option<&[u8]> {
    let (len, off) = (len as usize, off as usize);
    data.get(off..off.checked_add(len)?)
}

impl Catalog {
    /// Parses the content of a `.mo` file. Error (never a panic) if the format is invalid.
    pub fn parse(data: &[u8]) -> Result<Catalog, String> {
        let magic_le = rd(data, 0, false).ok_or("fichier .mo trop court")?;
        let big = if magic_le == MAGIC {
            false
        } else if magic_le.swap_bytes() == MAGIC {
            true
        } else {
            return Err("nombre magique .mo invalide".into());
        };
        let bad = || "truncated .mo table".to_string();
        let n = rd(data, 8, big).ok_or_else(bad)? as usize;
        let o_orig = rd(data, 12, big).ok_or_else(bad)? as usize;
        let o_tr = rd(data, 16, big).ok_or_else(bad)? as usize;
        if n > MAX_ENTRIES {
            return Err("catalogue .mo trop volumineux".into());
        }
        let mut cat = Catalog::default();
        for i in 0..n {
            let at = |base: usize| base.checked_add(i.checked_mul(8)?);
            let (lo, oo) = (at(o_orig).ok_or_else(bad)?, at(o_tr).ok_or_else(bad)?);
            let orig = slice(
                data,
                rd(data, lo, big).ok_or_else(bad)?,
                rd(data, lo.wrapping_add(4), big).ok_or_else(bad)?,
            )
            .ok_or_else(bad)?;
            let tr = slice(
                data,
                rd(data, oo, big).ok_or_else(bad)?,
                rd(data, oo.wrapping_add(4), big).ok_or_else(bad)?,
            )
            .ok_or_else(bad)?;
            let (Ok(orig), Ok(tr)) = (std::str::from_utf8(orig), std::str::from_utf8(tr)) else {
                continue;
            };
            if orig.is_empty() {
                if let Some(rule) = tr
                    .lines()
                    .find_map(|l| l.strip_prefix("Plural-Forms:"))
                    .and_then(PluralRule::parse)
                {
                    cat.plural = rule;
                }
                continue;
            }
            let key = orig.split('\0').next().unwrap_or(orig);
            let forms: Vec<String> = tr.split('\0').map(str::to_string).collect();
            if forms.iter().all(|f| f.is_empty()) {
                continue;
            }
            cat.entries.insert(key.to_string(), forms);
        }
        Ok(cat)
    }

    /// Number of messages.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the catalogue contains no message.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Translation of a msgid (singular form), if present and non-empty.
    pub fn get(&self, msgid: &str) -> Option<&str> {
        self.entries
            .get(msgid)
            .and_then(|f| f.first())
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// Translation of a message with a plural for `n`.
    pub fn get_plural(&self, singular: &str, n: u64) -> Option<&str> {
        let forms = self.entries.get(singular)?;
        let i = self.plural.index(n);
        forms.get(i).map(String::as_str).filter(|s| !s.is_empty())
    }
}

/// Builds a minimal little-endian `.mo` (useful for the tests, without depending on `msgfmt`).
#[cfg(test)]
pub(crate) fn build_mo(pairs: &[(&str, &str)], big: bool) -> Vec<u8> {
    let mut sorted: Vec<_> = pairs.to_vec();
    sorted.sort();
    let n = sorted.len() as u32;
    let w = |v: u32| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let head = 28u32;
    let o_orig = head;
    let o_tr = head + n * 8;
    let mut strings = Vec::new();
    let mut origs = Vec::new();
    let mut trs = Vec::new();
    let base = o_tr + n * 8;
    for (o, t) in &sorted {
        origs.push((o.len() as u32, base + strings.len() as u32));
        strings.extend_from_slice(o.as_bytes());
        strings.push(0);
        trs.push((t.len() as u32, base + strings.len() as u32));
        strings.extend_from_slice(t.as_bytes());
        strings.push(0);
    }
    let mut out = Vec::new();
    for v in [MAGIC, 0, n, o_orig, o_tr, 0, 0] {
        out.extend_from_slice(&w(v));
    }
    for (l, o) in origs.iter().chain(trs.iter()) {
        out.extend_from_slice(&w(*l));
        out.extend_from_slice(&w(*o));
    }
    out.extend_from_slice(&strings);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "Content-Type: text/plain; charset=UTF-8\nPlural-Forms: nplurals=2; plural=(n > 1);\n";

    #[test]
    fn reads_both_endiannesses() {
        for big in [false, true] {
            let mo = build_mo(&[("", HEADER), ("Hello", "Bonjour"), ("Empty", "")], big);
            let c = Catalog::parse(&mo).unwrap_or_default();
            assert_eq!(c.get("Hello"), Some("Bonjour"), "big={big}");
            assert_eq!(c.get("Empty"), None);
            assert_eq!(c.get("Missing"), None);
            assert_eq!(c.len(), 1);
        }
    }

    #[test]
    fn plural_entries_use_header_rule() {
        let mo = build_mo(
            &[
                ("", HEADER),
                ("{} file\0{} files", "{} fichier\0{} fichiers"),
            ],
            false,
        );
        let c = Catalog::parse(&mo).unwrap_or_default();
        assert_eq!(c.get_plural("{} file", 0), Some("{} fichier"));
        assert_eq!(c.get_plural("{} file", 1), Some("{} fichier"));
        assert_eq!(c.get_plural("{} file", 2), Some("{} fichiers"));
    }

    #[test]
    fn garbage_never_panics() {
        assert!(Catalog::parse(&[]).is_err());
        assert!(Catalog::parse(b"not a mo file at all").is_err());
        let mo = build_mo(&[("", HEADER), ("a", "b")], false);
        for cut in 0..mo.len() {
            let _ = Catalog::parse(&mo[..cut]);
        }
        // Offsets outside the file.
        let mut bad = mo.clone();
        bad[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Catalog::parse(&bad).is_err());
    }
}
