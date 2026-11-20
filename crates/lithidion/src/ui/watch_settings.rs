//! "On the Watch": the watch's own settings, in the sections of its Settings app. Changes are
//! sent at once when the watch is connected and wait for it otherwise.

use super::state::State;
use crate::backend::watch_prefs::View;
use crate::backend::{Command, Event};
use crate::settings::Clock;
use crate::store::WatchId;
use adw::prelude::*;
use pebble_protocol::hardware::AppPlatform;
use pebble_protocol::watchprefs::{self, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use uuid::Uuid;

/// The watch's defaults, for settings it has never reported (it stores only changed ones).
fn default(key: &str) -> Value {
    use Value::*;
    let off = QuickLaunch { enabled: false, app: None };
    let on = |u: &str| QuickLaunch { enabled: true, app: Uuid::parse_str(u).ok() };
    match key {
        "stationaryMode"
        | "lightEnabled"
        | "lightAmbientSensorEnabled"
        | "lightMotion"
        | "timelineQuickViewEnabled"
        | "musicShowVolumeControls"
        | "musicShowProgressBar"
        | "notifVibeDelay"
        | "notifBacklight"
        | "dndMotionBacklight"
        | "dndTouchBacklight"
        | "dndNotificationBacklight" => Bool(true),
        "lightTimeoutMs" => Number(3000),
        "lightIntensity" => Number(25),
        "lightDynamicMode" => Number(2),
        "lightPreset" => Number(1),
        "lightColor" | "lightColorNight" => Number(0xFFBFA2),
        "lightColorSunriseMinute" => Number(6 * 60),
        "lightColorSunsetMinute" => Number(18 * 60),
        "timelineQuickViewBeforeTimeMin" => Number(10),
        "mask" => Number(15),
        "dndShowNotifications" => Number(1),
        "vibeIntensity" => Number(2),
        "vibeScoreNotifications" => Number(9),
        "vibeScoreIncomingCalls" => Number(8),
        "vibeScoreAlarms" => Number(11),
        "notifWindowTimeout" => Number(180_000),
        "dndWeekdaySchedule" | "dndWeekendSchedule" => Schedule { from: (0, 0), to: (6, 0) },
        "qlBack" => on("2220d805-cf9a-4e12-92b9-5ca778aff6bb"),
        "qlSingleClickUp" => on("36d8c6ed-4c83-4fa1-a9e2-8f12dc941f8c"),
        "qlSingleClickDown" => on("79c76b48-6111-4e80-8deb-3119eebef33e"),
        k if k.starts_with("ql") => off,
        k if watchprefs::find(k).is_some_and(|p| p.kind == watchprefs::Kind::Bool) => Bool(false),
        _ => Number(0),
    }
}

type Updater = Box<dyn Fn(&Binder)>;

/// Binds controls to settings: each control sends its changes, and every update from the backend
/// sets the controls again without echoing them back.
struct Binder {
    state: Rc<State>,
    watch: WatchId,
    platform: Option<AppPlatform>,
    view: RefCell<View>,
    quiet: Cell<bool>,
    updaters: RefCell<Vec<Updater>>,
}

impl Binder {
    fn new(state: &Rc<State>, view: View) -> Rc<Binder> {
        let binder = Rc::new(Binder {
            state: state.clone(),
            watch: view.watch,
            platform: view.platform,
            view: RefCell::new(view),
            quiet: Cell::new(false),
            updaters: RefCell::default(),
        });
        let weak = Rc::downgrade(&binder);
        state.listen(move |e| {
            let Some(b) = weak.upgrade() else { return false };
            if let Event::WatchPrefs(v) = e {
                if v.watch == b.watch {
                    *b.view.borrow_mut() = (**v).clone();
                    b.refresh();
                }
            }
            true
        });
        binder
    }

    fn get(&self, key: &str) -> Value {
        let kind = watchprefs::find(key).map(|p| p.kind);
        let stored = self.view.borrow().values.get(key).cloned();
        kind.zip(stored).and_then(|(k, v)| watchprefs::decode(k, &v)).unwrap_or_else(|| match key {
            // Default size, numbered from where this platform starts.
            watchprefs::SYSTEM_TEXT_SIZE | watchprefs::NOTIFICATION_TEXT_SIZE => {
                Value::Number(1 + self.platform.map(watchprefs::text_size_offset).unwrap_or(0) as u32)
            }
            _ => default(key),
        })
    }

    fn number(&self, key: &str) -> u32 {
        match self.get(key) {
            Value::Number(n) => n,
            Value::Bool(b) => b as u32,
            _ => 0,
        }
    }

    fn set(&self, key: &str, value: Value) {
        if !self.quiet.get() && self.get(key) != value {
            self.state.send(Command::SetWatchPref(self.watch, key.to_string(), value));
        }
    }

    fn refresh(&self) {
        self.quiet.set(true);
        for u in self.updaters.borrow().iter() {
            u(self);
        }
        self.quiet.set(false);
    }

    fn on_update(&self, f: impl Fn(&Binder) + 'static) {
        self.quiet.set(true);
        f(self);
        self.quiet.set(false);
        self.updaters.borrow_mut().push(Box::new(f));
    }

    /// Whether this watch has the setting.
    fn shows(&self, key: &str) -> bool {
        let Some(pref) = watchprefs::find(key) else { return false };
        match self.platform {
            Some(p) => watchprefs::has(p, pref.needs),
            None => pref.needs == watchprefs::Needs::Nothing,
        }
    }

    fn switch(self: &Rc<Self>, group: &adw::PreferencesGroup, key: &'static str, title: &str) -> Option<adw::SwitchRow> {
        if !self.shows(key) {
            return None;
        }
        let row = adw::SwitchRow::builder().title(title).build();
        let weak = Rc::downgrade(self);
        row.connect_active_notify(move |r| {
            if let Some(b) = weak.upgrade() {
                b.set(key, Value::Bool(r.is_active()));
            }
        });
        let r = row.clone();
        self.on_update(move |b| r.set_active(matches!(b.get(key), Value::Bool(true))));
        group.add(&row);
        Some(row)
    }

    /// A list of numbered choices; a value the watch has that is not listed is shown as is.
    fn choice(
        self: &Rc<Self>,
        group: &adw::PreferencesGroup,
        key: &'static str,
        title: &str,
        options: &[(u32, &str)],
    ) -> Option<adw::ComboRow> {
        if !self.shows(key) {
            return None;
        }
        let codes: Vec<u32> = options.iter().map(|(c, _)| *c).collect();
        let names: Vec<&str> = options.iter().map(|(_, n)| *n).collect();
        let row = adw::ComboRow::builder().title(title).model(&gtk::StringList::new(&names)).build();
        {
            let (weak, codes) = (Rc::downgrade(self), codes.clone());
            row.connect_selected_notify(move |r| {
                if let (Some(b), Some(code)) = (weak.upgrade(), codes.get(r.selected() as usize)) {
                    b.set(key, Value::Number(*code));
                }
            });
        }
        let r = row.clone();
        self.on_update(move |b| {
            let n = b.number(key);
            if let Some(i) = codes.iter().position(|c| *c == n) {
                r.set_selected(i as u32);
            }
        });
        group.add(&row);
        Some(row)
    }

    /// A number shown in its own unit: the watch's value is `shown × scale`.
    fn spin(
        self: &Rc<Self>,
        group: &adw::PreferencesGroup,
        key: &'static str,
        title: &str,
        (min, max, step): (f64, f64, f64),
        scale: u32,
    ) -> Option<adw::SpinRow> {
        if !self.shows(key) {
            return None;
        }
        let row = adw::SpinRow::builder().title(title).adjustment(&gtk::Adjustment::new(min, min, max, step, step, 0.0)).build();
        let weak = Rc::downgrade(self);
        row.connect_value_notify(move |r| {
            if let Some(b) = weak.upgrade() {
                b.set(key, Value::Number(r.value().round() as u32 * scale));
            }
        });
        let r = row.clone();
        self.on_update(move |b| r.set_value((b.number(key) / scale) as f64));
        group.add(&row);
        Some(row)
    }

    /// A time of day, kept as minutes after midnight or as one end of a schedule.
    fn time(self: &Rc<Self>, group: &adw::PreferencesGroup, key: &'static str, title: &str, end: Option<bool>) -> Option<adw::ActionRow> {
        if !self.shows(key) {
            return None;
        }
        let row = adw::ActionRow::builder().title(title).build();
        let spin = |max: f64| {
            let s = gtk::SpinButton::with_range(0.0, max, 1.0);
            s.set_valign(gtk::Align::Center);
            s.set_wrap(true);
            s.set_numeric(true);
            s.connect_output(|s| {
                s.set_text(&format!("{:02}", s.value() as u32));
                glib::Propagation::Stop
            });
            s
        };
        let (hours, minutes) = (spin(23.0), spin(59.0));
        let suffix = gtk::Box::builder().spacing(4).build();
        suffix.append(&hours);
        suffix.append(&gtk::Label::new(Some(":")));
        suffix.append(&minutes);
        row.add_suffix(&suffix);
        let read = move |b: &Binder| -> (u8, u8) {
            match (b.get(key), end) {
                (Value::Schedule { from, .. }, Some(false)) => from,
                (Value::Schedule { to, .. }, Some(true)) => to,
                (Value::Number(m), None) => ((m / 60) as u8, (m % 60) as u8),
                _ => (0, 0),
            }
        };
        let write = {
            let (weak, hours, minutes) = (Rc::downgrade(self), hours.clone(), minutes.clone());
            move || {
                let Some(b) = weak.upgrade() else { return };
                let t = (hours.value() as u8, minutes.value() as u8);
                let value = match (b.get(key), end) {
                    (Value::Schedule { to, .. }, Some(false)) => Value::Schedule { from: t, to },
                    (Value::Schedule { from, .. }, Some(true)) => Value::Schedule { from, to: t },
                    _ => Value::Number(t.0 as u32 * 60 + t.1 as u32),
                };
                b.set(key, value);
            }
        };
        let w = write.clone();
        hours.connect_value_changed(move |_| w());
        minutes.connect_value_changed(move |_| write());
        self.on_update(move |b| {
            let (h, m) = read(b);
            hours.set_value(h as f64);
            minutes.set_value(m as f64);
        });
        group.add(&row);
        Some(row)
    }

    fn color(self: &Rc<Self>, group: &adw::PreferencesGroup, key: &'static str, title: &str) -> Option<adw::ActionRow> {
        if !self.shows(key) {
            return None;
        }
        let row = adw::ActionRow::builder().title(title).build();
        let button = gtk::ColorDialogButton::builder()
            .dialog(&gtk::ColorDialog::builder().with_alpha(false).build())
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&button);
        let weak = Rc::downgrade(self);
        button.connect_rgba_notify(move |btn| {
            let c = btn.rgba();
            let byte = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u32;
            if let Some(b) = weak.upgrade() {
                b.set(key, Value::Number(byte(c.red()) << 16 | byte(c.green()) << 8 | byte(c.blue())));
            }
        });
        self.on_update(move |b| {
            let n = b.number(key);
            let part = |shift: u32| ((n >> shift) & 0xFF) as f32 / 255.0;
            button.set_rgba(&gtk::gdk::RGBA::new(part(16), part(8), part(0), 1.0));
        });
        group.add(&row);
        Some(row)
    }

    /// Shows `rows` only while `key` satisfies `when`.
    fn reveal(&self, rows: Vec<Option<gtk::Widget>>, key: &'static str, when: fn(u32) -> bool) {
        let rows: Vec<gtk::Widget> = rows.into_iter().flatten().collect();
        self.on_update(move |b| {
            let shown = when(b.number(key));
            for r in &rows {
                r.set_visible(shown);
            }
        });
    }
}

