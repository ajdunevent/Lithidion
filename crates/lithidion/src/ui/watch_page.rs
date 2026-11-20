//! The Watch tab: the active watch's tile, or a prompt to add one.

use super::firmware;
use super::phone_data;
use super::state::{ago, sentence, stage_text, State};
use super::watch_art::WatchArt;
use crate::backend::{Command, Event, FirmwareSource, LinkState, Update};
use adw::prelude::*;
use std::cell::{Cell, OnceCell};
use std::rc::Rc;

pub struct WatchPage {
    pub widget: gtk::Stack,
    art: WatchArt,
    dot: gtk::Label,
    status: gtk::Label,
    battery_box: gtk::Box,
    battery: gtk::Label,
    firmware: gtk::Label,
    seen: gtk::Label,
    progress_box: gtk::Box,
    spinner: gtk::Spinner,
    progress: gtk::Label,
    retry: gtk::Button,
    notice: gtk::Label,
    update_box: gtk::Box,
    update_spinner: gtk::Spinner,
    update_title: gtk::Label,
    update_detail: gtk::Label,
    update_button: gtk::Button,
    update_bar: gtk::ProgressBar,
    update_action: Rc<Cell<UpdateAction>>,
    calendars: adw::ActionRow,
    weather: adw::ActionRow,
    nav: Rc<OnceCell<adw::NavigationView>>,
    on_watch: adw::PreferencesGroup,
}

type UpdateRow = (String, String, Option<(&'static str, UpdateAction)>, Option<f64>, bool);

/// What the update row's button does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateAction {
    Install,
    Retry,
    Check,
}

fn dim(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).css_classes(["dim-label"]).build()
}

impl WatchPage {
    pub fn new(state: &Rc<State>, on_add: impl Fn() + 'static, on_info: impl Fn() + 'static) -> Rc<WatchPage> {
        let nav_cell: Rc<OnceCell<adw::NavigationView>> = Rc::default();
        let empty = adw::StatusPage::builder()
            .icon_name("lithidion-watch-symbolic")
            .title("Add a Watch")
            .description("Lithidion connects to Pebble watches over Bluetooth.")
            .build();
        let add = gtk::Button::builder().label("Add a Watch").halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
        add.connect_clicked(move |_| on_add());
        empty.set_child(Some(&add));

        let art = WatchArt::new(96, true);
        let dot = gtk::Label::builder().label("●").css_classes(["status-dot"]).build();
        let status = gtk::Label::builder().xalign(0.0).css_classes(["heading"]).build();
        let status_row = gtk::Box::builder().spacing(8).build();
        status_row.append(&dot);
        status_row.append(&status);
        let battery_icon = gtk::Image::from_icon_name("lithidion-battery-symbolic");
        let battery = dim("");
        let battery_box = gtk::Box::builder().spacing(8).build();
        battery_box.append(&battery_icon);
        battery_box.append(&battery);
        let firmware = dim("");
        let seen = dim("");
        let facts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).valign(gtk::Align::Center).hexpand(true).build();
        for w in [status_row.upcast_ref::<gtk::Widget>(), battery_box.upcast_ref(), firmware.upcast_ref(), seen.upcast_ref()] {
            facts.append(w);
        }
        let top = gtk::Box::builder().spacing(16).margin_top(16).margin_bottom(16).margin_start(16).margin_end(48).build();
        top.append(&art.area);
        top.append(&facts);

        let spinner = gtk::Spinner::new();
        let progress = dim("");
        progress.set_hexpand(true);
        let retry = gtk::Button::builder().label("Connect Now").valign(gtk::Align::Center).css_classes(["pill"]).build();
        let progress_box = gtk::Box::builder().spacing(12).margin_start(16).margin_end(16).margin_top(12).margin_bottom(12).build();
        progress_box.append(&spinner);
        progress_box.append(&progress);
        progress_box.append(&retry);
        {
            let state = state.clone();
            retry.connect_clicked(move |_| state.send(Command::Reconnect));
        }
        let notice =
            gtk::Label::builder().xalign(0.0).wrap(true).margin_start(16).margin_end(16).margin_bottom(12).css_classes(["warning"]).build();

