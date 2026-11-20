//! Keeping the watch's timeline (calendar pins and their reminders) and weather in step with
//! the phone. What was sent is remembered per watch, so only changes go over Bluetooth.

use super::apps::{blob, lock, Db};
use super::eds::{self, Event};
use super::session::Protocol;
use super::weather::{self, Place};
use crate::settings;
use crate::store::WatchId;
use anyhow::Result;
use chrono::{Duration, Utc};
use pebble_protocol::blobdb::{self, Database, Status};
use pebble_protocol::timeline::{self, attr, icon, Attribute, Item, Kind, Layout};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use uuid::Uuid;

/// The watch's own Calendar app, parent of calendar pins.
const CALENDAR_APP: Uuid = Uuid::from_u128(0x6c6c6fc2_1912_4d25_8396_3547d1dfac5b);
const NS: Uuid = Uuid::from_u128(0x0e5a_4a1d_7b33_4f28_a1d9_44c6_37e2_8b51);

/// The window the official app sends: the past two days and the next seven.
pub const PAST_DAYS: i64 = 2;
pub const AHEAD_DAYS: i64 = 7;

const ATTR_LOCATION: u8 = 11;
const ATTR_HEADINGS: u8 = 25;
const ATTR_PARAGRAPHS: u8 = 26;
const ICON_CALENDAR: u32 = icon::CALENDAR;

fn hash(bytes: &[u8]) -> i64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish() as i64
}

fn text_list(id: u8, items: &[String]) -> Attribute {
    Attribute::text(id, &items.join("\0"), 512)
}

/// A calendar event's pin, and a reminder for each of its alarms still to come.
pub fn items(e: &Event, calendar_name: &str) -> Vec<(Database, Item)> {
    let id = Uuid::new_v5(&NS, format!("{}|{}|{}", e.calendar, e.uid, e.start.timestamp()).as_bytes());
    let minutes = ((e.end - e.start).num_minutes()).clamp(0, u16::MAX as i64) as u16;
    let mut attributes = vec![Attribute::text(attr::TITLE, &e.title, timeline::TITLE_MAX), Attribute::icon(attr::TINY_ICON, ICON_CALENDAR)];
    if let Some(l) = &e.location {
        attributes.push(Attribute::text(ATTR_LOCATION, l, timeline::TITLE_MAX));
    }
    let mut headings = Vec::new();
    let mut paragraphs = Vec::new();
    if let Some(d) = &e.description {
        headings.push(String::new());
        paragraphs.push(d.chars().take(300).collect());
    }
    if e.recurring {
        headings.push("Recurrence".to_string());
        paragraphs.push("Repeats".to_string());
    }
    headings.push("Calendar".to_string());
    paragraphs.push(calendar_name.to_string());
    attributes.push(text_list(ATTR_HEADINGS, &headings));
    attributes.push(text_list(ATTR_PARAGRAPHS, &paragraphs));
    let pin = Item {
        id,
        parent: CALENDAR_APP,
        timestamp: e.start.timestamp() as i32,
        duration: if e.all_day { 0 } else { minutes },
        kind: Kind::Pin,
        flags: if e.all_day { timeline::flags::ALL_DAY } else { 0 },
        status: 0,
        layout: Layout::CalendarPin,
        attributes,
        actions: Vec::new(),
    };
    let mut out = vec![(Database::Pins, pin)];
    let now = Utc::now();
    for before in &e.alarms {
        let at = e.start - Duration::minutes(*before);
        if at < now || e.all_day {
            continue;
        }
        let mut attributes =
            vec![Attribute::text(attr::TITLE, &e.title, timeline::TITLE_MAX), Attribute::icon(attr::TINY_ICON, icon::REMINDER)];
        if let Some(l) = &e.location {
            attributes.push(Attribute::text(ATTR_LOCATION, l, timeline::TITLE_MAX));
        }
        out.push((
            Database::Reminders,
            Item {
                id: Uuid::new_v5(&id, &before.to_le_bytes()),
                parent: id,
                timestamp: at.timestamp() as i32,
                duration: 0,
                kind: Kind::Reminder,
                flags: 0,
                status: 0,
                layout: Layout::GenericReminder,
                attributes,
                actions: Vec::new(),
            },
        ));
    }
    out
}

/// Sends what changed in one database keyed by UUID, and deletes what is no longer wanted.
async fn apply(protocol: &Protocol, db: &Db, watch: WatchId, database: Database, wanted: HashMap<Uuid, Vec<u8>>) -> Result<usize> {
    let wanted = wanted.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    apply_keyed(protocol, db, watch, database, wanted, |k| k.parse::<Uuid>().ok().map(|u| u.as_bytes().to_vec())).await
}