fn group(title: Option<&str>) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::new();
    if let Some(t) = title {
        g.set_title(t);
    }
    g
}

fn page(title: &str, binder: Rc<Binder>, groups: &[adw::PreferencesGroup]) -> adw::NavigationPage {
    let p = adw::PreferencesPage::new();
    for g in groups {
        p.add(g);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&p));
    let nav = adw::NavigationPage::builder().title(title).child(&toolbar).build();
    // The page keeps its binder for as long as it is shown.
    nav.connect_hidden(move |_| {
        let _ = &binder;
    });
    nav
}

fn w(row: Option<impl IsA<gtk::Widget>>) -> Option<gtk::Widget> {
    row.map(|r| r.upcast())
}

/// Text sizes as offered; the watch's numbers are `code + offset` on some platforms.
fn text_sizes(b: &Binder, same_as_system: bool) -> Vec<(u32, &'static str)> {
    let offset = b.platform.map(watchprefs::text_size_offset).unwrap_or(0) as u32;
    let mut sizes = vec![(offset, "Smaller"), (1 + offset, "Default"), (2 + offset, "Larger")];
    if same_as_system {
        sizes.push((4, "Same as System"));
    }
    sizes
}

fn display(state: &Rc<State>, view: View) -> adw::NavigationPage {
    let b = Binder::new(state, view);
    let general = group(None);
    b.choice(&general, watchprefs::SYSTEM_TEXT_SIZE, "Text Size", &text_sizes(&b, false));
    b.switch(&general, "displayOrientationLeftHanded", "Left-Handed");
    b.switch(&general, "stationaryMode", "Standby Mode");

    let light = group(Some("Backlight"));
    b.switch(&light, "lightEnabled", "Backlight");
    let preset = b.choice(&light, "lightPreset", "Mode", &[(1, "Standard"), (0, "Max Brightness"), (2, "Battery Saver"), (3, "Custom")]);
    let motion = b.switch(&light, "lightMotion", "Motion");
    let touch = b.choice(&light, "lightTouch", "Tap", &[(0, "Double Tap"), (1, "Tap"), (2, "Off")]);
    let custom = group(Some("Custom Backlight"));
    let intensity = b.choice(&custom, "lightIntensity", "Brightness", &[(10, "Low"), (25, "Medium"), (50, "High"), (100, "Blinding")]);
    let timeout = b.spin(&custom, "lightTimeoutMs", "Timeout (Seconds)", (1.0, 10.0, 1.0), 1000);
    let ambient = b.switch(&custom, "lightAmbientSensorEnabled", "Only in the Dark");
    let dynamic = b.choice(&custom, "lightDynamicMode", "Dynamic", &[(0, "Off"), (1, "Bright"), (2, "Standard"), (3, "Dim")]);
    b.reveal(vec![w(intensity), w(timeout), w(ambient), w(dynamic)], "lightPreset", |p| p == 3);
    b.reveal(vec![Some(custom.clone().upcast())], "lightPreset", |p| p == 3);

    let color = group(Some("Backlight Color"));
    b.color(&color, "lightColor", "Color");
    b.switch(&color, "lightColorDayNightEnabled", "Night Color");
    let night = b.color(&color, "lightColorNight", "At Night");
    let sunset = b.time(&color, "lightColorSunsetMinute", "From", None);
    let sunrise = b.time(&color, "lightColorSunriseMinute", "Until", None);
    b.reveal(vec![w(night), w(sunset), w(sunrise)], "lightColorDayNightEnabled", |on| on == 1);
    color.set_visible(b.shows("lightColor"));
    b.reveal(vec![w(preset), w(motion), w(touch)], "lightEnabled", |on| on == 1);
    page("Display", b, &[general, light, custom, color])
}

