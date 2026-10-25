//! Context shared by the hub pages and small run-time utilities.

use crate::toast::ToastHandle;
use gtk4 as gtk;
use linuxflow_core::traits::Controller;
use std::sync::Arc;

use gtk::{gio, glib};

/// What each page needs: the controller and the toasts.
#[derive(Clone)]
pub struct Ctx {
    pub controller: Arc<dyn Controller>,
    pub toast: ToastHandle,
}

impl Ctx {
    pub fn new(controller: Arc<dyn Controller>, toast: ToastHandle) -> Self {
        Self { controller, toast }
    }
}

/// Runs `work` in the GIO thread pool, then `done` on the GTK thread.
/// Used for `Controller` calls that can take time (microphone enumeration,
/// models, integrity check...). A job that panics is ignored (no `done`).
pub fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    glib::spawn_future_local(async move {
        if let Ok(value) = gio::spawn_blocking(work).await {
            done(value);
        }
    });
}
