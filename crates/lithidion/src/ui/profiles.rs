//! Profiles: the people whose watches, notification settings, and health history Lithidion
//! keeps apart. The chooser, adding a profile, and editing one.

use super::state::State;
use crate::backend::{Command, Event};
use crate::settings::{HealthSettings, Sex};
use adw::prelude::*;
use chrono::Datelike;
use std::cell::RefCell;
use std::rc::Rc;

const SEXES: [(Sex, &str); 3] = [(Sex::Female, "Female"), (Sex::Male, "Male"), (Sex::Other, "Other")];

/// Height, weight, year of birth, and sex, written into `current` as they change.
pub fn details_group(current: &Rc<RefCell<HealthSettings>>) -> adw::PreferencesGroup {
    let imperial = crate::units::imperial();
    let h = current.borrow().clone();
    let group = adw::PreferencesGroup::new();
    let spin = |title: &str, low: f64, high: f64, value: f64| {
        adw::SpinRow::builder().title(title).adjustment(&gtk::Adjustment::new(value, low, high, 1.0, 10.0, 0.0)).build()
    };
    let (height, weight) = if imperial {
        (
            spin("Height (in)", 36.0, 96.0, (h.height_mm as f64 / 25.4).round()),
            spin("Weight (lb)", 45.0, 660.0, (h.weight_dag as f64 / 45.359_237).round()),
        )
    } else {
        (
            spin("Height (cm)", 90.0, 245.0, (h.height_mm as f64 / 10.0).round()),
            spin("Weight (kg)", 20.0, 300.0, (h.weight_dag as f64 / 100.0).round()),
        )
    };
    {
        let current = current.clone();
        height.connect_value_notify(move |r| {
            let mm = if imperial { r.value() * 25.4 } else { r.value() * 10.0 };
            current.borrow_mut().height_mm = mm.round() as u16;
        });
    }
    {
        let current = current.clone();
        weight.connect_value_notify(move |r| {
            let dag = if imperial { r.value() * 45.359_237 } else { r.value() * 100.0 };
            current.borrow_mut().weight_dag = dag.round() as u16;
        });
    }
    let this_year = crate::host::Zone::now().year();
    let born = spin("Year of Birth", (this_year - 120) as f64, (this_year - 1) as f64, h.birth_year.unwrap_or(this_year - 30) as f64);
    {
        let current = current.clone();
        born.connect_value_notify(move |r| current.borrow_mut().birth_year = Some(r.value() as i32));
    }
    let names: Vec<&str> = SEXES.iter().map(|(_, n)| *n).collect();
    let sex = adw::ComboRow::builder().title("Sex").model(&gtk::StringList::new(&names)).build();
    sex.set_selected(SEXES.iter().position(|(s, _)| *s == h.sex).unwrap_or(0) as u32);
    {
        let current = current.clone();
        sex.connect_selected_notify(move |r| {
            if let Some((s, _)) = SEXES.get(r.selected() as usize) {
                current.borrow_mut().sex = *s;
            }
        });
    }
    for r in [height.upcast_ref::<gtk::Widget>(), weight.upcast_ref(), born.upcast_ref(), sex.upcast_ref()] {
        group.add(r);
    }
    group
}

/// Holds the details group, rebuilt when the units change.
fn details_box(details: &Rc<RefCell<HealthSettings>>) -> gtk::Box {
    let holder = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    holder.append(&details_group(details));
    holder
}

/// `[ Metric | Imperial ]`: sets the Units preference, which the details are entered in.
fn units_toggle(state: &Rc<State>, holder: &gtk::Box, details: &Rc<RefCell<HealthSettings>>) -> gtk::Box {
    let metric = gtk::ToggleButton::builder().label("Metric").build();
    let imperial = gtk::ToggleButton::builder().label("Imperial").group(&metric).build();
    if crate::units::imperial() {
        imperial.set_active(true);
    } else {
        metric.set_active(true);
    }
    let toggle = gtk::Box::builder().css_classes(["linked"]).halign(gtk::Align::Center).build();
    toggle.append(&metric);
    toggle.append(&imperial);
    let (state, holder, details) = (state.clone(), holder.clone(), details.clone());
    imperial.connect_toggled(move |b| {
        let u = if b.is_active() { crate::units::Units::Imperial } else { crate::units::Units::Metric };
        crate::units::use_units(u);
        state.send(Command::SetUnits(u));
        while let Some(c) = holder.first_child() {
            holder.remove(&c);
        }
        holder.append(&details_group(&details));
    });
    toggle
}

