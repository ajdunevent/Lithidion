//! Bluetooth and storage run on their own thread with a tokio runtime; the UI talks to it over
//! two channels, since GTK objects cannot cross threads. One watch is active at a time; a
//! supervisor task keeps it connected.

mod apps;
mod bluetooth;
mod companion;

/// A bundle's UUID, title, script, and message keys.
#[cfg(any(test, feature = "test-support"))]
pub fn pbw_for_tests(path: &std::path::Path) -> (uuid::Uuid, String, String, std::collections::HashMap<String, u32>) {
    let b = pbw::Pbw::open(path).unwrap();
    (b.info.uuid, b.info.title().to_string(), b.script().unwrap().unwrap(), b.info.app_keys.clone())
}
mod desktop;
mod diagnostics;
pub mod dictation;
mod eds;
pub mod pictures;
pub use eds::calendars as list_calendars;
mod firmware;
mod gatt_server;
pub mod health;
pub use session::time_zone;
mod link;
mod music;
mod notify;
mod pbw;
mod phonedata;
mod session;
pub mod watch_prefs;
pub mod weather;

pub use link::{Stage, Transport};

use crate::appstore::StoreApp;
use crate::pkjs::{self, Out};
use crate::settings;
use crate::store::{NotifyApp, NotifyEntry, NotifyMode, OnWatch, Outcome as Delivery, Store, Watch, WatchApp, WatchDetails, WatchId};
use anyhow::{anyhow, Result};
use bluer::Address;
use link::ServerSlot;
use pebble_protocol::capabilities::Capability;
use pebble_protocol::endpoint;
use pebble_protocol::firmware::{route, Route, Running};
use pebble_protocol::hardware::{AppPlatform, HardwarePlatform};
use pebble_protocol::system::{self, Reset};
use session::{Unprompted, WatchInfo};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, Instant};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub enum Command {
    StartScan,
    StopScan,
    /// Pairs with a watch found by the scan and makes it the active one.
    Pair(String),
    Switch(WatchId),
    /// Removes the watch and its Bluetooth bond; with `true`, factory resets it first when it is
    /// the connected watch.
    Forget(WatchId, bool),
    /// A name for the watch; empty for the model's name.
    RenameWatch(WatchId, String),
    /// Asks for a watch's settings; answered with `Event::WatchPrefs`.
    WatchPrefs(WatchId),
    SetWatchPref(WatchId, String, pebble_protocol::watchprefs::Value),
    SetWatchClock(WatchId, settings::Clock),
    /// Connects the active watch now instead of waiting for the next attempt.
    Reconnect,
    Ping,
    SyncTime,
    /// Saves the watch's stored log or core dump to a file.
    SaveDiagnostic(Diagnostic, std::path::PathBuf),
    Reboot,
    RebootIntoRecovery,
    /// Looks for a newer PebbleOS now.
    CheckFirmware,
    InstallFirmware(FirmwareSource),
    /// Downloads a store app and adds it to the active watch.
    InstallApp(Box<StoreApp>),
    /// Adds a `.pbw` to the active watch.
    InstallAppFile(PathBuf),
    RemoveApp(uuid::Uuid),
    /// Opens the app or face on the watch.
    LaunchApp(uuid::Uuid),
    /// Opens the app's settings page, through its script.
    AppSettings(uuid::Uuid),
    /// What the settings page gave back; empty when it was closed without.
    AppSettingsClosed(uuid::Uuid, String),
    /// Lets the app's script use the network, or stops it.
    AllowNetwork(uuid::Uuid, bool),
    /// The active watch's apps in this order; the ones not listed follow.
    ReorderApps(Vec<uuid::Uuid>),
    SetNotificationsEnabled(bool),
    SetNotifyMode(String, NotifyMode),
    ClearNotifyHistory,
    /// The calendars to send, by EDS source UID.
    SetCalendars(Vec<String>),
    SetWeather(crate::settings::WeatherSettings),
    /// The active watch's health profile and settings.
    SetHealth(crate::settings::HealthSettings),
    /// Asks the watch for what it has logged, and sends the summary again.
    RefreshHealth,
    /// Deletes the active profile's health history, or only what is older than this (UTC).
    ClearHealth(Option<i64>),
    /// The active profile's chart for a range, answered with `HealthTrend`.
    HealthTrend(health::Range),
    /// One day of the active profile, hour by hour, answered with `HealthDay`.
    HealthDay(chrono::NaiveDate),
    /// Adds a profile with a name and the wearer's details (height, weight, birth year, sex),
    /// and switches to it.
    AddProfile(String, crate::settings::HealthSettings),
    SwitchProfile(crate::store::ProfileId),
    /// A new name and details.
    EditProfile(crate::store::ProfileId, String, crate::settings::HealthSettings),
    /// Deletes a profile, its watches (bonds included), and everything kept for it.
    DeleteProfile(crate::store::ProfileId),
    /// The units preference changed: resend what depends on it.
    SetUnits(crate::units::Units),
    SetLaunchWithChooser(bool),
    SetCatalogSources(Vec<crate::appstore::Source>),
    /// Running in the background and starting at login.
    SetGeneral(settings::Settings),
    /// Turns dictation on (downloading the speech model if needed) or off (removing it).
    SetDictation(bool),
    /// Pictures in notifications, for watches that show them.
    SetPictures(bool),
    Shutdown,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum AppProgress {
    Downloading(f64),
    /// Sending the app to the watch, which asked for it.
    Sending(f64),
    Done,
    Failed(String),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum FirmwareSource {
    /// The newest PebbleOS release for the watch.
    Latest,
    /// The newest recovery firmware for the watch.
    LatestRecovery,
    File(PathBuf),
}

/// The active watch's firmware: what the check found, and an install's progress.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Update {
    Unknown,
    Checking,
    UpToDate,
    /// `missing`: the watch runs only its recovery firmware.
    Available {
        tag: String,
        size: u64,
        missing: bool,
    },
    CheckFailed(String),
    Downloading(f64),
    Installing(f64),
    Restarting {
        into_recovery: bool,
    },
    Failed(String),
}

impl Update {
    pub fn is_busy(&self) -> bool {
        matches!(self, Update::Downloading(_) | Update::Installing(_) | Update::Restarting { .. })
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum LinkState {
    NoWatch,
    Disconnected { error: Option<String>, retry_in: Option<Duration> },
    Connecting(Stage),
    Negotiating,
    Connected,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SessionInfo {
    pub transport: Transport,
    pub ppog_version: u8,
    pub tx_window: u8,
    pub rx_window: u8,
    pub recovery_mode: bool,
    pub unfaithful: Option<bool>,
    pub capabilities: Vec<Capability>,
    pub running_app: Option<uuid::Uuid>,
    pub firmware_slot: Option<u8>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct ProfileInfo {
    pub id: crate::store::ProfileId,
    pub name: String,
    /// Its watches' names.
    pub watches: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct FoundWatch {
    pub address: String,
    pub name: String,
    pub serial: Option<String>,
    pub rssi: Option<i16>,
    /// PebbleOS reports it set up already: neither in recovery firmware nor waiting for first use.
    pub set_up: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy)]
pub struct Paired {
    pub id: WatchId,
    /// The watch runs only its recovery firmware and needs PebbleOS.
    pub recovery: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub enum Event {
    Watches {
        watches: Vec<Watch>,
        active: Option<WatchId>,
    },
    Link(LinkState),
    Battery(Option<u8>),
    Session(Option<SessionInfo>),
    Found(Vec<FoundWatch>),
    ScanEnded(Option<String>),
    PairingStage(Stage),
    Paired(Result<Paired, String>),
    Firmware(Update),
    /// The active watch's apps, in order, without the ones being removed.
    Apps(Vec<WatchApp>),
    App(uuid::Uuid, AppProgress),
    NotificationsEnabled(bool),
    /// Apps that have sent notifications, by name.
    NotifyApps(Vec<NotifyApp>),
    /// Newest first.
    NotifyHistory(Vec<NotifyEntry>),
    /// The phone is silent or in Do Not Disturb.
    PhoneQuiet(bool),
    /// The active profile's health summary; `None` without a profile.
    Health(Option<Box<health::View>>),
    HealthTrend(Box<health::Trend>),
    HealthDay(Box<health::DayDetail>),
    /// A watch's own settings, as last known.
    WatchPrefs(Box<watch_prefs::View>),
    Profiles {
        list: Vec<ProfileInfo>,
        active: Option<crate::store::ProfileId>,
    },
    /// A script's settings page to show.
    AppSettings {
        uuid: uuid::Uuid,
        title: String,
        url: String,
    },
    /// A script tried the network without access.
    WantsNetwork {
        uuid: uuid::Uuid,
        title: String,
    },
    Toast(String),
    /// How far saving a log or core dump has got: lines or a fraction; `None` once it has ended.
    Diagnostic(Diagnostic, Option<f64>),
    Error(String),
    Dictation(dictation::State),
    Settings(Box<settings::View>),
}

pub fn spawn(db_path: PathBuf) -> (async_channel::Sender<Command>, async_channel::Receiver<Event>) {
    let (cmd_tx, cmd_rx) = async_channel::unbounded();
    let (evt_tx, evt_rx) = async_channel::unbounded();
    // Built here so the UI can use it for the app store from the start.
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = evt_tx.send_blocking(Event::Error(format!("cannot start the Bluetooth thread: {e}")));
            return (cmd_tx, evt_rx);
        }
    };
    crate::net::set_runtime(rt.handle().clone());
    std::thread::spawn(move || {
        rt.block_on(async move {
            match Store::open(&db_path) {
                Ok(store) => Backend::new(store, evt_tx, cmd_rx).run().await,
                Err(e) => {
                    let _ = evt_tx.send(Event::Error(format!("cannot open the database: {e:#}"))).await;
                }
            }
        });
    });
    (cmd_tx, evt_rx)
}

/// Messages from a supervisor to the backend loop.
enum Internal {
    Connected {
        address: Address,
        info: Box<WatchInfo>,
    },
    /// Time to try the active watch again after Bluetooth was unavailable.
    RetryActive,
    AddFailed(String),
    /// An app was added to `watch`; `launch` opens it once the watch has it.
    AppAdded {
        watch: WatchId,
        uuid: uuid::Uuid,
    },
    /// The watch's app list changed in the store.
    AppsChanged,
    Scan(bluetooth::Found),
    ScanEnded(Option<String>),
    Phone(desktop::PhoneEvent),
    /// The watch asked for a notification action.
    WatchAction(pebble_protocol::timeline::Invoke),
    /// The watch answered or declined a call.
    CallRequest(pebble_protocol::phone::Request),
    Music(Option<music::NowPlaying>),
    MusicButton(pebble_protocol::music::Button),
    /// A phone-control message ready to send (an incoming call with the caller's name).
    CallMessage(Vec<u8>),
    Volume(u8),
    /// New health data was stored for this watch.
    HealthData(crate::store::ProfileId),
    /// Whether the connected watch takes health settings.
    HealthSettable(bool),
}

/// What the watch keeps for bug reports.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diagnostic {
    Logs,
    CoreDump,
}

enum Control {
    Retry,
    Stop,
    Ping,
    SaveDiagnostic(Diagnostic, std::path::PathBuf),
    SyncTime,
    Reset(Reset),
    CheckFirmware,
    InstallFirmware(FirmwareSource),
    /// Sends what changed in the app list, then opens `launch` if given.
    SyncApps(Option<uuid::Uuid>),
    SendAppOrder,
    LaunchApp(uuid::Uuid),
    /// Asks the app's script for its settings page, opening the app first if need be.
    AppSettings(uuid::Uuid),
    AppSettingsClosed(uuid::Uuid, String),
    Network(uuid::Uuid, bool),
    /// A notification for the watch's notification database.
    Notify {
        key: uuid::Uuid,
        value: Vec<u8>,
    },
    Call(Vec<u8>),
    SyncCalendar,
    SyncWeather,
    SyncHealthSettings,
    SyncHealthStats,
    RequestHealth,
    /// Music-control messages to send, in order.
    Music(Vec<Vec<u8>>),
}

struct Supervisor {
    address: Address,
    control: mpsc::UnboundedSender<Control>,
    task: tokio::task::JoinHandle<()>,
}

struct Backend {
    /// Per app, the icon last cached this run.
    icon_sources: HashMap<String, String>,
    store: Arc<Mutex<Store>>,
    events: async_channel::Sender<Event>,
    commands: async_channel::Receiver<Command>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal: mpsc::UnboundedReceiver<Internal>,
    bluetooth: Option<Arc<bluetooth::Bluetooth>>,
    server: ServerSlot,
    supervisor: Option<Supervisor>,
    /// The watch being added, by address, until its first session starts.
    adding: Option<Address>,
    scan: Option<tokio::task::AbortHandle>,
    found: Vec<bluetooth::Found>,
    /// Shared with the supervisor: an install must not be interrupted.
    updating: Arc<AtomicBool>,
    /// Set while the active watch is connected and can take apps.
    apps_online: Arc<AtomicBool>,
    /// The phone is silent or in Do Not Disturb.
    quiet: bool,
    sent: notify::Sent,
    /// Notifications that arrived while no watch was connected.
    missed: notify::Missed,
    /// Calls the watch knows of, by ModemManager path.
    calls: HashMap<String, CallTrack>,
    next_cookie: u32,
    /// What the phone is playing.
    music: Option<music::NowPlaying>,
    /// The connected watch takes health settings; assumed until one says otherwise.
    health_settable: bool,
    /// The speech model being downloaded.
    dictation_download: Option<tokio::task::AbortHandle>,
}

/// The music-control messages for what is playing.
fn music_messages(now: Option<&music::NowPlaying>) -> Vec<Vec<u8>> {
    use pebble_protocol::music as m;
    match now {
        Some(n) => vec![
            m::player(&n.player, &n.identity),
            m::track(&n.artist, &n.album, &n.title, n.length_ms),
            m::play_state(if n.playing { m::State::Playing } else { m::State::Paused }, n.position_ms, if n.playing { 100 } else { 0 }),
        ],
        None => vec![m::track("", "", "", None), m::play_state(m::State::Unknown, 0, 0)],
    }
}

struct CallTrack {
    cookie: u32,
    active: bool,
}

/// How long a just-opened app gets before its script is asked for its settings page.
const SETTINGS_PAUSE: Duration = Duration::from_millis(1000);
const NOTIFICATIONS_ENABLED: &str = "notifications_enabled";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Backend {
    fn new(store: Store, events: async_channel::Sender<Event>, commands: async_channel::Receiver<Command>) -> Self {
        let (internal_tx, internal) = mpsc::unbounded_channel();
        Backend {
            store: Arc::new(Mutex::new(store)),
            events,
            commands,
            internal_tx,
            internal,
            bluetooth: None,
            server: ServerSlot::default(),
            supervisor: None,
            adding: None,
            scan: None,
            found: Vec::new(),
            updating: Arc::default(),
            apps_online: Arc::default(),
            quiet: false,
            sent: notify::Sent::default(),
            missed: notify::Missed::default(),
            calls: HashMap::new(),
            next_cookie: 1,
            music: None,
            icon_sources: HashMap::new(),
            health_settable: true,
            dictation_download: None,
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.events.try_send(e);
    }

    async fn set_dictation(&mut self, on: bool) {
        if let Err(e) = settings::set_dictation(&self.db(), on) {
            self.emit(Event::Error(format!("cannot save the dictation setting: {e:#}")));
        }
        dictation::set_enabled(on);
        if on {
            if dictation::model_ready() {
                self.emit(Event::Dictation(dictation::State::Ready));
            } else {
                self.download_speech_model();
            }
        } else {
            if let Some(d) = self.dictation_download.take() {
                d.abort();
            }
            dictation::remove_model().await;
            self.emit(Event::Dictation(dictation::State::Off));
        }
    }

    /// Fetches the speech model; dictation is turned back off if that fails.
    fn download_speech_model(&mut self) {
        if self.dictation_download.as_ref().is_some_and(|d| !d.is_finished()) {
            return;
        }
        let (events, store) = (self.events.clone(), self.store.clone());
        let task = tokio::spawn(async move {
            let progress = |p: f64| {
                let _ = events.try_send(Event::Dictation(dictation::State::Downloading(p)));
            };
            match dictation::download(crate::net::client(), progress).await {
                Ok(()) => {
                    let _ = events.try_send(Event::Dictation(dictation::State::Ready));
                }
                Err(e) => {
                    tracing::info!(error = format!("{e:#}"), "speech model not downloaded");
                    dictation::set_enabled(false);
                    let _ = settings::set_dictation(&store.lock().unwrap_or_else(|p| p.into_inner()), false);
                    let _ = events.try_send(Event::Dictation(dictation::State::Failed(format!("{e:#}"))));
                }
            }
        });
        self.dictation_download = Some(task.abort_handle());
    }

    /// Never held across an await: the supervisor and app tasks share the store.
    fn db(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn bluetooth(&mut self) -> Result<Arc<bluetooth::Bluetooth>> {
        if self.bluetooth.is_none() {
            self.bluetooth = Some(Arc::new(bluetooth::Bluetooth::open().await?));
        }
        Ok(self.bluetooth.clone().expect("set above"))
    }

    /// Keeps the app's icon for the Notifications tab: an icon name as it is, an image cached.
    fn note_icon(&mut self, p: &desktop::Posted) {
        let Some(source) = p.icon.clone().or_else(|| notify::desktop_icon(&p.app_id)) else { return };
        if self.icon_sources.get(&p.app_id) == Some(&source) {
            return;
        }
        self.icon_sources.insert(p.app_id.clone(), source.clone());
        if let Some(icon) = notify::cache_icon(&p.app_id, &source) {
            let _ = self.db().set_notify_icon(&p.app_id, &icon);
        }
    }

    fn send_settings(&self) {
        self.emit(Event::Settings(Box::new(settings::view(&self.db()))));
    }

    fn send_profiles(&self) {
        let db = self.db();
        let watches = db.all_watches().unwrap_or_default();
        let list = db
            .profiles()
            .unwrap_or_default()
            .into_iter()
            .map(|p| ProfileInfo {
                watches: watches.iter().filter(|w| w.profile == p.id).map(|w| w.nickname.clone().unwrap_or_else(|| w.name())).collect(),
                id: p.id,
                name: p.name,
            })
            .collect();
        let active = db.profile().ok().filter(|p| *p != 0);
        self.emit(Event::Profiles { list, active });
    }

    fn send_health(&self) {
        let profile = self.db().profile().ok().filter(|p| *p != 0);
        let active = self.active_id();
        let view = profile.and_then(|profile| {
            let db = self.db();
            let watch = active.and_then(|id| db.watch(id).ok().flatten());
            match health::view(&db, profile, watch.as_ref(), self.health_settable) {
                Ok(v) => Some(Box::new(v)),
                Err(e) => {
                    tracing::warn!(error = format!("{e:#}"), "no health summary");
                    None
                }
            }
        });
        self.emit(Event::Health(view));
    }

    fn send_watches(&self) {
        let db = self.db();
        match (db.watches(), settings::active_watch(&db)) {
            (Ok(watches), Ok(active)) => self.emit(Event::Watches { watches, active }),
            (Err(e), _) | (_, Err(e)) => self.emit(Event::Error(format!("cannot read the watch list: {e:#}"))),
        }
        drop(db);
        self.send_apps();
    }

    fn active_id(&self) -> Option<WatchId> {
        settings::active_watch(&self.db()).ok().flatten()
    }

    fn send_apps(&self) {
        let list = match self.active_id() {
            Some(id) => self.db().watch_apps(id).unwrap_or_default(),
            None => Vec::new(),
        };
        self.emit(Event::Apps(list.into_iter().filter(|a| a.state != OnWatch::ToDelete).collect()));
    }

    fn install_app(&mut self, source: AppSource) {
        let Some(watch) = self.active_id() else {
            self.emit(Event::Error("Add a watch before installing apps".to_string()));
            return;
        };
        let platform =
            self.db().watch(watch).ok().flatten().and_then(|w| w.hardware).and_then(|h| HardwarePlatform::from_number(h).app_platform);
        let (db, events, internal) = (self.store.clone(), self.events.clone(), self.internal_tx.clone());
        tokio::spawn(async move {
            let (result, uuid) = match source {
                AppSource::Store(app) => {
                    let uuid = app.uuid;
                    let Some(platform) = platform else {
                        let _ =
                            events.send(Event::App(uuid, AppProgress::Failed("the watch's platform is not known yet".to_string()))).await;
                        return;
                    };
                    let mut last = -1.0;
                    let progress = |f: f64| {
                        if f - last >= 0.02 {
                            last = f;
                            let _ = events.try_send(Event::App(uuid, AppProgress::Downloading(f)));
                        }
                    };
                    let _ = events.try_send(Event::App(uuid, AppProgress::Downloading(0.0)));
                    let result = apps::download(db.clone(), *app, platform, progress).await;
                    if result.is_ok() {
                        let _ = events.try_send(Event::App(uuid, AppProgress::Downloading(1.0)));
                    }
                    (result, Some(uuid))
                }
                AppSource::File(path) => (apps::from_file(db.clone(), path).await, None),
            };
            match result {
                Ok(app) => {
                    let added = apps::lock(&db).add_to_watch(watch, app.uuid);
                    if let Err(e) = added {
                        let _ = events.send(Event::Error(format!("cannot save the app: {e:#}"))).await;
                        return;
                    }
                    tracing::info!(uuid = %app.uuid, title = %app.title, version = %app.version, "app added");
                    // A store install keeps showing its finished download until the watch takes it.
                    if uuid.is_none() {
                        let _ = events.send(Event::Toast(format!("{} added", app.title))).await;
                    }
                    let _ = internal.send(Internal::AppAdded { watch, uuid: app.uuid });
                }
                Err(e) => {
                    tracing::warn!(error = format!("{e:#}"), "app not added");
                    match uuid {
                        Some(u) => {
                            let _ = events.send(Event::App(u, AppProgress::Failed(format!("{e:#}")))).await;
                        }
                        None => {
                            let _ = events.send(Event::Error(format!("The app was not added: {e:#}"))).await;
                        }
                    }
                }
            }
        });
    }

    fn notifications_enabled(&self) -> bool {
        self.db().setting(NOTIFICATIONS_ENABLED).ok().flatten().is_none_or(|v| v == "1")
    }

    fn send_notify_state(&self) {
        self.emit(Event::NotificationsEnabled(self.notifications_enabled()));
        self.send_notify_lists();
    }

    /// Sends what arrived while no watch was connected and is still recent and open on the
    /// desktop. Only the newest alerts, so a watch coming back does not buzz for each one.
    fn send_missed(&mut self) {
        let missed = self.missed.take(now());
        if missed.is_empty() || !self.notifications_enabled() {
            return;
        }
        tracing::info!(count = missed.len(), "sending notifications missed while away");
        let newest = missed.len() - 1;
        for (i, m) in missed.into_iter().enumerate() {
            let silent = i != newest || !m.alert || self.quiet;
            self.send_notification(&m.posted, silent, m.time);
            if let Some(log) = m.log {
                let _ = self.db().set_notification_outcome(log, Delivery::SentLate);
            }
        }
        self.send_notify_lists();
    }

    fn send_notification(&mut self, p: &desktop::Posted, silent: bool, time: i64) {
        let id = self.sent.id_for(p);
        let picture = pictures::attach(id, p.picture.as_ref());
        let item = notify::item(id, p, silent, time, picture);
        self.control(Control::Notify { key: id, value: item.encode() });
        self.sent.record(p.id, item, &p.app_id);
    }

    fn send_notify_lists(&self) {
        let (apps, history) = {
            let db = self.db();
            (db.notify_apps().unwrap_or_default(), db.notification_history().unwrap_or_default())
        };
        self.emit(Event::NotifyApps(apps));
        self.emit(Event::NotifyHistory(history));
    }

    /// Starts following the phone's notifications, quiet state, and calls.
    fn watch_phone(&self) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                let _ = internal.send(Internal::Phone(e));
            }
        });
        let t = tx.clone();
        tokio::spawn(desktop::keep("notifications", move || desktop::notifications(t.clone())));
        let t = tx.clone();
        tokio::spawn(desktop::keep("Do Not Disturb", move || desktop::quiet(t.clone())));
        let t = tx.clone();
        tokio::spawn(desktop::keep("time zone", move || desktop::time_zone(t.clone())));
        tokio::spawn(desktop::keep("clock format", desktop::clock_format));
        tokio::spawn(desktop::keep("calls", move || desktop::calls(tx.clone())));
        let (mtx, mut mrx) = mpsc::unbounded_channel();
        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            while let Some(now) = mrx.recv().await {
                let _ = internal.send(Internal::Music(now));
            }
        });
        tokio::spawn(desktop::keep("music players", move || music::players(mtx.clone())));
    }

    fn phone_event(&mut self, e: desktop::PhoneEvent) {
        use desktop::{CallState, PhoneEvent};
        match e {
            PhoneEvent::TimeZone(name) => {
                let Ok(tz) = name.parse::<chrono_tz::Tz>() else { return };
                if crate::host::zone_name().as_deref() == Some(name.as_str()) {
                    return;
                }
                tracing::info!(zone = %name, "time zone changed");
                crate::host::set_zone(tz);
                self.control(Control::SyncTime);
                self.control(Control::SyncCalendar);
                self.send_health();
            }
            PhoneEvent::Posted(p) => {
                if p.app_id == "net.dunevent.Lithidion" || p.app_name == "Lithidion" {
                    return;
                }
                let time = now();
                let mode = self.db().seen_notify_app(&p.app_id, &p.app_name, time).unwrap_or(NotifyMode::On);
                self.note_icon(&p);
                let online = self.apps_online.load(Ordering::Relaxed);
                let outcome = if !self.notifications_enabled() {
                    Delivery::Off
                } else if mode == NotifyMode::Off {
                    Delivery::AppOff
                } else if !online {
                    Delivery::Away
                } else if self.quiet && mode == NotifyMode::On {
                    Delivery::SentQuietly
                } else {
                    Delivery::Sent
                };
                tracing::debug!(app = %p.app_id, ?outcome, "notification");
                let title = if p.title.is_empty() { p.app_name.clone() } else { p.title.clone() };
                let log = match self.db().log_notification(&p.app_id, &title, &p.body, time, outcome) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        tracing::warn!(error = %e, "notification not logged");
                        None
                    }
                };
                if outcome == Delivery::Away {
                    self.missed.push(notify::MissedItem { posted: p.clone(), time, alert: mode == NotifyMode::On, log });
                }
                if matches!(outcome, Delivery::Sent | Delivery::SentQuietly) {
                    self.send_notification(&p, outcome == Delivery::SentQuietly, time);
                }
                self.send_notify_lists();
            }
            PhoneEvent::Closed(phone_id) => {
                self.missed.closed(phone_id);
                if let Some(item) = self.sent.closed(phone_id) {
                    pictures::forget(item.id);
                    self.control(Control::Notify { key: item.id, value: item.encode() });
                }
            }
            PhoneEvent::Quiet(q) => {
                self.quiet = q;
                self.emit(Event::PhoneQuiet(q));
            }
            PhoneEvent::Call(c) => {
                use pebble_protocol::phone;
                match c.state {
                    CallState::Ringing if c.incoming && !self.calls.contains_key(&c.path) => {
                        let cookie = self.next_cookie;
                        self.next_cookie = self.next_cookie.wrapping_add(1).max(1);
                        self.calls.insert(c.path, CallTrack { cookie, active: false });
                        // The caller's name from the phone's contacts, else the number.
                        let internal = self.internal_tx.clone();
                        tokio::spawn(async move {
                            let name = tokio::time::timeout(Duration::from_secs(2), eds::contact_name(&c.number))
                                .await
                                .ok()
                                .and_then(|r| r.ok())
                                .flatten();
                            let name = name.unwrap_or_else(|| c.number.clone());
                            let _ = internal.send(Internal::CallMessage(phone::incoming(cookie, &c.number, &name)));
                        });
                    }
                    CallState::Active => {
                        if let Some(t) = self.calls.get_mut(&c.path).filter(|t| !t.active) {
                            t.active = true;
                            let cookie = t.cookie;
                            self.control(Control::Call(phone::start(cookie)));
                        }
                    }
                    CallState::Ended => {
                        if let Some(t) = self.calls.remove(&c.path) {
                            self.control(Control::Call(phone::end(t.cookie)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn remove_app(&mut self, uuid: uuid::Uuid) {
        let Some(watch) = self.active_id() else { return };
        let state = self.db().watch_apps(watch).ok().and_then(|l| l.into_iter().find(|a| a.app.uuid == uuid)).map(|a| a.state);
        match state {
            None => return,
            // Never reached the watch: nothing to remove there.
            Some(OnWatch::ToInsert) => {
                let _ = self.db().drop_from_watch(watch, uuid);
                apps::forget_if_unused(&self.store, uuid);
            }
            Some(_) => {
                let _ = self.db().set_on_watch(watch, uuid, OnWatch::ToDelete);
            }
        }
        self.send_apps();
        self.control(Control::SyncApps(None));
    }

    /// Notes which apps have scripts, for apps added before Lithidion looked.
    fn read_scripts(&self) {
        let (db, internal) = (self.store.clone(), self.internal_tx.clone());
        tokio::task::spawn_blocking(move || {
            let unread = apps::lock(&db).apps_unread().unwrap_or_default();
            if unread.is_empty() {
                return;
            }
            for uuid in unread {
                let found = pbw::Pbw::open(&pbw::path(uuid)).map(|b| (b.has_script(), b.info.can("configurable")));
                let (script, configurable) = found.unwrap_or((false, false));
                let _ = apps::lock(&db).set_app_script(uuid, script, configurable);
            }
            let _ = internal.send(Internal::AppsChanged);
        });
    }

    async fn run(mut self) {
        crate::units::load(&self.db());
        self.read_scripts();
        self.send_profiles();
        self.send_settings();
        self.send_watches();
        self.send_health();
        self.send_notify_state();
        self.watch_phone();
        pictures::set_enabled(settings::pictures(&self.db()));
        let dictation = settings::dictation(&self.db());
        dictation::set_enabled(dictation);
        if dictation && !dictation::model_ready() {
            self.download_speech_model();
        }
        self.start_active().await;
        let mut calendar_tick = tokio::time::interval_at(Instant::now() + Duration::from_secs(900), Duration::from_secs(900));
        let mut weather_tick = tokio::time::interval_at(Instant::now() + Duration::from_secs(1800), Duration::from_secs(1800));
        loop {
            tokio::select! {
                _ = calendar_tick.tick() => self.control(Control::SyncCalendar),
                _ = weather_tick.tick() => self.control(Control::SyncWeather),
                cmd = self.commands.recv() => match cmd {
                    Ok(Command::Shutdown) | Err(_) => break,
                    Ok(c) => self.command(c).await,
                },
                Some(msg) = self.internal.recv() => self.internal_msg(msg).await,
            }
        }
        self.stop_scan();
        self.stop_supervisor().await;
    }

    async fn start_active(&mut self) {
        let active = {
            let db = self.db();
            settings::active_watch(&db).ok().flatten().and_then(|id| db.watch(id).ok().flatten())
        };
        match active {
            Some(w) => match w.address.parse::<Address>() {
                Ok(a) => self.supervise(a, w.hardware).await,
                Err(_) => self.emit(Event::Error(format!("stored address {} is not valid", w.address))),
            },
            None => self.emit(Event::Link(LinkState::NoWatch)),
        }
    }

    async fn command(&mut self, cmd: Command) {
        if self.updating.load(Ordering::Relaxed) {
            let refused = "Not while PebbleOS is being installed".to_string();
            match cmd {
                Command::StartScan => return self.emit(Event::ScanEnded(Some(refused))),
                Command::Pair(_) => return self.emit(Event::Paired(Err(refused))),
                Command::Switch(_)
                | Command::Forget(..)
                | Command::Reboot
                | Command::RebootIntoRecovery
                | Command::SwitchProfile(_)
                | Command::DeleteProfile(_) => {
                    return self.emit(Event::Toast(refused));
                }
                _ => {}
            }
        }
        let changes_settings = matches!(
            cmd,
            Command::AllowNetwork(..)
                | Command::SetCalendars(_)
                | Command::SetWeather(_)
                | Command::SwitchProfile(_)
                | Command::AddProfile(..)
                | Command::EditProfile(..)
                | Command::DeleteProfile(_)
                | Command::SetPictures(_)
                | Command::SetUnits(_)
                | Command::SetLaunchWithChooser(_)
                | Command::SetCatalogSources(_)
                | Command::SetGeneral(_)
        );
        self.dispatch(cmd).await;
        if changes_settings {
            self.send_settings();
        }
    }

    async fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::StartScan => self.start_scan().await,
            Command::StopScan => self.stop_scan(),
            Command::Pair(address) => {
                self.stop_scan();
                let Ok(a) = address.parse::<Address>() else {
                    self.emit(Event::Paired(Err(format!("{address} is not a Bluetooth address"))));
                    return;
                };
                let ext = self.found.iter().find(|f| f.address == a).and_then(|f| f.record.as_ref()?.extended.clone());
                let hardware = ext.as_ref().map(|x| x.hardware_platform);
                // A watch fresh from a factory reset has forgotten any bond BlueZ still keeps, and
                // refuses the old keys before the pairing state can be read.
                let fresh = ext.as_ref().is_some_and(|x| x.first_use || x.running_recovery);
                if let (true, Ok(bt)) = (fresh, self.bluetooth().await) {
                    let bonded = match bt.adapter.device(a) {
                        Ok(d) => d.is_paired().await.unwrap_or(false),
                        Err(_) => false,
                    };
                    if bonded {
                        tracing::info!(%a, "removing the bond of a watch that was reset");
                        if let Err(e) = bluetooth::remove_bond(&bt.adapter, a).await {
                            tracing::warn!(error = format!("{e:#}"), "old bond not removed");
                        }
                    }
                }
                self.adding = Some(a);
                self.supervise(a, hardware).await;
            }
            Command::Switch(id) => {
                if settings::active_watch(&self.db()).ok().flatten() == Some(id) && self.supervisor.is_some() {
                    return;
                }
                if let Err(e) = settings::set_active_watch(&self.db(), Some(id)) {
                    self.emit(Event::Error(format!("{e:#}")));
                    return;
                }
                self.adding = None;
                self.send_watches();
                self.send_health();
                self.start_active().await;
            }
            Command::Forget(id, reset) => {
                let connected = self.active_id() == Some(id) && self.supervisor.is_some();
                if reset && connected {
                    self.control(Control::Reset(Reset::FactoryReset));
                    // Time for the message to leave before the link is closed.
                    sleep(Duration::from_secs(2)).await;
                }
                self.forget(id).await
            }
            Command::WatchPrefs(id) => {
                let v = watch_prefs::view(&self.db(), id);
                self.emit(Event::WatchPrefs(Box::new(v)));
            }
            Command::SetWatchPref(id, key, value) => {
                if let Err(e) = watch_prefs::set(&self.db(), id, &key, value) {
                    return self.emit(Event::Error(format!("cannot change the watch setting: {e:#}")));
                }
                let v = watch_prefs::view(&self.db(), id);
                self.emit(Event::WatchPrefs(Box::new(v)));
            }
            Command::SetWatchClock(id, clock) => {
                if let Err(e) = settings::set_watch_clock(&self.db(), id, clock) {
                    return self.emit(Event::Error(format!("cannot change the watch's clock: {e:#}")));
                }
                watch_prefs::wake();
                let v = watch_prefs::view(&self.db(), id);
                self.emit(Event::WatchPrefs(Box::new(v)));
            }
            Command::RenameWatch(id, name) => {
                if let Err(e) = self.db().set_nickname(id, Some(&name)) {
                    return self.emit(Event::Error(format!("cannot rename the watch: {e:#}")));
                }
                self.send_watches();
                self.send_profiles();
            }
            Command::Reconnect => match self.supervisor {
                Some(_) => self.control(Control::Retry),
                None => self.start_active().await,
            },
            Command::Ping => self.control(Control::Ping),
            Command::SaveDiagnostic(kind, path) => self.control(Control::SaveDiagnostic(kind, path)),
            Command::SyncTime => self.control(Control::SyncTime),
            Command::Reboot => self.control(Control::Reset(Reset::Reboot)),
            Command::RebootIntoRecovery => self.control(Control::Reset(Reset::IntoRecovery)),
            Command::CheckFirmware => self.control(Control::CheckFirmware),
            Command::InstallFirmware(source) => self.control(Control::InstallFirmware(source)),
            Command::InstallApp(app) => self.install_app(AppSource::Store(app)),
            Command::InstallAppFile(path) => self.install_app(AppSource::File(path)),
            Command::RemoveApp(uuid) => self.remove_app(uuid),
            Command::LaunchApp(uuid) => self.control(Control::LaunchApp(uuid)),
            Command::AppSettings(uuid) => {
                if self.supervisor.is_none() {
                    self.emit(Event::Toast("Settings open while the watch is connected".to_string()));
                }
                self.control(Control::AppSettings(uuid));
            }
            Command::AppSettingsClosed(uuid, response) => self.control(Control::AppSettingsClosed(uuid, response)),
            Command::AllowNetwork(uuid, allowed) => {
                let profile = self.db().profile().unwrap_or(0);
                if let Err(e) = crate::settings::set_network_allowed(&self.db(), profile, uuid, allowed) {
                    tracing::warn!(error = %e, "network access not saved");
                }
                self.control(Control::Network(uuid, allowed));
                // The app list shows it.
                self.send_apps();
            }
            Command::SetNotificationsEnabled(on) => {
                let _ = self.db().set_setting(NOTIFICATIONS_ENABLED, if on { "1" } else { "0" });
                self.emit(Event::NotificationsEnabled(on));
            }
            Command::SetNotifyMode(app, mode) => {
                let _ = self.db().set_notify_mode(&app, mode);
                self.send_notify_lists();
            }
            Command::ClearNotifyHistory => {
                let _ = self.db().clear_notification_history();
                self.send_notify_lists();
            }
            Command::SetCalendars(uids) => {
                let _ = settings::set_calendars(&self.db(), &uids);
                self.control(Control::SyncCalendar);
            }
            Command::SetWeather(w) => {
                let _ = settings::set_weather(&self.db(), &w);
                self.control(Control::SyncWeather);
            }
            Command::SetHealth(h) => {
                let profile = self.db().profile().unwrap_or(0);
                if let Err(e) = settings::set_health(&self.db(), profile, &h) {
                    self.emit(Event::Error(format!("cannot save the health settings: {e:#}")));
                }
                self.send_health();
                self.control(Control::SyncHealthSettings);
            }
            Command::RefreshHealth => {
                self.send_health();
                self.control(Control::RequestHealth);
            }
            Command::HealthTrend(range) => {
                let profile = self.db().profile().unwrap_or(0);
                let trend = health::trend(&self.db(), profile, range);
                match trend {
                    Ok(t) => self.emit(Event::HealthTrend(Box::new(t))),
                    Err(e) => tracing::warn!(error = format!("{e:#}"), "no health chart"),
                }
            }
            Command::HealthDay(date) => {
                let profile = self.db().profile().unwrap_or(0);
                let day = health::day(&self.db(), profile, date);
                match day {
                    Ok(d) => self.emit(Event::HealthDay(Box::new(d))),
                    Err(e) => tracing::warn!(error = format!("{e:#}"), "no health day"),
                }
            }
            Command::ClearHealth(before) => {
                let profile = self.db().profile().unwrap_or(0);
                if let Err(e) = self.db().clear_health(profile, before) {
                    self.emit(Event::Error(format!("cannot clear the health history: {e:#}")));
                }
                self.send_health();
                self.control(Control::SyncHealthStats);
            }
            Command::AddProfile(name, details) => {
                let added = self.db().add_profile(&name, chrono::Utc::now().timestamp());
                match added {
                    Ok(id) => {
                        // The first profile may have taken over settings saved before profiles.
                        let mut h = settings::health(&self.db(), id);
                        h.set_details(&details);
                        let _ = settings::set_health(&self.db(), id, &h);
                        self.switch_profile(id).await;
                    }
                    Err(e) => self.emit(Event::Error(format!("cannot add the profile: {e:#}"))),
                }
            }
            Command::SwitchProfile(id) => self.switch_profile(id).await,
            Command::EditProfile(id, name, details) => {
                let db = self.db();
                if let Err(e) = db.rename_profile(id, &name) {
                    drop(db);
                    return self.emit(Event::Error(format!("cannot save the profile: {e:#}")));
                }
                let mut h = settings::health(&db, id);
                h.set_details(&details);
                let _ = settings::set_health(&db, id, &h);
                let active = db.profile().ok() == Some(id);
                drop(db);
                self.send_profiles();
                if active {
                    self.send_health();
                    self.control(Control::SyncHealthSettings);
                }
            }
            Command::DeleteProfile(id) => self.delete_profile(id).await,
            Command::SetDictation(on) => self.set_dictation(on).await,
            Command::SetPictures(on) => {
                if let Err(e) = settings::set_pictures(&self.db(), on) {
                    self.emit(Event::Error(format!("cannot save the pictures setting: {e:#}")));
                }
                pictures::set_enabled(on);
            }
            Command::SetLaunchWithChooser(choose) => {
                if let Err(e) = settings::set_launch_with_chooser(&self.db(), choose) {
                    self.emit(Event::Error(format!("cannot save the setting: {e:#}")));
                }
            }
            Command::SetCatalogSources(sources) => {
                if let Err(e) = settings::set_catalog_sources(&self.db(), &sources) {
                    self.emit(Event::Error(format!("cannot save the setting: {e:#}")));
                }
            }
            Command::SetGeneral(general) => {
                if let Err(e) = general.save(&self.db()) {
                    self.emit(Event::Error(format!("cannot save the setting: {e:#}")));
                }
            }
            Command::SetUnits(units) => {
                if let Err(e) = crate::units::set(&self.db(), units) {
                    self.emit(Event::Error(format!("cannot save the units: {e:#}")));
                }
                watch_prefs::wake();
                self.send_health();
                self.control(Control::SyncHealthSettings);
                self.control(Control::SyncWeather);
            }
            Command::ReorderApps(order) => {
                if let Some(watch) = self.active_id() {
                    if let Err(e) = self.db().reorder_watch_apps(watch, &order) {
                        self.emit(Event::Error(format!("cannot save the order: {e:#}")));
                    }
                    self.send_apps();
                    self.control(Control::SendAppOrder);
                }
            }
            Command::Shutdown => {}
        }
    }

    fn control(&self, c: Control) {
        if let Some(s) = &self.supervisor {
            let _ = s.control.send(c);
        }
    }

    async fn internal_msg(&mut self, msg: Internal) {
        match msg {
            Internal::Connected { address, info } => {
                let id = match self.db().add_watch(&address.to_string(), now()) {
                    Ok(id) => id,
                    Err(e) => {
                        self.emit(Event::Error(format!("cannot save the watch: {e:#}")));
                        return;
                    }
                };
                let v = &info.version;
                let details = WatchDetails {
                    model: info.model,
                    hardware: Some(v.running.hardware_platform),
                    serial: Some(v.serial.clone()).filter(|s| !s.is_empty()),
                    board: Some(v.board.clone()).filter(|s| !s.is_empty()),
                    firmware: (!v.running.is_recovery()).then(|| v.running.tag.clone()),
                    recovery_firmware: v.recovery.is_present().then(|| v.recovery.tag.clone()),
                    language: v.language.clone().filter(|s| !s.is_empty()),
                };
                if let Err(e) = self.db().update_details(id, &details, now()) {
                    tracing::warn!(error = %e, "watch details not saved");
                }
                let added = self.adding == Some(address);
                if added {
                    self.adding = None;
                    let _ = settings::set_active_watch(&self.db(), Some(id));
                }
                // The list goes first so the UI can name the new watch.
                self.send_watches();
                self.send_health();
                self.control(Control::Music(music_messages(self.music.as_ref())));
                if !info.version.running.is_recovery() {
                    self.send_missed();
                }
                if added {
                    self.emit(Event::Paired(Ok(Paired { id, recovery: info.version.running.is_recovery() })));
                }
            }
            Internal::AppAdded { watch, uuid } => {
                let online = self.apps_online.load(Ordering::Relaxed) && self.active_id() == Some(watch);
                if self.active_id() == Some(watch) {
                    self.send_apps();
                }
                if online {
                    self.control(Control::SyncApps(Some(uuid)));
                } else {
                    // It goes on the watch at its next connection.
                    self.emit(Event::App(uuid, AppProgress::Done));
                }
            }
            Internal::AppsChanged => self.send_apps(),
            Internal::HealthData(profile) => {
                if self.db().profile().ok() == Some(profile) {
                    self.send_health();
                }
            }
            Internal::HealthSettable(settable) => {
                self.health_settable = settable;
                self.send_health();
            }
            Internal::Phone(e) => self.phone_event(e),
            Internal::Music(now) => {
                self.music = now;
                self.control(Control::Music(music_messages(self.music.as_ref())));
            }
            Internal::CallMessage(payload) => self.control(Control::Call(payload)),
            Internal::Volume(percent) => self.control(Control::Music(vec![pebble_protocol::music::volume(percent)])),
            Internal::MusicButton(button) => {
                use pebble_protocol::music::Button;
                let action = match button {
                    Button::GetCurrent => {
                        self.control(Control::Music(music_messages(self.music.as_ref())));
                        return;
                    }
                    Button::VolumeUp | Button::VolumeDown => {
                        let step = if button == Button::VolumeUp { 0.05 } else { -0.05 };
                        let internal = self.internal_tx.clone();
                        tokio::task::spawn_blocking(move || match music::change_volume(step) {
                            Ok(percent) => {
                                let _ = internal.send(Internal::Volume(percent));
                            }
                            Err(e) => tracing::warn!(error = format!("{e:#}"), "volume not changed"),
                        });
                        return;
                    }
                    Button::PlayPause => music::Action::PlayPause,
                    Button::Pause => music::Action::Pause,
                    Button::Play => music::Action::Play,
                    Button::Next => music::Action::Next,
                    Button::Previous => music::Action::Previous,
                };
                if let Some(player) = self.music.as_ref().map(|n| n.player.clone()) {
                    tokio::spawn(async move {
                        if let Err(e) = music::act(&player, action).await {
                            tracing::info!(error = format!("{e:#}"), "player did not respond");
                        }
                    });
                }
            }
            Internal::WatchAction(invoke) => {
                let Some((phone_id, app)) = self.sent.origin(invoke.item) else { return };
                match invoke.action {
                    notify::ACTION_DISMISS => {
                        tokio::spawn(async move {
                            if let Err(e) = desktop::close_notification(phone_id).await {
                                tracing::info!(error = %e, "notification not closed on the phone");
                            }
                        });
                    }
                    notify::ACTION_MUTE => {
                        let _ = self.db().set_notify_mode(&app, NotifyMode::Off);
                        self.send_notify_lists();
                    }
                    _ => {}
                }
            }
            Internal::CallRequest(request) => {
                use pebble_protocol::phone::Request;
                let (cookie, answer) = match request {
                    Request::Answer(c) => (c, true),
                    Request::Hangup(c) => (c, false),
                };
                let path = self.calls.iter().find(|(_, t)| t.cookie == cookie).map(|(p, _)| p.clone());
                if let Some(path) = path {
                    tokio::spawn(async move {
                        if let Err(e) = desktop::call_action(path, answer).await {
                            tracing::warn!(error = %e, answer, "call not changed");
                        }
                    });
                }
            }
            Internal::RetryActive => {
                if self.supervisor.is_none() && self.adding.is_none() {
                    self.start_active().await;
                }
            }
            Internal::AddFailed(e) => {
                self.adding = None;
                self.supervisor = None;
                self.emit(Event::Paired(Err(e)));
                self.send_watches();
                self.start_active().await;
            }
            Internal::Scan(found) => {
                match self.found.iter_mut().find(|f| f.address == found.address) {
                    Some(f) => *f = found,
                    None => self.found.push(found),
                }
                let list = self.found.iter().map(found_watch).collect();
                self.emit(Event::Found(list));
            }
            Internal::ScanEnded(error) => {
                self.scan = None;
                self.emit(Event::ScanEnded(error));
            }
        }
    }

    async fn start_scan(&mut self) {
        self.stop_scan();
        self.found.clear();
        self.emit(Event::Found(Vec::new()));
        let bt = match self.bluetooth().await {
            Ok(bt) => bt,
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "scan not started");
                self.emit(Event::ScanEnded(Some(e.to_string())));
                return;
            }
        };
        let skip: Vec<Address> = self.db().all_watches().unwrap_or_default().iter().filter_map(|w| w.address.parse().ok()).collect();
        let internal = self.internal_tx.clone();
        let task = tokio::spawn(async move {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let forward = {
                let internal = internal.clone();
                tokio::spawn(async move {
                    while let Some(f) = rx.recv().await {
                        let _ = internal.send(Internal::Scan(f));
                    }
                })
            };
            let result = bluetooth::scan(&bt.adapter, Duration::from_secs(60), skip, tx).await;
            let _ = forward.await;
            let _ = internal.send(Internal::ScanEnded(result.err().map(|e| e.to_string())));
        });
        self.scan = Some(task.abort_handle());
    }

    fn stop_scan(&mut self) {
        if let Some(s) = self.scan.take() {
            s.abort();
        }
    }

    async fn stop_supervisor(&mut self) {
        if let Some(s) = self.supervisor.take() {
            let _ = s.control.send(Control::Stop);
            if tokio::time::timeout(Duration::from_secs(5), s.task).await.is_err() {
                tracing::warn!(address = %s.address, "the connection did not stop in time");
            }
        }
        self.emit(Event::Battery(None));
        self.emit(Event::Session(None));
        report(&self.events, &self.updating, Update::Unknown);
    }

    async fn supervise(&mut self, address: Address, hardware: Option<u8>) {
        self.stop_supervisor().await;
        let bt = match self.bluetooth().await {
            Ok(bt) => bt,
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "Bluetooth unavailable");
                let message = e.to_string();
                if self.adding.take().is_some() {
                    self.emit(Event::Paired(Err(message)));
                } else {
                    let delay = Duration::from_secs(60);
                    self.emit(Event::Link(LinkState::Disconnected { error: Some(message), retry_in: Some(delay) }));
                    let internal = self.internal_tx.clone();
                    tokio::spawn(async move {
                        sleep(delay).await;
                        let _ = internal.send(Internal::RetryActive);
                    });
                }
                return;
            }
        };
        let (control, control_rx) = mpsc::unbounded_channel();
        let platform = hardware.and_then(|h| HardwarePlatform::from_number(h).app_platform);
        let task = tokio::spawn(supervise(
            Ctx {
                bt,
                address,
                platform,
                adding: self.adding == Some(address),
                server: self.server.clone(),
                releases: Arc::default(),
                updating: self.updating.clone(),
                store: self.store.clone(),
                apps_online: self.apps_online.clone(),
                phone_data_lock: Arc::default(),
                events: self.events.clone(),
                internal: self.internal_tx.clone(),
            },
            control_rx,
        ));
        self.supervisor = Some(Supervisor { address, control, task });
    }

    async fn switch_profile(&mut self, id: crate::store::ProfileId) {
        if self.db().profile().ok() == Some(id) && self.supervisor.is_some() {
            self.send_profiles();
            return;
        }
        self.stop_supervisor().await;
        if let Err(e) = self.db().set_profile(id) {
            return self.emit(Event::Error(format!("{e:#}")));
        }
        self.adding = None;
        self.send_profiles();
        self.send_watches();
        self.send_health();
        self.send_notify_state();
        self.start_active().await;
    }

    async fn delete_profile(&mut self, id: crate::store::ProfileId) {
        let active = self.db().profile().ok() == Some(id);
        if active {
            self.stop_supervisor().await;
        }
        let watches: Vec<Watch> = self.db().all_watches().unwrap_or_default().into_iter().filter(|w| w.profile == id).collect();
        for w in watches {
            if let (Ok(address), Ok(bt)) = (w.address.parse::<Address>(), self.bluetooth().await) {
                if let Err(e) = bluetooth::remove_bond(&bt.adapter, address).await {
                    tracing::warn!(error = %e, "bond not removed");
                }
            }
        }
        if let Err(e) = self.db().delete_profile(id) {
            self.emit(Event::Error(format!("cannot delete the profile: {e:#}")));
        }
        self.send_profiles();
        if active {
            self.send_watches();
            self.send_health();
            self.send_notify_state();
            self.emit(Event::Link(LinkState::NoWatch));
        }
    }

    async fn forget(&mut self, id: WatchId) {
        let Ok(Some(watch)) = self.db().watch(id) else { return };
        let active = settings::active_watch(&self.db()).ok().flatten() == Some(id);
        if active {
            self.stop_supervisor().await;
        }
        if let (Ok(address), Ok(bt)) = (watch.address.parse::<Address>(), self.bluetooth().await) {
            if let Err(e) = bluetooth::remove_bond(&bt.adapter, address).await {
                tracing::warn!(error = %e, "bond not removed");
            }
        }
        if let Err(e) = self.db().remove_watch(id) {
            self.emit(Event::Error(format!("cannot remove the watch: {e:#}")));
            return;
        }
        if active {
            let next = self.db().watches().ok().and_then(|w| w.first().map(|w| w.id));
            let _ = settings::set_active_watch(&self.db(), next);
            self.send_watches();
            self.send_health();
            self.start_active().await;
        } else {
            self.send_watches();
        }
        self.emit(Event::Toast(format!("{} removed", watch.name())));
    }
}

