//! The Notifications tab: forwarding on or off, each app's mode, and recent notifications.

use super::state::{ago, State};
use crate::backend::{Command, Event};
use crate::store::{NotifyMode, Outcome};
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const MODES: [(NotifyMode, &str); 3] = [(NotifyMode::On, "On"), (NotifyMode::Off, "Off"), (NotifyMode::Always, "Always")];

fn outcome_text(o: Outcome) -> &'static str {
    match o {
        Outcome::Sent => "Sent",
        Outcome::SentQuietly => "Sent quietly",
        Outcome::AppOff => "App off",
        Outcome::Off => "Forwarding off",
        Outcome::Away => "Watch not connected",
        Outcome::SentLate => "Sent after reconnecting",
    }
}

pub struct NotificationsPage {
    pub widget: gtk::ScrolledWindow,
    enabled: adw::SwitchRow,
    quiet: adw::ActionRow,
    apps: adw::PreferencesGroup,
    recent: adw::PreferencesGroup,
    clear: gtk::Button,
    rows: RefCell<Vec<(adw::PreferencesGroup, adw::PreferencesRow)>>,
    /// Set while the switch is being moved to match the backend, so it sends nothing.
    updating: Cell<bool>,
}

impl NotificationsPage {
    pub fn new(state: &Rc<State>) -> Rc<NotificationsPage> {
        let main = adw::PreferencesGroup::new();
        let enabled = adw::SwitchRow::builder().title("Send Notifications to the Watch").active(true).build();
        let quiet = adw::ActionRow::builder().title("Do Not Disturb Is On").visible(false).build();
        quiet.add_prefix(&gtk::Image::from_icon_name("lithidion-notifications-symbolic"));
        main.add(&enabled);
        main.add(&quiet);
        let apps = adw::PreferencesGroup::builder().title("Apps").build();
        let recent = adw::PreferencesGroup::builder().title("Recent").build();
        let clear = gtk::Button::builder().label("Clear").valign(gtk::Align::Center).css_classes(["flat"]).build();
        recent.set_header_suffix(Some(&clear));
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        column.append(&main);
        column.append(&apps);
        column.append(&recent);
        let clamp = adw::Clamp::builder().maximum_size(640).child(&column).build();
        let widget = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();

        let page = Rc::new(NotificationsPage {
            widget,
            enabled,
            quiet,
            apps,
            recent,
            clear,
            rows: RefCell::default(),
            updating: Cell::new(false),
        });
        {
            let (state, page2) = (state.clone(), Rc::downgrade(&page));
            page.enabled.connect_active_notify(move |row| {
                if page2.upgrade().is_some_and(|p| !p.updating.get()) {
                    state.send(Command::SetNotificationsEnabled(row.is_active()));
                }
            });
        }
        {
            let state = state.clone();
            page.clear.connect_clicked(move |_| state.send(Command::ClearNotifyHistory));
        }
        page.refresh(state);
        {
            let (page2, state2) = (page.clone(), Rc::downgrade(state));
            state.listen(move |e| {
                let Some(state) = state2.upgrade() else { return false };
                if matches!(e, Event::NotificationsEnabled(_) | Event::NotifyApps(_) | Event::NotifyHistory(_) | Event::PhoneQuiet(_)) {
                    page2.refresh(&state);
                }
                true
            });
        }
        page
    }

    fn add(&self, group: &adw::PreferencesGroup, row: impl IsA<adw::PreferencesRow>) {
        let row: adw::PreferencesRow = row.upcast();
        group.add(&row);
        self.rows.borrow_mut().push((group.clone(), row));
    }

    fn refresh(&self, state: &Rc<State>) {
        self.updating.set(true);
        self.enabled.set_active(state.notifications_enabled.get());
        self.updating.set(false);
        self.quiet.set_visible(state.phone_quiet.get());
        for (group, row) in self.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }

