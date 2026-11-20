//! The watch switcher, with Add a Watch as a page inside it.

use super::setup_page;
use super::state::{ago, closed_flag, stage_text, State};
use crate::backend::{Command, Event, FoundWatch};
use crate::store::WatchId;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub fn present(state: &Rc<State>, parent: &impl IsA<gtk::Widget>, toasts: &adw::ToastOverlay, start_with_add: bool) {
    let dialog = adw::Dialog::builder().title("Watches").content_width(420).content_height(560).build();
    let closed = closed_flag(&dialog);
    let nav = adw::NavigationView::new();

    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let caption = gtk::Label::builder()
        .label("One watch is connected at a time. Switching disconnects the current one; its apps, settings, and history stay here.")
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    column.append(&list);
    column.append(&caption);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).vexpand(true).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    let root = adw::NavigationPage::builder().title("Watches").tag("watches").child(&toolbar).build();
    nav.add(&root);

    let on_setup: Rc<dyn Fn(WatchId)> = {
        let (state, dialog, toasts, closed, nav) = (state.clone(), dialog.downgrade(), toasts.clone(), closed.clone(), nav.clone());
        Rc::new(move |id| {
            let Some(dialog) = dialog.upgrade() else { return };
            let page = setup_page::page(&state, &dialog, &toasts, closed.clone(), id);
            // Replaces Add a Watch: going back to the search makes no sense now.
            nav.replace(&[page]);
        })
    };
    let add_page = AddPage::new(state, &dialog, toasts, closed.clone(), on_setup);
    let fill = {
        let (state, list, dialog, nav, add) = (state.clone(), list.clone(), dialog.clone(), nav.clone(), add_page.page.clone());
        move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let active = state.active.get();
            // Nothing may interrupt an install.
            let busy = state.firmware.borrow().is_busy();
            for w in state.watches.borrow().iter() {
                let name = state.display_name(w);
                let is_active = Some(w.id) == active;
                let subtitle = if is_active {
                    let mut t = state.link_text();
                    if let (true, Some(b)) = (state.is_connected(), state.battery.get()) {
                        t.push_str(&format!(" · {b} %"));
                    }
                    t
                } else {
                    match w.last_connected {
                        Some(t) => format!("Last connected {}", ago(t)),
                        None => "Never connected".to_string(),
                    }
                };
                let row = adw::ActionRow::builder().title(&name).subtitle(&subtitle).build();
                row.add_prefix(&gtk::Image::from_icon_name("lithidion-watch-symbolic"));
                if is_active {
                    let check = gtk::Image::from_icon_name("lithidion-check-symbolic");
                    check.add_css_class("accent");
                    check.set_tooltip_text(Some("Active watch"));
                    row.add_suffix(&check);
                } else {
                    let switch = gtk::Button::builder().label("Switch").valign(gtk::Align::Center).sensitive(!busy).build();
                    let (state, dialog, id) = (state.clone(), dialog.downgrade(), w.id);
                    switch.connect_clicked(move |_| {
                        state.send(Command::Switch(id));
                        if let Some(d) = dialog.upgrade() {
                            d.close();
                        }
                    });
                    row.add_suffix(&switch);
                }
                list.append(&row);
            }
            let add_row = adw::ActionRow::builder().title("Add a Watch").activatable(true).sensitive(!busy).build();
            if busy {
                add_row.set_subtitle("Available once PebbleOS is installed");
            }
            add_row.add_prefix(&gtk::Image::from_icon_name("lithidion-add-symbolic"));
            add_row.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
            let (nav, add) = (nav.clone(), add.clone());
            add_row.connect_activated(move |_| nav.push(&add));
            list.append(&add_row);
        }
    };
    fill();
    {
        let (fill, closed) = (fill.clone(), closed.clone());
        state.listen(move |e| {
            if closed.get() {
                return false;
            }
            if matches!(e, Event::Watches { .. } | Event::Link(_) | Event::Battery(_) | Event::Firmware(_)) {
                fill();
            }
            true
        });
    }
    {
        let state = state.clone();
        nav.connect_popped(move |_, page| {
            if page.tag().as_deref() == Some("add") {
                state.send(Command::StopScan);
            }
        });
    }
    {
        let state = state.clone();
        dialog.connect_closed(move |_| state.send(Command::StopScan));
    }
    dialog.set_child(Some(&nav));
    if start_with_add {
        nav.push(&add_page.page);
    }
    dialog.present(Some(parent));
}

#[derive(Clone)]
struct AddPage {
    page: adw::NavigationPage,
}

fn signal_text(rssi: Option<i16>) -> &'static str {
    match rssi {
        Some(r) if r >= -65 => "strong signal",
        Some(r) if r >= -85 => "good signal",
        Some(_) => "weak signal",
        None => "signal unknown",
    }
}