fn time_and_timeline(state: &Rc<State>, view: View) -> adw::NavigationPage {
    let b = Binder::new(state, view);
    let time = group(None);
    let clock = adw::ComboRow::builder().title("Clock").model(&gtk::StringList::new(&["System", "12-Hour", "24-Hour"])).build();
    {
        let weak = Rc::downgrade(&b);
        clock.connect_selected_notify(move |r| {
            let Some(b) = weak.upgrade() else { return };
            let choice = [Clock::System, Clock::Twelve, Clock::TwentyFour][r.selected().min(2) as usize];
            if !b.quiet.get() && b.view.borrow().clock != choice {
                b.state.send(Command::SetWatchClock(b.watch, choice));
            }
        });
        let c = clock.clone();
        b.on_update(move |b| {
            let i = match b.view.borrow().clock {
                Clock::System => 0,
                Clock::Twelve => 1,
                Clock::TwentyFour => 2,
            };
            c.set_selected(i);
        });
    }
    time.add(&clock);
    b.switch(&time, "timezoneSource", "Manual Time Zone");

    let quick = group(Some("Quick View"));
    b.switch(&quick, "timelineQuickViewEnabled", "Quick View");
    let before = b.spin(&quick, "timelineQuickViewBeforeTimeMin", "Minutes Before", (0.0, 30.0, 1.0), 1);
    let fit = b.choice(&quick, "timelineQuickViewWatchfaceFit", "Other Watchfaces", &[(0, "Overlay"), (2, "Shift Up"), (1, "Squish Up")]);
    b.reveal(vec![w(before), w(fit)], "timelineQuickViewEnabled", |on| on == 1);
    page("Time & Timeline", b, &[time, quick])
}