        let update_spinner = gtk::Spinner::new();
        let update_title = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["heading"]).build();
        let update_detail = dim("");
        let update_text =
            gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
        update_text.append(&update_title);
        update_text.append(&update_detail);
        let update_button = gtk::Button::builder().valign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
        let update_row = gtk::Box::builder().spacing(12).build();
        update_row.append(&update_spinner);
        update_row.append(&update_text);
        update_row.append(&update_button);
        let update_bar = gtk::ProgressBar::new();
        let update_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        update_box.append(&update_row);
        update_box.append(&update_bar);
        let update_action = Rc::new(Cell::new(UpdateAction::Check));
        {
            let (state, action) = (state.clone(), update_action.clone());
            update_button.connect_clicked(move |b| on_update_clicked(&state, b, action.get()));
        }

        let card_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        card_box.append(&top);
        card_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        card_box.append(&progress_box);
        card_box.append(&update_box);
        card_box.append(&notice);
        let info = gtk::Button::builder()
            .icon_name("lithidion-info-symbolic")
            .tooltip_text("About This Watch")
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(6)
            .margin_end(6)
            .css_classes(["flat", "circular"])
            .build();
        info.connect_clicked(move |_| on_info());
        let card = gtk::Overlay::builder().child(&card_box).css_classes(["card"]).build();
        card.add_overlay(&info);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        column.append(&card);
        let on_watch = {
            let nav = nav_cell.clone();
            super::watch_settings::group_for(state, move || nav.get().cloned())
        };
        on_watch.set_margin_top(12);
        column.append(&on_watch);
        let data = adw::PreferencesGroup::builder().title(format!("From This {}", crate::host::title())).margin_top(12).build();
        let calendars = adw::ActionRow::builder().title("Calendars").activatable(true).build();
        calendars.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
        let weather = adw::ActionRow::builder().title("Weather").activatable(true).build();
        weather.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
        data.add(&calendars);
        data.add(&weather);
        column.append(&data);
        let clamp = adw::Clamp::builder().maximum_size(600).child(&column).build();
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();

        let widget = gtk::Stack::new();
        widget.add_named(&empty, Some("empty"));
        widget.add_named(&scroller, Some("watch"));

        let page = Rc::new(WatchPage {
            widget,
            art,
            dot,
            status,
            battery_box,
            battery,
            firmware,
            seen,
            progress_box,
            spinner,
            progress,
            retry,
            notice,
            update_box,
            update_spinner,
            update_title,
            update_detail,
            update_button,
            update_bar,
            update_action,
            calendars,
            weather,
            on_watch,
            nav: nav_cell,
        });
        {
            let (state2, page2) = (state.clone(), Rc::downgrade(&page));
            page.calendars.connect_activated(move |_| {
                if let Some(nav) = page2.upgrade().and_then(|p| p.nav.get().cloned()) {
                    nav.push(&phone_data::calendars_page(&state2));
                }
            });
            let (state2, page2) = (state.clone(), Rc::downgrade(&page));
            page.weather.connect_activated(move |_| {
                if let Some(nav) = page2.upgrade().and_then(|p| p.nav.get().cloned()) {
                    nav.push(&phone_data::weather_page(&state2, &nav));
                }
            });
        }
        page.refresh(state);
        {
            // The listener owns the page; the window keeps only its widget.
            let (page, state2) = (page.clone(), Rc::downgrade(state));
            state.listen(move |e| {
                let Some(state) = state2.upgrade() else { return false };
                if matches!(
                    e,
                    Event::Watches { .. }
                        | Event::Link(_)
                        | Event::Battery(_)
                        | Event::Session(_)
                        | Event::Firmware(_)
                        | Event::WatchPrefs(_)
                        | Event::Apps(_)
                ) {
                    page.refresh(&state);
                }
                true
            });
        }
        page
    }

    fn refresh(&self, state: &State) {
        let Some(watch) = state.active_watch() else {
            self.widget.set_visible_child_name("empty");
            return;
        };
        self.widget.set_visible_child_name("watch");
        super::watch_settings::update(state, &self.on_watch);
        let name = state.display_name(&watch);
        self.art.set_finish(watch.finish(), &name);
        self.art.set_ratio(super::watch_art::screen_ratio(watch.platform()));
        let link = state.link.borrow().clone();
        let connected = matches!(link, LinkState::Connected);
        // The running app's store picture on the screen, while connected.
        let running = state.session.borrow().as_ref().and_then(|s| s.running_app).filter(|_| connected);
        let apps = state.apps.borrow();
        let picture = running.and_then(|uuid| apps.iter().find(|a| a.app.uuid == uuid)).and_then(|a| {
            let face = a.app.kind == crate::store::AppKind::Watchface;
            a.app.image.as_deref().map(|url| (url, face))
        });
        self.art.set_screen(picture);
        drop(apps);
        self.status.set_label(&state.link_text());
        for c in ["connected", "connecting"] {
            self.dot.remove_css_class(c);
        }
        match link {
            LinkState::Connected => self.dot.add_css_class("connected"),
            LinkState::Connecting(_) | LinkState::Negotiating => self.dot.add_css_class("connecting"),
            _ => {}
        }
        match state.battery.get().filter(|_| connected) {
            Some(level) => {
                self.battery_box.set_visible(true);
                self.battery.set_label(&format!("{level} %"));
            }
            None => self.battery_box.set_visible(false),
        }
        let session = state.session.borrow().clone();
        let recovery = session.as_ref().is_some_and(|s| s.recovery_mode);
        let fw = match (&watch.firmware, recovery) {
            (_, true) => "Running recovery firmware".to_string(),
            (Some(tag), false) => format!("PebbleOS {}", tag.trim_start_matches('v')),
            (None, false) => String::new(),
        };
        self.firmware.set_visible(!fw.is_empty());
        self.firmware.set_label(&fw);
        let seen = match (connected, watch.last_connected) {
            (true, _) => String::new(),
            (false, Some(t)) => format!("Last connected {}", ago(t)),
            (false, None) => "Never connected".to_string(),
        };
        self.seen.set_visible(!seen.is_empty());
        self.seen.set_label(&seen);

        let (busy, text, show_retry) = match &link {
            LinkState::Connecting(s) => (true, stage_text(*s).to_string(), false),
            LinkState::Negotiating => (true, "Starting the session…".to_string(), false),
            LinkState::Disconnected { error, retry_in } => {
                let mut t = error.clone().map(|e| sentence(&e)).unwrap_or_else(|| "Not connected.".to_string());
                if let Some(d) = retry_in {
                    let mins = d.as_secs().div_ceil(60);
                    t.push_str(&format!("\nTrying again in {mins} min."));
                }
                (false, t, true)
            }
            LinkState::NoWatch | LinkState::Connected => (false, String::new(), false),
        };
        self.progress_box.set_visible(busy || show_retry);
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.progress.set_label(&text);
        self.retry.set_visible(show_retry);
        self.refresh_update(state, connected, recovery);
        let update_shown = self.update_box.is_visible();
        let notice =
            if recovery && !update_shown { "The watch is running its recovery firmware; most features need PebbleOS." } else { "" };
        self.notice.set_visible(!notice.is_empty());
        self.notice.set_label(notice);
    }
}

