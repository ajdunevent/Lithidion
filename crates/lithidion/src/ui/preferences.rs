//! Preferences: settings kept on the phone. Settings stored on a watch live under the Watch tab.

use super::state::State;
use crate::appstore::Source;
use crate::backend::dictation::State as Dictation;
use crate::backend::{Command, Event};
use adw::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

pub fn present(state: &Rc<State>, parent: &impl IsA<gtk::Widget>, toasts: &adw::ToastOverlay) {
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::builder().title("General").icon_name("lithidion-settings-symbolic").build();
    let settings = state.settings.borrow().clone();

    let background = adw::PreferencesGroup::builder().title("Background").build();
    let run = adw::SwitchRow::builder()
        .title("Run in Background")
        .subtitle("Stay connected with the window closed")
        .active(settings.run_in_background)
        .build();
    let login =
        adw::SwitchRow::builder().title("Start at Login").subtitle("Start hidden and connect").active(settings.start_at_login).build();
    background.add(&run);
    background.add(&login);
    page.add(&background);

    let general = adw::PreferencesGroup::builder().title("General").build();
    let units = adw::ComboRow::builder().title("Units").model(&gtk::StringList::new(&["System", "Metric", "Imperial"])).build();
    units.set_selected(crate::units::Units::ALL.iter().position(|u| *u == crate::units::chosen()).unwrap_or(0) as u32);
    let launch = adw::ComboRow::builder().title("Launch With Profile").model(&gtk::StringList::new(&["Last Used", "Select"])).build();
    launch.set_selected(state.pref(|p| p.launch_with_chooser) as u32);
    let dictation = adw::SwitchRow::builder().title("Dictation").build();
    let pictures = adw::SwitchRow::builder()
        .title("Pictures in Notifications")
        .active(state.pref(|p| p.pictures))
        .build();
    general.add(&units);
    general.add(&launch);
    general.add(&dictation);
    general.add(&pictures);
    {
        let state = state.clone();
        pictures.connect_active_notify(move |r| state.send(crate::backend::Command::SetPictures(r.is_active())));
    }
    page.add(&general);
    {
        let state = state.clone();
        units.connect_selected_notify(move |r| {
            let u = crate::units::Units::ALL[(r.selected() as usize).min(2)];
            state.send(crate::backend::Command::SetUnits(u));
        });
    }
    {
        let state = state.clone();
        launch.connect_selected_notify(move |r| {
            state.send(crate::backend::Command::SetLaunchWithChooser(r.selected() == 1));
        });
    }

    let catalog = adw::PreferencesGroup::builder().title("Catalog").build();
    let chosen = super::store_page::catalog_sources(state);
    let rows: Vec<(Source, adw::SwitchRow)> =
        Source::ALL.into_iter().map(|s| (s, adw::SwitchRow::builder().title(s.name()).active(chosen.contains(&s)).build())).collect();
    let rows = Rc::new(rows);
    // The last store left on cannot be switched off.
    let lock_last = {
        let rows = rows.clone();
        move || {
            let on = rows.iter().filter(|(_, r)| r.is_active()).count();
            for (_, r) in rows.iter() {
                r.set_sensitive(!(on == 1 && r.is_active()));
            }
        }
    };
    lock_last();
    for (_, row) in rows.iter() {
        catalog.add(row);
        let (state, rows, lock_last) = (state.clone(), rows.clone(), lock_last.clone());
        row.connect_active_notify(move |_| {
            lock_last();
            let sources: Vec<Source> = rows.iter().filter(|(_, r)| r.is_active()).map(|(s, _)| *s).collect();
            state.send(crate::backend::Command::SetCatalogSources(sources));
        });
    }
    page.add(&catalog);
    dialog.add(&page);

    {
        let state = state.clone();
        run.connect_active_notify(move |r| {
            state.settings.borrow_mut().run_in_background = r.is_active();
            state.save_settings();
        });
    }
    {
        let (state, toasts) = (state.clone(), toasts.clone());
        let reverting = Rc::new(Cell::new(false));
        login.connect_active_notify(move |r| {
            if reverting.get() {
                return;
            }
            let enabled = r.is_active();
            let (state, toasts, row, reverting) = (state.clone(), toasts.clone(), r.clone(), reverting.clone());
            glib::spawn_future_local(async move {
                match crate::autostart::set(enabled).await {
                    Ok(()) => {
                        state.settings.borrow_mut().start_at_login = enabled;
                        state.save_settings();
                    }
                    Err(e) => {
                        reverting.set(true);
                        row.set_active(!enabled);
                        reverting.set(false);
                        toasts.add_toast(adw::Toast::new(&glib::markup_escape_text(&format!("{e:#}"))));
                    }
                }
            });
        });
    }
    dictation_row(state, &dialog, &dictation);
    state.preferences_open.set(true);
    {
        let state = state.clone();
        dialog.connect_closed(move |_| state.preferences_open.set(false));
    }
    dialog.present(Some(parent));
}

/// The dictation switch: on downloads the speech model, with its progress shown until it is in
/// place; off removes it.
fn dictation_row(state: &Rc<State>, dialog: &adw::PreferencesDialog, row: &adw::SwitchRow) {
    let show = |row: &adw::SwitchRow, d: &Dictation| match d {
        Dictation::Downloading(p) => row.set_subtitle(&format!("Downloading speech model… {}%", (p * 100.0) as u32)),
        _ => row.set_subtitle(""),
    };
    let current = state.dictation.borrow().clone().unwrap_or(Dictation::Off);
    row.set_active(matches!(current, Dictation::Ready | Dictation::Downloading(_)));
    show(row, &current);
    let reverting = Rc::new(Cell::new(false));
    {
        let (state, reverting) = (state.clone(), reverting.clone());
        row.connect_active_notify(move |r| {
            if !reverting.get() {
                state.send(Command::SetDictation(r.is_active()));
            }
        });
    }
    let (r, d, closed) = (row.downgrade(), dialog.downgrade(), super::state::closed_flag(dialog));
    state.listen(move |e| {
        let (Some(row), Some(dialog)) = (r.upgrade(), d.upgrade()) else { return false };
        if closed.get() {
            return false;
        }
        if let Event::Dictation(s) = e {
            show(&row, s);
            if let Dictation::Failed(why) = s {
                reverting.set(true);
                row.set_active(false);
                reverting.set(false);
                let text = super::state::sentence(why);
                dialog.add_toast(adw::Toast::builder().title(glib::markup_escape_text(&text)).timeout(8).build());
            }
        }
        true
    });
}