fn quick_launch(state: &Rc<State>, view: View) -> adw::NavigationPage {
    let b = Binder::new(state, view);
    // Off, the system's own apps, then the watch's installed apps.
    let mut choices: Vec<(Option<Uuid>, String)> = vec![(None, "Off".to_string())];
    let touch = b.platform.is_some_and(|p| watchprefs::has(p, watchprefs::Needs::Touch));
    for (id, name) in pebble_protocol::apps::SYSTEM_APPS {
        let skip = ["System", "Watchfaces", "Quick Launch (Nothing)", "Phone Notifications", "TicToc", "Kickstart"];
        if skip.contains(&name) || (name == "Touch" && !touch) {
            continue;
        }
        choices.push((Uuid::parse_str(id).ok(), name.to_string()));
    }
    choices[1..].sort_by(|a, b| a.1.cmp(&b.1));
    for a in state.apps.borrow().iter() {
        if a.app.kind == crate::store::AppKind::Watchapp && !a.app.hidden {
            choices.push((Some(a.app.uuid), a.app.title.clone()));
        }
    }
    let choices = Rc::new(choices);
    let names: Vec<&str> = choices.iter().map(|(_, n)| n.as_str()).collect();

    let groups: Vec<adw::PreferencesGroup> = [
        (Some("Hold"), &[("qlUp", "Up"), ("qlSelect", "Select"), ("qlDown", "Down"), ("qlBack", "Back")][..]),
        (Some("Tap"), &[("qlSingleClickUp", "Up"), ("qlSingleClickDown", "Down")][..]),
        (Some("Hold Two Buttons"), &[("qlComboBackUp", "Back and Up"), ("qlComboUpDown", "Up and Down")][..]),
    ]
    .into_iter()
    .map(|(title, keys)| {
        let g = group(title);
        for &(key, label) in keys {
            let row = adw::ComboRow::builder().title(label).model(&gtk::StringList::new(&names)).build();
            {
                let (weak, choices) = (Rc::downgrade(&b), choices.clone());
                row.connect_selected_notify(move |r| {
                    let (Some(b), Some((app, _))) = (weak.upgrade(), choices.get(r.selected() as usize)) else { return };
                    b.set(key, Value::QuickLaunch { enabled: app.is_some(), app: *app });
                });
            }
            let (r, choices) = (row.clone(), choices.clone());
            b.on_update(move |b| {
                let i = match b.get(key) {
                    Value::QuickLaunch { enabled: true, app } => choices.iter().position(|(a, _)| *a == app).unwrap_or(0),
                    _ => 0,
                };
                r.set_selected(i as u32);
            });
            g.add(&row);
        }
        g
    })
    .collect();
    page("Quick Launch", b, &groups)
}

