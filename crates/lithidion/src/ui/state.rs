//! What the UI knows about the backend, and listeners for windows that come and go.

use crate::backend::{AppProgress, Command, Event, FirmwareSource, LinkState, SessionInfo, Stage, Update};
use crate::settings::Settings;
use crate::store::{NotifyApp, NotifyEntry, WatchApp};
use crate::store::{Watch, WatchId};
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

type Listener = Box<dyn Fn(&Event) -> bool>;

pub struct State {
    pub commands: async_channel::Sender<Command>,
    pub settings: RefCell<Settings>,
    /// Settings as the backend last sent them.
    pub prefs: RefCell<Option<Box<crate::settings::View>>>,
    pub dictation: RefCell<Option<crate::backend::dictation::State>>,
    pub watches: RefCell<Vec<Watch>>,
    pub active: Cell<Option<WatchId>>,
    pub link: RefCell<LinkState>,
    pub battery: Cell<Option<u8>>,
    pub session: RefCell<Option<SessionInfo>>,
    pub firmware: RefCell<Update>,
    /// The last install asked for, for trying again.
    pub last_install: RefCell<Option<FirmwareSource>>,
    /// The active watch's apps, in order.
    pub apps: RefCell<Vec<WatchApp>>,
    /// Apps being downloaded or sent, by UUID.
    pub app_progress: RefCell<HashMap<uuid::Uuid, AppProgress>>,
    pub notifications_enabled: Cell<bool>,
    pub phone_quiet: Cell<bool>,
    pub notify_apps: RefCell<Vec<NotifyApp>>,
    pub notify_history: RefCell<Vec<NotifyEntry>>,
    /// The active profile's health summary.
    pub health: RefCell<Option<Box<crate::backend::health::View>>>,
    /// The active watch's own settings.
    pub watch_prefs: RefCell<Option<Box<crate::backend::watch_prefs::View>>>,
    pub profiles: RefCell<Vec<crate::backend::ProfileInfo>>,
    pub profile: Cell<Option<crate::store::ProfileId>>,
    /// Preferences are open, and show dictation messages themselves.
    pub preferences_open: Cell<bool>,
    /// Called for every event; a listener returning `false` is dropped.
    listeners: RefCell<Vec<Listener>>,
}

impl State {
    pub fn new(commands: async_channel::Sender<Command>) -> Rc<State> {
        Rc::new(State {
            commands,
            settings: RefCell::default(),
            prefs: RefCell::default(),
            dictation: RefCell::default(),
            watches: RefCell::default(),
            active: Cell::new(None),
            link: RefCell::new(LinkState::NoWatch),
            battery: Cell::new(None),
            session: RefCell::default(),
            firmware: RefCell::new(Update::Unknown),
            last_install: RefCell::default(),
            apps: RefCell::default(),
            app_progress: RefCell::default(),
            notifications_enabled: Cell::new(true),
            phone_quiet: Cell::new(false),
            notify_apps: RefCell::default(),
            notify_history: RefCell::default(),
            health: RefCell::default(),
            watch_prefs: RefCell::default(),
            profiles: RefCell::default(),
            profile: Cell::new(None),
            preferences_open: Cell::new(false),
            listeners: RefCell::default(),
        })
    }

    pub fn send(&self, c: Command) {
        let _ = self.commands.try_send(c);
    }

