//! Confirmation dialog (`adw::MessageDialog`).
//!
//! `adw::AlertDialog` only exists from libadwaita 1.5, above our baseline (1.4):
//! `MessageDialog` (1.2) is therefore used, deprecated in recent versions but present.

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use linuxflow_i18n::tr;

/// Asks for a confirmation; `on_confirm` is only called if the user validates.
// MessageDialog is deprecated from libadwaita 1.6 but remains the only option at 1.4.
#[allow(deprecated)]
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    confirm_label: &str,
    on_confirm: impl Fn() + 'static,
) {
    use adw::prelude::*;
    let window = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = adw::MessageDialog::new(window.as_ref(), Some(heading), Some(body));
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("confirm", confirm_label);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "confirm" {
            on_confirm();
        }
    });
    dialog.present();
}
