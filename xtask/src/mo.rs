//! Compilation of a `.po` catalogue into `.mo` (binary GNU gettext format), in Rust: replaces
//! `msgfmt`. Same semantics as `msgfmt`: "fuzzy", obsolete or untranslated entries
//! are ignored; the header (empty msgid) is kept; plural forms are joined by NUL;
//! the context (`msgctxt`) precedes the msgid, separated by EOT (0x04).

use crate::common::{Ctx, Result};
use std::collections::BTreeMap;

#[derive(Default)]
struct Entry {
    ctxt: Option<String>,
    id: Option<String>,
    id_plural: Option<String>,
    strs: BTreeMap<usize, String>,
    fuzzy: bool,
}

enum Field {
    None,
    Ctxt,
    Id,
    IdPlural,
    Str(usize),
}

fn unquote(s: &str) -> Result<String> {
    let s = s.trim();
    let inner = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .ok_or_else(|| format!("malformed string: {s}"))?;
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('a') => out.push('\u{7}'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('v') => out.push('\u{b}'),
            Some(other) => return Err(format!("unsupported escape \\{other}")),
            None => return Err("dangling backslash".into()),
        }
    }
    Ok(out)
}

/// Parses a `.po` and returns the (key, value) pairs to write into the `.mo`.
pub fn parse_po(text: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut cur = Entry::default();
    let mut field = Field::None;
    let mut started = false;
    let flush = |entries: &mut Vec<Entry>, cur: &mut Entry, started: &mut bool| {
        if *started {
            entries.push(std::mem::take(cur));
        }
        *started = false;
    };
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let at = |e: String| format!("line {}: {e}", n + 1);
        if line.is_empty() {
            flush(&mut entries, &mut cur, &mut started);
            field = Field::None;
        } else if line.starts_with("#~") {
            continue; // obsolete entry
        } else if let Some(flags) = line.strip_prefix("#,") {
            if started && matches!(field, Field::Str(_)) {
                flush(&mut entries, &mut cur, &mut started);
            }
            if flags.split(',').any(|f| f.trim() == "fuzzy") {
                cur.fuzzy = true;
            }
            started = true;
        } else if line.starts_with('#') {
            if started && matches!(field, Field::Str(_)) {
                flush(&mut entries, &mut cur, &mut started);
            }
            started = true;
        } else if let Some(rest) = line.strip_prefix("msgctxt") {
            if started && matches!(field, Field::Str(_)) {
                flush(&mut entries, &mut cur, &mut started);
            }
            started = true;
            cur.ctxt = Some(unquote(rest).map_err(at)?);
            field = Field::Ctxt;
        } else if let Some(rest) = line.strip_prefix("msgid_plural") {
            cur.id_plural = Some(unquote(rest).map_err(at)?);
            field = Field::IdPlural;
        } else if let Some(rest) = line.strip_prefix("msgid") {
            if started && matches!(field, Field::Str(_)) {
                flush(&mut entries, &mut cur, &mut started);
            }
            started = true;
            cur.id = Some(unquote(rest).map_err(at)?);
            field = Field::Id;
        } else if let Some(rest) = line.strip_prefix("msgstr") {
            let (idx, rest) = match rest.strip_prefix('[') {
                Some(r) => {
                    let close = r.find(']').ok_or_else(|| at("missing ]".into()))?;
                    let i: usize = r[..close].parse().ctx(&at("plural index".into()))?;
                    (i, &r[close + 1..])
                }
                None => (0, rest),
            };
            cur.strs.insert(idx, unquote(rest).map_err(at)?);
            field = Field::Str(idx);
        } else if line.starts_with('"') {
            let piece = unquote(line).map_err(at)?;
            match field {
                Field::Ctxt => cur.ctxt.get_or_insert_with(String::new).push_str(&piece),
                Field::Id => cur.id.get_or_insert_with(String::new).push_str(&piece),
                Field::IdPlural => cur
                    .id_plural
                    .get_or_insert_with(String::new)
                    .push_str(&piece),
                Field::Str(i) => cur.strs.entry(i).or_default().push_str(&piece),
                Field::None => return Err(at("string outside an entry".into())),
            }
        } else {
            return Err(at(format!("unrecognised line: {line}")));
        }
    }
    flush(&mut entries, &mut cur, &mut started);

    let mut out: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for e in entries {
        let Some(id) = e.id else { continue };
        let is_header = id.is_empty() && e.ctxt.is_none();
        if (e.fuzzy && !is_header) || e.strs.values().all(String::is_empty) {
            continue;
        }
        let mut key = String::new();
        if let Some(c) = &e.ctxt {
            key.push_str(c);
            key.push('\u{4}');
        }
        key.push_str(&id);
        if let Some(p) = &e.id_plural {
            key.push('\0');
            key.push_str(p);
        }
        let mut value = e.strs.values().cloned().collect::<Vec<_>>().join("\0");
        if is_header {
            // like msgfmt: the creation date of the template is not copied into the catalogue
            value = value
                .split_inclusive('\n')
                .filter(|l| !l.starts_with("POT-Creation-Date:"))
                .collect();
        }
        out.insert(key.into_bytes(), value.into_bytes());
    }
    Ok(out.into_iter().collect())
}