fn scrolled(content: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder().maximum_size(560).child(content).build();
    gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build()
}

fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build()
}

/// The profile list and Add Profile. `chosen` runs once a profile is picked or added.
pub fn chooser(state: &Rc<State>, menu: &gio::Menu, chosen: Rc<dyn Fn()>) -> adw::NavigationView {
    let nav = adw::NavigationView::new();
    let list = adw::PreferencesGroup::new();
    let add_group = adw::PreferencesGroup::new();
    let add = adw::ActionRow::builder().title("Add Profile").activatable(true).build();
    add.add_prefix(&gtk::Image::from_icon_name("lithidion-add-symbolic"));
    add_group.add(&add);
    let content = column();
    content.append(&list);
    content.append(&add_group);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder().show_back_button(false).build();
    header.pack_end(
        &gtk::MenuButton::builder().icon_name("lithidion-menu-symbolic").menu_model(menu).tooltip_text("Main Menu").primary(true).build(),
    );
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled(&content)));
    nav.add(&adw::NavigationPage::builder().title("Profiles").tag("profiles").child(&toolbar).build());

    let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::default();
    let fill = {
        let (state, list, rows, chosen) = (state.clone(), list.clone(), rows.clone(), chosen.clone());
        move || {
            for r in rows.borrow_mut().drain(..) {
                list.remove(&r);
            }
            for p in state.profiles.borrow().iter() {
                let subtitle = if p.watches.is_empty() { "No watches".to_string() } else { p.watches.join(", ") };
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&p.name))
                    .subtitle(glib::markup_escape_text(&subtitle))
                    .activatable(true)
                    .build();
                row.add_prefix(&adw::Avatar::new(32, Some(&p.name), true));
                if state.profile.get() == Some(p.id) {
                    row.add_suffix(&gtk::Image::from_icon_name("lithidion-check-symbolic"));
                }
                let (state, chosen, id) = (state.clone(), chosen.clone(), p.id);
                row.connect_activated(move |_| {
                    state.send(Command::SwitchProfile(id));
                    chosen();
                });
                list.add(&row);
                rows.borrow_mut().push(row);
            }
            list.set_visible(!rows.borrow().is_empty());
        }
    };
    fill();
    state.listen(move |e| {
        if matches!(e, Event::Profiles { .. }) {
            fill();
        }
        true
    });
    {
        let (state, nav2) = (state.clone(), nav.clone());
        add.connect_activated(move |_| nav2.push(&add_page(&state, &nav2, chosen.clone())));
    }
    nav
}

fn add_page(state: &Rc<State>, nav: &adw::NavigationView, chosen: Rc<dyn Fn()>) -> adw::NavigationPage {
    let details = Rc::new(RefCell::new(HealthSettings::default()));
    let name = adw::EntryRow::builder().title("Name").build();
    let name_group = adw::PreferencesGroup::new();
    name_group.add(&name);
    let content = column();
    let holder = details_box(&details);
    content.append(&units_toggle(state, &holder, &details));
    content.append(&name_group);
    content.append(&holder);
    let create = gtk::Button::builder().label("Add").css_classes(["suggested-action"]).sensitive(false).build();
    let header = adw::HeaderBar::new();
    header.pack_end(&create);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled(&content)));
    let page = adw::NavigationPage::builder().title("Add Profile").child(&toolbar).build();
    {
        let create = create.clone();
        name.connect_changed(move |e| create.set_sensitive(!e.text().trim().is_empty()));
    }
    let go = {
        let (state, nav, name) = (state.clone(), nav.clone(), name.clone());
        move || {
            let text = name.text().trim().to_string();
            if text.is_empty() {
                return;
            }
            state.send(Command::AddProfile(text, details.borrow().clone()));
            nav.pop_to_tag("profiles");
            chosen();
        }
    };
    let go = Rc::new(go);
    {
        let go = go.clone();
        create.connect_clicked(move |_| go());
    }
    name.connect_entry_activated(move |_| go());
    page.connect_shown(move |_| {
        name.grab_focus();
    });
    page
}

