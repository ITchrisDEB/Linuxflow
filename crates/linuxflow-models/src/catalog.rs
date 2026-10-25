//! Model catalogue: embedded `data/models.json`, overridable by `LINUXFLOW_MODEL_CATALOG`.
//!
//! No URL or checksum is written in the code: everything comes from the catalogue.
//! An invalid entry is ignored with a warning; a missing checksum makes the entry
//! "unverified" (the download remains possible, with an explicit warning in the interface).

use std::path::Path;

use serde::Deserialize;

/// Catalogue embedded at build time.
const EMBEDDED: &str = include_str!("../data/models.json");

/// Environment variable: path of a catalogue that replaces the embedded one.
pub const ENV_CATALOG: &str = "LINUXFLOW_MODEL_CATALOG";

/// Role of a file in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    /// Transcription model.
    Stt,
    /// Voice activity detection (companion file).
    Vad,
}

/// Supported languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Languages {
    #[default]
    Multilingual,
    EnglishOnly,
}

/// Recommended use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Usage {
    /// Requires a usable GPU.
    Gpu,
    /// Runs properly on the CPU.
    Cpu,
    /// Light (CPU or GPU).
    #[default]
    Light,
}

/// Validated catalogue entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    pub name: String,
    pub family: String,
    pub kind: ModelKind,
    /// Name of the file in the models folder (without a path separator).
    pub file: String,
    pub download_bytes: u64,
    pub disk_bytes: u64,
    pub ram_bytes: u64,
    pub vram_bytes: u64,
    pub languages: Languages,
    pub quantization: String,
    /// Mirrors: the first one is the official one.
    pub urls: Vec<String>,
    /// Lower-case hexadecimal SHA-256; `None` = unverified.
    pub sha256: Option<String>,
    pub license: String,
    pub usage: Usage,
    /// Accuracy rank (higher = more accurate) for the transcription models.
    pub rank: u32,
    /// File required for operation (VAD).
    pub required: bool,
}

impl ModelEntry {
    pub fn is_verified(&self) -> bool {
        self.sha256.is_some()
    }

    /// Estimated memory shown: VRAM for GPU models, RAM otherwise.
    pub fn memory_bytes(&self) -> u64 {
        if self.usage == Usage::Gpu {
            self.vram_bytes
        } else {
            self.ram_bytes
        }
    }
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    models: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawEntry {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    family: String,
    kind: ModelKind,
    file: String,
    download_bytes: u64,
    #[serde(default)]
    disk_bytes: u64,
    #[serde(default)]
    ram_bytes: u64,
    #[serde(default)]
    vram_bytes: u64,
    #[serde(default)]
    languages: Languages,
    #[serde(default)]
    quantization: String,
    urls: Vec<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    license: String,
    #[serde(default)]
    usage: Usage,
    #[serde(default)]
    rank: u32,
    #[serde(default)]
    required: bool,
}

/// Model catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    endpoint: String,
    entries: Vec<ModelEntry>,
    warnings: Vec<String>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
}

fn valid_file(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 128
        && f != "."
        && f != ".."
        && !f.contains(['/', '\\', '\0'])
        && !f.starts_with('.')
        && ![".part", ".lock", ".import", ".etag"]
            .iter()
            .any(|s| f.ends_with(s))
}