/// Serialises into `.mo` format (little-endian, without a hash table: allowed by the format).
pub fn write_mo(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let n = entries.len() as u32;
    let orig_table = 28u32;
    let trans_table = orig_table + 8 * n;
    let strings_at = trans_table + 8 * n;
    let mut orig_desc = Vec::new();
    let mut trans_desc = Vec::new();
    let mut blob = Vec::new();
    for (k, _) in entries {
        orig_desc.push((k.len() as u32, strings_at + blob.len() as u32));
        blob.extend_from_slice(k);
        blob.push(0);
    }
    for (_, v) in entries {
        trans_desc.push((v.len() as u32, strings_at + blob.len() as u32));
        blob.extend_from_slice(v);
        blob.push(0);
    }
    let mut out = Vec::new();
    for w in [
        0x9504_12deu32,
        0,
        n,
        orig_table,
        trans_table,
        0,
        trans_table + 8 * n,
    ] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    for (len, off) in orig_desc.iter().chain(trans_desc.iter()) {
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
    }
    out.extend_from_slice(&blob);
    out
}

/// `.po` -> `.mo`.
pub fn compile(po_text: &str) -> Result<Vec<u8>> {
    Ok(write_mo(&parse_po(po_text)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PO: &str = r#"
msgid ""
msgstr ""
"Content-Type: text/plain; charset=UTF-8\n"
"Plural-Forms: nplurals=2; plural=(n > 1);\n"

#: a.rs
msgid "Hello"
msgstr "Bonjour"

#, fuzzy
msgid "Fuzzy"
msgstr "Approx"

msgid "Untranslated"
msgstr ""

msgctxt "menu"
msgid "Open"
msgstr "Ouvrir"

msgid "{} word"
msgid_plural "{} words"
msgstr[0] "{} mot"
msgstr[1] "{} mots"

msgid "Two\n"
"lines \"quoted\""
msgstr "Deux\n"
"lignes"

#~ msgid "Gone"
#~ msgstr "Parti"
"#;

    #[test]
    fn fuzzy_untranslated_and_obsolete_entries_are_dropped() {
        let e = parse_po(PO).unwrap();
        let keys: Vec<String> = e
            .iter()
            .map(|(k, _)| {
                String::from_utf8_lossy(k)
                    .replace('\0', "|")
                    .replace('\u{4}', "^")
            })
            .collect();
        assert!(keys.contains(&"Hello".to_string()));
        assert!(keys.contains(&"menu^Open".to_string()));
        assert!(keys.contains(&"{} word|{} words".to_string()));
        assert!(keys.contains(&"Two\nlines \"quoted\"".to_string()));
        assert!(keys.contains(&String::new()), "header kept");
        assert_eq!(keys.len(), 5, "{keys:?}");
    }

    #[test]
    fn the_header_drops_the_template_creation_date_like_msgfmt() {
        let po = "msgid \"\"\nmsgstr \"\"\n\"POT-Creation-Date: 2026\\n\"\n\"Language: fr\\n\"\n";
        let e = parse_po(po).unwrap();
        assert_eq!(e[0].1, b"Language: fr\n");
    }

    #[test]
    fn the_mo_layout_is_valid() {
        let mo = compile(PO).unwrap();
        let word = |i: usize| u32::from_le_bytes(mo[i..i + 4].try_into().unwrap());
        assert_eq!(word(0), 0x9504_12de);
        let n = word(8) as usize;
        assert_eq!(n, 5);
        let (ot, tt) = (word(12) as usize, word(16) as usize);
        // the msgids are sorted by bytes, and each string ends with a NUL
        let mut prev: Vec<u8> = Vec::new();
        for i in 0..n {
            let (len, off) = (word(ot + 8 * i) as usize, word(ot + 8 * i + 4) as usize);
            assert_eq!(mo[off + len], 0);
            let id = mo[off..off + len].to_vec();
            assert!(id >= prev);
            prev = id;
            let (tlen, toff) = (word(tt + 8 * i) as usize, word(tt + 8 * i + 4) as usize);
            assert_eq!(mo[toff + tlen], 0);
        }
    }
}