    pub fn listen(&self, f: impl Fn(&Event) -> bool + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    pub fn apply(&self, e: &Event) {
        match e {
            Event::Watches { watches, active } => {
                *self.watches.borrow_mut() = watches.clone();
                self.active.set(*active);
            }
            Event::Link(l) => *self.link.borrow_mut() = l.clone(),
            Event::Battery(b) => self.battery.set(*b),
            Event::Settings(v) => {
                crate::units::use_units(v.units);
                *self.settings.borrow_mut() = v.general.clone();
                *self.prefs.borrow_mut() = Some(v.clone());
            }
            Event::Dictation(d) => *self.dictation.borrow_mut() = Some(d.clone()),
            Event::Session(s) => *self.session.borrow_mut() = s.clone(),
            Event::Firmware(u) => *self.firmware.borrow_mut() = u.clone(),
            Event::Apps(list) => *self.apps.borrow_mut() = list.clone(),
            Event::NotificationsEnabled(on) => self.notifications_enabled.set(*on),
            Event::PhoneQuiet(q) => self.phone_quiet.set(*q),
            Event::NotifyApps(list) => *self.notify_apps.borrow_mut() = list.clone(),
            Event::NotifyHistory(list) => *self.notify_history.borrow_mut() = list.clone(),
            Event::Health(v) => *self.health.borrow_mut() = v.clone(),
            Event::WatchPrefs(v) => *self.watch_prefs.borrow_mut() = Some(v.clone()),
            Event::Profiles { list, active } => {
                *self.profiles.borrow_mut() = list.clone();
                self.profile.set(*active);
            }
            Event::App(uuid, p) => {
                let mut progress = self.app_progress.borrow_mut();
                match p {
                    AppProgress::Done | AppProgress::Failed(_) => {
                        progress.remove(uuid);
                    }
                    other => {
                        progress.insert(*uuid, other.clone());
                    }
                }
            }
            _ => {}
        }
        let listeners = std::mem::take(&mut *self.listeners.borrow_mut());
        let kept: Vec<_> = listeners.into_iter().filter(|f| f(e)).collect();
        let mut current = self.listeners.borrow_mut();
        let added = std::mem::take(&mut *current);
        *current = kept;
        current.extend(added);
    }

    pub fn profile_name(&self) -> Option<String> {
        let id = self.profile.get()?;
        self.profiles.borrow().iter().find(|p| p.id == id).map(|p| p.name.clone())
    }

    pub fn active_watch(&self) -> Option<Watch> {
        let id = self.active.get()?;
        self.watches.borrow().iter().find(|w| w.id == id).cloned()
    }

    /// The active watch's app platform, once it is known.
    pub fn platform(&self) -> Option<pebble_protocol::hardware::AppPlatform> {
        let hw = self.active_watch()?.hardware?;
        pebble_protocol::hardware::HardwarePlatform::from_number(hw).app_platform
    }

    pub fn installed(&self, uuid: uuid::Uuid) -> Option<WatchApp> {
        self.apps.borrow().iter().find(|a| a.app.uuid == uuid).cloned()
    }

    pub fn is_connected(&self) -> bool {
        matches!(*self.link.borrow(), LinkState::Connected)
    }

    pub fn save_settings(&self) {
        self.send(Command::SetGeneral(self.settings.borrow().clone()));
    }

    /// A setting as the backend last sent it, or its default before it has.
    pub fn pref<T: Default>(&self, f: impl FnOnce(&crate::settings::View) -> T) -> T {
        self.prefs.borrow().as_deref().map(f).unwrap_or_default()
    }

    /// One line for the header and lists: "Connected", "Pairing…", "Not connected".
    pub fn link_text(&self) -> String {
        link_text(&self.link.borrow())
    }

    /// The watch's display name, with the end of its serial when two watches share a model.
    pub fn display_name(&self, w: &Watch) -> String {
        if let Some(n) = &w.nickname {
            return n.clone();
        }
        let name = w.name();
        let twins = self.watches.borrow().iter().filter(|o| o.name() == name).count() > 1;
        match (twins, w.short_serial()) {
            (true, Some(s)) => format!("{name} {s}"),
            _ => name,
        }
    }
}

/// Set when `dialog` closes. Listeners check it: closures holding the dialog can keep it alive
/// after it closes, so a weak reference does not tell.
pub fn closed_flag(dialog: &impl IsA<adw::Dialog>) -> Rc<Cell<bool>> {
    let closed = Rc::new(Cell::new(false));
    let c = closed.clone();
    dialog.as_ref().connect_closed(move |_| c.set(true));
    closed
}

pub fn stage_text(s: Stage) -> &'static str {
    match s {
        Stage::Connecting => "Connecting…",
        Stage::Pairing => "Pairing, confirm on the watch…",
        Stage::Opening => "Opening the connection…",
    }
}

pub fn link_text(l: &LinkState) -> String {
    match l {
        LinkState::NoWatch => "No watch".to_string(),
        LinkState::Disconnected { .. } => "Not connected".to_string(),
        LinkState::Connecting(Stage::Pairing) => "Pairing…".to_string(),
        LinkState::Connecting(_) | LinkState::Negotiating => "Connecting…".to_string(),
        LinkState::Connected => "Connected".to_string(),
    }
}

/// "3 minutes ago", "yesterday", "12 Nov".
pub fn ago(unix: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let d = (now - unix).max(0);
    match d {
        0..=59 => "just now".to_string(),
        60..=3599 => plural(d / 60, "minute"),
        3600..=86_399 => plural(d / 3600, "hour"),
        86_400..=172_799 => "yesterday".to_string(),
        _ if d < 7 * 86_400 => plural(d / 86_400, "day"),
        _ => super::locale::short_date(unix),
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

/// Capitalised, ending in a full stop.
pub fn sentence(s: &str) -> String {
    let mut c = s.chars();
    let mut out = match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}