/// Sends what changed in one database, and deletes what is no longer wanted. Keys are kept by
/// name; `key` gives the bytes on the wire.
pub(super) async fn apply_keyed(
    protocol: &Protocol,
    db: &Db,
    watch: WatchId,
    database: Database,
    wanted: HashMap<String, Vec<u8>>,
    key: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<usize> {
    let sent = lock(db).sent_items(watch, database as u8)?;
    let mut changes = 0;
    for (name, value) in &wanted {
        let h = hash(value);
        let Some(k) = key(name) else { continue };
        if sent.get(name) == Some(&h) {
            continue;
        }
        match blob(protocol, |t| blobdb::insert(t, database, &k, value)).await? {
            Status::Success => {
                lock(db).set_sent_item(watch, database as u8, name, h)?;
                changes += 1;
            }
            s => tracing::info!(?database, key = name, status = ?s, "the watch did not take an item"),
        }
    }
    for name in sent.keys() {
        if wanted.contains_key(name) {
            continue;
        }
        let Some(k) = key(name) else { continue };
        match blob(protocol, |t| blobdb::delete(t, database, &k)).await? {
            Status::Success | Status::KeyDoesNotExist => {
                lock(db).drop_sent_item(watch, database as u8, name)?;
                changes += 1;
            }
            s => tracing::info!(?database, key = name, status = ?s, "the watch did not remove an item"),
        }
    }
    Ok(changes)
}

/// Brings the watch's calendar pins and reminders in line with the chosen calendars.
pub async fn sync_calendar(protocol: &Protocol, db: &Db, watch: WatchId) -> Result<()> {
    let chosen = settings::calendars(&lock(db));
    let now = Utc::now();
    let (events, names) = if chosen.is_empty() {
        (Vec::new(), HashMap::new())
    } else {
        let names: HashMap<String, String> = eds::calendars().await?.into_iter().map(|c| (c.uid, c.name)).collect();
        let present: Vec<String> = chosen.into_iter().filter(|c| names.contains_key(c)).collect();
        (eds::events(&present, now - Duration::days(PAST_DAYS), now + Duration::days(AHEAD_DAYS)).await?, names)
    };
    let mut pins = HashMap::new();
    let mut reminders = HashMap::new();
    for e in &events {
        let name = names.get(&e.calendar).map(String::as_str).unwrap_or("");
        for (database, item) in items(e, name) {
            let target = if database == Database::Pins { &mut pins } else { &mut reminders };
            target.insert(item.id, item.encode());
        }
    }
    // Pins first: a reminder refers to its pin.
    let changed =
        apply(protocol, db, watch, Database::Pins, pins).await? + apply(protocol, db, watch, Database::Reminders, reminders).await?;
    tracing::info!(events = events.len(), changed, "calendar sent");
    Ok(())
}

/// Brings the watch's weather places in line with the settings; `v4` for watches that take the
/// richer record.
pub async fn sync_weather(protocol: &Protocol, db: &Db, watch: WatchId, v4: bool) -> Result<()> {
    let w = settings::weather(&lock(db));
    let imperial = crate::units::imperial();
    let mut places: Vec<(Option<Place>, Place)> = Vec::new();
    if w.here {
        match weather::current_location().await {
            Ok(p) => places.push((None, p)),
            Err(e) => tracing::info!(error = format!("{e:#}"), "current location not available"),
        }
    }
    places.extend(w.places.iter().map(|p| (Some(p.clone()), p.clone())));
    let mut wanted = HashMap::new();
    for (saved, place) in &places {
        let forecast = match weather::forecast(w.provider, place, imperial).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(place = %place.name, error = format!("{e:#}"), "no forecast");
                continue;
            }
        };
        let (record, extra) = weather::record(place, saved.is_none(), &forecast);
        let value = if v4 { record.encode_v4(&extra) } else { record.encode_v3() };
        wanted.insert(weather::key(saved.as_ref()), value);
    }
    // A failed fetch keeps the old forecast rather than removing the place.
    if wanted.is_empty() && !places.is_empty() {
        anyhow::bail!("no forecast could be fetched");
    }
    let changed = apply(protocol, db, watch, Database::Weather, wanted).await?;
    tracing::info!(places = places.len(), changed, provider = w.provider.name(), "weather sent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn pins_and_reminders() {
        let start = Utc::now() + Duration::hours(3);
        let e = Event {
            calendar: "cal".into(),
            uid: "u".into(),
            title: "Dentist".into(),
            location: Some("Main St".into()),
            description: None,
            start,
            end: start + Duration::minutes(45),
            all_day: false,
            recurring: false,
            alarms: vec![15, 600],
        };
        let out = items(&e, "Personal");
        // The alarm 10 hours before has passed.
        assert_eq!(out.len(), 2);
        let (db, pin) = &out[0];
        assert_eq!((*db, pin.kind, pin.duration), (Database::Pins, Kind::Pin, 45));
        let (db, reminder) = &out[1];
        assert_eq!(
            (*db, reminder.parent, reminder.timestamp),
            (Database::Reminders, pin.id, (start - Duration::minutes(15)).timestamp() as i32)
        );
        // Stable ids, so a resync updates instead of duplicating.
        assert_eq!(items(&e, "Personal")[0].1.id, pin.id);
        let all_day = Event { all_day: true, start: Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap(), ..e };
        let (_, p) = &items(&all_day, "Personal")[0];
        assert_eq!((p.flags, p.duration), (timeline::flags::ALL_DAY, 0));
    }
}
