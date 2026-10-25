//! Clipboard portal (tied to the RemoteDesktop session): fallback for GNOME, where the absence of
//! data-control prevents an application without focus from setting the selection.

use std::sync::Arc;

use crate::inject::backend::InjectError;
use crate::inject::portal::{PortalSession, TEXT_MIMES};

use super::{ClipError, ClipboardBackend, Selection};

pub struct PortalClipboard {
    session: Arc<PortalSession>,
}

impl PortalClipboard {
    /// `session` must have been created with `PortalConfig { clipboard: true, .. }`.
    pub fn new(session: Arc<PortalSession>) -> Self {
        Self { session }
    }
}

fn map(e: InjectError) -> ClipError {
    match e {
        InjectError::Failed(m) | InjectError::TimedOut(m) => ClipError::Failed(m),
        InjectError::Unavailable(m) | InjectError::Blocked(m) => ClipError::Unavailable(m),
    }
}

impl ClipboardBackend for PortalClipboard {
    fn name(&self) -> &'static str {
        "portal-clipboard"
    }

    fn get_text(&self, selection: Selection) -> Result<Option<String>, ClipError> {
        if selection != Selection::Clipboard {
            return Err(ClipError::Unavailable(
                "PRIMARY is not handled by the portal".into(),
            ));
        }
        match self.session.clipboard_read(TEXT_MIMES[0]) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) => Err(map(e)),
        }
    }

    fn set_text(&self, text: &str, _selections: &[Selection]) -> Result<(), ClipError> {
        let items = TEXT_MIMES
            .iter()
            .map(|m| (m.to_string(), text.as_bytes().to_vec()))
            .collect();
        self.session.clipboard_set(items).map_err(map)
    }
}
