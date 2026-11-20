//! What the phone sends the watch besides notifications: calendars for the timeline, and
//! weather places and their source.

use super::state::{sentence, State};
use crate::backend::weather::{self, Place, Provider};
use crate::backend::{self, Command};
use crate::net;
use crate::settings::WeatherSettings;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

fn page(title: &str, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let clamp = adw::Clamp::builder().maximum_size(640).child(content).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    adw::NavigationPage::builder().title(title).child(&toolbar).build()
}

fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build()
}

/// "2 calendars" or "None", for the Watch tab.
pub fn calendars_summary(state: &State) -> String {
    let n = state.pref(|p| p.calendars.len());
    match n {
        0 => "None sent yet".to_string(),
        1 => "1 calendar".to_string(),
        n => format!("{n} calendars"),
    }
}

pub fn weather_summary(state: &State) -> String {
    let Some(w) = state.prefs.borrow().as_ref().map(|p| p.weather.clone()) else { return String::new() };
    let mut places: Vec<String> = Vec::new();
    if w.here {
        places.push("Current location".to_string());
    }
    places.extend(w.places.iter().map(|p| p.name.split(',').next().unwrap_or(&p.name).to_string()));
    if places.is_empty() {
        return format!("{} · No places", w.provider.name());
    }
    format!("{} · {}", w.provider.name(), places.join(", "))
}

/// A switch per calendar; none are sent until switched on.
pub fn calendars_page(state: &Rc<State>) -> adw::NavigationPage {
    let content = column();
    let group = adw::PreferencesGroup::builder().title("Calendars").build();
    let status = gtk::Label::builder().wrap(true).margin_top(12).css_classes(["dim-label"]).build();
    let spinner = gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).margin_top(24).build();
    content.append(&spinner);
    content.append(&group);
    content.append(&status);
    group.set_visible(false);
    let page = page("Calendars", &content);

    let chosen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(state.pref(|p| p.calendars.clone())));
    let state = state.clone();
    glib::spawn_future_local(async move {
        let found = net::run(backend::list_calendars()).await;
        spinner.set_visible(false);
        match found {
            Ok(list) if list.is_empty() => status.set_label(&format!(
                "This {} has no calendars yet. Add one in GNOME Calendar or Settings → Online Accounts.",
                crate::host::noun()
            )),
            Ok(list) => {
                group.set_visible(true);
                for cal in list {
                    let row = adw::SwitchRow::builder()
                        .title(glib::markup_escape_text(&cal.name))
                        .active(chosen.borrow().contains(&cal.uid))
                        .build();
                    if let Some(a) = &cal.account {
                        row.set_subtitle(&glib::markup_escape_text(a));
                    }
                    let (state, chosen, uid) = (state.clone(), chosen.clone(), cal.uid.clone());
                    row.connect_active_notify(move |r| {
                        let mut c = chosen.borrow_mut();
                        c.retain(|u| *u != uid);
                        if r.is_active() {
                            c.push(uid.clone());
                        }
                        state.send(Command::SetCalendars(c.clone()));
                    });
                    group.add(&row);
                }
            }
            Err(e) => status.set_label(&sentence(&format!("{e:#}"))),
        }
    });
    page
}

/// Rebuilds the list of saved places.
type Refill = Rc<dyn Fn()>;

