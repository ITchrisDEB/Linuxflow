//! Brief notifications, through `adw::ToastOverlay`.

use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

/// Display duration of a toast, in seconds (GTK only accepts whole seconds).
pub const TOAST_TIMEOUT_SECS: u32 = 2;

/// Cloneable handle to the hub's `ToastOverlay`.
#[derive(Clone)]
pub struct ToastHandle {
    overlay: adw::ToastOverlay,
    current: Rc<RefCell<Option<adw::Toast>>>,
    last: Rc<RefCell<Option<String>>>,
}

impl Default for ToastHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl ToastHandle {
    pub fn new() -> Self {
        Self {
            overlay: adw::ToastOverlay::new(),
            current: Rc::default(),
            last: Rc::default(),
        }
    }

    /// The root widget to put in the hierarchy (it wraps the content).
    pub fn overlay(&self) -> &adw::ToastOverlay {
        &self.overlay
    }

    /// Shows a message, replacing the previous one that is still visible.
    pub fn show(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if let Some(previous) = self.current.borrow_mut().take() {
            previous.dismiss();
        }
        let toast = adw::Toast::new(text);
        // The messages come from the core (errors, names): never interpreted as markup.
        toast.set_use_markup(false);
        toast.set_timeout(TOAST_TIMEOUT_SECS);
        self.overlay.add_toast(toast.clone());
        *self.current.borrow_mut() = Some(toast);
        *self.last.borrow_mut() = Some(text.to_string());
    }

    /// Last message shown (useful for the tests).
    pub fn last_message(&self) -> Option<String> {
        self.last.borrow().clone()
    }
}