        let apps = state.notify_apps.borrow().clone();
        if apps.is_empty() {
            self.add(
                &self.apps,
                adw::ActionRow::builder().title("Apps appear here after their first notification").css_classes(["dim-label"]).build(),
            );
        }
        let labels = gtk::StringList::new(&MODES.map(|(_, l)| l));
        for app in apps {
            let row = adw::ComboRow::builder().title(glib::markup_escape_text(&app.name)).model(&labels).build();
            if app.id != app.name {
                row.set_subtitle(&glib::markup_escape_text(&app.id));
            }
            row.add_prefix(&app_icon(app.icon.as_deref()));
            row.set_selected(MODES.iter().position(|(m, _)| *m == app.mode).unwrap_or(0) as u32);
            let (state, id) = (state.clone(), app.id.clone());
            row.connect_selected_notify(move |r| {
                if let Some((mode, _)) = MODES.get(r.selected() as usize) {
                    if *mode != app.mode {
                        state.send(Command::SetNotifyMode(id.clone(), *mode));
                    }
                }
            });
            self.add(&self.apps, row);
        }

        let history = state.notify_history.borrow().clone();
        self.clear.set_visible(!history.is_empty());
        if history.is_empty() {
            self.add(&self.recent, adw::ActionRow::builder().title("No notifications yet").css_classes(["dim-label"]).build());
        }
        for entry in history.iter().take(100) {
            let mut subtitle = format!("{} · {} · {}", entry.app, ago(entry.time), outcome_text(entry.outcome));
            if !entry.body.is_empty() {
                let body: String = entry.body.chars().take(120).collect();
                subtitle = format!("{body}\n{subtitle}");
            }
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&entry.title))
                .subtitle(glib::markup_escape_text(&subtitle))
                .subtitle_lines(3)
                .build();
            row.add_prefix(&app_icon(entry.icon.as_deref()));
            if !matches!(entry.outcome, Outcome::Sent | Outcome::SentQuietly) {
                row.add_css_class("dim-label");
            }
            self.add(&self.recent, row);
        }
    }
}

/// A cached image, a themed icon, or the generic one when the theme lacks it.
fn app_icon(icon: Option<&str>) -> gtk::Image {
    let image = match icon {
        Some(path) if path.starts_with('/') && std::path::Path::new(path).is_file() => gtk::Image::from_file(path),
        Some(name)
            if !name.starts_with('/') && gtk::gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(name)) =>
        {
            gtk::Image::from_icon_name(name)
        }
        _ => gtk::Image::from_icon_name("lithidion-notifications-symbolic"),
    };
    image.set_pixel_size(32);
    image
}

#[cfg(test)]
mod tests {
    /// Renders the tab with sample data to a PNG in the temp folder:
    /// `xvfb-run cargo test -- --ignored notifications_layout --test-threads=1`.
    #[test]
    #[ignore]
    fn notifications_layout() {
        use crate::backend::Event;
        use crate::store::{NotifyApp, NotifyEntry, NotifyMode, Outcome};
        use adw::prelude::*;
        adw::init().unwrap();
        let (tx, _rx) = async_channel::unbounded();
        let state = super::State::new(tx);
        let page = super::NotificationsPage::new(&state);
        let app = |id: &str, name: &str, mode| NotifyApp { id: id.into(), name: name.into(), mode, last_seen: 0, icon: None };
        state.apply(&Event::NotifyApps(vec![
            app("sm.puri.Chatty", "Chats", NotifyMode::Always),
            app("org.gnome.Calendar", "Calendar", NotifyMode::On),
            app("Telephony", "Telephony", NotifyMode::Off),
        ]));
        let now = chrono::Utc::now().timestamp();
        let entry = |title: &str, body: &str, ago: i64, outcome| NotifyEntry {
            app: "Chats".into(),
            icon: Some("mail-unread-symbolic".into()),
            title: title.into(),
            body: body.into(),
            time: now - ago,
            outcome,
        };
        state.apply(&Event::NotifyHistory(vec![
            entry("Alex", "Are we still on for tonight?", 60, Outcome::SentQuietly),
            entry("Sam", "", 4000, Outcome::Sent),
            entry("Missed call", "555 0100", 90_000, Outcome::Away),
        ]));
        state.apply(&Event::PhoneQuiet(true));
        let window = adw::Window::builder().default_width(360).default_height(900).content(&page.widget).build();
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
        texture.save_to_png(std::env::temp_dir().join("notifications.png")).unwrap();
    }
}