fn found_watch(f: &bluetooth::Found) -> FoundWatch {
    let ext = f.record.as_ref().and_then(|r| r.extended.as_ref());
    let model = ext.and_then(|x| pebble_protocol::hardware::model(x.model as i32)).map(|(m, _)| m.to_string());
    let family = ext.and_then(|x| HardwarePlatform::from_number(x.hardware_platform).app_platform).map(|p| p.family_name().to_string());
    FoundWatch {
        address: f.address.to_string(),
        name: model.or(family).or_else(|| f.name.clone()).unwrap_or_else(|| "Pebble".to_string()),
        serial: f.record.as_ref().map(|r| r.serial.clone()).filter(|s| !s.is_empty()),
        rssi: f.rssi,
        set_up: ext.is_some_and(|x| !x.running_recovery && !x.first_use),
    }
}

struct Ctx {
    bt: Arc<bluetooth::Bluetooth>,
    address: Address,
    platform: Option<AppPlatform>,
    adding: bool,
    server: ServerSlot,
    /// The last release seen, so reconnecting does not ask GitHub every time.
    releases: Arc<Mutex<Option<(Instant, firmware::Release)>>>,
    /// Set while firmware is being downloaded or installed, or while the watch restarts after it.
    updating: Arc<AtomicBool>,
    store: apps::Db,
    apps_online: Arc<AtomicBool>,
    /// One calendar or weather sync at a time.
    phone_data_lock: Arc<tokio::sync::Mutex<()>>,
    events: async_channel::Sender<Event>,
    internal: mpsc::UnboundedSender<Internal>,
}