impl WatchPage {
    /// The tab's navigation stack, for the calendar and weather pages.
    pub fn set_nav(self: &Rc<Self>, state: &Rc<State>, nav: &adw::NavigationView) {
        let _ = self.nav.set(nav.clone());
        self.refresh_data(state);
        let (page, state) = (Rc::downgrade(self), state.clone());
        nav.connect_popped(move |_, _| {
            if let Some(p) = page.upgrade() {
                p.refresh_data(&state);
            }
        });
    }

    fn refresh_data(&self, state: &State) {
        self.calendars.set_subtitle(&phone_data::calendars_summary(state));
        self.weather.set_subtitle(&glib::markup_escape_text(&phone_data::weather_summary(state)));
    }

    fn refresh_update(&self, state: &State, connected: bool, recovery: bool) {
        let update = state.firmware.borrow().clone();
        let what = firmware::what(state);
        // title, detail, button, progress, spinning
        let shown: Option<UpdateRow> = match &update {
            Update::Available { tag, size, missing: true } if connected => Some((
                "PebbleOS Is Not Installed".to_string(),
                format!("The watch has only its recovery firmware. PebbleOS {} is {}.", firmware::version(tag), firmware::size(*size)),
                Some(("Install", UpdateAction::Install)),
                None,
                false,
            )),
            Update::Available { tag, size, missing: false } if connected => Some((
                format!("PebbleOS {} Is Available", firmware::version(tag)),
                firmware::size(*size),
                Some(("Update", UpdateAction::Install)),
                None,
                false,
            )),
            Update::CheckFailed(e) if connected && recovery => {
                Some(("Cannot Look for PebbleOS".to_string(), sentence(e), Some(("Try Again", UpdateAction::Check)), None, false))
            }
            Update::Checking if connected && recovery => Some(("Looking for PebbleOS…".to_string(), String::new(), None, None, true)),
            Update::Downloading(f) => Some((format!("Downloading {what}…"), percent(*f), None, Some(*f), false)),
            Update::Installing(f) => {
                Some(("Installing on the Watch…".to_string(), format!("{} · Keep the watch close", percent(*f)), None, Some(*f), false))
            }
            Update::Restarting { into_recovery } => Some((
                "The Watch Is Restarting".to_string(),
                if *into_recovery {
                    "The update continues once the watch is in its recovery firmware.".to_string()
                } else if what == "PebbleOS" {
                    "PebbleOS starts in a minute or so.".to_string()
                } else {
                    format!("{what} is put in place while it starts.")
                },
                None,
                None,
                true,
            )),
            Update::Failed(e) => Some((
                "The Update Failed".to_string(),
                sentence(e),
                connected.then_some((
                    "Try Again",
                    if state.last_install.borrow().is_some() { UpdateAction::Retry } else { UpdateAction::Check },
                )),
                None,
                false,
            )),
            _ => None,
        };
        let Some((title, detail, button, progress, spinning)) = shown else {
            self.update_box.set_visible(false);
            self.update_spinner.set_spinning(false);
            return;
        };
        self.update_box.set_visible(true);
        self.update_title.set_label(&title);
        self.update_detail.set_visible(!detail.is_empty());
        self.update_detail.set_label(&detail);
        self.update_spinner.set_visible(spinning);
        self.update_spinner.set_spinning(spinning);
        self.update_bar.set_visible(progress.is_some());
        self.update_bar.set_fraction(progress.unwrap_or(0.0));
        match button {
            Some((label, action)) => {
                self.update_button.set_visible(true);
                self.update_button.set_label(label);
                self.update_action.set(action);
            }
            None => self.update_button.set_visible(false),
        }
    }
}

