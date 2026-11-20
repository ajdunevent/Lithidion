//! Asking before a firmware install, shared by the Watch tab and Advanced.

use super::state::State;
use crate::backend::{Command, FirmwareSource};
use adw::prelude::*;
use std::rc::Rc;

/// "2.6 MB".
pub fn size(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Shows the version the way the watch tile does: "4.47.0".
pub fn version(tag: &str) -> &str {
    tag.trim_start_matches('v')
}

pub fn confirm(state: &Rc<State>, parent: &impl IsA<gtk::Widget>, heading: &str, body: &str, verb: &str, source: FirmwareSource) {
    let alert = adw::AlertDialog::new(Some(heading), Some(body));
    alert.add_responses(&[("cancel", "Cancel"), ("go", verb)]);
    alert.set_response_appearance("go", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("go"));
    alert.set_close_response("cancel");
    let state = state.clone();
    alert.connect_response(None, move |_, r| {
        if r == "go" {
            install(&state, source.clone());
        }
    });
    alert.present(Some(parent));
}

pub fn install(state: &State, source: FirmwareSource) {
    *state.last_install.borrow_mut() = Some(source.clone());
    state.send(Command::InstallFirmware(source));
}

/// What the current or last install is: "PebbleOS", "Recovery Firmware" or "Firmware".
pub fn what(state: &State) -> &'static str {
    match &*state.last_install.borrow() {
        Some(FirmwareSource::LatestRecovery) => "Recovery Firmware",
        Some(FirmwareSource::File(_)) => "Firmware",
        _ => "PebbleOS",
    }
}

/// The part of every confirmation that says what happens.
pub const WHAT_HAPPENS: &str =
    "The watch restarts when it is done, which takes a few minutes. Keep it close and Lithidion running until then.";

/// Picks a `.pbz` and asks before installing it.
pub fn choose_file(state: &Rc<State>, parent: &impl IsA<gtk::Widget>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Firmware Bundles"));
    filter.add_pattern("*.pbz");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let chooser = gtk::FileDialog::builder().title("Install Firmware").accept_label("Choose").filters(&filters).modal(true).build();
    let window = parent.as_ref().root().and_downcast::<gtk::Window>();
    let (state, parent) = (state.clone(), parent.as_ref().clone());
    chooser.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
        let Some(path) = result.ok().and_then(|f| f.path()) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        confirm(
            &state,
            &parent,
            "Install This Firmware?",
            &format!("{name} is checked against the watch before anything is sent. {WHAT_HAPPENS}"),
            "Install",
            FirmwareSource::File(path),
        );
    });
}
