//! Advanced: the active watch's connection details and maintenance actions.

use super::firmware;
use super::state::{closed_flag, State};
use crate::backend::{Command, Diagnostic, Event, FirmwareSource, LinkState, Update};
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

fn value_row(title: &str) -> (adw::ActionRow, gtk::Label) {
    let row = adw::ActionRow::builder().title(title).build();
    let value = gtk::Label::builder().selectable(true).css_classes(["dim-label"]).wrap(true).xalign(1.0).build();
    row.add_suffix(&value);
    (row, value)
}

fn action_row(title: &str, subtitle: &str, button: &str, destructive: bool) -> (adw::ActionRow, gtk::Button) {
    let row = adw::ActionRow::builder().title(title).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    let b = gtk::Button::builder().label(button).valign(gtk::Align::Center).build();
    if destructive {
        b.add_css_class("destructive-action");
    }
    row.add_suffix(&b);
    (row, b)
}

fn confirm(parent: &adw::Dialog, heading: &str, body: &str, verb: &str, then: impl Fn() + 'static) {
    let alert = adw::AlertDialog::new(Some(heading), Some(body));
    alert.add_responses(&[("cancel", "Cancel"), ("go", verb)]);
    alert.set_response_appearance("go", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    alert.connect_response(None, move |_, r| {
        if r == "go" {
            then();
        }
    });
    alert.present(Some(parent));
}

pub fn present(state: &Rc<State>, parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::Dialog::builder().title("Advanced").content_width(460).content_height(640).build();
    let closed = closed_flag(&dialog);
    let title = adw::WindowTitle::new("Advanced", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let page = adw::PreferencesPage::new();

    let conn = adw::PreferencesGroup::builder().title("Connection").build();
    let (state_row, state_v) = value_row("State");
    let (transport_row, transport_v) = value_row("Transport");
    let (version_row, version_v) = value_row("PPoGATT Version");
    let (window_row, window_v) = value_row("Windows");
    let (unfaithful_row, unfaithful_v) = value_row("Used With Another Device");
    let (app_row, app_v) = value_row("Running App");
    for r in [&state_row, &transport_row, &version_row, &window_row, &unfaithful_row, &app_row] {
        conn.add(r);
    }
    app_row.set_subtitle_selectable(true);
    page.add(&conn);

    let caps_group = adw::PreferencesGroup::new();
    let caps = adw::ExpanderRow::builder().title("Watch Capabilities").build();
    caps_group.add(&caps);
    page.add(&caps_group);

    let actions = adw::PreferencesGroup::builder().title("Actions").build();
    let (ping_row, ping) = action_row("Ping", "", "Ping", false);
    let (time_row, time) = action_row("Send Time", "", "Send", false);
    let (reboot_row, reboot) = action_row("Restart Watch", "", "Restart", true);
    for r in [&ping_row, &time_row, &reboot_row] {
        actions.add(r);
    }
    page.add(&actions);

    let diag_group = adw::PreferencesGroup::builder().title("Diagnostics").build();
    let (logs_row, logs) = action_row("Save Watch Logs", "", "Save…", false);
    let (dump_row, dump) = action_row("Save Core Dump", "", "Save…", false);
    diag_group.add(&logs_row);
    diag_group.add(&dump_row);
    page.add(&diag_group);
    // The saves under way.
    let saving: Rc<RefCell<Vec<Diagnostic>>> = Rc::default();

    let fw_group = adw::PreferencesGroup::builder().title("Firmware").build();
    let (file_row, file) = action_row("Install Firmware From File", "", "Choose…", false);
    let (latest_prf_row, latest_prf) = action_row("Install Latest Recovery Firmware", "", "Install", true);
    let (prf_row, prf) = action_row("Restart into Recovery Firmware", "For reinstalling PebbleOS", "Restart", true);
    // What an install is doing, and how it ended.
    let progress_row = adw::ActionRow::builder().visible(false).build();
    let progress_bar = gtk::ProgressBar::builder().valign(gtk::Align::Center).width_request(96).build();
    let progress_spinner = gtk::Spinner::builder().valign(gtk::Align::Center).build();
    progress_row.add_suffix(&progress_bar);
    progress_row.add_suffix(&progress_spinner);
    for r in [&progress_row, &file_row, &latest_prf_row, &prf_row] {
        fw_group.add(r);
    }
    page.add(&fw_group);

    let apps_group = adw::PreferencesGroup::builder().title("Apps").build();
    let (pbw_row, pbw) = action_row("Install App From File", "", "Choose…", false);
    apps_group.add(&pbw_row);
    page.add(&apps_group);
    {
        let state = state.clone();
        pbw.connect_clicked(move |b| choose_pbw(&state, b));
    }
    {
        let state = state.clone();
        file.connect_clicked(move |b| firmware::choose_file(&state, b));
    }
    {
        let state = state.clone();
        latest_prf.connect_clicked(move |b| {
            firmware::confirm(
                &state,
                b,
                "Install Recovery Firmware?",
                &format!(
                    "Lithidion downloads the newest recovery firmware from the PebbleOS releases on GitHub. It needs PebbleOS running, and replaces nothing else. {}",
                    firmware::WHAT_HAPPENS
                ),
                "Install",
                FirmwareSource::LatestRecovery,
            )
        });
    }
    {
        let state = state.clone();
        ping.connect_clicked(move |_| state.send(Command::Ping));
    }
    {
        let state = state.clone();
        time.connect_clicked(move |_| state.send(Command::SyncTime));
    }
    {
        let (state, d) = (state.clone(), dialog.clone());
        reboot.connect_clicked(move |_| {
            let state = state.clone();
            confirm(&d, "Restart the Watch?", "The connection drops and comes back once the watch has started.", "Restart", move || {
                state.send(Command::Reboot)
            });
        });
    }
    {
        let (state, d) = (state.clone(), dialog.clone());
        prf.connect_clicked(move |_| {
            let state = state.clone();
            confirm(
                &d,
                "Restart into Recovery Firmware?",
                "The watch starts its recovery firmware and stays there until PebbleOS is installed again.",
                "Restart",
                move || state.send(Command::RebootIntoRecovery),
            );
        });
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    let caps_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>> = Rc::default();
    // Set once the watch restarts after an install, to say it worked when it is back.
    let restarted = Rc::new(std::cell::Cell::new(false));
    for (kind, button) in [(Diagnostic::Logs, &logs), (Diagnostic::CoreDump, &dump)] {
        let (state, saving) = (state.clone(), saving.clone());
        button.connect_clicked(move |b| choose_diagnostic_file(&state, b, kind, &saving));
    }
    let refresh = {
        let state = state.clone();
        let saving = saving.clone();
        move || {
            let name = state.active_watch().map(|w| state.display_name(&w)).unwrap_or_else(|| "No watch".to_string());
            title.set_subtitle(&name);
            let link = state.link.borrow().clone();
            state_v.set_label(&match &link {
                LinkState::Disconnected { error: Some(e), .. } => format!("Not connected: {e}"),
                other => super::state::link_text(other),
            });
            let session = state.session.borrow().clone();
            let connected = matches!(link, LinkState::Connected);
            pbw.set_sensitive(state.active.get().is_some());
            let busy = state.firmware.borrow().is_busy();
            for b in [&ping, &time, &reboot, &prf, &file, &latest_prf] {
                b.set_sensitive(connected && !busy);
            }
            logs.set_sensitive(connected && !busy && !saving.borrow().contains(&Diagnostic::Logs));
            dump.set_sensitive(connected && !busy && !saving.borrow().contains(&Diagnostic::CoreDump));
            let recovery = session.as_ref().is_some_and(|s| s.recovery_mode);
            latest_prf.set_sensitive(connected && !busy && !recovery);
            let dash = "–".to_string();
            match &session {
                Some(s) => {
                    transport_v.set_label(s.transport.label());
                    version_v.set_label(&s.ppog_version.to_string());
                    window_v.set_label(&format!("{} out, {} in", s.tx_window, s.rx_window));
                    unfaithful_v.set_label(match s.unfaithful {
                        Some(true) => "Yes",
                        Some(false) => "No",
                        None => "Not reported",
                    });
                    match s.running_app {
                        Some(u) => {
                            app_v.set_label(&app_name(&state, u));
                            app_row.set_subtitle(&u.to_string());
                        }
                        None => {
                            app_v.set_label(&dash);
                            app_row.set_subtitle("");
                        }
                    }
                }
                None => {
                    for v in [&transport_v, &version_v, &window_v, &unfaithful_v, &app_v] {
                        v.set_label(&dash);
                    }
                    app_row.set_subtitle("");
                }
            }
            let update = state.firmware.borrow().clone();
            let what = firmware::what(&state);
            if matches!(update, Update::Restarting { .. }) {
                restarted.set(true);
            }
            // title, detail, progress, spinning
            let shown: Option<(String, String, Option<f64>, bool)> = match &update {
                Update::Downloading(f) => Some((format!("Downloading {what}…"), percent(*f), Some(*f), false)),
                Update::Installing(f) => {
                    Some((format!("Sending {what} to the Watch…"), format!("{} · Keep the watch close", percent(*f)), Some(*f), false))
                }
                Update::Restarting { into_recovery: true } => Some((
                    "Restarting into Recovery Firmware…".to_string(),
                    "The install continues once it has started.".to_string(),
                    None,
                    true,
                )),
                Update::Restarting { .. } => {
                    Some(("The Watch Is Restarting…".to_string(), format!("{what} is put in place while it starts."), None, true))
                }
                Update::Failed(e) => {
                    restarted.set(false);
                    Some((format!("{what} Was Not Installed"), super::state::sentence(e), None, false))
                }
                _ if restarted.get() && connected => {
                    Some((format!("{what} Installed"), "The watch has restarted.".to_string(), None, false))
                }
                _ => None,
            };
            match shown {
                Some((t, detail, progress, spinning)) => {
                    progress_row.set_visible(true);
                    progress_row.set_title(&t);
                    progress_row.set_subtitle(&detail);
                    progress_bar.set_visible(progress.is_some());
                    progress_bar.set_fraction(progress.unwrap_or(0.0));
                    progress_spinner.set_visible(spinning);
                    progress_spinner.set_spinning(spinning);
                }
                None => {
                    progress_row.set_visible(false);
                    progress_spinner.set_spinning(false);
                }
            }
            for r in caps_rows.borrow_mut().drain(..) {
                caps.remove(&r);
            }
            let list: Vec<_> = session.as_ref().map(|s| s.capabilities.clone()).unwrap_or_default();
            caps.set_subtitle(&match &session {
                Some(_) => format!("{} reported", list.len()),
                None => "Known once connected".to_string(),
            });
            for c in list {
                let r = adw::ActionRow::builder().title(c.label()).build();
                caps.add_row(&r);
                caps_rows.borrow_mut().push(r);
            }
        }
    };
    refresh();
    {
        state.listen(move |e| {
            if closed.get() {
                return false;
            }
            if let Event::Diagnostic(kind, progress) = e {
                let (row, unit) = match kind {
                    Diagnostic::Logs => (&logs_row, "lines"),
                    Diagnostic::CoreDump => (&dump_row, ""),
                };
                let mut saving = saving.borrow_mut();
                saving.retain(|k| k != kind);
                match progress {
                    Some(p) if unit.is_empty() => row.set_subtitle(&percent(*p)),
                    Some(p) => row.set_subtitle(&format!("{} {unit}", *p as u64)),
                    None => row.set_subtitle(""),
                }
                if progress.is_some() {
                    saving.push(*kind);
                }
                drop(saving);
                refresh();
            }
            if matches!(e, Event::Watches { .. } | Event::Link(_) | Event::Session(_) | Event::Firmware(_) | Event::Apps(_)) {
                refresh();
            }
            true
        });
    }
    dialog.present(Some(parent));
}

/// Asks where to save the watch's log or core dump, then saves it there.
fn choose_diagnostic_file(state: &Rc<State>, button: &gtk::Button, kind: Diagnostic, saving: &Rc<RefCell<Vec<Diagnostic>>>) {
    let date = chrono::Local::now().format("%Y-%m-%d");
    let (title, name) = match kind {
        Diagnostic::Logs => ("Save Watch Logs", format!("pebble-logs-{date}.txt")),
        Diagnostic::CoreDump => ("Save Core Dump", format!("pebble-core-dump-{date}.bin")),
    };
    let chooser = gtk::FileDialog::builder().title(title).accept_label("Save").initial_name(name).modal(true).build();
    let window = button.root().and_downcast::<gtk::Window>();
    let (state, saving, button) = (state.clone(), saving.clone(), button.clone());
    chooser.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
        if let Some(path) = result.ok().and_then(|f| f.path()) {
            saving.borrow_mut().push(kind);
            button.set_sensitive(false);
            state.send(Command::SaveDiagnostic(kind, path));
        }
    });
}

fn percent(f: f64) -> String {
    format!("{}%", (f * 100.0).round() as u32)
}

/// An installed app's title, or the name of a built-in one.
fn app_name(state: &State, uuid: uuid::Uuid) -> String {
    let installed = state.apps.borrow().iter().find(|a| a.app.uuid == uuid).map(|a| a.app.title.clone());
    installed
        .or_else(|| pebble_protocol::apps::system_app_name(uuid).map(str::to_string))
        .unwrap_or_else(|| format!("Not installed from this {}", crate::host::noun()))
}

fn choose_pbw(state: &Rc<State>, parent: &gtk::Button) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Pebble Apps"));
    filter.add_pattern("*.pbw");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let chooser = gtk::FileDialog::builder().title("Install App").accept_label("Install").filters(&filters).modal(true).build();
    let window = parent.root().and_downcast::<gtk::Window>();
    let state = state.clone();
    chooser.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
        if let Some(path) = result.ok().and_then(|f| f.path()) {
            state.send(Command::InstallAppFile(path));
        }
    });
}
