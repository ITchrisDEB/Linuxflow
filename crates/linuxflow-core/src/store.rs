//! Generic, thread-safe JSON file with atomic writes.
//!
//! Any read or parse error gives the default values back (the warning goes to
//! the supplied callback, never to standard output). The file is only rewritten by `set`/`update`.

use crate::error::Result;
use serde::{de::DeserializeOwned, Serialize};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Permissions of the created file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    /// 0600: history, logs, settings.
    Private,
    /// 0644: non-sensitive data.
    Shared,
}

impl FileMode {
    pub fn bits(self) -> u32 {
        match self {
            FileMode::Private => 0o600,
            FileMode::Shared => 0o644,
        }
    }
}

/// Receiving the read warnings (to be wired to the log).
pub type WarnFn = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Store<T> {
    path: PathBuf,
    mode: FileMode,
    data: Mutex<T>,
    warn: WarnFn,
}

impl<T> Store<T>
where
    T: Serialize + DeserializeOwned + Default + Clone,
{
    /// Opens the file; absent or invalid -> default values (writing nothing), without a log.
    pub fn open(path: impl Into<PathBuf>, mode: FileMode) -> Self {
        Self::open_with(path, mode, Arc::new(|_| {}))
    }

    pub fn open_with(path: impl Into<PathBuf>, mode: FileMode, warn: WarnFn) -> Self {
        let path = path.into();
        let data = load::<T>(&path, &*warn);
        Self {
            path,
            mode,
            data: Mutex::new(data),
            warn,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    fn lock(&self) -> MutexGuard<'_, T> {
        // A panic in another thread must not make the data inaccessible.
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self) -> T {
        self.lock().clone()
    }

    /// Replaces the value and writes it. If the write fails, the in-memory value is unchanged.
    pub fn set(&self, value: T) -> Result<()> {
        let mut guard = self.lock();
        self.save(&value)?;
        *guard = value;
        Ok(())
    }

    /// Modifies a copy, writes it, then swaps it in: no partial effect if the write fails.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R> {
        self.try_update(|v| Ok(f(v)))
    }

    /// Like `update`, but `f` may refuse the modification (nothing is written or changed).
    pub fn try_update<R>(&self, f: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        let mut guard = self.lock();
        let mut next = guard.clone();
        let out = f(&mut next)?;
        self.save(&next)?;
        *guard = next;
        Ok(out)
    }

    /// Reads the file again (same rules as at opening).
    pub fn reload(&self) {
        let fresh = load::<T>(&self.path, &*self.warn);
        *self.lock() = fresh;
    }

    fn save(&self, value: &T) -> Result<()> {
        // 2-space indentation, no trailing newline: identical to JSON.stringify(data, null, 2).
        let text = serde_json::to_string_pretty(value)?;
        write_atomic(&self.path, text.as_bytes(), self.mode)?;
        Ok(())
    }
}

/// Reads and parses the file; any error -> `T::default()`.
fn load<T: DeserializeOwned + Default>(path: &Path, warn: &dyn Fn(&str)) -> T {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            warn(&format!(
                "cannot read {} ({e}): using defaults",
                path.display()
            ));
            return T::default();
        }
    };
    let reason = match std::str::from_utf8(&bytes) {
        Err(_) => "not valid UTF-8".to_owned(),
        Ok(text) => {
            let text = text.strip_prefix('\u{feff}').unwrap_or(text);
            match serde_json::from_str::<T>(text) {
                Ok(v) => return v,
                Err(e) => e.to_string(),
            }
        }
    };
    // The original file is backed up before any rewrite with the defaults.
    let saved = match backup_corrupt(path) {
        Ok(Some(b)) => format!("copy kept in {}", b.display()),
        Ok(None) => "file was empty".to_owned(),
        Err(e) => format!("backup failed: {e}"),
    };
    warn(&format!(
        "{} is invalid ({reason}): using defaults; {saved}",
        path.display()
    ));
    T::default()
}

