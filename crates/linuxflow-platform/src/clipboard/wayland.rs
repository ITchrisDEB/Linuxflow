//! data-control Wayland (`ext_data_control` / `zwlr_data_control`) via wl-clipboard-rs : CLIPBOARD
//! and PRIMARY, without focus. The text set is served by a thread until another
//! application takes the selection.

use std::time::Duration;

use wl_clipboard_rs::copy::{
    self, ClipboardType as CopyType, MimeSource, MimeType as CopyMime, Options, Seat as CopySeat,
    Source,
};
use wl_clipboard_rs::paste::{
    self, ClipboardType as PasteType, MimeType as PasteMime, Seat as PasteSeat,
};

use super::{
    read_limited, ClipError, ClipItem, ClipboardBackend, Selection, SelectionData,
    MAX_SNAPSHOT_BYTES,
};

const READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct WaylandClipboard;

impl WaylandClipboard {
    pub fn new() -> Self {
        Self
    }
}

fn paste_type(sel: Selection) -> PasteType {
    match sel {
        Selection::Clipboard => PasteType::Regular,
        Selection::Primary => PasteType::Primary,
    }
}

/// Copy type for a set of selections (`None` if empty).
pub(crate) fn copy_type(selections: &[Selection]) -> Option<CopyType> {
    let clip = selections.contains(&Selection::Clipboard);
    let prim = selections.contains(&Selection::Primary);
    match (clip, prim) {
        (true, true) => Some(CopyType::Both),
        (true, false) => Some(CopyType::Regular),
        (false, true) => Some(CopyType::Primary),
        (false, false) => None,
    }
}

fn map_paste_err(e: paste::Error) -> ClipError {
    use paste::Error::*;
    match e {
        NoSeats
        | MissingProtocol { .. }
        | SocketOpenError(_)
        | WaylandConnection(_)
        | PrimarySelectionUnsupported => ClipError::Unavailable(e.to_string()),
        other => ClipError::Failed(other.to_string()),
    }
}

fn map_copy_err(e: copy::Error) -> ClipError {
    use copy::Error::*;
    match e {
        NoSeats
        | MissingProtocol { .. }
        | SocketOpenError(_)
        | WaylandConnection(_)
        | PrimarySelectionUnsupported => ClipError::Unavailable(e.to_string()),
        other => ClipError::Failed(other.to_string()),
    }
}

fn is_empty_err(e: &paste::Error) -> bool {
    matches!(e, paste::Error::ClipboardEmpty | paste::Error::NoMimeType)
}

/// Prepares then serves in the background (without `fork`, suited to a multi-threaded application).
fn serve(options: Options, sources: Vec<MimeSource>) -> Result<(), ClipError> {
    let prepared = options.prepare_copy_multi(sources).map_err(map_copy_err)?;
    std::thread::Builder::new()
        .name("linuxflow-clip-serve".into())
        .spawn(move || {
            if let Err(e) = prepared.serve() {
                tracing::debug!("presse-papiers Wayland : service interrompu ({e})");
            }
        })
        .map(|_| ())
        .map_err(|e| ClipError::Failed(format!("thread : {e}")))
}

impl ClipboardBackend for WaylandClipboard {
    fn name(&self) -> &'static str {
        "wayland-data-control"
    }

    fn supports_primary(&self) -> bool {
        true
    }

    fn get_text(&self, selection: Selection) -> Result<Option<String>, ClipError> {
        match paste::get_contents(
            paste_type(selection),
            PasteSeat::Unspecified,
            PasteMime::Text,
        ) {
            Ok((reader, _mime)) => {
                let (bytes, _) = read_limited(reader, MAX_SNAPSHOT_BYTES, READ_TIMEOUT)?;
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            }
            Err(e) if is_empty_err(&e) => Ok(None),
            Err(e) => Err(map_paste_err(e)),
        }
    }

    fn set_text(&self, text: &str, selections: &[Selection]) -> Result<(), ClipError> {
        let Some(kind) = copy_type(selections) else {
            return Ok(());
        };
        let make = |kind: CopyType| {
            let mut options = Options::new();
            options.clipboard(kind).seat(CopySeat::All).foreground(true);
            // Tells the history managers not to keep the transcription.
            options.sensitive(true);
            let source = MimeSource {
                source: Source::Bytes(text.as_bytes().to_vec().into_boxed_slice()),
                mime_type: CopyMime::Text,
            };
            serve(options, vec![source])
        };
        match make(kind) {
            // PRIMARY not handled by this compositor: fall back to CLIPBOARD only.
            Err(ClipError::Unavailable(_)) if kind == CopyType::Both => make(CopyType::Regular),
            other => other,
        }
    }

    fn snapshot_selection(&self, selection: Selection) -> Result<SelectionData, ClipError> {
        let ty = paste_type(selection);
        let mimes = match paste::get_mime_types_ordered(ty, PasteSeat::Unspecified) {
            Ok(m) => m,
            Err(e) if is_empty_err(&e) => return Ok(SelectionData::default()),
            Err(e) => return Err(map_paste_err(e)),
        };
        let mut data = SelectionData::default();
        let mut total = 0usize;
        for mime in mimes {
            let fetched =
                paste::get_contents(ty, PasteSeat::Unspecified, PasteMime::Specific(&mime));
            let Ok((reader, _)) = fetched else {
                data.incomplete = true;
                continue;
            };
            match read_limited(
                reader,
                MAX_SNAPSHOT_BYTES.saturating_sub(total),
                READ_TIMEOUT,
            ) {
                Ok((bytes, false)) => {
                    total += bytes.len();
                    data.items.push(ClipItem { mime, data: bytes });
                }
                _ => data.incomplete = true,
            }
        }
        Ok(data)
    }

    fn restore_selection(
        &self,
        selection: Selection,
        data: &SelectionData,
    ) -> Result<(), ClipError> {
        let kind = match selection {
            Selection::Clipboard => CopyType::Regular,
            Selection::Primary => CopyType::Primary,
        };
        if data.items.is_empty() {
            return copy::clear(kind, CopySeat::All).map_err(map_copy_err);
        }
        let mut options = Options::new();
        options.clipboard(kind).seat(CopySeat::All).foreground(true);
        options.omit_additional_text_mime_types(true);
        // The restored content may be a password copied from a manager: it is marked
        // "sensitive" as when writing, so that cliphist/Klipper do not record it.
        options.sensitive(true);
        let sources = data
            .items
            .iter()
            .map(|item| MimeSource {
                source: Source::Bytes(item.data.clone().into_boxed_slice()),
                mime_type: CopyMime::Specific(item.mime.clone()),
            })
            .collect();
        serve(options, sources)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn selection_sets_map_to_copy_types() {
        assert_eq!(copy_type(&[Selection::Clipboard]), Some(CopyType::Regular));
        assert_eq!(copy_type(&[Selection::Primary]), Some(CopyType::Primary));
        assert_eq!(
            copy_type(&[Selection::Clipboard, Selection::Primary]),
            Some(CopyType::Both)
        );
        assert_eq!(copy_type(&[]), None);
    }

    #[test]
    fn missing_protocol_is_unavailable_not_failure() {
        let e = copy::Error::MissingProtocol {
            name: "ext_data_control",
            version: 1,
        };
        assert!(matches!(map_copy_err(e), ClipError::Unavailable(_)));
        let e = paste::Error::NoSeats;
        assert!(matches!(map_paste_err(e), ClipError::Unavailable(_)));
        assert!(is_empty_err(&paste::Error::ClipboardEmpty));
    }
}