impl Ctx {
    fn emit(&self, e: Event) {
        let _ = self.events.try_send(e);
    }

    fn firmware(&self, u: Update) {
        report(&self.events, &self.updating, u);
    }
}

/// Sends a firmware state, keeping the backend's "updating" flag in step with it.
fn report(events: &async_channel::Sender<Event>, updating: &AtomicBool, u: Update) {
    updating.store(u.is_busy(), Ordering::Relaxed);
    let _ = events.try_send(Event::Firmware(u));
}

/// Retry delays: 10 seconds for the first, then 1 minute up to the 10th attempt, 5 minutes after
/// that, 10 minutes once an hour of failures has passed.
fn retry_delay(attempt: u32, failing_for: Duration) -> Duration {
    if attempt <= 1 {
        Duration::from_secs(10)
    } else if failing_for >= Duration::from_secs(3600) {
        Duration::from_secs(600)
    } else if attempt > 10 {
        Duration::from_secs(300)
    } else {
        Duration::from_secs(60)
    }
}

enum Ended {
    Stopped,
    Lost(anyhow::Error),
}

async fn supervise(ctx: Ctx, mut control: mpsc::UnboundedReceiver<Control>) {
    let mut attempt = 0u32;
    let mut failing_since: Option<Instant> = None;
    let mut platform = ctx.platform;
    let mut adding = ctx.adding;
    let mut add_retried = false;
    let mut flow = Flow::default();
    loop {
        let mut connected = false;
        let ended = connect_and_run(&ctx, adding, &mut platform, &mut control, &mut connected, &mut flow).await;
        if connected {
            adding = false;
            attempt = 0;
            failing_since = None;
        }
        match ended {
            Ended::Stopped => break,
            Ended::Lost(e) => {
                tracing::warn!(error = format!("{e:#}"), "connection ended");
                ctx.emit(Event::Battery(None));
                ctx.emit(Event::Session(None));
                let restarting = flow.restart_until.is_some_and(|t| Instant::now() < t);
                if flow.restart_until.is_some() && !restarting {
                    flow.restart_until = None;
                    flow.pending = None;
                    ctx.firmware(Update::Failed("the watch did not come back after restarting".to_string()));
                }
                // The watch resets its side of the session only when the link drops.
                if let Ok(device) = ctx.bt.adapter.device(ctx.address) {
                    let _ = tokio::time::timeout(Duration::from_secs(5), device.disconnect()).await;
                }
                if adding {
                    // A failure after the bond leaves a paired watch: one more attempt connects it.
                    let bonded = match ctx.bt.adapter.device(ctx.address) {
                        Ok(d) => d.is_paired().await.unwrap_or(false),
                        Err(_) => false,
                    };
                    if bonded && !add_retried {
                        add_retried = true;
                        tracing::info!("bonded; trying once more");
                        sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    let _ = ctx.internal.send(Internal::AddFailed(e.to_string()));
                    break;
                }
                attempt += 1;
                let since = *failing_since.get_or_insert_with(Instant::now);
                // A restarting watch takes a minute or so; it is expected back sooner.
                let (delay, error) =
                    if restarting { (Duration::from_secs(10), None) } else { (retry_delay(attempt, since.elapsed()), Some(e.to_string())) };
                ctx.emit(Event::Link(LinkState::Disconnected { error, retry_in: Some(delay) }));
                let wake = Instant::now() + delay;
                loop {
                    tokio::select! {
                        _ = sleep(wake.saturating_duration_since(Instant::now())) => break,
                        c = control.recv() => match c {
                            Some(Control::Retry) => break,
                            Some(Control::Stop) | None => return,
                            Some(_) => {}
                        }
                    }
                }
            }
        }
    }
}

async fn connect_and_run(
    ctx: &Ctx,
    adding: bool,
    platform: &mut Option<AppPlatform>,
    control: &mut mpsc::UnboundedReceiver<Control>,
    connected: &mut bool,
    flow: &mut Flow,
) -> Ended {
    let address = ctx.address;
    let stage = |s: Stage| {
        ctx.emit(Event::Link(LinkState::Connecting(s)));
        if adding {
            ctx.emit(Event::PairingStage(s));
        }
    };
    let establish = link::establish(&ctx.bt.adapter, address, *platform, &ctx.server, stage);
    let link = tokio::select! {
        l = link::with_limit(Duration::from_secs(150), "connecting", establish) => match l {
            Ok(l) => l,
            Err(e) => return Ended::Lost(e),
        },
        c = wait_stop(control) => return c,
    };
    let mut link = link;
    let device = link.device.clone();
    let transport = link.transport;
    let mut battery = link.take_battery();
    let (protocol, opened, pump) = session::start(link);
    let mut early = protocol.subscribe();
    let pump = tokio::spawn(pump);
    let _pump_guard = AbortOnDrop(pump.abort_handle());

    ctx.emit(Event::Link(LinkState::Negotiating));
    let opened = tokio::select! {
        o = opened => match o {
            Ok(o) => o,
            Err(_) => return Ended::Lost(pump_error(pump).await),
        },
        c = wait_stop(control) => return c,
    };
    let info = tokio::select! {
        i = session::negotiate(&protocol, &mut early) => match i {
            Ok(i) => i,
            Err(e) => return Ended::Lost(e),
        },
        c = wait_stop(control) => return c,
    };
    drop(early);
    // Subscribed after the session start, so its frames are not handled twice.
    let mut frames = protocol.subscribe();
    let hardware = info.version.hardware();
    if hardware.app_platform.is_some() {
        *platform = hardware.app_platform;
    }
    pictures::set_watch(info.version.capabilities.contains(pebble_protocol::capabilities::Capability::NotificationImages));
    let recovery_mode = info.version.running.is_recovery();
    let mut session_info = SessionInfo {
        transport,
        ppog_version: opened.version,
        tx_window: opened.tx_window,
        rx_window: opened.rx_window,
        recovery_mode,
        unfaithful: info.version.unfaithful,
        capabilities: info.version.capabilities.iter().collect(),
        running_app: info.running_app,
        firmware_slot: info.version.running.slot(),
    };
    *connected = true;
    flow.restart_until = None;
    let watch = WatchFirmware { running: Running::from_info(&info.version.running), hardware_revision: hardware.revision.to_string() };
    let unfaithful = info.version.unfaithful == Some(true);
    let watch_id = apps::lock(&ctx.store).add_watch(&address.to_string(), chrono::Utc::now().timestamp()).ok();
    // Health needs PebbleOS, as apps do.
    let health_watch = watch_id.filter(|_| !recovery_mode).map(|id| health::Watch {
        id,
        profile: apps::lock(&ctx.store).watch(id).ok().flatten().map(|w| w.profile).unwrap_or(0),
        firmware: info.version.running.version().unwrap_or(system::Version { major: 0, minor: 0, patch: 0 }),
        heart_rate: pebble_protocol::hardware::has_heart_rate(info.model, hardware.app_platform),
        settings: session_info.capabilities.contains(&Capability::HealthInsights),
    });
    let watch_js = companion::watch_info(&info.version, info.model, hardware.app_platform.map(|p| p.codename()));
    let serial = info.version.serial.clone();
    let profile = watch_id.and_then(|id| apps::lock(&ctx.store).watch(id).ok().flatten()).map(|w| w.profile).unwrap_or(0);
    let _ = ctx.internal.send(Internal::Connected { address, info: Box::new(info) });
    ctx.emit(Event::Session(Some(session_info.clone())));
    ctx.emit(Event::Link(LinkState::Connected));

    let mut job: Option<Job> = None;
    match flow.pending.take() {
        Some(path) if watch.running.recovery => {
            job = Some(Job::install(ctx, &protocol, &watch, FirmwareSource::File(path), true));
        }
        Some(_) => ctx.firmware(Update::Failed("the watch did not restart into its recovery firmware".to_string())),
        None => job = Some(Job::check(ctx, &watch, false)),
    }

    // Apps need PebbleOS, and the platform to pick a build.
    let app_target = match (recovery_mode, watch_id, *platform) {
        (false, Some(id), Some(p)) => Some((id, p)),
        _ => None,
    };
    let mut app_sync: Option<Task<Result<apps::Synced>>> = None;
    // The script of the app in the foreground, and an app whose settings wait for its script.
    let start_script = |app: Option<uuid::Uuid>| match (app_target, app) {
        (Some(_), Some(u)) => companion::start(&ctx.store, u, profile, &watch_js, &serial),
        _ => None,
    };
    let mut script = start_script(session_info.running_app);
    let mut settings_for: Option<(uuid::Uuid, tokio::time::Instant)> = None;
    // When to ask a script Lithidion just opened for its settings: its watch app first gets a
    // moment to send what the script may need.
    let mut settings_at: Option<(uuid::Uuid, tokio::time::Instant)> = None;
    let mut sync_again = false;
    let mut phone_data_sent = false;
    let weather_v4 = session_info.capabilities.contains(&Capability::WeatherDbV4);
    let mut fetch: Option<Task<(uuid::Uuid, Result<()>)>> = None;
    let mut in_flight = InFlight::new(ctx);
    // The app just opened, until the watch asks for it or shows it already has it.
    let mut fetch_deadline = tokio::time::Instant::now();
    in_flight.online(app_target.is_some());
    // Quiet notifications stay quiet only with this watch setting on.
    if app_target.is_some() && session_info.capabilities.contains(&Capability::BlobDbVersion) {
        let protocol = protocol.clone();
        tokio::spawn(async move {
            use pebble_protocol::blobdb::{insert_with_timestamp, Database};
            let stamp = chrono::Utc::now().timestamp() as u32;
            match apps::blob(&protocol, |t| insert_with_timestamp(t, Database::Settings, stamp, notify::RESPECT_SILENCE_KEY, &[1])).await {
                Ok(s) => tracing::debug!(status = ?s, "respect phone silence set"),
                Err(e) => tracing::info!(error = %e, "respect phone silence not set"),
            }
        });
    }
    // The watch's own settings, while this session lasts.
    let _watch_prefs = match app_target {
        Some((id, _)) if session_info.capabilities.contains(&Capability::BlobDbVersion) => {
            let (protocol, db, events) = (protocol.clone(), ctx.store.clone(), ctx.events.clone());
            Some(Task(tokio::spawn(async move {
                if let Err(e) = watch_prefs::run(protocol, db, id, events).await {
                    tracing::info!(error = format!("{e:#}"), "watch settings sync ended");
                }
            })))
        }
        _ => None,
    };
    if let Some((id, p)) = app_target {
        let takeover = unfaithful || !apps::lock(&ctx.store).databases_cleared(id).unwrap_or(false);
        app_sync = Some(start_sync(ctx, &protocol, id, p, takeover));
    }
    let mut datalog = health::Receiver::default();
    let mut dictation = dictation::Dictation::default();
    // New health data is summed up once the watch has sent a batch; the watch's history is
    // brought up to date at most hourly, and when the day changes.
    let mut health_due: Option<tokio::time::Instant> = None;
    let mut stats_sent: Option<(Instant, chrono::NaiveDate)> = None;
    if let Some(h) = health_watch {
        let _ = ctx.internal.send(Internal::HealthSettable(h.settings));
        datalog.start(&protocol);
        health::request_sync(&protocol, &ctx.store, h.profile);
    }

    let mut time_sync = tokio::time::interval_at(Instant::now() + Duration::from_secs(3600), Duration::from_secs(3600));
    let gone = link::disconnected(&device);
    tokio::pin!(gone);
    loop {
        tokio::select! {
            _ = &mut gone => {
                if job.as_ref().is_some_and(|j| j.installing) {
                    ctx.firmware(Update::Failed("the watch disconnected during the update".to_string()));
                }
                return Ended::Lost(anyhow!("the watch disconnected"));
            }
            outcome = async { (&mut job.as_mut().expect("guarded").handle).await }, if job.is_some() => {
                job = None;
                match outcome {
                    Ok(Ok(Outcome::Checked)) => {}
                    Ok(Ok(Outcome::Installed)) => {
                        flow.restart_until = Some(Instant::now() + Duration::from_secs(300));
                        ctx.firmware(Update::Restarting { into_recovery: false });
                    }
                    Ok(Ok(Outcome::ViaRecovery(path))) => {
                        flow.pending = Some(path);
                        flow.restart_until = Some(Instant::now() + Duration::from_secs(300));
                        let _ = protocol.send(endpoint::RESET, &system::reset(Reset::IntoRecovery));
                        ctx.firmware(Update::Restarting { into_recovery: true });
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = format!("{e:#}"), "firmware update failed");
                        let _ = protocol.send(endpoint::SYSTEM_MESSAGE, &system::FIRMWARE_UPDATE_FAILED);
                        ctx.firmware(Update::Failed(format!("{e:#}")));
                    }
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => ctx.firmware(Update::Failed(format!("the update stopped: {e}"))),
                }
            }
            synced = async { (&mut app_sync.as_mut().expect("guarded").0).await }, if app_sync.is_some() => {
                app_sync = None;
                match synced {
                    Ok(Ok(s)) => {
                        if s.changed {
                            let _ = ctx.internal.send(Internal::AppsChanged);
                        }
                        for (uuid, title, why) in &s.failed {
                            if in_flight.queued == Some(*uuid) {
                                in_flight.queued = None;
                                ctx.emit(Event::App(*uuid, AppProgress::Failed(format!("the watch did not take it: {why}"))));
                            } else {
                                ctx.emit(Event::Error(format!("{title} could not be put on the watch: {why}")));
                            }
                        }
                        // An app added during this sync is sent by the next one: open it after that.
                        if !sync_again {
                            if let Some(u) = in_flight.queued.take() {
                                let _ = apps::launch(&protocol, u);
                                in_flight.wait_for(u);
                                fetch_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = format!("{e:#}"), "apps not synced");
                        match in_flight.queued.take() {
                            Some(u) => ctx.emit(Event::App(u, AppProgress::Failed(format!("{e:#}")))),
                            None => ctx.emit(Event::Error(format!("The watch's apps could not be updated: {e:#}"))),
                        }
                    }
                    Err(_) => {}
                }
                // The calendar and weather follow the first sync, which may clear the watch.
                if !phone_data_sent {
                    phone_data_sent = true;
                    if let Some((id, _)) = app_target {
                        spawn_phone_data(ctx, &protocol, id, true, true, weather_v4);
                    }
                    if let Some(h) = health_watch {
                        spawn_health(ctx, &protocol, h, true, true);
                        stats_sent = Some((Instant::now(), crate::host::Zone::now().date_naive()));
                    }
                }
                if std::mem::take(&mut sync_again) {
                    if let Some((id, p)) = app_target {
                        app_sync = Some(start_sync(ctx, &protocol, id, p, false));
                    }
                }
            }
            _ = tokio::time::sleep_until(fetch_deadline), if in_flight.waiting.is_some() => {
                // Opened without a fetch: the watch had it already.
                in_flight.finish_waiting();
            }
            fetched = async { (&mut fetch.as_mut().expect("guarded").0).await }, if fetch.is_some() => {
                fetch = None;
                in_flight.sending = None;
                if let Ok((uuid, result)) = fetched {
                    match result {
                        Ok(()) => ctx.emit(Event::App(uuid, AppProgress::Done)),
                        Err(e) => {
                            tracing::warn!(error = format!("{e:#}"), %uuid, "app not sent");
                            ctx.emit(Event::App(uuid, AppProgress::Failed(format!("{e:#}"))));
                        }
                    }
                }
            }
            Some(level) = battery.recv() => ctx.emit(Event::Battery(Some(level))),
            out = async { script.as_mut().expect("guarded").out.recv().await }, if script.is_some() => {
                let c = script.as_mut().expect("guarded");
                let (uuid, title) = (c.runner.uuid, c.runner.title.clone());
                match out {
                    None => script = None,
                    Some(Out::Ready) => {
                        if settings_for.is_some_and(|(u, _)| u == uuid) {
                            settings_at = Some((uuid, tokio::time::Instant::now() + SETTINGS_PAUSE));
                        }
                    }
                    Some(Out::Send { transaction, tuples }) => c.send(&protocol, transaction, tuples),
                    Some(Out::Notify { title: heading, body }) => {
                        // Through the phone's own path, so the app can be muted like any other.
                        let id = u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().expect("four bytes")) | 0x8000_0000;
                        let _ = ctx.internal.send(Internal::Phone(desktop::PhoneEvent::Posted(desktop::Posted {
                            id,
                            replaces: 0,
                            app_id: format!("pebble:{uuid}"),
                            app_name: title,
                            title: heading,
                            body,
                            urgency: 1,
                            category: None,
                            icon: None,
                            picture: None,
                        })));
                    }
                    Some(Out::Open(url)) => {
                        let asked = c.settings_asked.take().is_some_and(|t| t.elapsed() < Duration::from_secs(30));
                        if asked {
                            settings_for = None;
                            ctx.emit(Event::AppSettings { uuid, title, url });
                        } else {
                            tracing::info!(%uuid, "a page the script opened on its own was not shown");
                        }
                    }
                    Some(Out::WantsNetwork) => ctx.emit(Event::WantsNetwork { uuid, title }),
                }
            }
            _ = async { tokio::time::sleep_until(settings_at.expect("guarded").1).await }, if settings_at.is_some() => {
                if let (Some((uuid, _)), Some(c)) = (settings_at.take(), script.as_mut()) {
                    if c.runner.uuid == uuid {
                        c.settings_asked = Some(tokio::time::Instant::now());
                        c.runner.send(pkjs::In::ShowSettings);
                    }
                }
            }
            _ = async { tokio::time::sleep_until(settings_for.expect("guarded").1).await }, if settings_for.is_some() => {
                if let Some((uuid, _)) = settings_for.take() {
                    let title = apps::lock(&ctx.store).app_title(uuid).unwrap_or_else(|| "The app".to_string());
                    ctx.emit(Event::Error(format!("{title} did not open its settings")));
                }
            }
            _ = async { tokio::time::sleep_until(health_due.expect("guarded")).await }, if health_due.is_some() => {
                health_due = None;
                if let Some(h) = health_watch {
                    let _ = ctx.internal.send(Internal::HealthData(h.profile));
                    let today = crate::host::Zone::now().date_naive();
                    let stale = stats_sent.is_none_or(|(at, day)| at.elapsed() > Duration::from_secs(3600) || day != today);
                    // Before the first app sync, which may clear the watch, it would be undone.
                    if stale && phone_data_sent {
                        spawn_health(ctx, &protocol, h, false, true);
                        stats_sent = Some((Instant::now(), today));
                    }
                }
            }
            frame = frames.recv() => match frame {
                Ok(f) if f.endpoint == endpoint::APP_MESSAGE => match pebble_protocol::appmessage::parse(&f.payload) {
                    Ok(pebble_protocol::appmessage::Message::Push { transaction, uuid, tuples }) => {
                        use pebble_protocol::appmessage::{ack, nack};
                        match &script {
                            Some(c) if c.runner.uuid == uuid => {
                                let _ = protocol.send(endpoint::APP_MESSAGE, &ack(transaction));
                                c.runner.send(pkjs::In::Message(tuples));
                            }
                            _ => {
                                tracing::debug!(%uuid, "an app message with no script to take it");
                                let _ = protocol.send(endpoint::APP_MESSAGE, &nack(transaction));
                            }
                        }
                    }
                    // Answers, taken by whoever sent the message.
                    Ok(_) => {}
                    Err(e) => tracing::debug!(error = %e, "app message not understood"),
                },
                Ok(f) if f.endpoint == endpoint::DATA_LOG => {
                    let id = health_watch.map(|h| (h.profile, h.id));
                    if let health::Received::Health { last: true } = datalog.handle(&protocol, &ctx.store, id, &f.payload) {
                        health_due.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(5));
                    }
                }
                Ok(f) if f.endpoint == endpoint::HEALTH_SYNC => {
                    tracing::debug!(acknowledged = ?pebble_protocol::health::parse_sync_response(&f.payload), "health sync answered");
                }
                Ok(f) if f.endpoint == endpoint::TIMELINE_ACTIONS => {
                    use pebble_protocol::timeline::{self, icon};
                    let Ok(invoke) = timeline::parse_invoke(&f.payload) else { continue };
                    let (ok, glyph, text) = match invoke.action {
                        notify::ACTION_DISMISS => (true, icon::RESULT_DISMISSED, "Dismissed"),
                        notify::ACTION_MUTE => (true, icon::RESULT_MUTE, "Muted"),
                        _ => (false, icon::RESULT_FAILED, "Not available"),
                    };
                    let _ = protocol.send(endpoint::TIMELINE_ACTIONS, &timeline::action_response(invoke.item, ok, glyph, text));
                    let _ = ctx.internal.send(Internal::WatchAction(invoke));
                }
                Ok(f) if f.endpoint == endpoint::IMAGING => pictures::request(&protocol, &f.payload),
                Ok(f) if f.endpoint == endpoint::VOICE_CONTROL => dictation.voice(&protocol, &f.payload),
                Ok(f) if f.endpoint == endpoint::AUDIO_STREAM => dictation.audio(&protocol, &f.payload),
                Ok(f) if f.endpoint == endpoint::MUSIC_CONTROL => {
                    if let Ok(Some(button)) = pebble_protocol::music::parse(&f.payload) {
                        tracing::debug!(?button, "music button");
                        let _ = ctx.internal.send(Internal::MusicButton(button));
                    }
                }
                Ok(f) if f.endpoint == endpoint::PHONE_CONTROL => {
                    if let Ok(Some(request)) = pebble_protocol::phone::parse(&f.payload) {
                        tracing::info!(?request, "call request from the watch");
                        let _ = ctx.internal.send(Internal::CallRequest(request));
                    }
                }
                Ok(f) if f.endpoint == endpoint::APP_FETCH => {
                    let Ok(request) = pebble_protocol::apps::FetchRequest::parse(&f.payload) else { continue };
                    tracing::info!(uuid = %request.uuid, app_id = request.app_id, "the watch asks for an app");
                    let busy = fetch.is_some() || job.as_ref().is_some_and(|j| j.installing);
                    match app_target {
                        Some((id, p)) if !busy => {
                            if in_flight.waiting == Some(request.uuid) {
                                in_flight.waiting = None;
                            }
                            in_flight.sending = Some(request.uuid);
                            fetch = Some(start_fetch(ctx, &protocol, id, p, request));
                        }
                        _ => {
                            let _ = protocol.send(endpoint::APP_FETCH, &pebble_protocol::apps::fetch_response(pebble_protocol::apps::FetchResult::Busy));
                        }
                    }
                }
                Ok(f) => {
                    if let Unprompted::RunningApp(app) = session::handle(&protocol, &f) {
                        if script.as_ref().map(|c| c.runner.uuid) != app {
                            drop(script.take());
                            script = start_script(app);
                        }
                        session_info.running_app = app;
                        ctx.emit(Event::Session(Some(session_info.clone())));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return Ended::Lost(anyhow!("the session ended")),
            },
            _ = time_sync.tick() => {
                let _ = protocol.send(endpoint::TIME, &session::time_message());
                let today = crate::host::Zone::now().date_naive();
                if let (Some(h), Some((_, day))) = (health_watch, stats_sent) {
                    if day != today {
                        spawn_health(ctx, &protocol, h, true, true);
                        stats_sent = Some((Instant::now(), today));
                    }
                }
            }
            c = control.recv() => match c {
                Some(Control::Stop) | None => return Ended::Stopped,
                Some(Control::Retry) => {}
                Some(Control::Ping) => {
                    let protocol = protocol.clone();
                    let events = ctx.events.clone();
                    tokio::spawn(async move {
                        let message = match session::ping(&protocol).await {
                            Ok(d) => format!("The watch answered in {} ms", d.as_millis()),
                            Err(e) => format!("No answer: {e:#}"),
                        };
                        let _ = events.send(Event::Toast(message)).await;
                    });
                }
                Some(Control::SaveDiagnostic(kind, path)) => {
                    let protocol = protocol.clone();
                    let events = ctx.events.clone();
                    tokio::spawn(async move {
                        // Lines a hundred at a time, or whole percents.
                        let shown = std::sync::atomic::AtomicI64::new(-1);
                        let progress = |p: f64| {
                            let step = if kind == Diagnostic::Logs { (p / 100.0) as i64 } else { (p * 100.0) as i64 };
                            if shown.swap(step, std::sync::atomic::Ordering::Relaxed) != step {
                                let _ = events.try_send(Event::Diagnostic(kind, Some(p)));
                            }
                        };
                        let result = match kind {
                            Diagnostic::Logs => diagnostics::logs(&protocol, &path, |n| progress(n as f64)).await,
                            Diagnostic::CoreDump => diagnostics::core_dump(&protocol, &path, progress).await,
                        };
                        let what = match kind {
                            Diagnostic::Logs => "log",
                            Diagnostic::CoreDump => "core dump",
                        };
                        let message = match result {
                            Ok(diagnostics::Saved::Done) => format!("The watch's {what} was saved"),
                            Ok(diagnostics::Saved::Nothing) => format!("The watch has no {what}"),
                            Err(e) => format!("The {what} was not saved: {e:#}"),
                        };
                        let _ = events.send(Event::Diagnostic(kind, None)).await;
                        let _ = events.send(Event::Toast(message)).await;
                    });
                }
                Some(Control::SyncTime) => {
                    if protocol.send(endpoint::TIME, &session::time_message()).is_ok() {
                        ctx.emit(Event::Toast("Time sent to the watch".to_string()));
                    }
                }
                Some(Control::SyncApps(launch)) => {
                    if let Some(new) = launch {
                        // Only the latest one is opened; an earlier one is simply on the watch.
                        if let Some(old) = in_flight.queued.replace(new).filter(|o| *o != new) {
                            ctx.emit(Event::App(old, AppProgress::Done));
                        }
                    }
                    match (app_sync.is_some(), app_target) {
                        (true, _) => sync_again = true,
                        (false, Some((id, p))) => app_sync = Some(start_sync(ctx, &protocol, id, p, false)),
                        (false, None) => {}
                    }
                }
                Some(Control::SendAppOrder) => {
                    if let Some((id, _)) = app_target {
                        let (protocol, db) = (protocol.clone(), ctx.store.clone());
                        tokio::spawn(async move { apps::send_order(&protocol, &db, id).await });
                    }
                }
                Some(Control::LaunchApp(uuid)) => {
                    let _ = apps::launch(&protocol, uuid);
                }
                Some(Control::AppSettings(uuid)) => match script.as_mut() {
                    Some(c) if c.runner.uuid == uuid => {
                        c.settings_asked = Some(tokio::time::Instant::now());
                        c.runner.send(pkjs::In::ShowSettings);
                        settings_for = Some((uuid, tokio::time::Instant::now() + Duration::from_secs(15)));
                    }
                    _ if app_target.is_some() => {
                        // Its script starts when the watch opens it; the page is asked for once
                        // the script is ready and the app has had a moment.
                        let _ = apps::launch(&protocol, uuid);
                        settings_for = Some((uuid, tokio::time::Instant::now() + Duration::from_secs(20)));
                    }
                    _ => {}
                },
                Some(Control::AppSettingsClosed(uuid, response)) => {
                    if let Some(c) = script.as_ref().filter(|c| c.runner.uuid == uuid) {
                        c.runner.send(pkjs::In::SettingsClosed(response));
                    }
                }
                Some(Control::Network(uuid, allowed)) => {
                    if let Some(c) = script.as_ref().filter(|c| c.runner.uuid == uuid) {
                        c.set_network(allowed);
                    }
                }
                Some(Control::Notify { key, value }) => {
                    if app_target.is_some() {
                        let protocol = protocol.clone();
                        tokio::spawn(async move {
                            use pebble_protocol::blobdb::{insert, Database, Status};
                            match apps::blob(&protocol, |t| insert(t, Database::Notifications, key.as_bytes(), &value)).await {
                                Ok(Status::Success) => {}
                                Ok(s) => tracing::info!(status = ?s, "the watch did not take a notification"),
                                Err(e) => tracing::info!(error = %e, "notification not sent"),
                            }
                        });
                    }
                }
                Some(Control::Call(payload)) => {
                    let _ = protocol.send(endpoint::PHONE_CONTROL, &payload);
                }
                Some(Control::SyncCalendar) => {
                    if let Some((id, _)) = app_target {
                        spawn_phone_data(ctx, &protocol, id, true, false, weather_v4);
                    }
                }
                Some(Control::SyncWeather) => {
                    if let Some((id, _)) = app_target {
                        spawn_phone_data(ctx, &protocol, id, false, true, weather_v4);
                    }
                }
                Some(Control::SyncHealthSettings) => {
                    if let Some(h) = health_watch {
                        spawn_health(ctx, &protocol, h, true, false);
                    }
                }
                Some(Control::SyncHealthStats) => {
                    if let Some(h) = health_watch.filter(|_| phone_data_sent) {
                        spawn_health(ctx, &protocol, h, false, true);
                        stats_sent = Some((Instant::now(), crate::host::Zone::now().date_naive()));
                    }
                }
                Some(Control::RequestHealth) => {
                    if let Some(h) = health_watch {
                        health::request_sync(&protocol, &ctx.store, h.profile);
                    }
                }
                Some(Control::Music(messages)) => {
                    if app_target.is_some() {
                        for m in messages {
                            let _ = protocol.send(endpoint::MUSIC_CONTROL, &m);
                        }
                    }
                }
                Some(Control::CheckFirmware) => {
                    if !job.as_ref().is_some_and(|j| j.installing) {
                        job = Some(Job::check(ctx, &watch, true));
                    }
                }
                Some(Control::InstallFirmware(source)) => {
                    if job.as_ref().is_some_and(|j| j.installing) {
                        ctx.emit(Event::Toast("An update is already being installed".to_string()));
                    } else {
                        job = Some(Job::install(ctx, &protocol, &watch, source, false));
                    }
                }
                Some(Control::Reset(kind)) => {
                    let _ = protocol.send(endpoint::RESET, &system::reset(kind));
                    ctx.emit(Event::Toast(match kind {
                        Reset::IntoRecovery => "The watch is restarting into recovery firmware".to_string(),
                        _ => "The watch is restarting".to_string(),
                    }));
                }
            },
        }
    }
}

/// Apps whose progress the UI is showing while this connection lasts; whatever is left when it
/// ends is reported done, as the app is saved and goes on the watch at the next connection.
struct InFlight {
    events: async_channel::Sender<Event>,
    online: Arc<AtomicBool>,
    /// Added, to be opened once a sync has put it on the watch.
    queued: Option<uuid::Uuid>,
    waiting: Option<uuid::Uuid>,
    sending: Option<uuid::Uuid>,
}

impl InFlight {
    fn new(ctx: &Ctx) -> InFlight {
        InFlight { events: ctx.events.clone(), online: ctx.apps_online.clone(), queued: None, waiting: None, sending: None }
    }

    fn online(&self, online: bool) {
        self.online.store(online, Ordering::Relaxed);
    }

    fn wait_for(&mut self, uuid: uuid::Uuid) {
        self.finish_waiting();
        self.waiting = Some(uuid);
    }

    fn finish_waiting(&mut self) {
        if let Some(u) = self.waiting.take() {
            let _ = self.events.try_send(Event::App(u, AppProgress::Done));
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.online.store(false, Ordering::Relaxed);
        self.finish_waiting();
        for u in [self.queued.take(), self.sending.take()].into_iter().flatten() {
            let _ = self.events.try_send(Event::App(u, AppProgress::Done));
        }
    }
}

enum AppSource {
    Store(Box<StoreApp>),
    File(PathBuf),
}

/// A task stopped when dropped.
struct Task<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Sends the calendar and the weather, one sync of each at a time.
fn spawn_phone_data(ctx: &Ctx, protocol: &session::Protocol, watch: WatchId, calendar: bool, weather: bool, weather_v4: bool) {
    let (protocol, db, lock) = (protocol.clone(), ctx.store.clone(), ctx.phone_data_lock.clone());
    tokio::spawn(async move {
        let _guard = lock.lock().await;
        if calendar {
            if let Err(e) = phonedata::sync_calendar(&protocol, &db, watch).await {
                tracing::warn!(error = format!("{e:#}"), "calendar not sent");
            }
        }
        if weather {
            if let Err(e) = phonedata::sync_weather(&protocol, &db, watch, weather_v4).await {
                tracing::warn!(error = format!("{e:#}"), "weather not sent");
            }
        }
    });
}

fn spawn_health(ctx: &Ctx, protocol: &session::Protocol, watch: health::Watch, settings: bool, stats: bool) {
    let (protocol, db, lock) = (protocol.clone(), ctx.store.clone(), ctx.phone_data_lock.clone());
    tokio::spawn(async move {
        let _guard = lock.lock().await;
        if settings {
            if let Err(e) = health::sync_settings(&protocol, &db, watch).await {
                tracing::warn!(error = format!("{e:#}"), "health settings not sent");
            }
        }
        if stats {
            if let Err(e) = health::sync_stats(&protocol, &db, watch).await {
                tracing::warn!(error = format!("{e:#}"), "health history not sent");
            }
        }
    });
}

fn start_sync(
    ctx: &Ctx,
    protocol: &session::Protocol,
    watch: WatchId,
    platform: AppPlatform,
    takeover: bool,
) -> Task<Result<apps::Synced>> {
    let (protocol, db) = (protocol.clone(), ctx.store.clone());
    Task(tokio::spawn(async move { apps::sync(&protocol, &db, watch, platform, takeover).await }))
}

fn start_fetch(
    ctx: &Ctx,
    protocol: &session::Protocol,
    watch: WatchId,
    platform: AppPlatform,
    request: pebble_protocol::apps::FetchRequest,
) -> Task<(uuid::Uuid, Result<()>)> {
    let (protocol, db, events) = (protocol.clone(), ctx.store.clone(), ctx.events.clone());
    Task(tokio::spawn(async move {
        let uuid = request.uuid;
        let mut last = -1.0;
        let progress = |f: f64| {
            if f - last >= 0.02 {
                last = f;
                let _ = events.try_send(Event::App(uuid, AppProgress::Sending(f)));
            }
        };
        (uuid, apps::send_app(&protocol, &db, watch, platform, request, progress).await)
    }))
}

/// Firmware work that outlives one connection: a restart the supervisor waits for, and a
/// bundle to install once the watch is in recovery firmware.
#[derive(Default)]
struct Flow {
    restart_until: Option<Instant>,
    pending: Option<PathBuf>,
}

#[derive(Clone)]
struct WatchFirmware {
    running: Running,
    hardware_revision: String,
}

enum Outcome {
    Checked,
    Installed,
    ViaRecovery(PathBuf),
}

/// An update check or install, stopped when dropped.
struct Job {
    handle: tokio::task::JoinHandle<Result<Outcome>>,
    installing: bool,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Job {
    fn check(ctx: &Ctx, watch: &WatchFirmware, fresh: bool) -> Job {
        let (events, updating, releases, watch) = (ctx.events.clone(), ctx.updating.clone(), ctx.releases.clone(), watch.clone());
        let handle = tokio::spawn(async move {
            let emit = |u| {
                report(&events, &updating, u);
            };
            emit(Update::Checking);
            match check_update(&releases, &watch, fresh).await {
                Ok(u) => emit(u),
                Err(e) => {
                    tracing::info!(error = format!("{e:#}"), "update check failed");
                    emit(Update::CheckFailed(format!("{e:#}")));
                }
            }
            Ok(Outcome::Checked)
        });
        Job { handle, installing: false }
    }

    fn install(ctx: &Ctx, protocol: &session::Protocol, watch: &WatchFirmware, source: FirmwareSource, resumed: bool) -> Job {
        let (events, updating, releases, watch, protocol) =
            (ctx.events.clone(), ctx.updating.clone(), ctx.releases.clone(), watch.clone(), protocol.clone());
        let handle = tokio::spawn(async move {
            let emit = move |u| {
                report(&events, &updating, u);
            };
            let path = match source {
                FirmwareSource::File(p) => p,
                FirmwareSource::Latest | FirmwareSource::LatestRecovery => {
                    let kind = if source == FirmwareSource::Latest { firmware::Kind::Normal } else { firmware::Kind::Recovery };
                    let client = firmware::client()?;
                    let release = latest_release(&releases, &client, true).await?;
                    let asset = firmware::asset(&release, kind, &watch.hardware_revision)
                        .ok_or_else(|| anyhow!("PebbleOS {} has no build for {}", release.tag_name, watch.hardware_revision))?;
                    emit(Update::Downloading(0.0));
                    let mut last = 0.0;
                    firmware::download(&client, asset, |f| {
                        // About a hundred updates for the whole download.
                        if f - last >= 0.01 || f >= 1.0 {
                            last = f;
                            emit(Update::Downloading(f));
                        }
                    })
                    .await?
                }
            };
            emit(Update::Installing(0.0));
            let builds = {
                let path = path.clone();
                tokio::task::spawn_blocking(move || firmware::read_bundle(&path)).await??
            };
            let index = match route(&builds, &watch.running) {
                None => bail_route(&builds)?,
                Some(Route::ViaRecovery(i)) if !resumed => {
                    tracing::info!(build = %builds[i].folder, "installing through recovery firmware");
                    return Ok(Outcome::ViaRecovery(path));
                }
                Some(Route::ViaRecovery(_)) => return Err(anyhow!("the watch is not in its recovery firmware")),
                Some(Route::Direct(i)) => i,
            };
            let build = &builds[index];
            if build.manifest.is_recovery() && watch.running.recovery {
                return Err(anyhow!("the recovery firmware cannot be replaced while it is running; restart into PebbleOS first"));
            }
            pebble_protocol::firmware::check(build, &watch.hardware_revision, watch.running.target_slot()).map_err(|e| anyhow!(e))?;
            tracing::info!(
                build = %build.folder,
                tag = ?build.manifest.firmware.version_tag,
                kind = %build.manifest.firmware.kind,
                size = build.manifest.total_size(),
                "installing firmware"
            );
            let mut last = 0.0;
            firmware::install(&protocol, build, |f| {
                if f - last >= 0.005 || f >= 1.0 {
                    last = f;
                    emit(Update::Installing(f));
                }
            })
            .await?;
            Ok(Outcome::Installed)
        });
        Job { handle, installing: true }
    }
}

fn bail_route(builds: &[pebble_protocol::firmware::Build]) -> Result<usize> {
    let slots: Vec<_> = builds.iter().map(|b| b.manifest.firmware.slot).collect();
    Err(anyhow!("this bundle has no build this watch can install now (slots {slots:?})"))
}

async fn latest_release(
    cache: &Mutex<Option<(Instant, firmware::Release)>>,
    client: &reqwest::Client,
    fresh: bool,
) -> Result<firmware::Release> {
    let cached = cache.lock().expect("not poisoned").clone();
    if let Some((at, release)) = cached {
        // GitHub allows 60 unauthenticated requests an hour.
        if !fresh && at.elapsed() < Duration::from_secs(6 * 3600) || at.elapsed() < Duration::from_secs(60) {
            return Ok(release);
        }
    }
    let release = firmware::latest_release(client).await?;
    *cache.lock().expect("not poisoned") = Some((Instant::now(), release.clone()));
    Ok(release)
}

async fn check_update(cache: &Mutex<Option<(Instant, firmware::Release)>>, watch: &WatchFirmware, fresh: bool) -> Result<Update> {
    let client = firmware::client()?;
    let release = latest_release(cache, &client, fresh).await?;
    let Some(asset) = firmware::asset(&release, firmware::Kind::Normal, &watch.hardware_revision) else {
        return Err(anyhow!("PebbleOS {} has no build for {}", release.tag_name, watch.hardware_revision));
    };
    let available = Update::Available { tag: release.tag_name.clone(), size: asset.size, missing: watch.running.recovery };
    if watch.running.recovery {
        return Ok(available);
    }
    let newer = match (system::parse_version(&watch.running.tag), system::parse_version(&release.tag_name)) {
        (Some(running), Some(latest)) => latest > running,
        _ => false,
    };
    Ok(if newer { available } else { Update::UpToDate })
}

async fn wait_stop(control: &mut mpsc::UnboundedReceiver<Control>) -> Ended {
    loop {
        match control.recv().await {
            Some(Control::Stop) | None => return Ended::Stopped,
            Some(_) => {}
        }
    }
}

async fn pump_error(pump: tokio::task::JoinHandle<Result<()>>) -> anyhow::Error {
    match pump.await {
        Ok(Err(e)) => e,
        Ok(Ok(())) => anyhow!("the session ended"),
        Err(e) => anyhow!("the session failed: {e}"),
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod profile_flow {
    use super::*;

    /// Adding a profile through the backend, as the UI does; no Bluetooth needed.
    #[test]
    fn add_profile_reaches_the_ui() {
        let dir = std::env::temp_dir().join(format!("lithidion-profile-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("test.db");
        let _ = std::fs::remove_file(&db);
        let (tx, rx) = spawn(db.clone());
        tx.send_blocking(Command::AddProfile("AJ".into(), crate::settings::HealthSettings::default())).unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(8);
        let (mut active, mut health) = (None, false);
        while std::time::Instant::now() < end {
            match rx.try_recv() {
                Ok(Event::Profiles { active: a, .. }) => active = a,
                // A deadlock here once left the Health tab without a profile.
                Ok(Event::Health(v)) => health = v.is_some(),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
            if active.is_some() && health {
                break;
            }
        }
        assert!(active.is_some());
        assert!(health);
        let store = Store::open(&db).unwrap();
        assert_eq!(store.profiles().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