fn quiet_time(state: &Rc<State>, view: View) -> adw::NavigationPage {
    let b = Binder::new(state, view);
    let general = group(None);
    b.switch(&general, "dndManuallyEnabled", "Quiet Time");
    b.switch(&general, "dndSmartEnabled", "During Calendar Events");
    let schedules: Vec<adw::PreferencesGroup> =
        [("Weekdays", "dndWeekdayScheduleEnabled", "dndWeekdaySchedule"), ("Weekends", "dndWeekendScheduleEnabled", "dndWeekendSchedule")]
            .into_iter()
            .map(|(title, enabled, schedule)| {
                let g = group(Some(title));
                b.switch(&g, enabled, "Scheduled");
                let from = b.time(&g, schedule, "From", Some(false));
                let to = b.time(&g, schedule, "Until", Some(true));
                b.reveal(vec![w(from), w(to)], enabled, |on| on == 1);
                g
            })
            .collect();
    let during = group(Some("During Quiet Time"));
    b.choice(&during, "dndInterruptionsMask", "Interruptions", &[(0, "None"), (2, "Phone Calls")]);
    b.choice(&during, "dndShowNotifications", "Notifications", &[(1, "Show"), (0, "Hide")]);
    b.switch(&during, "dndAutoDismiss", "Dismiss Automatically");
    b.switch(&during, "dndMotionBacklight", "Motion Backlight");
    if b.platform.is_some_and(|p| watchprefs::has(p, watchprefs::Needs::Touch)) {
        b.switch(&during, "dndTouchBacklight", "Tap Backlight");
    }
    b.switch(&during, "dndNotificationBacklight", "Notification Backlight");
    let mut groups = vec![general];
    groups.extend(schedules);
    groups.push(during);
    page("Quiet Time", b, &groups)
}

