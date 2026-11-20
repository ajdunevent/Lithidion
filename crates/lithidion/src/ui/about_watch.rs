//! About This Watch, opened from the ⓘ button on the watch tile.

use super::state::{closed_flag, State};
use crate::backend::{Command, Event};
use adw::prelude::*;
use pebble_protocol::hardware::HardwarePlatform;
use std::rc::Rc;

fn row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(value).css_classes(["property"]).build();
    row.set_subtitle_selectable(true);
    row
}

/// The watch's name, with a pencil that turns it into an entry; the check (or Enter) saves it.
/// An empty name goes back to the model's.
fn name_editor(state: &Rc<State>, watch: &crate::store::Watch, name: &str) -> (adw::PreferencesGroup, gtk::Button) {
    let label = gtk::Label::builder().label(name).xalign(0.0).css_classes(["title-2"]).ellipsize(gtk::pango::EllipsizeMode::End).build();
    let entry = gtk::Entry::builder().text(watch.nickname.as_deref().unwrap_or(name)).placeholder_text(watch.name()).hexpand(true).build();
    let stack = gtk::Stack::builder().hexpand(true).build();
    stack.add_named(&label, Some("label"));
    stack.add_named(&entry, Some("entry"));
    let pencil = gtk::Button::builder()
        .icon_name("lithidion-edit-symbolic")
        .tooltip_text("Rename")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let header = gtk::Box::builder().spacing(6).build();
    header.append(&stack);
    header.append(&pencil);
    let group = adw::PreferencesGroup::new();
    group.add(&header);

    let editing = Rc::new(std::cell::Cell::new(false));
    let finish = {
        let (state, id, label, entry, stack, pencil, editing) =
            (state.clone(), watch.id, label.clone(), entry.clone(), stack.clone(), pencil.clone(), editing.clone());
        let default = watch.name();
        Rc::new(move || {
            let text = entry.text().trim().to_string();
            label.set_label(if text.is_empty() { &default } else { &text });
            state.send(Command::RenameWatch(id, text));
            stack.set_visible_child_name("label");
            pencil.set_icon_name("lithidion-edit-symbolic");
            pencil.set_tooltip_text(Some("Rename"));
            editing.set(false);
        })
    };
    {
        let (finish, entry, stack, editing) = (finish.clone(), entry.clone(), stack.clone(), editing.clone());
        pencil.connect_clicked(move |b| {
            if editing.get() {
                finish();
                return;
            }
            editing.set(true);
            stack.set_visible_child_name("entry");
            b.set_icon_name("lithidion-check-symbolic");
            b.set_tooltip_text(Some("Save Name"));
            entry.grab_focus();
        });
    }
    entry.connect_activate(move |_| finish());
    (group, pencil)
}

