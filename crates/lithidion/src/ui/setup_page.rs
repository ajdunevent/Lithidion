//! The last step of adding a watch that has only its recovery firmware: installing PebbleOS.
//! On an unmetered connection the install starts by itself; on a metered one it waits for the
//! Install button.

use super::firmware;
use super::state::{sentence, State};
use crate::backend::{Command, Event, FirmwareSource, LinkState, Update};
use crate::store::WatchId;
use adw::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// Title, body, spinning, progress, main button, and whether the file and skip buttons show.
type View = (String, String, bool, Option<f64>, Option<(&'static str, Main)>, bool);

/// What the main button does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Main {
    Install,
    Check,
    Retry,
    Done,
}

pub fn page(
    state: &Rc<State>,
    dialog: &adw::Dialog,
    toasts: &adw::ToastOverlay,
    closed: Rc<Cell<bool>>,
    id: WatchId,
) -> adw::NavigationPage {
    let icon = gtk::Image::builder()
        .icon_name("lithidion-watch-symbolic")
        .pixel_size(40)
        .css_classes(["add-watch-icon"])
        .halign(gtk::Align::Center)
        .build();
    let title = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).css_classes(["title-2"]).build();
    let body = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).css_classes(["dim-label"]).build();
    let spinner = gtk::Spinner::builder().halign(gtk::Align::Center).build();
    let bar = gtk::ProgressBar::builder().margin_start(12).margin_end(12).build();
    let main = gtk::Button::builder().halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
    let file = gtk::Button::builder().label("Install From File…").halign(gtk::Align::Center).css_classes(["pill"]).build();
    let skip = gtk::Button::builder().label("Skip").halign(gtk::Align::Center).css_classes(["flat"]).build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();
    for w in [icon.upcast_ref::<gtk::Widget>(), title.upcast_ref(), body.upcast_ref(), spinner.upcast_ref(), bar.upcast_ref()] {
        column.append(w);
    }
    let buttons = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(12).build();
    buttons.append(&main);
    buttons.append(&file);
    buttons.append(&skip);
    column.append(&buttons);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).vexpand(true).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::builder().show_back_button(false).build());
    toolbar.set_content(Some(&scroller));
    let page = adw::NavigationPage::builder().title("Set Up the Watch").tag("setup").can_pop(false).child(&toolbar).build();

    let name = state.watches.borrow().iter().find(|w| w.id == id).map(|w| state.display_name(w)).unwrap_or_else(|| "The watch".to_string());
    // An install was asked for and has not reported back yet.
    let waiting = Rc::new(Cell::new(false));
    // The install starts by itself once at most; after a failure it waits for a button.
    let auto_tried = Rc::new(Cell::new(false));
    let action = Rc::new(Cell::new(Main::Done));

    let finish = {
        let (dialog, toasts, name) = (dialog.downgrade(), toasts.clone(), name.clone());
        move || {
            toasts.add_toast(adw::Toast::new(&format!("{name} added")));
            if let Some(d) = dialog.upgrade() {
                d.close();
            }
        }
    };
    {
        let finish = finish.clone();
        skip.connect_clicked(move |_| finish());
    }
    {
        let state = state.clone();
        file.connect_clicked(move |b| firmware::choose_file(&state, b));
    }
    {
        let (state, action, waiting, finish) = (state.clone(), action.clone(), waiting.clone(), finish.clone());
        main.connect_clicked(move |_| match action.get() {
            Main::Install => {
                waiting.set(true);
                firmware::install(&state, FirmwareSource::Latest);
            }
            Main::Check => state.send(Command::CheckFirmware),
            Main::Retry => {
                let last = state.last_install.borrow().clone();
                firmware::install(&state, last.unwrap_or(FirmwareSource::Latest));
            }
            Main::Done => finish(),
        });
    }

    let render = {
        let state = state.clone();
        move || {
            let update = state.firmware.borrow().clone();
            if !matches!(update, Update::Available { .. }) {
                waiting.set(false);
            }
            let metered = gio::NetworkMonitor::default().is_network_metered();
            if matches!(update, Update::Available { missing: true, .. }) && !metered && !auto_tried.get() {
                auto_tried.set(true);
                waiting.set(true);
                firmware::install(&state, FirmwareSource::Latest);
            }
            let connected = matches!(*state.link.borrow(), LinkState::Connected);
            let in_recovery = state.session.borrow().as_ref().map(|s| s.recovery_mode);
            let installed = connected && in_recovery == Some(false) && state.active.get() == Some(id);

            let (t, b, spin, progress, button, extras): View = if installed {
                let tag = state.active_watch().and_then(|w| w.firmware).unwrap_or_default();
                (
                    if tag.is_empty() {
                        "PebbleOS Is Installed".to_string()
                    } else {
                        format!("PebbleOS {} Is Installed", firmware::version(&tag))
                    },
                    format!("{name} is ready."),
                    false,
                    None,
                    Some(("Done", Main::Done)),
                    false,
                )
            } else {
                match &update {
                    Update::Available { missing: true, .. } if waiting.get() => {
                        ("Installing PebbleOS…".to_string(), "Starting…".to_string(), true, None, None, false)
                    }
                    Update::Available { tag, size, missing: true } => {
                        let why = if metered { " This connection is metered, so the download waits for you." } else { "" };
                        (
                            "Install PebbleOS".to_string(),
                            format!(
                                "{name} needs PebbleOS {} ({}) before it can be used.{why}",
                                firmware::version(tag),
                                firmware::size(*size)
                            ),
                            false,
                            None,
                            Some(("Install", Main::Install)),
                            true,
                        )
                    }
                    Update::CheckFailed(e) => {
                        ("Cannot Find PebbleOS".to_string(), sentence(e), false, None, Some(("Try Again", Main::Check)), true)
                    }
                    Update::Downloading(f) => ("Downloading PebbleOS…".to_string(), percent(*f), false, Some(*f), None, false),
                    Update::Installing(f) => (
                        "Installing PebbleOS…".to_string(),
                        format!("{} · Keep the watch close", percent(*f)),
                        false,
                        Some(*f),
                        None,
                        false,
                    ),
                    Update::Restarting { .. } => (
                        "Waiting for the Watch to Restart…".to_string(),
                        "PebbleOS starts in a minute or so.".to_string(),
                        true,
                        None,
                        None,
                        false,
                    ),
                    Update::Failed(e) => {
                        ("The Install Failed".to_string(), sentence(e), false, None, Some(("Try Again", Main::Retry)), true)
                    }
                    _ if !connected => {
                        (format!("Setting Up {name}"), "Waiting for the watch to connect…".to_string(), true, None, None, true)
                    }
                    _ => (format!("Setting Up {name}"), "Looking for PebbleOS…".to_string(), true, None, None, true),
                }
            };
            title.set_label(&t);
            body.set_visible(!b.is_empty());
            body.set_label(&b);
            spinner.set_visible(spin);
            spinner.set_spinning(spin);
            bar.set_visible(progress.is_some());
            bar.set_fraction(progress.unwrap_or(0.0));
            match button {
                Some((label, a)) => {
                    main.set_visible(true);
                    main.set_label(label);
                    action.set(a);
                }
                None => main.set_visible(false),
            }
            file.set_visible(extras && connected && in_recovery == Some(true));
            skip.set_visible(extras);
        }
    };
    render();
    state.listen(move |e| {
        if closed.get() {
            return false;
        }
        if matches!(e, Event::Firmware(_) | Event::Link(_) | Event::Session(_) | Event::Watches { .. }) {
            render();
        }
        true
    });
    page
}

fn percent(f: f64) -> String {
    format!("{} %", (f * 100.0).floor() as u32)
}