fn notifications(state: &Rc<State>, view: View) -> adw::NavigationPage {
    let b = Binder::new(state, view);
    let g = group(None);
    b.choice(&g, "mask", "Show", &[(15, "All"), (2, "Phone Calls Only"), (0, "None")]);
    b.choice(
        &g,
        "notifWindowTimeout",
        "Timeout",
        &[
            (15_000, "15 Seconds"),
            (30_000, "30 Seconds"),
            (60_000, "1 Minute"),
            (180_000, "3 Minutes"),
            (300_000, "5 Minutes"),
            (600_000, "10 Minutes"),
        ],
    );
    b.choice(&g, watchprefs::NOTIFICATION_TEXT_SIZE, "Text Size", &text_sizes(&b, true));
    b.switch(&g, "notifVibeDelay", "Vibrate After Animation");
    b.switch(&g, "notifBacklight", "Backlight");
    // The alternative banner is drawn only on black-and-white watches.
    if b.platform.is_some_and(|p| matches!(p, AppPlatform::Aplite | AppPlatform::Diorite | AppPlatform::Flint)) {
        b.switch(&g, "notifDesignStyle", "Alternative Style");
    }
    b.switch(&g, "notifDismissalSync", "Remove Dismissed from History");
    page("Notifications", b, &[g])
}