impl AddPage {
    fn new(
        state: &Rc<State>,
        dialog: &adw::Dialog,
        toasts: &adw::ToastOverlay,
        closed: Rc<std::cell::Cell<bool>>,
        on_setup: Rc<dyn Fn(WatchId)>,
    ) -> AddPage {
        let icon = gtk::Image::builder()
            .icon_name("lithidion-bluetooth-symbolic")
            .pixel_size(40)
            .css_classes(["add-watch-icon"])
            .halign(gtk::Align::Center)
            .build();
        let title = gtk::Label::builder().label("Add a Watch").css_classes(["title-2"]).build();
        let body = gtk::Label::builder()
            .label("Keep the watch nearby with Bluetooth on. Pairing finishes with a confirmation on the watch.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();
        let intro = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(6).build();
        intro.append(&icon);
        intro.append(&title);
        intro.append(&body);

        let spinner = gtk::Spinner::builder().spinning(true).build();
        let heading = gtk::Label::builder()
            .label("Searching nearby…")
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["heading"])
            .build();
        let again = gtk::Button::builder().label("Search Again").visible(false).css_classes(["flat"]).build();
        let search_row = gtk::Box::builder().spacing(10).margin_top(12).build();
        search_row.append(&spinner);
        search_row.append(&heading);
        search_row.append(&again);

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        let placeholder =
            gtk::Label::builder().label("No watches found yet").margin_top(18).margin_bottom(18).css_classes(["dim-label"]).build();
        list.set_placeholder(Some(&placeholder));

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        column.append(&intro);
        column.append(&search_row);
        column.append(&list);
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).vexpand(true).build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&scroller));
        let page = adw::NavigationPage::builder().title("Add a Watch").tag("add").child(&toolbar).build();

        let pairing: Rc<RefCell<Option<String>>> = Rc::default();
        let found: Rc<RefCell<Vec<FoundWatch>>> = Rc::default();
        let render = {
            let (state, list, pairing, found) = (state.clone(), list.clone(), pairing.clone(), found.clone());
            move || {
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                let busy = pairing.borrow().clone();
                for f in found.borrow().iter() {
                    let mut subtitle = String::new();
                    if let Some(s) = &f.serial {
                        subtitle.push_str(&s[s.len().saturating_sub(4)..]);
                        subtitle.push_str(" · ");
                    }
                    subtitle.push_str(signal_text(f.rssi));
                    let row = adw::ActionRow::builder().title(&f.name).subtitle(&subtitle).build();
                    row.add_prefix(&gtk::Image::from_icon_name("lithidion-watch-symbolic"));
                    if busy.as_deref() == Some(f.address.as_str()) {
                        row.add_suffix(&gtk::Spinner::builder().spinning(true).build());
                    } else {
                        let pair = gtk::Button::builder()
                            .label("Pair")
                            .valign(gtk::Align::Center)
                            .css_classes(["suggested-action"])
                            .sensitive(busy.is_none())
                            .build();
                        let (state, pairing, address, set_up) = (state.clone(), pairing.clone(), f.address.clone(), f.set_up);
                        pair.connect_clicked(move |b| {
                            let start = {
                                let (state, pairing, address, b) = (state.clone(), pairing.clone(), address.clone(), b.clone());
                                move || {
                                    *pairing.borrow_mut() = Some(address.clone());
                                    b.set_sensitive(false);
                                    state.send(Command::Pair(address.clone()));
                                }
                            };
                            if !set_up {
                                start();
                                return;
                            }
                            let alert = adw::AlertDialog::new(
                                Some("Factory Reset Recommended"),
                                Some("This watch is not in recovery firmware. A factory reset before pairing gives it a clean start."),
                            );
                            alert.add_responses(&[("cancel", "Cancel"), ("pair", "Pair Anyway")]);
                            alert.set_default_response(Some("cancel"));
                            alert.set_close_response("cancel");
                            alert.connect_response(None, move |_, r| {
                                if r == "pair" {
                                    start();
                                }
                            });
                            alert.present(Some(b));
                        });
                        row.add_suffix(&pair);
                    }
                    list.append(&row);
                }
            }
        };
        {
            let state2 = state.clone();
            page.connect_shown(move |_| state2.send(Command::StartScan));
        }
        {
            let (state2, spinner, heading, again) = (state.clone(), spinner.clone(), heading.clone(), again.clone());
            again.connect_clicked(move |b| {
                b.set_visible(false);
                spinner.set_visible(true);
                heading.set_label("Searching nearby…");
                state2.send(Command::StartScan);
            });
        }
        {
            let (page_weak, dialog, toasts) = (page.downgrade(), dialog.downgrade(), toasts.clone());
            let (state2, pairing, found, render) = (Rc::downgrade(state), pairing.clone(), found.clone(), render.clone());
            state.listen(move |e| {
                let (Some(_page), Some(state)) = (page_weak.upgrade(), state2.upgrade()) else { return false };
                if closed.get() {
                    return false;
                }
                match e {
                    Event::Found(list) => {
                        *found.borrow_mut() = list.clone();
                        render();
                    }
                    Event::ScanEnded(error) => {
                        if pairing.borrow().is_none() {
                            spinner.set_visible(false);
                            heading.set_label(&match error {
                                Some(e) => format!("Search failed: {e}"),
                                None => "Search finished".to_string(),
                            });
                            again.set_visible(true);
                        }
                    }
                    Event::PairingStage(stage) => {
                        spinner.set_visible(true);
                        heading.set_label(stage_text(*stage));
                        again.set_visible(false);
                    }
                    Event::Paired(result) => {
                        *pairing.borrow_mut() = None;
                        match result {
                            Ok(p) if p.recovery => on_setup(p.id),
                            Ok(p) => {
                                let id = &p.id;
                                let name = state
                                    .watches
                                    .borrow()
                                    .iter()
                                    .find(|w| w.id == *id)
                                    .map(|w| state.display_name(w))
                                    .unwrap_or_else(|| "The watch".to_string());
                                toasts.add_toast(adw::Toast::new(&format!("{name} added")));
                                if let Some(d) = dialog.upgrade() {
                                    d.close();
                                }
                            }
                            Err(e) => {
                                toasts.add_toast(
                                    adw::Toast::builder()
                                        .title(glib::markup_escape_text(&format!("Pairing failed: {e}")))
                                        .timeout(8)
                                        .build(),
                                );
                                render();
                                spinner.set_visible(false);
                                heading.set_label("Pairing failed");
                                again.set_visible(true);
                            }
                        }
                    }
                    _ => {}
                }
                true
            });
        }
        AddPage { page }
    }
}
