//! The watch's own settings (PROTOCOL.md section 20): read from the watch with BlobDB v2, kept
//! per watch, and written back with BlobDB v1. Changes made while the watch is away wait for it.

use super::apps::{self, Db};
use super::session::Protocol;
use super::Event;
use crate::settings;
use crate::store::{Store, WatchId};
use anyhow::{anyhow, Result};
use pebble_protocol::blobdb::{insert_with_timestamp, v2, Database, Status};
use pebble_protocol::endpoint;
use pebble_protocol::watchprefs::{self, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::sync::{broadcast, Notify};

/// Woken when a setting changes on this side, or something they follow does (units, the clock).
static WAKE: Notify = Notify::const_new();
static TOKEN: AtomicU16 = AtomicU16::new(1);

pub fn wake() {
    WAKE.notify_one();
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// What the UI shows for a watch.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct View {
    pub watch: WatchId,
    pub values: HashMap<String, Vec<u8>>,
    pub clock: settings::Clock,
    /// The watch keeps a settings database; older firmware does not.
    pub supported: bool,
    pub platform: Option<pebble_protocol::hardware::AppPlatform>,
}

pub fn view(store: &Store, watch: WatchId) -> View {
    let values = store.watch_prefs(watch).unwrap_or_default().into_iter().map(|(k, p)| (k, p.value)).collect();
    View {
        watch,
        values,
        clock: settings::watch_clock(store, watch),
        supported: settings::watch_has_settings(store, watch),
        platform: store.watch(watch).ok().flatten().and_then(|w| w.platform()),
    }
}

/// A change from this side, to send when the watch is there.
pub fn set(store: &Store, watch: WatchId, key: &str, value: Value) -> Result<()> {
    let pref = watchprefs::find(key).ok_or_else(|| anyhow!("unknown watch setting {key}"))?;
    let bytes = watchprefs::encode(pref.kind, value).ok_or_else(|| anyhow!("{key} cannot hold {value:?}"))?;
    store.set_watch_pref(watch, key, &bytes, now(), true)?;
    wake();
    Ok(())
}

/// Queues the settings Lithidion decides: the clock, and the watch's weather units.
fn follow(store: &Store, watch: WatchId) -> Result<()> {
    let imperial = crate::units::imperial();
    let wanted = [
        (watchprefs::CLOCK_24H, Value::Bool(settings::watch_clock(store, watch).twenty_four())),
        (watchprefs::UNITS_WIND, Value::Number(watchprefs::wind_units(imperial))),
        (watchprefs::UNITS_TEMPERATURE, Value::Number(watchprefs::temperature_units(imperial))),
    ];
    let known = store.watch_prefs(watch)?;
    for (key, value) in wanted {
        let kind = watchprefs::find(key).expect("listed").kind;
        let current = known.get(key).and_then(|p| watchprefs::decode(kind, &p.value));
        if current != Some(value) {
            let bytes = watchprefs::encode(kind, value).expect("fits");
            store.set_watch_pref(watch, key, &bytes, now(), true)?;
        }
    }
    Ok(())
}

/// A setting the watch sent. Keeps whichever side changed it last.
fn take(store: &Store, watch: WatchId, key: &str, value: &[u8], stamp: i64) -> Result<Status> {
    let Some(pref) = watchprefs::find(key) else { return Ok(Status::Success) };
    if watchprefs::decode(pref.kind, value).is_none() {
        return Ok(Status::InvalidData);
    }
    let known = store.watch_prefs(watch)?;
    if let Some(mine) = known.get(key) {
        if mine.stamp > stamp {
            return Ok(Status::DataStale);
        }
    }
    store.set_watch_pref(watch, key, value, stamp, false)?;
    // A clock set on the watch itself overrides following this device.
    if key == watchprefs::CLOCK_24H {
        let twenty_four = matches!(watchprefs::decode(pref.kind, value), Some(Value::Bool(true)));
        let clock = settings::watch_clock(store, watch);
        if clock.twenty_four() != twenty_four {
            let fixed = if twenty_four { settings::Clock::TwentyFour } else { settings::Clock::Twelve };
            settings::set_watch_clock(store, watch, fixed)?;
        }
    }
    Ok(Status::Success)
}

/// Runs for one session with a watch that has the settings database: reads the watch's
/// settings first, so changes made on it while away are kept, then sends what changed here.
pub async fn run(protocol: Protocol, db: Db, watch: WatchId, events: async_channel::Sender<Event>) -> Result<()> {
    let emit = |db: &Db| {
        let v = view(&apps::lock(db), watch);
        let _ = events.try_send(Event::WatchPrefs(Box::new(v)));
    };
    settings::set_watch_has_settings(&apps::lock(&db), watch)?;
    emit(&db);
    let mut frames = protocol.subscribe();
    let version = match request(&protocol, v2::version).await.as_deref().map(v2::parse) {
        Ok(Ok(v2::Incoming::Version { version, .. })) => version,
        _ => {
            tracing::info!("BlobDB v2 not available; watch settings are only sent");
            0
        }
    };
    if version >= 1 && request(&protocol, |t| v2::dirty_all(t, Database::Settings)).await.is_ok() {
        // The watch's settings, until it says it is done.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match tokio::time::timeout_at(deadline, frames.recv()).await {
                Err(_) => {
                    tracing::info!("the watch did not finish sending its settings");
                    break;
                }
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => tracing::warn!(n, "watch settings frames dropped"),
                Ok(Err(broadcast::error::RecvError::Closed)) => return Ok(()),
                Ok(Ok(frame)) => {
                    if handle(&protocol, &db, watch, &frame)? == Handled::SyncDone {
                        break;
                    }
                }
            }
        }
    }
    follow(&apps::lock(&db), watch)?;
    emit(&db);
    send_pending(&protocol, &db, watch).await;
    loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                Ok(frame) => {
                    if handle(&protocol, &db, watch, &frame)? != Handled::Nothing {
                        emit(&db);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => tracing::warn!(n, "watch settings frames dropped"),
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = WAKE.notified() => {
                follow(&apps::lock(&db), watch)?;
                emit(&db);
                send_pending(&protocol, &db, watch).await;
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Handled {
    Nothing,
    Setting,
    SyncDone,
}

/// Takes a setting the watch sends, or its end of sync, and answers it.
fn handle(protocol: &Protocol, db: &Db, watch: WatchId, frame: &pebble_protocol::frame::Frame) -> Result<Handled> {
    if frame.endpoint != endpoint::BLOBDB_V2 {
        return Ok(Handled::Nothing);
    }
    let Ok(incoming) = v2::parse(&frame.payload) else { return Ok(Handled::Nothing) };
    let (status, handled) = match &incoming {
        v2::Incoming::Write { db: d, key, value, timestamp, .. } if *d == Database::Settings as u8 => {
            let status = match watchprefs::key_from_bytes(key) {
                Some(key) => {
                    let status = take(&apps::lock(db), watch, &key, value, *timestamp as i64)?;
                    tracing::debug!(%key, ?status, "watch setting received");
                    status
                }
                None => Status::InvalidData,
            };
            (status, Handled::Setting)
        }
        v2::Incoming::Write { .. } => (Status::Success, Handled::Nothing),
        v2::Incoming::SyncDone { db: d, .. } => {
            let done = if *d == Database::Settings as u8 { Handled::SyncDone } else { Handled::Nothing };
            (Status::Success, done)
        }
        _ => return Ok(Handled::Nothing),
    };
    if let Some(answer) = v2::answer(&incoming, status) {
        protocol.send(endpoint::BLOBDB_V2, &answer)?;
    }
    Ok(handled)
}

async fn request(protocol: &Protocol, payload: impl Fn(u16) -> Vec<u8>) -> Result<Vec<u8>> {
    let token = TOKEN.fetch_add(1, Ordering::Relaxed).max(1);
    protocol.request(endpoint::BLOBDB_V2, &payload(token), endpoint::BLOBDB_V2, |p| v2::answers(p, token), Duration::from_secs(5)).await
}

/// Sends what changed on this side; what fails waits for the next try.
async fn send_pending(protocol: &Protocol, db: &Db, watch: WatchId) {
    let pending: Vec<(String, Vec<u8>, i64)> = match apps::lock(db).watch_prefs(watch) {
        Ok(m) => m.into_iter().filter(|(_, p)| p.pending).map(|(k, p)| (k, p.value, p.stamp)).collect(),
        Err(_) => return,
    };
    for (key, value, stamp) in pending {
        let key_bytes = watchprefs::key_bytes(&key);
        let result = apps::blob(protocol, |t| insert_with_timestamp(t, Database::Settings, stamp as u32, &key_bytes, &value)).await;
        match result {
            // Stale: the watch changed it later, and its value comes back with the next sync.
            Ok(Status::Success | Status::DataStale) => {
                let _ = apps::lock(db).watch_pref_sent(watch, &key, stamp);
            }
            Ok(status) => tracing::info!(%key, ?status, "watch setting refused"),
            Err(e) => {
                tracing::info!(%key, error = %e, "watch setting not sent");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watch that answers the version and resend requests, sends one setting, and takes writes.
    #[tokio::test]
    async fn session_reads_and_writes() {
        use pebble_protocol::frame::{Frame, Reassembler};
        let db: Db = std::sync::Arc::new(std::sync::Mutex::new(Store::in_memory()));
        apps::lock(&db).set_watch_pref(1, "lightMotion", &[0], 50, true).unwrap();
        settings::set_watch_clock(&apps::lock(&db), 1, settings::Clock::Twelve).unwrap();
        let (protocol, mut out, to_phone) = Protocol::loopback();
        let (events, _rx) = async_channel::unbounded();
        let session = tokio::spawn(run(protocol.clone(), db.clone(), 1, events));
        let mut reassembler = Reassembler::new();
        let mut log = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while log.len() < 7 && tokio::time::Instant::now() < deadline {
            let Ok(Some(bytes)) = tokio::time::timeout(Duration::from_secs(1), out.recv()).await else { continue };
            reassembler.push(&bytes);
            while let Some(f) = reassembler.next_frame() {
                let p = f.payload.clone();
                let reply = |endpoint, payload: Vec<u8>| {
                    let _ = to_phone.send(Frame { endpoint, payload });
                };
                match (f.endpoint, p[0]) {
                    (endpoint::BLOBDB_V2, 0x0B) => {
                        log.push("version".to_string());
                        reply(endpoint::BLOBDB_V2, vec![0x8B, p[1], p[2], 1, 1]);
                    }
                    (endpoint::BLOBDB_V2, 0x0C) => {
                        log.push("dirty all".to_string());
                        reply(endpoint::BLOBDB_V2, vec![0x8C, p[1], p[2], 1]);
                        // The watch's 24-hour clock, changed after anything here.
                        let mut w = vec![0x09, 9, 0, 12];
                        w.extend_from_slice(&(now() as u32 + 100).to_le_bytes());
                        w.push(9);
                        w.extend_from_slice(b"clock24h\0");
                        w.extend_from_slice(&[1, 0, 1]);
                        reply(endpoint::BLOBDB_V2, w);
                        reply(endpoint::BLOBDB_V2, vec![0x0A, 10, 0, 12]);
                    }
                    (endpoint::BLOBDB_V2, 0x89) => log.push(format!("answered {}", p[3])),
                    (endpoint::BLOBDB_V2, 0x8A) => log.push("sync done".to_string()),
                    (endpoint::BLOBDB_V1, 0x0D) => {
                        let key_len = p[8] as usize;
                        log.push(format!("insert {}", String::from_utf8_lossy(&p[9..9 + key_len - 1])));
                        reply(endpoint::BLOBDB_V1, vec![p[1], p[2], 1]);
                    }
                    _ => {}
                }
            }
        }
        // The last answer is taken before the session stops.
        let settled = tokio::time::Instant::now() + Duration::from_secs(2);
        while apps::lock(&db).watch_prefs(1).unwrap().values().any(|p| p.pending) && tokio::time::Instant::now() < settled {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        session.abort();
        // The local change and the followed units go out; the clock had been set on the watch.
        assert_eq!(&log[..4], ["version", "dirty all", "answered 1", "sync done"]);
        let mut inserts: Vec<&str> = log[4..].iter().map(String::as_str).collect();
        inserts.sort();
        assert!(inserts.contains(&"insert lightMotion"), "{log:?}");
        assert!(inserts.contains(&"insert unitsWind"), "{log:?}");
        assert!(!inserts.contains(&"insert clock24h"), "the watch's own clock is kept: {log:?}");
        let store = apps::lock(&db);
        assert_eq!(settings::watch_clock(&store, 1), settings::Clock::TwentyFour);
        assert!(!store.watch_prefs(1).unwrap()["lightMotion"].pending);
        assert!(settings::watch_has_settings(&store, 1));
    }

    #[test]
    fn newer_side_wins_and_the_clock_follows_the_watch() {
        let store = Store::in_memory();
        let w = 1;
        // The watch's older value loses to a change made here.
        store.set_watch_pref(w, "lightMotion", &[0], 200, true).unwrap();
        assert_eq!(take(&store, w, "lightMotion", &[1], 100).unwrap(), Status::DataStale);
        assert_eq!(take(&store, w, "lightMotion", &[1], 300).unwrap(), Status::Success);
        let p = &store.watch_prefs(w).unwrap()["lightMotion"];
        assert_eq!((p.value.as_slice(), p.pending), (&[1u8][..], false));
        // Unknown keys are accepted and dropped; bad values refused.
        assert_eq!(take(&store, w, "workerId", &[1], 1).unwrap(), Status::Success);
        assert!(!store.watch_prefs(w).unwrap().contains_key("workerId"));
        assert_eq!(take(&store, w, "lightTimeoutMs", &[1], 1).unwrap(), Status::InvalidData);
        // 24-hour time set on the watch fixes the choice.
        settings::set_watch_clock(&store, w, settings::Clock::Twelve).unwrap();
        take(&store, w, watchprefs::CLOCK_24H, &[1], 400).unwrap();
        assert_eq!(settings::watch_clock(&store, w), settings::Clock::TwentyFour);
        // What Lithidion decides is queued only when it differs.
        follow(&store, w).unwrap();
        assert!(!store.watch_prefs(w).unwrap()[watchprefs::CLOCK_24H].pending);
        assert!(store.watch_prefs(w).unwrap()[watchprefs::UNITS_WIND].pending);
    }
}