pub fn present(state: &Rc<State>, parent: &impl IsA<gtk::Widget>) {
    let Some(watch) = state.active_watch() else { return };
    let name = state.display_name(&watch);
    let dialog = adw::Dialog::builder().title("About This Watch").content_width(420).content_height(600).build();
    let page = adw::PreferencesPage::new();

    page.add(&name_editor(state, &watch, &name).0);
    let about = adw::PreferencesGroup::new();
    about.add(&row("Model", &watch.name()));
    if let Some(finish) = watch.finish() {
        about.add(&row("Finish", finish));
    }
    if let Some(serial) = &watch.serial {
        about.add(&row("Serial Number", serial));
    }
    about.add(&row("Bluetooth Address", &watch.address));
    if let Some(h) = watch.hardware {
        let hw = HardwarePlatform::from_number(h);
        let platform = hw.app_platform.map(|p| format!(" · {} apps", p.codename())).unwrap_or_default();
        about.add(&row("Hardware", &format!("{}{platform}", hw.revision)));
    }
    page.add(&about);

    let firmware = adw::PreferencesGroup::builder().title("Firmware").build();
    let slot = state.session.borrow().as_ref().and_then(|s| s.firmware_slot);
    if let Some(fw) = &watch.firmware {
        let slot = slot.map(|s| format!(" · slot {s}")).unwrap_or_default();
        firmware.add(&row("PebbleOS", &format!("{}{slot}", fw.trim_start_matches('v'))));
    }
    firmware
        .add(&row("Recovery Firmware", watch.recovery_firmware.as_deref().map(|r| r.trim_start_matches('v')).unwrap_or("None installed")));
    if let Some(board) = &watch.board {
        firmware.add(&row("Board", board));
    }
    if let Some(language) = &watch.language {
        firmware.add(&row("Language", language));
    }
    page.add(&firmware);

    let forget =
        gtk::Button::builder().label("Forget This Watch").halign(gtk::Align::Center).css_classes(["pill", "destructive-action"]).build();
    let footer = adw::PreferencesGroup::new();
    footer.add(&forget);
    page.add(&footer);
    {
        let (state, dialog, id, name) = (state.clone(), dialog.clone(), watch.id, name.clone());
        forget.connect_clicked(move |_| {
            let connected = matches!(*state.link.borrow(), crate::backend::LinkState::Connected);
            // The watch keeps its side of the pairing, and stays hidden from other devices.
            let alert = adw::AlertDialog::new(
                Some(&format!("Forget {name}?")),
                Some(&format!(
                    "The watch stays paired with this {} until its pairing is removed in the watch's Settings → Bluetooth, or the watch is reset.",
                    crate::host::noun()
                )),
            );
            alert.add_responses(&[("cancel", "Cancel"), ("forget", "Forget")]);
            alert.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
            if connected {
                alert.add_response("reset", "Reset and Forget");
                alert.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
            }
            alert.set_default_response(Some("cancel"));
            alert.set_close_response("cancel");
            let (state, d) = (state.clone(), dialog.clone());
            alert.connect_response(None, move |_, response| {
                if response == "forget" || response == "reset" {
                    state.send(Command::Forget(id, response == "reset"));
                    d.close();
                }
            });
            alert.present(Some(&dialog));
        });
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));
    {
        // Closes when the watch stops being the active one.
        let (d, id, closed) = (dialog.downgrade(), watch.id, closed_flag(&dialog));
        state.listen(move |e| {
            let Some(d) = d.upgrade().filter(|_| !closed.get()) else { return false };
            if let Event::Watches { active, .. } = e {
                if *active != Some(id) {
                    d.close();
                    return false;
                }
            }
            true
        });
    }
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    /// Renders the name header, then the same while editing, to PNGs in the temp folder:
    /// `xvfb-run cargo test -- --ignored name_editor_layout`.
    #[test]
    #[ignore]
    fn name_editor_layout() {
        use adw::prelude::*;
        adw::init().unwrap();
        let (tx, _rx) = async_channel::unbounded();
        let state = super::State::new(tx);
        let watch = crate::store::Watch {
            id: 1,
            address: "24:A3:0A:D4:7B:24".into(),
            model: Some(37),
            hardware: None,
            serial: None,
            board: None,
            firmware: None,
            recovery_firmware: None,
            language: None,
            added: 0,
            last_connected: None,
            profile: 1,
            nickname: None,
        };
        let (group, pencil) = super::name_editor(&state, &watch, "Pebble Time 2");
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let window = adw::Window::builder().default_width(360).default_height(200).content(&page).build();
        window.present();
        let ctx = glib::MainContext::default();
        let snap = |name: &str| {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(800);
            while std::time::Instant::now() < end {
                ctx.iteration(false);
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
            let texture = window.native().unwrap().renderer().unwrap().render_texture(snapshot.to_node().unwrap(), None);
            texture.save_to_png(std::env::temp_dir().join(name)).unwrap();
        };
        snap("name.png");
        pencil.emit_clicked();
        snap("name-editing.png");
    }
}
