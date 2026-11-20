//! Local SQLite store: known watches, settings, and apps. Each watch's own data hangs off its id;
//! an app's bundle is kept once however many watches have it.

use anyhow::{Context, Result};
use pebble_protocol::health::{Minute, Session, SessionKind};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Applied in order; `PRAGMA user_version` counts how many have run.
const MIGRATIONS: &[&str] = &[
    "
CREATE TABLE watches (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    address           TEXT NOT NULL UNIQUE,
    model             INTEGER,
    hardware          INTEGER,
    serial            TEXT,
    board             TEXT,
    firmware          TEXT,
    recovery_firmware TEXT,
    language          TEXT,
    added             INTEGER NOT NULL,
    last_connected    INTEGER
);
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
",
    "
CREATE TABLE apps (
    uuid         TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    title        TEXT NOT NULL,
    developer    TEXT NOT NULL,
    version      TEXT NOT NULL,
    hidden       INTEGER NOT NULL DEFAULT 0,
    store_source TEXT,
    store_id     TEXT,
    image        TEXT,
    added        INTEGER NOT NULL
);
CREATE TABLE watch_apps (
    watch_id INTEGER NOT NULL,
    uuid     TEXT NOT NULL,
    position INTEGER NOT NULL,
    state    INTEGER NOT NULL,
    PRIMARY KEY (watch_id, uuid)
);
ALTER TABLE watches ADD COLUMN databases_cleared INTEGER NOT NULL DEFAULT 0;
",
    "
CREATE TABLE notification_apps (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    mode       INTEGER NOT NULL DEFAULT 1,
    last_seen  INTEGER NOT NULL
);
CREATE TABLE notifications (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    app_id   TEXT NOT NULL,
    title    TEXT NOT NULL,
    body     TEXT NOT NULL,
    time     INTEGER NOT NULL,
    outcome  INTEGER NOT NULL
);
CREATE INDEX notifications_time ON notifications (time);
",
    "
CREATE TABLE watch_timeline (
    watch_id INTEGER NOT NULL,
    db       INTEGER NOT NULL,
    key      TEXT NOT NULL,
    hash     INTEGER NOT NULL,
    PRIMARY KEY (watch_id, db, key)
);
",
    "
CREATE TABLE health_minutes (
    watch_id     INTEGER NOT NULL,
    time         INTEGER NOT NULL,
    steps        INTEGER NOT NULL,
    orientation  INTEGER NOT NULL,
    vmc          INTEGER NOT NULL,
    light        INTEGER NOT NULL,
    flags        INTEGER NOT NULL,
    resting_cal  INTEGER NOT NULL,
    active_cal   INTEGER NOT NULL,
    distance_cm  INTEGER NOT NULL,
    heart_rate   INTEGER NOT NULL,
    hr_weight    INTEGER NOT NULL,
    hr_zone      INTEGER NOT NULL,
    spo2         INTEGER NOT NULL,
    PRIMARY KEY (watch_id, time)
) WITHOUT ROWID;
CREATE TABLE health_sessions (
    watch_id     INTEGER NOT NULL,
    start        INTEGER NOT NULL,
    kind         INTEGER NOT NULL,
    seconds      INTEGER NOT NULL,
    utc_to_local INTEGER NOT NULL,
    steps        INTEGER NOT NULL,
    active_kcal  INTEGER NOT NULL,
    resting_kcal INTEGER NOT NULL,
    distance_m   INTEGER NOT NULL,
    PRIMARY KEY (watch_id, start, kind)
) WITHOUT ROWID;
",
    // Profiles. Rows already there get profile 0 ("not yet in a profile"), which the first
    // profile created takes over.
    "
CREATE TABLE profiles (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    name  TEXT NOT NULL,
    added INTEGER NOT NULL
);
ALTER TABLE watches ADD COLUMN profile_id INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notification_apps RENAME TO notification_apps_old;
CREATE TABLE notification_apps (
    profile_id INTEGER NOT NULL,
    id         TEXT NOT NULL,
    name       TEXT NOT NULL,
    mode       INTEGER NOT NULL DEFAULT 1,
    last_seen  INTEGER NOT NULL,
    icon       TEXT,
    PRIMARY KEY (profile_id, id)
);
INSERT INTO notification_apps (profile_id, id, name, mode, last_seen)
    SELECT 0, id, name, mode, last_seen FROM notification_apps_old;
DROP TABLE notification_apps_old;
ALTER TABLE notifications ADD COLUMN profile_id INTEGER NOT NULL DEFAULT 0;
ALTER TABLE health_minutes RENAME TO health_minutes_old;
CREATE TABLE health_minutes (
    profile_id   INTEGER NOT NULL,
    time         INTEGER NOT NULL,
    watch_id     INTEGER NOT NULL,
    steps        INTEGER NOT NULL,
    orientation  INTEGER NOT NULL,
    vmc          INTEGER NOT NULL,
    light        INTEGER NOT NULL,
    flags        INTEGER NOT NULL,
    resting_cal  INTEGER NOT NULL,
    active_cal   INTEGER NOT NULL,
    distance_cm  INTEGER NOT NULL,
    heart_rate   INTEGER NOT NULL,
    hr_weight    INTEGER NOT NULL,
    hr_zone      INTEGER NOT NULL,
    spo2         INTEGER NOT NULL,
    PRIMARY KEY (profile_id, time)
) WITHOUT ROWID;
INSERT OR IGNORE INTO health_minutes
    SELECT 0, time, watch_id, steps, orientation, vmc, light, flags, resting_cal, active_cal,
           distance_cm, heart_rate, hr_weight, hr_zone, spo2
    FROM health_minutes_old;
DROP TABLE health_minutes_old;
ALTER TABLE health_sessions RENAME TO health_sessions_old;
CREATE TABLE health_sessions (
    profile_id   INTEGER NOT NULL,
    start        INTEGER NOT NULL,
    kind         INTEGER NOT NULL,
    watch_id     INTEGER NOT NULL,
    seconds      INTEGER NOT NULL,
    utc_to_local INTEGER NOT NULL,
    steps        INTEGER NOT NULL,
    active_kcal  INTEGER NOT NULL,
    resting_kcal INTEGER NOT NULL,
    distance_m   INTEGER NOT NULL,
    PRIMARY KEY (profile_id, start, kind)
) WITHOUT ROWID;
INSERT OR IGNORE INTO health_sessions
    SELECT 0, start, kind, watch_id, seconds, utc_to_local, steps, active_kcal, resting_kcal, distance_m
    FROM health_sessions_old;
DROP TABLE health_sessions_old;
",
    "ALTER TABLE watches ADD COLUMN nickname TEXT;",
    "
-- NULL until the app's bundle has been read.
ALTER TABLE apps ADD COLUMN script INTEGER;
ALTER TABLE apps ADD COLUMN configurable INTEGER NOT NULL DEFAULT 0;
-- PebbleKit JS localStorage, per profile and app.
CREATE TABLE script_storage (
    profile_id INTEGER NOT NULL,
    uuid       TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    PRIMARY KEY (profile_id, uuid, key)
);
",
    "
CREATE TABLE watch_prefs (
    watch_id INTEGER NOT NULL,
    key      TEXT NOT NULL,
    value    BLOB NOT NULL,
    stamp    INTEGER NOT NULL,
    pending  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (watch_id, key)
);
",
];