/// Name, details, and deleting the active profile.
pub fn edit(state: &Rc<State>, parent: &impl IsA<gtk::Widget>) {
    let Some(id) = state.profile.get() else { return };
    let details = Rc::new(RefCell::new(state.health.borrow().as_ref().map(|v| v.settings.clone()).unwrap_or_default()));
    let dialog = adw::Dialog::builder().title("Edit Profile").content_width(420).content_height(560).build();
    let name = adw::EntryRow::builder().title("Name").text(state.profile_name().unwrap_or_default()).build();
    let name_group = adw::PreferencesGroup::new();
    name_group.add(&name);
    let delete_group = adw::PreferencesGroup::new();
    let delete = adw::ActionRow::builder().title("Delete Profile…").activatable(true).css_classes(["error"]).build();
    delete_group.add(&delete);
    let content = column();
    let holder = details_box(&details);
    content.append(&units_toggle(state, &holder, &details));
    content.append(&name_group);
    content.append(&holder);
    content.append(&delete_group);
    let save = gtk::Button::builder().label("Save").css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
    let cancel = gtk::Button::builder().label("Cancel").build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled(&content)));
    dialog.set_child(Some(&toolbar));
    {
        let save = save.clone();
        name.connect_changed(move |e| save.set_sensitive(!e.text().trim().is_empty()));
    }
    {
        let d = dialog.clone();
        cancel.connect_clicked(move |_| {
            d.close();
        });
    }
    {
        let (state, d, name) = (state.clone(), dialog.clone(), name.clone());
        save.connect_clicked(move |_| {
            state.send(Command::EditProfile(id, name.text().trim().to_string(), details.borrow().clone()));
            d.close();
        });
    }
    {
        let (state, d) = (state.clone(), dialog.clone());
        delete.connect_activated(move |_| {
            let who = state.profile_name().unwrap_or_default();
            let first = adw::AlertDialog::new(
                Some(&format!("Delete {who}?")),
                Some("Its watches are removed from this device, and its health history and settings are deleted."),
            );
            first.add_responses(&[("cancel", "Cancel"), ("next", "Delete")]);
            first.set_response_appearance("next", adw::ResponseAppearance::Destructive);
            first.set_default_response(Some("cancel"));
            first.set_close_response("cancel");
            let (state, d2) = (state.clone(), d.clone());
            first.connect_response(None, move |_, r| {
                if r != "next" {
                    return;
                }
                let second =
                    adw::AlertDialog::new(Some("This Cannot Be Undone"), Some(&format!("Delete {who} and everything kept for it?")));
                second.add_responses(&[("cancel", "Cancel"), ("delete", "Delete Profile")]);
                second.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                second.set_default_response(Some("cancel"));
                second.set_close_response("cancel");
                let (state, d3) = (state.clone(), d2.clone());
                second.connect_response(None, move |_, r| {
                    if r == "delete" {
                        state.send(Command::DeleteProfile(id));
                        d3.close();
                    }
                });
                second.present(Some(&d2));
            });
            first.present(Some(&d));
        });
    }
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ProfileInfo;

    fn render(widget: &impl IsA<gtk::Widget>, name: &str) {
        let window = adw::Window::builder().default_width(360).default_height(700).content(widget).build();
        super::super::add_icons(&WidgetExt::display(&window));
        window.present();
        let ctx = glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < end {
            ctx.iteration(false);
        }
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        let texture = window.native().unwrap().renderer().unwrap().render_texture(snapshot.to_node().unwrap(), None);
        texture.save_to_png(std::env::temp_dir().join(name)).unwrap();
    }

    /// Renders the chooser and Add Profile to PNGs in the temp folder:
    /// `xvfb-run cargo test -- --ignored profiles_layout`.
    #[test]
    #[ignore]
    fn profiles_layout() {
        adw::init().unwrap();
        let (tx, _rx) = async_channel::unbounded();
        let state = State::new(tx);
        state.apply(&Event::Profiles {
            list: vec![
                ProfileInfo { id: 1, name: "AJ".into(), watches: vec!["Pebble Time 2".into()] },
                ProfileInfo { id: 2, name: "Sam".into(), watches: vec![] },
            ],
            active: Some(1),
        });
        let nav = chooser(&state, &super::super::chooser_menu(), Rc::new(|| {}));
        render(&nav, "profiles.png");
        let nav = adw::NavigationView::new();
        nav.add(&add_page(&state, &nav, Rc::new(|| {})));
        render(&nav, "profile-add.png");
    }
}