/// Copies an unreadable file to `<name>.corrupt`, then `<name>.corrupt.1`, `.2`... (0600). An
/// older backup is never overwritten; content that was already backed up is not copied again.
/// `Ok(None)`: empty file, nothing to back up.
pub fn backup_corrupt(path: &Path) -> io::Result<Option<PathBuf>> {
    let content = fs::read(path)?;
    if content.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for i in 0..1000u32 {
        let name = if i == 0 {
            format!("{base}.corrupt")
        } else {
            format!("{base}.corrupt.{i}")
        };
        let candidate = path.with_file_name(name);
        match fs::read(&candidate) {
            Ok(old) if old == content => return Ok(Some(candidate)),
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                write_atomic(&candidate, &content, FileMode::Private)?;
                return Ok(Some(candidate));
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many backups"))
}

/// Reads a UTF-8 text file without BOM; `None` if absent or unreadable.
pub fn read_text_lenient(path: &Path) -> Option<String> {
    let text = String::from_utf8(fs::read(path).ok()?).ok()?;
    Some(
        text.strip_prefix('\u{feff}')
            .map(str::to_owned)
            .unwrap_or(text),
    )
}

/// Creates a folder (and its parents) with 0700.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Atomic write: neighbouring temporary file, `sync_all`, rename.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: FileMode) -> io::Result<()> {
    write_atomic_hooked(path, bytes, mode, || Ok(()))
}

/// `before_rename` runs between writing the `.tmp` and the rename (fault injection in tests).
pub(crate) fn write_atomic_hooked(
    path: &Path,
    bytes: &[u8],
    mode: FileMode,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    let mut tmp_name: OsString = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode.bits())
            .open(&tmp)?;
        // `mode` only applies at creation: it is also forced for a pre-existing `.tmp`.
        file.set_permissions(fs::Permissions::from_mode(mode.bits()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        before_rename()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        return result;
    }
    // Durability of the rename (best effort).
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::error::Error;
    use serde::Deserialize;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Conf {
        zeta: u32,
        alpha: String,
        added: bool,
    }
    impl Default for Conf {
        fn default() -> Self {
            Conf {
                zeta: 1,
                alpha: "a".into(),
                added: true,
            }
        }
    }

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_file_gives_defaults_and_writes_nothing() {
        let d = dir();
        let p = d.path().join("c.json");
        let s = Store::<Conf>::open(&p, FileMode::Private);
        assert_eq!(s.get(), Conf::default());
        assert!(!p.exists());
    }

    #[test]
    fn corrupt_json_gives_defaults_and_warns() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(&p, "{ not json").unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let s = Store::<Conf>::open_with(
            &p,
            FileMode::Private,
            Arc::new(move |m| {
                assert!(m.contains("c.json"));
                n2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(s.get(), Conf::default());
        assert_eq!(n.load(Ordering::SeqCst), 1);
        // The corrupt file is not touched as long as nothing is written.
        assert_eq!(fs::read_to_string(&p).unwrap(), "{ not json");
    }

    #[test]
    fn invalid_utf8_gives_defaults() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(
            Store::<Conf>::open(&p, FileMode::Private).get(),
            Conf::default()
        );
    }

    #[test]
    fn old_file_gets_new_keys_from_defaults() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(&p, r#"{"zeta": 9}"#).unwrap();
        let s = Store::<Conf>::open(&p, FileMode::Private);
        assert_eq!(
            s.get(),
            Conf {
                zeta: 9,
                alpha: "a".into(),
                added: true
            }
        );
    }

    #[test]
    fn bom_is_tolerated() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(&p, "\u{feff}{\"zeta\": 5}").unwrap();
        assert_eq!(Store::<Conf>::open(&p, FileMode::Private).get().zeta, 5);
    }

    #[test]
    fn array_or_scalar_gives_defaults() {
        let d = dir();
        for (i, content) in ["[1,2,3]", "42", "\"texte\"", "null", ""]
            .iter()
            .enumerate()
        {
            let p = d.path().join(format!("c{i}.json"));
            fs::write(&p, content).unwrap();
            assert_eq!(
                Store::<Conf>::open(&p, FileMode::Private).get(),
                Conf::default(),
                "{content}"
            );
        }
    }

    #[test]
    fn vec_store_rejects_object() {
        let d = dir();
        let p = d.path().join("h.json");
        fs::write(&p, r#"{"a":1}"#).unwrap();
        assert!(Store::<Vec<u32>>::open(&p, FileMode::Private)
            .get()
            .is_empty());
    }

    #[test]
    fn set_writes_two_space_indent_no_trailing_newline_and_key_order() {
        let d = dir();
        let p = d.path().join("sub/dir/c.json");
        let s = Store::<Conf>::open(&p, FileMode::Shared);
        s.set(Conf {
            zeta: 2,
            alpha: "b".into(),
            added: false,
        })
        .unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert_eq!(
            text,
            "{\n  \"zeta\": 2,\n  \"alpha\": \"b\",\n  \"added\": false\n}"
        );
        assert!(!p.with_extension("json.tmp").exists());
        // Rechargement identique.
        assert_eq!(Store::<Conf>::open(&p, FileMode::Shared).get().alpha, "b");
    }

    #[test]
    fn json_value_store_preserves_key_order() {
        let d = dir();
        let p = d.path().join("v.json");
        fs::write(&p, r#"{"z":1,"a":2,"m":{"y":1,"b":2}}"#).unwrap();
        let s = Store::<serde_json::Map<String, serde_json::Value>>::open(&p, FileMode::Private);
        s.update(|m| {
            m.insert("k".into(), 3.into());
        })
        .unwrap();
        let text = fs::read_to_string(&p).unwrap();
        let zi = text.find("\"z\"").unwrap();
        let ai = text.find("\"a\"").unwrap();
        let mi = text.find("\"m\"").unwrap();
        let ki = text.find("\"k\"").unwrap();
        assert!(zi < ai && ai < mi && mi < ki);
        assert!(text.find("\"y\"").unwrap() < text.find("\"b\"").unwrap());
    }

    #[test]
    fn update_and_try_update() {
        let d = dir();
        let p = d.path().join("c.json");
        let s = Store::<Conf>::open(&p, FileMode::Private);
        assert_eq!(
            s.update(|c| {
                c.zeta += 1;
                c.zeta
            })
            .unwrap(),
            2
        );
        let err = s.try_update(|c| {
            c.zeta = 99;
            Err::<(), _>(Error::msg("non"))
        });
        assert!(err.is_err());
        assert_eq!(s.get().zeta, 2);
        assert_eq!(Store::<Conf>::open(&p, FileMode::Private).get().zeta, 2);
    }

    #[test]
    fn write_failure_keeps_memory_unchanged() {
        let d = dir();
        let blocker = d.path().join("file");
        fs::write(&blocker, "x").unwrap();
        // The parent "folder" is a file: the write fails.
        let s = Store::<Conf>::open(blocker.join("c.json"), FileMode::Private);
        assert!(s
            .set(Conf {
                zeta: 7,
                ..Conf::default()
            })
            .is_err());
        assert_eq!(s.get().zeta, 1);
        assert!(s.update(|c| c.zeta = 8).is_err());
        assert_eq!(s.get().zeta, 1);
    }

    #[test]
    fn simulated_crash_between_tmp_and_rename_keeps_old_file() {
        let d = dir();
        let p = d.path().join("c.json");
        let s = Store::<Conf>::open(&p, FileMode::Private);
        s.set(Conf {
            zeta: 3,
            ..Conf::default()
        })
        .unwrap();
        let before = fs::read(&p).unwrap();
        let r = write_atomic_hooked(&p, b"{\"zeta\": 100}", FileMode::Private, || {
            Err(io::Error::other("crash"))
        });
        assert!(r.is_err());
        assert_eq!(fs::read(&p).unwrap(), before);
        assert_eq!(Store::<Conf>::open(&p, FileMode::Private).get().zeta, 3);
    }

    #[test]
    fn leftover_tmp_file_is_ignored_and_replaced() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(d.path().join("c.json.tmp"), "junk").unwrap();
        let s = Store::<Conf>::open(&p, FileMode::Private);
        assert_eq!(s.get(), Conf::default());
        s.set(Conf {
            zeta: 4,
            ..Conf::default()
        })
        .unwrap();
        assert_eq!(Store::<Conf>::open(&p, FileMode::Private).get().zeta, 4);
        assert!(!d.path().join("c.json.tmp").exists());
    }

    #[test]
    fn permissions_follow_mode() {
        let d = dir();
        for (mode, bits) in [(FileMode::Private, 0o600), (FileMode::Shared, 0o644)] {
            let p = d.path().join(format!("p{bits:o}.json"));
            let s = Store::<Conf>::open(&p, mode);
            s.set(Conf::default()).unwrap();
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, bits);
        }
        // An existing file that is too open is tightened at the next write.
        let p = d.path().join("open.json");
        fs::write(&p, "{}").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        let s = Store::<Conf>::open(&p, FileMode::Private);
        s.set(Conf::default()).unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn created_parent_dirs_are_private() {
        let d = dir();
        let p = d.path().join("a/b/c.json");
        Store::<Conf>::open(&p, FileMode::Private)
            .set(Conf::default())
            .unwrap();
        assert_eq!(
            fs::metadata(d.path().join("a/b"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn concurrent_updates_are_serialized() {
        let d = dir();
        let s = Arc::new(Store::<Conf>::open(
            d.path().join("c.json"),
            FileMode::Private,
        ));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        s.update(|c| c.zeta += 1).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(s.get().zeta, 81);
        assert_eq!(
            Store::<Conf>::open(s.path(), FileMode::Private).get().zeta,
            81
        );
    }

    #[test]
    fn reload_picks_up_external_change() {
        let d = dir();
        let p = d.path().join("c.json");
        let s = Store::<Conf>::open(&p, FileMode::Private);
        fs::write(&p, r#"{"zeta": 12}"#).unwrap();
        s.reload();
        assert_eq!(s.get().zeta, 12);
    }

    #[test]
    fn unreadable_file_is_backed_up_before_any_rewrite() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(&p, "{ real data ").unwrap();
        let s = Store::<Conf>::open(&p, FileMode::Private);
        let b = d.path().join("c.json.corrupt");
        assert_eq!(fs::read_to_string(&b).unwrap(), "{ real data ");
        assert_eq!(
            fs::metadata(&b).unwrap().permissions().mode() & 0o777,
            0o600
        );
        s.set(Conf::default()).unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "{ real data ");
        assert!(serde_json::from_str::<Conf>(&fs::read_to_string(&p).unwrap()).is_ok());
    }

    #[test]
    fn backups_are_numbered_never_overwritten_and_not_duplicated() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(&p, "{ un").unwrap();
        drop(Store::<Conf>::open(&p, FileMode::Private));
        drop(Store::<Conf>::open(&p, FileMode::Private)); // same content: no new copy
        assert!(!d.path().join("c.json.corrupt.1").exists());
        fs::write(&p, "{ deux").unwrap();
        drop(Store::<Conf>::open(&p, FileMode::Private));
        fs::write(&p, "{ trois").unwrap();
        drop(Store::<Conf>::open(&p, FileMode::Private));
        assert_eq!(
            fs::read_to_string(d.path().join("c.json.corrupt")).unwrap(),
            "{ un"
        );
        assert_eq!(
            fs::read_to_string(d.path().join("c.json.corrupt.1")).unwrap(),
            "{ deux"
        );
        assert_eq!(
            fs::read_to_string(d.path().join("c.json.corrupt.2")).unwrap(),
            "{ trois"
        );
    }

    #[test]
    fn invalid_utf8_and_wrong_shape_are_backed_up_but_empty_and_missing_are_not() {
        let d = dir();
        let p = d.path().join("c.json");
        fs::write(&p, [0xff, 0xfe, b'x']).unwrap();
        drop(Store::<Conf>::open(&p, FileMode::Private));
        assert!(d.path().join("c.json.corrupt").exists());
        let q = d.path().join("e.json");
        fs::write(&q, "  \n").unwrap();
        drop(Store::<Conf>::open(&q, FileMode::Private));
        drop(Store::<Conf>::open(
            d.path().join("absent.json"),
            FileMode::Private,
        ));
        assert!(!d.path().join("e.json.corrupt").exists());
        assert!(!d.path().join("absent.json.corrupt").exists());
        let r = d.path().join("r.json");
        fs::write(&r, "[1,2]").unwrap();
        drop(Store::<Conf>::open(&r, FileMode::Private));
        assert_eq!(
            fs::read_to_string(d.path().join("r.json.corrupt")).unwrap(),
            "[1,2]"
        );
    }
}