/// How one app's notifications reach the watch.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyMode {
    Off = 0,
    /// Sent, quietly while the phone is in Do Not Disturb.
    On = 1,
    /// Sent with an alert even in Do Not Disturb.
    Always = 2,
}

impl NotifyMode {
    pub fn from_i64(v: i64) -> NotifyMode {
        match v {
            0 => NotifyMode::Off,
            2 => NotifyMode::Always,
            _ => NotifyMode::On,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct NotifyApp {
    pub id: String,
    pub name: String,
    pub mode: NotifyMode,
    pub last_seen: i64,
    /// A cached image file, or an icon name.
    pub icon: Option<String>,
}

/// What became of a notification.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Sent = 0,
    /// Sent without an alert (Do Not Disturb).
    SentQuietly = 1,
    /// The app is switched off.
    AppOff = 2,
    /// Forwarding is switched off.
    Off = 3,
    /// No watch was connected.
    Away = 4,
    /// No watch was connected, and it was sent when the watch came back.
    SentLate = 5,
}

impl Outcome {
    fn from_i64(v: i64) -> Outcome {
        match v {
            0 => Outcome::Sent,
            1 => Outcome::SentQuietly,
            2 => Outcome::AppOff,
            3 => Outcome::Off,
            5 => Outcome::SentLate,
            _ => Outcome::Away,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct NotifyEntry {
    pub app: String,
    pub icon: Option<String>,
    pub title: String,
    pub body: String,
    pub time: i64,
    pub outcome: Outcome,
}

/// History keeps a week, and at most this many.
pub const HISTORY_DAYS: i64 = 7;
pub const HISTORY_MAX: i64 = 200;

/// Where an app stands on one watch.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnWatch {
    /// Not yet in the watch's app database.
    ToInsert = 0,
    Inserted = 1,
    /// Removed here, still to be removed from the watch.
    ToDelete = 2,
}

impl OnWatch {
    fn from_i64(v: i64) -> OnWatch {
        match v {
            1 => OnWatch::Inserted,
            2 => OnWatch::ToDelete,
            _ => OnWatch::ToInsert,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Watchface,
    Watchapp,
}

impl AppKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AppKind::Watchface => "watchface",
            AppKind::Watchapp => "watchapp",
        }
    }

    pub fn parse(s: &str) -> Option<AppKind> {
        match s {
            "watchface" => Some(AppKind::Watchface),
            "watchapp" => Some(AppKind::Watchapp),
            _ => None,
        }
    }
}

/// An app known here; its bundle is `apps/<uuid>.pbw` in the data folder.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct App {
    pub uuid: uuid::Uuid,
    pub kind: AppKind,
    pub title: String,
    pub developer: String,
    pub version: String,
    pub hidden: bool,
    /// "pebble" or "rebble", with the app's id there.
    pub store: Option<(String, String)>,
    /// A store image: the list image, or a screenshot for faces.
    pub image: Option<String>,
    pub added: i64,
    /// Has a phone-side script (PebbleKit JS).
    pub script: bool,
    /// Its script offers a settings page.
    pub configurable: bool,
}

/// An app on one watch, in the watch's order.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct WatchApp {
    pub app: App,
    pub position: i64,
    pub state: OnWatch,
}

pub type WatchId = i64;
pub type ProfileId = i64;

const ACTIVE_PROFILE: &str = "active_profile";

/// Settings kept per profile, under `<key>:p<id>` (see `settings::profile_key`).
pub const PROFILE_KEYS: [&str; 6] = ["active_watch", "calendars", "weather_provider", "weather_here", "weather_places", "health"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchPref {
    pub value: Vec<u8>,
    /// Unix seconds of the change.
    pub stamp: i64,
    pub pending: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Watch {
    pub id: WatchId,
    pub address: String,
    pub model: Option<i32>,
    pub hardware: Option<u8>,
    pub serial: Option<String>,
    pub board: Option<String>,
    pub firmware: Option<String>,
    pub recovery_firmware: Option<String>,
    pub language: Option<String>,
    pub added: i64,
    pub last_connected: Option<i64>,
    pub profile: ProfileId,
    /// A name the user gave it.
    pub nickname: Option<String>,
}

impl Watch {
    /// "Pebble Time 2", from the model, else the hardware platform, else a generic name.
    pub fn name(&self) -> String {
        use pebble_protocol::hardware::{model, HardwarePlatform};
        if let Some((name, _)) = self.model.and_then(model) {
            return name.to_string();
        }
        if let Some(app) = self.hardware.and_then(|h| HardwarePlatform::from_number(h).app_platform) {
            return app.family_name().to_string();
        }
        "Pebble".to_string()
    }

    pub fn platform(&self) -> Option<pebble_protocol::hardware::AppPlatform> {
        self.hardware.and_then(|h| pebble_protocol::hardware::HardwarePlatform::from_number(h).app_platform)
    }

    pub fn finish(&self) -> Option<&'static str> {
        self.model.and_then(pebble_protocol::hardware::model).map(|(_, f)| f)
    }

    /// The last four characters of the serial, to tell watches of one model apart.
    pub fn short_serial(&self) -> Option<String> {
        let s = self.serial.as_deref()?;
        (s.len() >= 4).then(|| s[s.len() - 4..].to_string())
    }
}

/// What the watch reported in its last session.
#[derive(Debug, Clone, Default)]
pub struct WatchDetails {
    pub model: Option<i32>,
    pub hardware: Option<u8>,
    pub serial: Option<String>,
    pub board: Option<String>,
    pub firmware: Option<String>,
    pub recovery_firmware: Option<String>,
    pub language: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn in_memory() -> Store {
        let mut store = Store { conn: Connection::open_in_memory().unwrap() };
        store.migrate().unwrap();
        store
    }

    fn migrate(&mut self) -> Result<()> {
        let done: i64 = self.conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(done as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("database migration {} failed", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// The active profile's watches.
    pub fn watches(&self) -> Result<Vec<Watch>> {
        let profile = self.profile()?;
        Ok(self.all_watches()?.into_iter().filter(|w| w.profile == profile).collect())
    }

    /// Every profile's watches.
    pub fn all_watches(&self) -> Result<Vec<Watch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, address, model, hardware, serial, board, firmware, recovery_firmware, language, added, last_connected,
                    profile_id, nickname
             FROM watches ORDER BY added",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Watch {
                id: r.get(0)?,
                address: r.get(1)?,
                model: r.get(2)?,
                hardware: r.get(3)?,
                serial: r.get(4)?,
                board: r.get(5)?,
                firmware: r.get(6)?,
                recovery_firmware: r.get(7)?,
                language: r.get(8)?,
                added: r.get(9)?,
                last_connected: r.get(10)?,
                profile: r.get(11)?,
                nickname: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn watch(&self, id: WatchId) -> Result<Option<Watch>> {
        Ok(self.all_watches()?.into_iter().find(|w| w.id == id))
    }

    pub fn watch_by_address(&self, address: &str) -> Result<Option<Watch>> {
        Ok(self.all_watches()?.into_iter().find(|w| w.address.eq_ignore_ascii_case(address)))
    }

    /// Adds a watch, or returns its existing id.
    pub fn add_watch(&self, address: &str, now: i64) -> Result<WatchId> {
        if let Some(w) = self.watch_by_address(address)? {
            return Ok(w.id);
        }
        self.conn
            .execute("INSERT INTO watches (address, added, profile_id) VALUES (?1, ?2, ?3)", params![address, now, self.profile()?])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_details(&self, id: WatchId, d: &WatchDetails, connected_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE watches SET model = COALESCE(?2, model), hardware = COALESCE(?3, hardware), serial = COALESCE(?4, serial),
                board = COALESCE(?5, board), firmware = COALESCE(?6, firmware), recovery_firmware = ?7,
                language = COALESCE(?8, language), last_connected = ?9
             WHERE id = ?1",
            params![id, d.model, d.hardware, d.serial, d.board, d.firmware, d.recovery_firmware, d.language, connected_at],
        )?;
        Ok(())
    }

    pub fn remove_watch(&self, id: WatchId) -> Result<()> {
        self.conn.execute("DELETE FROM watch_apps WHERE watch_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM watch_timeline WHERE watch_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM watch_prefs WHERE watch_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM watches WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// The watch's settings as last known, by key.
    pub fn watch_prefs(&self, watch: WatchId) -> Result<std::collections::HashMap<String, WatchPref>> {
        let mut stmt = self.conn.prepare("SELECT key, value, stamp, pending FROM watch_prefs WHERE watch_id = ?1")?;
        let rows = stmt.query_map([watch], |r| {
            Ok((r.get::<_, String>(0)?, WatchPref { value: r.get(1)?, stamp: r.get(2)?, pending: r.get::<_, i64>(3)? != 0 }))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records a setting; `pending` when it still has to reach the watch.
    pub fn set_watch_pref(&self, watch: WatchId, key: &str, value: &[u8], stamp: i64, pending: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO watch_prefs (watch_id, key, value, stamp, pending) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (watch_id, key) DO UPDATE SET value = ?3, stamp = ?4, pending = ?5",
            params![watch, key, value, stamp, pending as i64],
        )?;
        Ok(())
    }

    /// Marks a setting sent, unless it changed again since `stamp`.
    pub fn watch_pref_sent(&self, watch: WatchId, key: &str, stamp: i64) -> Result<()> {
        self.conn
            .execute("UPDATE watch_prefs SET pending = 0 WHERE watch_id = ?1 AND key = ?2 AND stamp = ?3", params![watch, key, stamp])?;
        Ok(())
    }

    /// `None` or an empty name goes back to the model's name.
    pub fn set_nickname(&self, id: WatchId, name: Option<&str>) -> Result<()> {
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        self.conn.execute("UPDATE watches SET nickname = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    /// Whether Lithidion has cleared this watch's databases since it was last used elsewhere.
    pub fn databases_cleared(&self, id: WatchId) -> Result<bool> {
        let v: Option<i64> = self.conn.query_row("SELECT databases_cleared FROM watches WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(v == Some(1))
    }

    pub fn set_databases_cleared(&self, id: WatchId, cleared: bool) -> Result<()> {
        self.conn.execute("UPDATE watches SET databases_cleared = ?2 WHERE id = ?1", params![id, cleared as i64])?;
        Ok(())
    }

    /// Records that an app sent a notification; returns its mode (new apps are on).
    pub fn seen_notify_app(&self, id: &str, name: &str, now: i64) -> Result<NotifyMode> {
        let profile = self.profile()?;
        self.conn.execute(
            "INSERT INTO notification_apps (profile_id, id, name, last_seen) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(profile_id, id) DO UPDATE SET name = excluded.name, last_seen = excluded.last_seen",
            params![profile, id, name, now],
        )?;
        let mode: i64 =
            self.conn
                .query_row("SELECT mode FROM notification_apps WHERE profile_id = ?1 AND id = ?2", params![profile, id], |r| r.get(0))?;
        Ok(NotifyMode::from_i64(mode))
    }

    pub fn set_notify_mode(&self, id: &str, mode: NotifyMode) -> Result<()> {
        self.conn.execute(
            "UPDATE notification_apps SET mode = ?3 WHERE profile_id = ?1 AND id = ?2",
            params![self.profile()?, id, mode as i64],
        )?;
        Ok(())
    }

    /// An app's icon: a cached file or an icon name.
    pub fn set_notify_icon(&self, id: &str, icon: &str) -> Result<()> {
        self.conn.execute("UPDATE notification_apps SET icon = ?2 WHERE id = ?1", params![id, icon])?;
        Ok(())
    }

    pub fn notify_apps(&self) -> Result<Vec<NotifyApp>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, mode, last_seen, icon FROM notification_apps WHERE profile_id = ?1 ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([self.profile()?], |r| {
            Ok(NotifyApp { id: r.get(0)?, name: r.get(1)?, mode: NotifyMode::from_i64(r.get(2)?), last_seen: r.get(3)?, icon: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds to the history and trims it to a week and [`HISTORY_MAX`] entries.
    /// Returns the entry's id, for `set_notification_outcome`.
    pub fn log_notification(&self, app_id: &str, title: &str, body: &str, time: i64, outcome: Outcome) -> Result<i64> {
        let profile = self.profile()?;
        self.conn.execute(
            "INSERT INTO notifications (profile_id, app_id, title, body, time, outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![profile, app_id, title, body, time, outcome as i64],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute("DELETE FROM notifications WHERE time < ?1", [time - HISTORY_DAYS * 86_400])?;
        self.conn.execute(
            "DELETE FROM notifications WHERE profile_id = ?1 AND id NOT IN
                (SELECT id FROM notifications WHERE profile_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![profile, HISTORY_MAX],
        )?;
        Ok(id)
    }

    pub fn set_notification_outcome(&self, id: i64, outcome: Outcome) -> Result<()> {
        self.conn.execute("UPDATE notifications SET outcome = ?2 WHERE id = ?1", params![id, outcome as i64])?;
        Ok(())
    }

    /// Newest first, with the app's display name.
    pub fn notification_history(&self) -> Result<Vec<NotifyEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(a.name, n.app_id), n.title, n.body, n.time, n.outcome, a.icon
             FROM notifications n LEFT JOIN notification_apps a ON a.id = n.app_id AND a.profile_id = n.profile_id
             WHERE n.profile_id = ?1 ORDER BY n.id DESC",
        )?;
        let rows = stmt.query_map([self.profile()?], |r| {
            Ok(NotifyEntry {
                app: r.get(0)?,
                title: r.get(1)?,
                body: r.get(2)?,
                time: r.get(3)?,
                outcome: Outcome::from_i64(r.get(4)?),
                icon: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// What was last sent to a watch's database (pins, reminders, weather): key → content hash.
    pub fn sent_items(&self, watch: WatchId, db: u8) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self.conn.prepare("SELECT key, hash FROM watch_timeline WHERE watch_id = ?1 AND db = ?2")?;
        let rows = stmt.query_map(params![watch, db], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_sent_item(&self, watch: WatchId, db: u8, key: &str, hash: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO watch_timeline (watch_id, db, key, hash) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(watch_id, db, key) DO UPDATE SET hash = excluded.hash",
            params![watch, db, key, hash],
        )?;
        Ok(())
    }

    pub fn drop_sent_item(&self, watch: WatchId, db: u8, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM watch_timeline WHERE watch_id = ?1 AND db = ?2 AND key = ?3", params![watch, db, key])?;
        Ok(())
    }

    /// After the watch's databases were cleared: nothing is on it any more.
    pub fn forget_sent_items(&self, watch: WatchId) -> Result<()> {
        self.conn.execute("DELETE FROM watch_timeline WHERE watch_id = ?1", [watch])?;
        Ok(())
    }

    /// Stores minutes the watch logged. A minute logged twice keeps the version with more steps,
    /// as the watch may send one again after counting more.
    pub fn add_health_minutes(&mut self, profile: ProfileId, watch: WatchId, minutes: &[Minute]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO health_minutes VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(profile_id, time) DO UPDATE SET
                    watch_id = excluded.watch_id, steps = excluded.steps, orientation = excluded.orientation, vmc = excluded.vmc,
                    light = excluded.light, flags = excluded.flags, resting_cal = excluded.resting_cal,
                    active_cal = excluded.active_cal, distance_cm = excluded.distance_cm,
                    heart_rate = excluded.heart_rate, hr_weight = excluded.hr_weight,
                    hr_zone = excluded.hr_zone, spo2 = excluded.spo2
                 WHERE excluded.steps > health_minutes.steps",
            )?;
            for m in minutes {
                let flags = m.plugged_in as u8 | (m.active as u8) << 1;
                stmt.execute(params![
                    profile,
                    m.time,
                    watch,
                    m.steps,
                    m.orientation,
                    m.vmc,
                    m.light,
                    flags,
                    m.resting_cal,
                    m.active_cal,
                    m.distance_cm,
                    m.heart_rate,
                    m.heart_rate_weight,
                    m.heart_rate_zone,
                    m.spo2
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Stores sessions the watch logged; one sent again while still going keeps the longer one.
    pub fn add_health_sessions(&mut self, profile: ProfileId, watch: WatchId, sessions: &[Session]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO health_sessions VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(profile_id, start, kind) DO UPDATE SET
                    watch_id = excluded.watch_id, seconds = excluded.seconds, utc_to_local = excluded.utc_to_local,
                    steps = excluded.steps, active_kcal = excluded.active_kcal,
                    resting_kcal = excluded.resting_kcal, distance_m = excluded.distance_m
                 WHERE excluded.seconds > health_sessions.seconds",
            )?;
            for s in sessions {
                stmt.execute(params![
                    profile,
                    s.start,
                    s.kind as u8,
                    watch,
                    s.seconds,
                    s.utc_to_local,
                    s.steps,
                    s.active_kcal,
                    s.resting_kcal,
                    s.distance_m
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Minutes from `from` up to `to`, UTC seconds, in order.
    pub fn health_minutes(&self, profile: ProfileId, from: i64, to: i64) -> Result<Vec<Minute>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT time, steps, orientation, vmc, light, flags, resting_cal, active_cal, distance_cm,
                    heart_rate, hr_weight, hr_zone, spo2
             FROM health_minutes WHERE profile_id = ?1 AND time >= ?2 AND time < ?3 ORDER BY time",
        )?;
        let rows = stmt.query_map(params![profile, from, to], |r| {
            let flags: u8 = r.get(5)?;
            Ok(Minute {
                time: r.get(0)?,
                steps: r.get(1)?,
                orientation: r.get(2)?,
                vmc: r.get(3)?,
                light: r.get(4)?,
                plugged_in: flags & 1 != 0,
                active: flags & 2 != 0,
                resting_cal: r.get(6)?,
                active_cal: r.get(7)?,
                distance_cm: r.get(8)?,
                heart_rate: r.get(9)?,
                heart_rate_weight: r.get(10)?,
                heart_rate_zone: r.get(11)?,
                spo2: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Steps between each pair of neighbouring times in `bounds`, UTC seconds.
    pub fn health_steps(&self, profile: ProfileId, bounds: &[i64]) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT COALESCE(SUM(steps), 0) FROM health_minutes WHERE profile_id = ?1 AND time >= ?2 AND time < ?3")?;
        bounds.windows(2).map(|w| Ok(stmt.query_row(params![profile, w[0], w[1]], |r| r.get(0))?)).collect()
    }

    /// Sessions that start from `from` up to `to`, in order.
    pub fn health_sessions(&self, profile: ProfileId, from: i64, to: i64) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT start, kind, seconds, utc_to_local, steps, active_kcal, resting_kcal, distance_m
             FROM health_sessions WHERE profile_id = ?1 AND start >= ?2 AND start < ?3 ORDER BY start, kind",
        )?;
        let rows = stmt.query_map(params![profile, from, to], |r| {
            Ok((
                r.get::<_, u16>(1)?,
                Session {
                    kind: SessionKind::Sleep,
                    start: r.get(0)?,
                    seconds: r.get(2)?,
                    utc_to_local: r.get(3)?,
                    steps: r.get(4)?,
                    active_kcal: r.get(5)?,
                    resting_kcal: r.get(6)?,
                    distance_m: r.get(7)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, s) = row?;
            if let Some(kind) = SessionKind::from_u16(kind) {
                out.push(Session { kind, ..s });
            }
        }
        Ok(out)
    }

    /// The first and last minute kept for a watch.
    pub fn health_span(&self, profile: ProfileId) -> Result<Option<(i64, i64)>> {
        let span: (Option<i64>, Option<i64>) =
            self.conn.query_row("SELECT MIN(time), MAX(time) FROM health_minutes WHERE profile_id = ?1", [profile], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        Ok(span.0.zip(span.1))
    }

    /// Deletes a profile's health history, or only what is older than `before`.
    pub fn clear_health(&self, profile: ProfileId, before: Option<i64>) -> Result<()> {
        let before = before.unwrap_or(i64::MAX);
        self.conn.execute("DELETE FROM health_minutes WHERE profile_id = ?1 AND time < ?2", params![profile, before])?;
        self.conn.execute("DELETE FROM health_sessions WHERE profile_id = ?1 AND start < ?2", params![profile, before])?;
        Ok(())
    }

    /// The profile in use; 0 before any exists.
    pub fn profile(&self) -> Result<ProfileId> {
        Ok(self.setting(ACTIVE_PROFILE)?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn set_profile(&self, id: ProfileId) -> Result<()> {
        self.set_setting(ACTIVE_PROFILE, &id.to_string())
    }

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        let mut stmt = self.conn.prepare("SELECT id, name FROM profiles ORDER BY name COLLATE NOCASE, id")?;
        let rows = stmt.query_map([], |r| Ok(Profile { id: r.get(0)?, name: r.get(1)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds a profile. The first one takes over what was kept before profiles existed: watches,
    /// notification settings and history, health history, and the calendar, weather, and
    /// health settings.
    pub fn add_profile(&mut self, name: &str, now: i64) -> Result<ProfileId> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO profiles (name, added) VALUES (?1, ?2)", params![name, now])?;
        let id = tx.last_insert_rowid();
        let first: i64 = tx.query_row("SELECT COUNT(*) FROM profiles", [], |r| r.get(0))?;
        if first == 1 {
            for table in ["watches", "notification_apps", "notifications", "health_minutes", "health_sessions", "script_storage"] {
                tx.execute(&format!("UPDATE {table} SET profile_id = ?1 WHERE profile_id = 0"), [id])?;
            }
            // Settings saved before profiles were global, or per watch for health.
            let health: Option<String> = tx
                .query_row("SELECT value FROM settings WHERE key LIKE 'health:%' AND key NOT LIKE 'health:p%' LIMIT 1", [], |r| r.get(0))
                .optional()?;
            if let Some(v) = health {
                tx.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![format!("health:p{id}"), v])?;
            }
            tx.execute("DELETE FROM settings WHERE key LIKE 'health:%' AND key NOT LIKE 'health:p%'", [])?;
            for key in PROFILE_KEYS {
                tx.execute("UPDATE OR REPLACE settings SET key = ?2 WHERE key = ?1", params![key, crate::settings::profile_key(key, id)])?;
            }
            tx.execute(
                "UPDATE OR REPLACE settings SET key = key || ':p' || ?1 WHERE (key LIKE 'network:%' OR key = 'account_seed') AND key NOT LIKE '%:p%'",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn rename_profile(&self, id: ProfileId, name: &str) -> Result<()> {
        self.conn.execute("UPDATE profiles SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    /// Deletes a profile and everything kept for it. Its watches go first (see `remove_watch`).
    pub fn delete_profile(&self, id: ProfileId) -> Result<()> {
        for w in self.all_watches()?.into_iter().filter(|w| w.profile == id) {
            self.remove_watch(w.id)?;
        }
        self.clear_health(id, None)?;
        self.conn.execute("DELETE FROM notification_apps WHERE profile_id = ?1", [id])?;
        self.conn.execute("DELETE FROM notifications WHERE profile_id = ?1", [id])?;
        self.conn.execute("DELETE FROM script_storage WHERE profile_id = ?1", [id])?;
        self.conn.execute("DELETE FROM settings WHERE key LIKE ?1", [format!("%:p{id}")])?;
        self.conn.execute("DELETE FROM profiles WHERE id = ?1", [id])?;
        if self.profile()? == id {
            self.clear_setting(ACTIVE_PROFILE)?;
        }
        Ok(())
    }

    pub fn clear_notification_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM notifications WHERE profile_id = ?1", [self.profile()?])?;
        Ok(())
    }

    pub fn save_app(&self, a: &App) -> Result<()> {
        let (source, store_id) = a.store.clone().unzip();
        self.conn.execute(
            "INSERT INTO apps (uuid, kind, title, developer, version, hidden, store_source, store_id, image, added, script, configurable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(uuid) DO UPDATE SET kind = excluded.kind, title = excluded.title, developer = excluded.developer,
                version = excluded.version, hidden = excluded.hidden, script = excluded.script, configurable = excluded.configurable,
                store_source = COALESCE(excluded.store_source, store_source), store_id = COALESCE(excluded.store_id, store_id),
                image = COALESCE(excluded.image, image)",
            params![
                a.uuid.to_string(),
                a.kind.as_str(),
                a.title,
                a.developer,
                a.version,
                a.hidden as i64,
                source,
                store_id,
                a.image,
                a.added,
                a.script as i64,
                a.configurable as i64
            ],
        )?;
        Ok(())
    }

    fn app_from_row(r: &rusqlite::Row, at: usize) -> rusqlite::Result<App> {
        let uuid: String = r.get(at)?;
        let kind: String = r.get(at + 1)?;
        let source: Option<String> = r.get(at + 6)?;
        let id: Option<String> = r.get(at + 7)?;
        Ok(App {
            uuid: uuid.parse().unwrap_or_default(),
            kind: AppKind::parse(&kind).unwrap_or(AppKind::Watchapp),
            title: r.get(at + 2)?,
            developer: r.get(at + 3)?,
            version: r.get(at + 4)?,
            hidden: r.get::<_, i64>(at + 5)? != 0,
            store: source.zip(id),
            image: r.get(at + 8)?,
            added: r.get(at + 9)?,
            script: r.get::<_, Option<i64>>(at + 10)?.unwrap_or(0) != 0,
            configurable: r.get::<_, i64>(at + 11)? != 0,
        })
    }

    #[cfg(test)]
    pub fn app(&self, uuid: uuid::Uuid) -> Result<Option<App>> {
        Ok(self
            .conn
            .query_row(
                "SELECT uuid, kind, title, developer, version, hidden, store_source, store_id, image, added, script, configurable
                 FROM apps WHERE uuid = ?1",
                [uuid.to_string()],
                |r| Self::app_from_row(r, 0),
            )
            .optional()?)
    }

    /// The watch's apps in order, those still to be removed included.
    pub fn watch_apps(&self, watch: WatchId) -> Result<Vec<WatchApp>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.uuid, a.kind, a.title, a.developer, a.version, a.hidden, a.store_source, a.store_id, a.image, a.added,
                    a.script, a.configurable, w.position, w.state
             FROM watch_apps w JOIN apps a ON a.uuid = w.uuid WHERE w.watch_id = ?1 ORDER BY w.position",
        )?;
        let rows = stmt.query_map([watch], |r| {
            Ok(WatchApp { app: Self::app_from_row(r, 0)?, position: r.get(12)?, state: OnWatch::from_i64(r.get(13)?) })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds an app at the end of the watch's list, or marks it for sending again (an update).
    pub fn add_to_watch(&self, watch: WatchId, uuid: uuid::Uuid) -> Result<()> {
        let next: i64 =
            self.conn.query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM watch_apps WHERE watch_id = ?1", [watch], |r| r.get(0))?;
        self.conn.execute(
            "INSERT INTO watch_apps (watch_id, uuid, position, state) VALUES (?1, ?2, ?3, 0)
             ON CONFLICT(watch_id, uuid) DO UPDATE SET state = 0",
            params![watch, uuid.to_string(), next],
        )?;
        Ok(())
    }

    /// Marks every watch with this app for sending it again, after its bundle changed.
    pub fn resend_app(&self, uuid: uuid::Uuid) -> Result<()> {
        self.conn.execute("UPDATE watch_apps SET state = 0 WHERE uuid = ?1 AND state = 1", [uuid.to_string()])?;
        Ok(())
    }

    pub fn set_on_watch(&self, watch: WatchId, uuid: uuid::Uuid, state: OnWatch) -> Result<()> {
        self.conn.execute(
            "UPDATE watch_apps SET state = ?3 WHERE watch_id = ?1 AND uuid = ?2",
            params![watch, uuid.to_string(), state as i64],
        )?;
        Ok(())
    }

    /// Takes an app off the watch's list once the watch has removed it.
    pub fn drop_from_watch(&self, watch: WatchId, uuid: uuid::Uuid) -> Result<()> {
        self.conn.execute("DELETE FROM watch_apps WHERE watch_id = ?1 AND uuid = ?2", params![watch, uuid.to_string()])?;
        Ok(())
    }

    /// After the watch's databases were cleared: everything kept is to be sent again, and what was
    /// to be removed is gone already.
    pub fn reset_watch_apps(&self, watch: WatchId) -> Result<()> {
        self.conn.execute("DELETE FROM watch_apps WHERE watch_id = ?1 AND state = 2", [watch])?;
        self.conn.execute("UPDATE watch_apps SET state = 0 WHERE watch_id = ?1", [watch])?;
        Ok(())
    }

    /// Sets the order of the listed apps (positions 0, 1, …), the others keeping theirs after.
    pub fn reorder_watch_apps(&self, watch: WatchId, order: &[uuid::Uuid]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let rest: Vec<String> = {
            let mut stmt = tx.prepare("SELECT uuid FROM watch_apps WHERE watch_id = ?1 ORDER BY position")?;
            let all = stmt.query_map([watch], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let listed: Vec<String> = order.iter().map(|u| u.to_string()).collect();
            all.into_iter().filter(|u| !listed.contains(u)).collect()
        };
        for (i, u) in order.iter().map(|u| u.to_string()).chain(rest).enumerate() {
            tx.execute("UPDATE watch_apps SET position = ?3 WHERE watch_id = ?1 AND uuid = ?2", params![watch, u, i as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forgets an app no watch has any more; returns whether it did.
    pub fn forget_app_if_unused(&self, uuid: uuid::Uuid) -> Result<bool> {
        let used: i64 = self.conn.query_row("SELECT COUNT(*) FROM watch_apps WHERE uuid = ?1", [uuid.to_string()], |r| r.get(0))?;
        if used > 0 {
            return Ok(false);
        }
        self.conn.execute("DELETE FROM apps WHERE uuid = ?1", [uuid.to_string()])?;
        self.conn.execute("DELETE FROM script_storage WHERE uuid = ?1", [uuid.to_string()])?;
        self.conn.execute(
            "DELETE FROM settings WHERE key = ?1 OR key LIKE ?2",
            params![format!("network:{uuid}"), format!("network:{uuid}:p%")],
        )?;
        Ok(true)
    }

    pub fn app_title(&self, uuid: uuid::Uuid) -> Option<String> {
        self.conn.query_row("SELECT title FROM apps WHERE uuid = ?1", [uuid.to_string()], |r| r.get(0)).optional().ok().flatten()
    }

    /// Apps whose bundles have not been read for a script yet.
    pub fn apps_unread(&self) -> Result<Vec<uuid::Uuid>> {
        let mut stmt = self.conn.prepare("SELECT uuid FROM apps WHERE script IS NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()?.parse().ok()).collect())
    }

    pub fn set_app_script(&self, uuid: uuid::Uuid, script: bool, configurable: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE apps SET script = ?2, configurable = ?3 WHERE uuid = ?1",
            params![uuid.to_string(), script as i64, configurable as i64],
        )?;
        Ok(())
    }

    pub fn script_value(&self, profile: ProfileId, app: uuid::Uuid, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM script_storage WHERE profile_id = ?1 AND uuid = ?2 AND key = ?3",
                params![profile, app.to_string(), key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_script_value(&self, profile: ProfileId, app: uuid::Uuid, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO script_storage (profile_id, uuid, key, value) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(profile_id, uuid, key) DO UPDATE SET value = excluded.value",
            params![profile, app.to_string(), key, value],
        )?;
        Ok(())
    }

    pub fn remove_script_value(&self, profile: ProfileId, app: uuid::Uuid, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM script_storage WHERE profile_id = ?1 AND uuid = ?2 AND key = ?3",
            params![profile, app.to_string(), key],
        )?;
        Ok(())
    }

    pub fn clear_script_values(&self, profile: ProfileId, app: uuid::Uuid) -> Result<()> {
        self.conn.execute("DELETE FROM script_storage WHERE profile_id = ?1 AND uuid = ?2", params![profile, app.to_string()])?;
        Ok(())
    }

    pub fn script_keys(&self, profile: ProfileId, app: uuid::Uuid) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT key FROM script_storage WHERE profile_id = ?1 AND uuid = ?2 ORDER BY key")?;
        let rows = stmt.query_map(params![profile, app.to_string()], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Keys starting with `prefix`.
    pub fn setting_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT key FROM settings WHERE substr(key, 1, length(?1)) = ?1")?;
        let keys = stmt.query_map([prefix], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(keys)
    }

    pub fn clear_setting(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_take_over_and_keep_apart() {
        let mut s = Store::in_memory();
        // Before any profile: a watch, an app's notification setting, and global settings.
        let w = s.add_watch("24:A3:0A:D4:7B:24", 1).unwrap();
        s.seen_notify_app("sm.puri.Chatty", "Chats", 1).unwrap();
        crate::settings::set_calendars(&s, &["cal".into()]).unwrap();
        crate::settings::set_active_watch(&s, Some(w)).unwrap();
        let aj = s.add_profile("AJ", 2).unwrap();
        s.set_profile(aj).unwrap();
        assert_eq!(s.watches().unwrap().len(), 1);
        assert_eq!(s.notify_apps().unwrap().len(), 1);
        assert_eq!(crate::settings::calendars(&s), ["cal"]);
        assert_eq!(crate::settings::active_watch(&s).unwrap(), Some(w));
        // A second profile starts empty, and adding a watch there leaves AJ's alone.
        let sam = s.add_profile("Sam", 3).unwrap();
        s.set_profile(sam).unwrap();
        assert!(s.watches().unwrap().is_empty());
        assert!(s.notify_apps().unwrap().is_empty());
        assert!(crate::settings::calendars(&s).is_empty());
        s.add_watch("11:22:33:44:55:66", 4).unwrap();
        assert_eq!(s.all_watches().unwrap().len(), 2);
        // Deleting Sam takes Sam's watch and settings; AJ keeps everything.
        crate::settings::set_calendars(&s, &["sam".into()]).unwrap();
        s.delete_profile(sam).unwrap();
        assert_eq!(s.profile().unwrap(), 0);
        assert_eq!(s.all_watches().unwrap().len(), 1);
        assert_eq!(s.profiles().unwrap().len(), 1);
        assert_eq!(s.setting("calendars:p2").unwrap(), None);
        s.set_profile(aj).unwrap();
        assert_eq!(crate::settings::calendars(&s), ["cal"]);
    }

    #[test]
    fn data_from_before_profiles_moves_over() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute_batch(
            "INSERT INTO watches (address, added) VALUES ('24:A3:0A:D4:7B:24', 1);
             INSERT INTO notification_apps (id, name, last_seen) VALUES ('sm.puri.Chatty', 'Chats', 1);
             INSERT INTO health_minutes VALUES (1, 60, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
             INSERT INTO health_sessions VALUES (1, 100, 1, 3600, 0, 0, 0, 0, 0);
             INSERT INTO settings VALUES ('health:1', '{\"tracking\":false}');",
        )
        .unwrap();
        let mut s = Store { conn };
        s.migrate().unwrap();
        let id = s.add_profile("AJ", 2).unwrap();
        s.set_profile(id).unwrap();
        assert_eq!(s.watches().unwrap().len(), 1);
        assert_eq!(s.notify_apps().unwrap()[0].name, "Chats");
        assert_eq!(s.health_minutes(id, 0, 1000).unwrap()[0].steps, 5);
        assert_eq!(s.health_sessions(id, 0, 1000).unwrap().len(), 1);
        assert!(!crate::settings::health(&s, id).tracking);
    }

    #[test]
    fn watches_and_settings() {
        let s = Store::in_memory();
        let a = s.add_watch("24:A3:0A:D4:7B:24", 10).unwrap();
        assert_eq!(s.add_watch("24:a3:0a:d4:7b:24", 11).unwrap(), a);
        let b = s.add_watch("11:22:33:44:55:66", 12).unwrap();
        let d = WatchDetails { model: Some(37), hardware: Some(18), serial: Some("C111A24B7B24".into()), ..Default::default() };
        s.update_details(a, &d, 20).unwrap();
        let w = s.watch(a).unwrap().unwrap();
        assert_eq!(w.name(), "Pebble Time 2");
        assert_eq!(w.finish(), Some("Black/Red"));
        assert_eq!(w.short_serial().as_deref(), Some("7B24"));
        assert_eq!(w.last_connected, Some(20));
        assert_eq!(s.watch(b).unwrap().unwrap().name(), "Pebble");
        s.set_nickname(b, Some(" Gym watch ")).unwrap();
        assert_eq!(s.watch(b).unwrap().unwrap().nickname.as_deref(), Some("Gym watch"));
        s.set_nickname(b, Some("")).unwrap();
        assert_eq!(s.watch(b).unwrap().unwrap().nickname, None);
        s.remove_watch(a).unwrap();
        assert_eq!(s.watches().unwrap().len(), 1);

        assert_eq!(s.setting("x").unwrap(), None);
        s.set_setting("x", "1").unwrap();
        s.set_setting("x", "2").unwrap();
        assert_eq!(s.setting("x").unwrap().as_deref(), Some("2"));
        s.clear_setting("x").unwrap();
        assert_eq!(s.setting("x").unwrap(), None);
    }

    fn app(n: u8, kind: AppKind) -> App {
        App {
            uuid: uuid::Uuid::from_bytes([n; 16]),
            kind,
            title: format!("App {n}"),
            developer: "Dev".into(),
            version: "1.0".into(),
            hidden: false,
            store: Some(("pebble".into(), format!("id{n}"))),
            image: None,
            added: 0,
            script: false,
            configurable: false,
        }
    }

    #[test]
    fn notification_apps_and_history() {
        let s = Store::in_memory();
        assert_eq!(s.seen_notify_app("sm.puri.Chatty", "Chats", 10).unwrap(), NotifyMode::On);
        s.set_notify_mode("sm.puri.Chatty", NotifyMode::Always).unwrap();
        assert_eq!(s.seen_notify_app("sm.puri.Chatty", "Chatty", 11).unwrap(), NotifyMode::Always);
        assert_eq!(s.notify_apps().unwrap()[0].name, "Chatty");
        let week = HISTORY_DAYS * 86_400;
        s.log_notification("sm.puri.Chatty", "old", "", 100, Outcome::Sent).unwrap();
        s.log_notification("sm.puri.Chatty", "new", "", 100 + week + 1, Outcome::SentQuietly).unwrap();
        let h = s.notification_history().unwrap();
        assert_eq!(h.len(), 1, "the week-old one is dropped");
        assert_eq!((h[0].app.as_str(), h[0].outcome), ("Chatty", Outcome::SentQuietly));
        for i in 0..(HISTORY_MAX + 5) {
            s.log_notification("x", &i.to_string(), "", 200 + week, Outcome::Sent).unwrap();
        }
        let h = s.notification_history().unwrap();
        assert_eq!(h.len() as i64, HISTORY_MAX);
        assert_eq!(h[0].title, (HISTORY_MAX + 4).to_string());
        s.clear_notification_history().unwrap();
        assert!(s.notification_history().unwrap().is_empty());

        s.set_sent_item(1, 1, "a", 5).unwrap();
        s.set_sent_item(1, 1, "a", 6).unwrap();
        s.set_sent_item(1, 5, "w", 7).unwrap();
        assert_eq!(s.sent_items(1, 1).unwrap().get("a"), Some(&6));
        s.drop_sent_item(1, 1, "a").unwrap();
        assert!(s.sent_items(1, 1).unwrap().is_empty());
        s.forget_sent_items(1).unwrap();
        assert!(s.sent_items(1, 5).unwrap().is_empty());
    }

    #[test]
    fn apps_per_watch() {
        let s = Store::in_memory();
        let w = s.add_watch("24:A3:0A:D4:7B:24", 10).unwrap();
        let other = s.add_watch("11:22:33:44:55:66", 10).unwrap();
        let (a, b, c) = (app(1, AppKind::Watchface), app(2, AppKind::Watchapp), app(3, AppKind::Watchapp));
        for x in [&a, &b, &c] {
            s.save_app(x).unwrap();
            s.add_to_watch(w, x.uuid).unwrap();
        }
        s.add_to_watch(other, a.uuid).unwrap();
        // Saving again keeps the store link when the new copy has none (a sideloaded update).
        s.save_app(&App { store: None, version: "1.1".into(), ..a.clone() }).unwrap();
        assert_eq!(s.app(a.uuid).unwrap().unwrap().store, a.store);
        assert_eq!(s.app(a.uuid).unwrap().unwrap().version, "1.1");

        let list = s.watch_apps(w).unwrap();
        assert_eq!(list.iter().map(|x| x.position).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(list.iter().all(|x| x.state == OnWatch::ToInsert));
        s.set_on_watch(w, a.uuid, OnWatch::Inserted).unwrap();
        s.set_on_watch(w, c.uuid, OnWatch::ToDelete).unwrap();
        s.reorder_watch_apps(w, &[b.uuid]).unwrap();
        let order: Vec<_> = s.watch_apps(w).unwrap().iter().map(|x| x.app.uuid).collect();
        assert_eq!(order, [b.uuid, a.uuid, c.uuid]);

        s.reset_watch_apps(w).unwrap();
        let after = s.watch_apps(w).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|x| x.state == OnWatch::ToInsert));
        assert!(s.forget_app_if_unused(c.uuid).unwrap());
        assert!(!s.forget_app_if_unused(a.uuid).unwrap());

        assert!(!s.databases_cleared(w).unwrap());
        s.set_databases_cleared(w, true).unwrap();
        assert!(s.databases_cleared(w).unwrap());
        s.remove_watch(w).unwrap();
        assert!(s.watch_apps(w).unwrap().is_empty());
        assert_eq!(s.watch_apps(other).unwrap().len(), 1);
    }
}