/// `http://` URL to the local machine (127.0.0.1, localhost, ::1), used by the tests.
fn is_loopback(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(h) = authority.strip_prefix('[') {
        h.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate(raw: RawEntry) -> Result<ModelEntry, String> {
    if !valid_id(&raw.id) {
        return Err(format!("invalid id {:?}", raw.id));
    }
    let id = raw.id.clone();
    if !valid_file(&raw.file) {
        return Err(format!("{id}: invalid file name {:?}", raw.file));
    }
    if raw.download_bytes == 0 {
        return Err(format!("{id}: download_bytes must be > 0"));
    }
    if raw.urls.is_empty() {
        return Err(format!("{id}: no URL"));
    }
    if let Some(u) = raw
        .urls
        .iter()
        .find(|u| !(u.starts_with("https://") || u.starts_with("http://")))
    {
        return Err(format!("{id}: unsupported URL scheme in {u:?}"));
    }
    let sha256 = match raw.sha256.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => {
            let s = s.to_ascii_lowercase();
            if !valid_sha256(&s) {
                return Err(format!("{id}: malformed sha256"));
            }
            Some(s)
        }
    };
    if sha256.is_none() {
        if let Some(u) = raw
            .urls
            .iter()
            .find(|u| u.starts_with("http://") && !is_loopback(u))
        {
            return Err(format!("{id}: plain http URL {u:?} refused without sha256"));
        }
    }
    Ok(ModelEntry {
        name: if raw.name.is_empty() {
            raw.id.clone()
        } else {
            raw.name
        },
        family: raw.family,
        kind: raw.kind,
        file: raw.file,
        download_bytes: raw.download_bytes,
        disk_bytes: if raw.disk_bytes == 0 {
            raw.download_bytes
        } else {
            raw.disk_bytes
        },
        ram_bytes: raw.ram_bytes,
        vram_bytes: raw.vram_bytes,
        languages: raw.languages,
        quantization: raw.quantization,
        urls: raw.urls,
        sha256,
        license: raw.license,
        usage: raw.usage,
        rank: raw.rank,
        required: raw.required,
        id: raw.id,
    })
}

impl Catalog {
    /// Embedded catalogue (the tests guarantee that it is valid).
    pub fn embedded() -> Catalog {
        Catalog::from_json(EMBEDDED).unwrap_or_else(|_| Catalog {
            endpoint: String::new(),
            entries: Vec::new(),
            warnings: vec!["embedded catalog is unreadable".into()],
        })
    }

    /// Parses a JSON catalogue. An invalid or duplicate entry is ignored (warning).
    pub fn from_json(text: &str) -> Result<Catalog, serde_json::Error> {
        let raw: RawFile = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
        let mut entries: Vec<ModelEntry> = Vec::new();
        let mut warnings = Vec::new();
        for (i, value) in raw.models.into_iter().enumerate() {
            let parsed = serde_json::from_value::<RawEntry>(value)
                .map_err(|e| e.to_string())
                .and_then(validate);
            match parsed {
                Ok(e) if entries.iter().any(|x| x.id == e.id) => {
                    warnings.push(format!("entry #{i}: duplicate id {:?}, ignored", e.id));
                }
                Ok(e) if entries.iter().any(|x| x.file == e.file) => {
                    warnings.push(format!(
                        "entry #{i} ignored: {}: file {:?} already used",
                        e.id, e.file
                    ));
                }
                Ok(e) => {
                    if e.sha256.is_none() {
                        warnings.push(format!("{}: no sha256, model is unverified", e.id));
                    }
                    entries.push(e);
                }
                Err(msg) => warnings.push(format!("entry #{i} ignored: {msg}")),
            }
        }
        Ok(Catalog {
            endpoint: raw.endpoint.trim_end_matches('/').to_string(),
            entries,
            warnings,
        })
    }

    /// Loads the catalogue: the file designated by `override_path` if it exists and is readable,
    /// otherwise the embedded catalogue (with a warning).
    pub fn load(override_path: Option<&Path>) -> Catalog {
        if let Some(p) = override_path {
            match std::fs::read_to_string(p) {
                Ok(text) => match Catalog::from_json(&text) {
                    Ok(c) => return c,
                    Err(e) => {
                        return Catalog::embedded_with_warning(format!(
                            "catalog override unreadable ({e})"
                        ))
                    }
                },
                Err(e) => {
                    return Catalog::embedded_with_warning(format!(
                        "catalog override not read ({e})"
                    ))
                }
            }
        }
        Catalog::embedded()
    }

    /// `load` with the path taken from `LINUXFLOW_MODEL_CATALOG`.
    pub fn load_from_env() -> Catalog {
        let path = std::env::var_os(ENV_CATALOG).filter(|v| !v.is_empty());
        Catalog::load(path.as_deref().map(Path::new))
    }

    fn embedded_with_warning(msg: String) -> Catalog {
        let mut c = Catalog::embedded();
        c.warnings
            .insert(0, format!("{msg}; using embedded catalog"));
        c
    }

    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Validation warnings (to log).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Origin of the official catalogue URLs (replaceable through the configuration).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Transcription entries, from the least to the most accurate.
    pub fn stt_by_rank(&self) -> Vec<&ModelEntry> {
        let mut v: Vec<&ModelEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind == ModelKind::Stt)
            .collect();
        v.sort_by_key(|e| e.rank);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_is_valid_and_complete() {
        let c = Catalog::embedded();
        assert!(c.warnings().is_empty(), "{:?}", c.warnings());
        let ids: Vec<&str> = c.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["small", "large-v3-q5_0", "large-v3", "silero-vad"]);
        assert!(c.entries().iter().all(ModelEntry::is_verified));
        let vad = c.get("silero-vad").unwrap();
        assert_eq!(vad.kind, ModelKind::Vad);
        assert!(vad.required);
        assert_eq!(c.get("small").unwrap().download_bytes, 487_601_967);
        assert_eq!(c.endpoint(), "https://huggingface.co");
    }

    #[test]
    fn invalid_entries_are_ignored_with_warning() {
        let json = r#"{"endpoint":"https://x.test/","models":[
          {"id":"ok","kind":"stt","file":"a.bin","download_bytes":10,"urls":["https://x.test/a"],
           "sha256":"0000000000000000000000000000000000000000000000000000000000000000"},
          {"id":"../evil","kind":"stt","file":"b.bin","download_bytes":10,"urls":["https://x.test/b"]},
          {"id":"badfile","kind":"stt","file":"../b.bin","download_bytes":10,"urls":["https://x.test/b"]},
          {"id":"nourl","kind":"stt","file":"c.bin","download_bytes":10,"urls":[]},
          {"id":"ftp","kind":"stt","file":"d.bin","download_bytes":10,"urls":["ftp://x/d"]},
          {"id":"zero","kind":"stt","file":"e.bin","download_bytes":0,"urls":["https://x.test/e"]},
          {"id":"badsum","kind":"stt","file":"f.bin","download_bytes":1,"urls":["https://x.test/f"],"sha256":"xyz"},
          {"id":"ok","kind":"stt","file":"g.bin","download_bytes":1,"urls":["https://x.test/g"]},
          {"nonsense":true}
        ]}"#;
        let c = Catalog::from_json(json).unwrap();
        assert_eq!(c.entries().len(), 1);
        assert_eq!(c.warnings().len(), 8, "{:?}", c.warnings());
        assert_eq!(c.endpoint(), "https://x.test");
    }

    #[test]
    fn missing_sum_means_unverified() {
        let json = r#"{"models":[{"id":"m","kind":"stt","file":"m.bin","download_bytes":5,
          "urls":["https://x.test/m"]}]}"#;
        let c = Catalog::from_json(json).unwrap();
        let m = c.get("m").unwrap();
        assert!(!m.is_verified());
        assert_eq!(m.disk_bytes, 5);
        assert!(c.warnings().iter().any(|w| w.contains("unverified")));
    }

    #[test]
    fn sha256_is_normalized_to_lowercase() {
        let upper = "A".repeat(64);
        let json = format!(
            r#"{{"models":[{{"id":"m","kind":"vad","file":"m.bin","download_bytes":5,"urls":["http://h/m"],"sha256":"{upper}"}}]}}"#
        );
        let c = Catalog::from_json(&json).unwrap();
        assert_eq!(
            c.get("m").unwrap().sha256.as_deref(),
            Some("a".repeat(64).as_str())
        );
    }

    #[test]
    fn user_override_replaces_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cat.json");
        std::fs::write(
            &p,
            r#"{"models":[{"id":"mine","kind":"stt","file":"m.bin","download_bytes":3,"urls":["https://h/m"]}]}"#,
        )
        .unwrap();
        let c = Catalog::load(Some(&p));
        assert_eq!(c.entries().len(), 1);
        assert_eq!(c.entries()[0].id, "mine");
    }

    #[test]
    fn broken_override_falls_back_to_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cat.json");
        std::fs::write(&p, "{ not json").unwrap();
        let c = Catalog::load(Some(&p));
        assert_eq!(c.entries().len(), 4);
        assert!(c.warnings()[0].contains("embedded"));
        let c2 = Catalog::load(Some(&dir.path().join("absent.json")));
        assert_eq!(c2.entries().len(), 4);
        assert!(!c2.warnings().is_empty());
    }

    #[test]
    fn duplicate_files_and_reserved_names_are_rejected() {
        let json = r#"{"models":[
          {"id":"a","kind":"stt","file":"x.bin","download_bytes":1,"urls":["https://h/a"]},
          {"id":"b","kind":"stt","file":"x.bin","download_bytes":1,"urls":["https://h/b"]},
          {"id":"c","kind":"stt","file":"y.bin.import","download_bytes":1,"urls":["https://h/c"]},
          {"id":"d","kind":"stt","file":"y.bin.part.etag","download_bytes":1,"urls":["https://h/d"]},
          {"id":"e","kind":"stt","file":"y.etag","download_bytes":1,"urls":["https://h/e"]}
        ]}"#;
        let c = Catalog::from_json(json).unwrap();
        let ids: Vec<&str> = c.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a"]);
        assert_eq!(
            c.warnings()
                .iter()
                .filter(|w| w.contains("ignored"))
                .count(),
            4,
            "{:?}",
            c.warnings()
        );
    }

    #[test]
    fn plain_http_requires_a_sum_except_loopback() {
        let sum = "a".repeat(64);
        let json = format!(
            r#"{{"models":[
          {{"id":"a","kind":"stt","file":"a.bin","download_bytes":1,"urls":["http://example.org/a"]}},
          {{"id":"b","kind":"stt","file":"b.bin","download_bytes":1,"urls":["https://ok.test/a","http://example.org/b"]}},
          {{"id":"c","kind":"stt","file":"c.bin","download_bytes":1,"urls":["http://example.org/c"],"sha256":"{sum}"}},
          {{"id":"d","kind":"stt","file":"d.bin","download_bytes":1,"urls":["http://127.0.0.1:8080/d"]}},
          {{"id":"e","kind":"stt","file":"e.bin","download_bytes":1,"urls":["http://localhost/e"]}},
          {{"id":"f","kind":"stt","file":"f.bin","download_bytes":1,"urls":["http://127.0.0.1.evil.test/f"]}}
        ]}}"#
        );
        let c = Catalog::from_json(&json).unwrap();
        let ids: Vec<&str> = c.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["c", "d", "e"], "{:?}", c.warnings());
    }

    #[test]
    fn tolerates_bom() {
        let c = Catalog::from_json("\u{feff}{\"models\":[]}").unwrap();
        assert!(c.entries().is_empty());
    }
}