fn vibration(state: &Rc<State>, view: View) -> adw::NavigationPage {
    let b = Binder::new(state, view);
    let common: &[(u32, &str)] = &[
        (8, "Pulse"),
        (9, "Nudge Nudge"),
        (10, "Jackhammer"),
        (12, "Mario"),
        (15, "Double Pulse"),
        (16, "Pebble Morse"),
        (17, "Heartbeat"),
        (18, "Double Tap"),
        (19, "Wave"),
        (20, "Imperial"),
    ];
    let with = |first: &[(u32, &'static str)]| -> Vec<(u32, &'static str)> { first.iter().chain(common).copied().collect() };
    let g = group(Some("Patterns"));
    b.choice(&g, "vibeScoreNotifications", "Notifications", &with(&[(1, "Off"), (2, "Standard Low"), (4, "Standard High")]));
    b.choice(&g, "vibeScoreIncomingCalls", "Calls", &with(&[(1, "Off"), (3, "Standard Low"), (5, "Standard High")]));
    b.choice(&g, "vibeScoreAlarms", "Alarms", &with(&[(11, "Reveille"), (14, "Gentle"), (3, "Standard Low"), (5, "Standard High")]));
    let other = group(None);
    b.choice(&other, "vibeIntensity", "Intensity", &[(0, "Low"), (1, "Medium"), (2, "High")]);
    b.choice(&other, "menuScrollVibeBehavior", "Menu Scrolling", &[(0, "Off"), (1, "On Wrap Around"), (2, "At the Ends")]);
    page("Vibration", b, &[g, other])
}

fn menus_and_music(state: &Rc<State>, view: View) -> adw::NavigationPage {
    let b = Binder::new(state, view);
    let menus = group(Some("Menus"));
    b.switch(&menus, "menuScrollWrapAround", "Wrap Around");
    let music = group(Some("Music"));
    b.switch(&music, "musicShowVolumeControls", "Volume Controls");
    b.switch(&music, "musicShowProgressBar", "Progress Bar");
    page("Menus & Music", b, &[menus, music])
}

type Builder = fn(&Rc<State>, View) -> adw::NavigationPage;

/// The group on the Watch tab; hidden for watches without a settings database.
pub fn group_for(state: &Rc<State>, nav: impl Fn() -> Option<adw::NavigationView> + 'static) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title("On the Watch").build();
    let nav = Rc::new(nav);
    let sections: [(&str, Builder); 7] = [
        ("Display", display),
        ("Time & Timeline", time_and_timeline),
        ("Quick Launch", quick_launch),
        ("Quiet Time", quiet_time),
        ("Notifications", notifications),
        ("Vibration", vibration),
        ("Menus & Music", menus_and_music),
    ];
    for (title, build) in sections {
        let row = adw::ActionRow::builder().title(title).use_markup(false).activatable(true).build();
        row.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
        let (state, nav) = (state.clone(), nav.clone());
        row.connect_activated(move |_| {
            let view = state.watch_prefs.borrow().as_deref().cloned();
            if let (Some(view), Some(nav)) = (view, nav()) {
                nav.push(&build(&state, view));
            }
        });
        g.add(&row);
    }
    g
}

/// Shows the group for the active watch, asking for its settings when they are not yet known.
pub fn update(state: &State, g: &adw::PreferencesGroup) {
    let active = state.active.get();
    let view = state.watch_prefs.borrow();
    match (active, view.as_deref()) {
        (Some(id), Some(v)) if v.watch == id => g.set_visible(v.supported),
        (Some(id), _) => {
            g.set_visible(false);
            state.send(Command::WatchPrefs(id));
        }
        (None, _) => g.set_visible(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders every page for a Time 2 with a few settings changed:
    /// `xvfb-run -s "-screen 0 1280x2400x24" cargo test -- --ignored watch_settings_layout`.
    /// The pages are saved to `/tmp/watch-settings-<n>.png`.
    #[test]
    #[ignore]
    fn watch_settings_layout() {
        adw::init().unwrap();
        let (tx, rx) = async_channel::unbounded();
        let state = State::new(tx);
        let mut view = View { watch: 1, supported: true, platform: Some(AppPlatform::Emery), ..Default::default() };
        view.values.insert("lightPreset".into(), vec![3]);
        view.values.insert("lightColorDayNightEnabled".into(), vec![1]);
        view.values.insert("dndWeekdayScheduleEnabled".into(), vec![1]);
        view.values.insert("dndWeekdaySchedule".into(), vec![22, 0, 7, 30]);
        view.values.insert(
            "qlUp".into(),
            watchprefs::encode(
                watchprefs::Kind::QuickLaunch,
                Value::QuickLaunch { enabled: true, app: Uuid::parse_str("1f03293d-47af-4f28-b960-f2b02a6dd757").ok() },
            )
            .unwrap(),
        );
        let builders: [Builder; 7] = [display, time_and_timeline, quick_launch, quiet_time, notifications, vibration, menus_and_music];
        for (n, build) in builders.iter().enumerate() {
            let page = build(&state, view.clone());
            let nav = adw::NavigationView::new();
            nav.push(&page);
            let window = adw::Window::builder().default_width(420).default_height(1200).content(&nav).build();
            super::super::add_icons(&gtk::prelude::WidgetExt::display(&window));
            window.present();
            let ctx = glib::MainContext::default();
            let end = std::time::Instant::now() + std::time::Duration::from_millis(800);
            while std::time::Instant::now() < end {
                ctx.iteration(false);
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
            if let Some(node) = snapshot.to_node() {
                let texture = window.native().unwrap().renderer().unwrap().render_texture(&node, None);
                texture.save_to_png(format!("/tmp/watch-settings-{n}.png")).unwrap();
            }
            window.close();
        }
        // Showing the pages sends nothing back.
        assert!(rx.try_recv().is_err(), "a page sent a change while being built");
        // A change is sent once.
        let b = Binder::new(&state, view);
        b.set("lightMotion", Value::Bool(false));
        assert!(matches!(rx.try_recv(), Ok(Command::SetWatchPref(1, k, Value::Bool(false))) if k == "lightMotion"));
    }
}