/// The weather source and places.
pub fn weather_page(state: &Rc<State>, nav: &adw::NavigationView) -> adw::NavigationPage {
    let current = Rc::new(RefCell::new(state.prefs.borrow().as_ref().map(|p| p.weather.clone()).unwrap_or(WeatherSettings {
        provider: Provider::OpenMeteo,
        here: true,
        places: Vec::new(),
    })));
    let content = column();
    let source = adw::PreferencesGroup::builder().title("Source").build();
    let names: Vec<&str> = Provider::ALL.iter().map(|p| p.name()).collect();
    let provider = adw::ComboRow::builder().title("Weather From").model(&gtk::StringList::new(&names)).build();
    provider.set_selected(Provider::ALL.iter().position(|p| *p == current.borrow().provider).unwrap_or(0) as u32);
    provider.set_subtitle(current.borrow().provider.note());
    source.add(&provider);
    let places = adw::PreferencesGroup::builder().title("Places").build();
    let here = adw::SwitchRow::builder().title("Current Location").active(current.borrow().here).build();
    places.add(&here);
    let add = adw::ActionRow::builder().title("Add a Place").activatable(true).build();
    add.add_prefix(&gtk::Image::from_icon_name("lithidion-add-symbolic"));
    content.append(&source);
    content.append(&places);
    let page = page("Weather", &content);

    let send = {
        let (state, current) = (state.clone(), current.clone());
        Rc::new(move || state.send(Command::SetWeather(current.borrow().clone())))
    };
    {
        let (current, send) = (current.clone(), send.clone());
        provider.connect_selected_notify(move |r| {
            let p = Provider::ALL[(r.selected() as usize).min(Provider::ALL.len() - 1)];
            r.set_subtitle(p.note());
            current.borrow_mut().provider = p;
            send();
        });
    }
    {
        let (current, send) = (current.clone(), send.clone());
        here.connect_active_notify(move |r| {
            current.borrow_mut().here = r.is_active();
            send();
        });
    }
    // Saved places: rebuilt on every change, followed by the Add row.
    let rows: Rc<RefCell<Vec<adw::PreferencesRow>>> = Rc::default();
    let fill: Rc<RefCell<Option<Refill>>> = Rc::default();
    let refill: Rc<dyn Fn()> = {
        let (places, rows, current, send, add, fill) =
            (places.clone(), rows.clone(), current.clone(), send.clone(), add.clone(), fill.clone());
        Rc::new(move || {
            for r in rows.borrow_mut().drain(..) {
                places.remove(&r);
            }
            if add.parent().is_some() {
                places.remove(&add);
            }
            for (i, p) in current.borrow().places.iter().enumerate() {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.name)).build();
                let remove = gtk::Button::builder()
                    .icon_name("lithidion-trash-symbolic")
                    .tooltip_text("Remove")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                let (current, send, fill) = (current.clone(), send.clone(), fill.clone());
                remove.connect_clicked(move |_| {
                    current.borrow_mut().places.remove(i);
                    send();
                    if let Some(f) = fill.borrow().clone() {
                        f();
                    }
                });
                row.add_suffix(&remove);
                places.add(&row);
                rows.borrow_mut().push(row.upcast());
            }
            places.add(&add);
        })
    };
    *fill.borrow_mut() = Some(refill.clone());
    refill();
    {
        let (nav, current, send, refill) = (nav.clone(), current.clone(), send.clone(), refill.clone());
        add.connect_activated(move |_| {
            let (nav2, current, send, refill) = (nav.clone(), current.clone(), send.clone(), refill.clone());
            nav.push(&search_page(move |place| {
                current.borrow_mut().places.push(place);
                send();
                refill();
                nav2.pop();
            }));
        });
    }
    page
}

/// Finding a place by name.
fn search_page(chosen: impl Fn(Place) + 'static) -> adw::NavigationPage {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    let entry = gtk::SearchEntry::builder().placeholder_text("City or town").build();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).visible(false).build();
    let status = gtk::Label::builder().wrap(true).css_classes(["dim-label"]).build();
    content.append(&entry);
    content.append(&list);
    content.append(&status);
    let page = page("Add a Place", &content);
    page.connect_shown({
        let entry = entry.clone();
        move |_| {
            entry.grab_focus();
        }
    });
    let chosen = Rc::new(chosen);
    let generation = Rc::new(std::cell::Cell::new(0u64));
    entry.connect_search_changed(move |e| {
        let text = e.text().trim().to_string();
        generation.set(generation.get() + 1);
        let mine = generation.get();
        while let Some(c) = list.first_child() {
            list.remove(&c);
        }
        list.set_visible(false);
        if text.len() < 2 {
            status.set_label("");
            return;
        }
        status.set_label("Searching…");
        let (list, status, generation, chosen) = (list.clone(), status.clone(), generation.clone(), chosen.clone());
        glib::spawn_future_local(async move {
            let found = net::run(weather::search(text)).await;
            if generation.get() != mine {
                return;
            }
            match found {
                Ok(places) if places.is_empty() => status.set_label("Nothing found"),
                Ok(places) => {
                    status.set_label("");
                    list.set_visible(true);
                    for p in places {
                        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.name)).activatable(true).build();
                        let chosen = chosen.clone();
                        row.connect_activated(move |_| chosen(p.clone()));
                        list.append(&row);
                    }
                }
                Err(e) => status.set_label(&sentence(&format!("{e:#}"))),
            }
        });
    });
    page
}

#[cfg(test)]
mod tests {
    /// Renders the weather page to a PNG in the temp folder:
    /// `xvfb-run cargo test -- --ignored weather_layout --test-threads=1`.
    #[test]
    #[ignore]
    fn weather_layout() {
        use adw::prelude::*;
        adw::init().unwrap();
        let (tx, _rx) = async_channel::unbounded();
        let state = super::State::new(tx);
        let nav = adw::NavigationView::new();
        nav.add(&super::weather_page(&state, &nav));
        let window = adw::Window::builder().default_width(360).default_height(640).content(&nav).build();
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
        texture.save_to_png(std::env::temp_dir().join("weather.png")).unwrap();
    }
}