fn on_update_clicked(state: &Rc<State>, button: &gtk::Button, action: UpdateAction) {
    match action {
        UpdateAction::Install => {
            let Update::Available { tag, size, missing } = state.firmware.borrow().clone() else { return };
            let heading = if missing { "Install PebbleOS?" } else { "Update PebbleOS?" };
            let body = format!(
                "Lithidion downloads PebbleOS {} ({}) from its releases on GitHub and sends it to the watch. {}",
                firmware::version(&tag),
                firmware::size(size),
                firmware::WHAT_HAPPENS
            );
            firmware::confirm(state, button, heading, &body, if missing { "Install" } else { "Update" }, FirmwareSource::Latest);
        }
        UpdateAction::Retry => {
            // Bound first: install() borrows `last_install` mutably.
            let last = state.last_install.borrow().clone();
            if let Some(source) = last {
                firmware::install(state, source);
            }
        }
        UpdateAction::Check => state.send(Command::CheckFirmware),
    }
}

fn percent(f: f64) -> String {
    format!("{} %", (f * 100.0).floor() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Watch tab of a connected Time 2 with a settings database, saved to `/tmp/watch-tab.png`:
    /// `xvfb-run -s "-screen 0 1280x2400x24" cargo test -- --ignored watch_tab_layout`.
    #[test]
    #[ignore]
    fn watch_tab_layout() {
        adw::init().unwrap();
        let (tx, rx) = async_channel::unbounded();
        let state = State::new(tx);
        let watch = crate::store::Watch {
            id: 1,
            address: "24:A3:0A:D4:7B:24".into(),
            model: Some(36),
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
        let page = WatchPage::new(&state, || {}, || {});
        state.apply(&Event::Watches { watches: vec![watch], active: Some(1) });
        // Asked for the settings of the watch it shows.
        assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|c| matches!(c, Command::WatchPrefs(1))));
        assert!(!page.on_watch.is_visible());
        let view = crate::backend::watch_prefs::View { watch: 1, supported: true, ..Default::default() };
        state.apply(&Event::WatchPrefs(Box::new(view)));
        assert!(page.on_watch.is_visible());
        let window = adw::Window::builder().default_width(420).default_height(1400).content(&page.widget).build();
        super::super::add_icons(&WidgetExt::display(&window));
        window.present();
        let ctx = glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_millis(800);
        while std::time::Instant::now() < end {
            ctx.iteration(false);
        }
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        let texture = window.native().unwrap().renderer().unwrap().render_texture(snapshot.to_node().unwrap(), None);
        texture.save_to_png("/tmp/watch-tab.png").unwrap();
    }
}
