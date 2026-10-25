//! X11 clipboard through arboard (text only). arboard serves the selection from an internal
//! thread as long as the instance lives.

use std::sync::Mutex;

use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};

use super::{ClipError, ClipItem, ClipboardBackend, Selection, SelectionData};

const TEXT_MIME: &str = "text/plain;charset=utf-8";

pub struct X11Clipboard {
    inner: Mutex<Option<arboard::Clipboard>>,
}

impl Default for X11Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

fn kind(sel: Selection) -> LinuxClipboardKind {
    match sel {
        Selection::Clipboard => LinuxClipboardKind::Clipboard,
        Selection::Primary => LinuxClipboardKind::Primary,
    }
}

impl X11Clipboard {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
    ) -> Result<T, ClipError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| ClipError::Failed("poisoned lock".into()))?;
        if guard.is_none() {
            *guard = Some(
                arboard::Clipboard::new()
                    .map_err(|e| ClipError::Unavailable(format!("X11 : {e}")))?,
            );
        }
        let Some(cb) = guard.as_mut() else {
            return Err(ClipError::Unavailable("X11".into()));
        };
        f(cb).map_err(|e| match e {
            arboard::Error::ClipboardNotSupported => ClipError::Unavailable(e.to_string()),
            other => ClipError::Failed(other.to_string()),
        })
    }
}

impl ClipboardBackend for X11Clipboard {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn supports_primary(&self) -> bool {
        true
    }

    fn get_text(&self, selection: Selection) -> Result<Option<String>, ClipError> {
        self.with(|cb| match cb.get().clipboard(kind(selection)).text() {
            Ok(t) => Ok(Some(t)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(e),
        })
    }

    fn set_text(&self, text: &str, selections: &[Selection]) -> Result<(), ClipError> {
        for sel in selections {
            self.with(|cb| cb.set().clipboard(kind(*sel)).text(text.to_string()))?;
        }
        Ok(())
    }

    /// Text only: non-text content (image…) is not backed up, hence not restored
    /// (it is never replaced by an empty clipboard).
    fn snapshot_selection(&self, selection: Selection) -> Result<SelectionData, ClipError> {
        match self.get_text(selection)? {
            Some(text) => Ok(SelectionData {
                items: vec![ClipItem {
                    mime: TEXT_MIME.to_string(),
                    data: text.into_bytes(),
                }],
                incomplete: true,
            }),
            None => Err(ClipError::Unavailable(
                "non-text content not backed up".into(),
            )),
        }
    }

    fn restore_selection(
        &self,
        selection: Selection,
        data: &SelectionData,
    ) -> Result<(), ClipError> {
        let text = data
            .items
            .iter()
            .find(|i| i.mime.starts_with("text/plain"))
            .map(|i| String::from_utf8_lossy(&i.data).into_owned());
        match text {
            Some(t) => self.set_text(&t, &[selection]),
            None => Err(ClipError::Unavailable("no text to restore".into())),
        }
    }
}
