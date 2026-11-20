//! Evolution Data Server, the phone's store for calendars and contacts (PROTOCOL.md section 16):
//! listing calendars, reading their events with recurrences expanded, and finding a caller's
//! name by number.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use std::collections::{HashMap, HashSet};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::Connection;

const PREFIX: &str = "org.gnome.evolution.dataserver.";
const SOURCE_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";

/// EDS puts its API version in its bus names (`Calendar8`, `Sources5`); finds the one in use.
async fn service(bus: &Connection, kind: &str) -> Result<String> {
    let dbus = zbus::fdo::DBusProxy::new(bus).await?;
    let mut names: Vec<String> = dbus.list_names().await?.into_iter().map(|n| n.to_string()).collect();
    names.extend(dbus.list_activatable_names().await?.into_iter().map(|n| n.to_string()));
    let wanted = format!("{PREFIX}{kind}");
    names
        .into_iter()
        .filter(|n| n.strip_prefix(&wanted).is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())))
        .max_by_key(|n| n[wanted.len()..].parse::<u32>().unwrap_or(0))
        .ok_or_else(|| anyhow!("Evolution Data Server ({kind}) is not available"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarInfo {
    pub uid: String,
    pub name: String,
    /// The account or collection it belongs to ("Personal", "Google").
    pub account: Option<String>,
}

/// One source's key-file `Data`: its sections and their keys.
fn key_file(data: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for line in data.lines().map(str::trim) {
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.to_string();
            out.entry(section.clone()).or_default();
        } else if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone()).or_default().insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

async fn sources(bus: &Connection) -> Result<Vec<(String, HashMap<String, HashMap<String, String>>)>> {
    let name = service(bus, "Sources").await?;
    let manager = zbus::Proxy::new(bus, name, SOURCE_PATH, "org.freedesktop.DBus.ObjectManager").await?;
    let managed: Managed = manager.call("GetManagedObjects", &()).await.context("the calendar sources cannot be read")?;
    let mut out = Vec::new();
    for ifaces in managed.into_values() {
        let Some(props) = ifaces.get("org.gnome.evolution.dataserver.Source") else { continue };
        let text = |k: &str| props.get(k).and_then(|v| String::try_from(v.clone()).ok());
        let (Some(uid), Some(data)) = (text("UID"), text("Data")) else { continue };
        out.push((uid, key_file(&data)));
    }
    Ok(out)
}

fn enabled(sections: &HashMap<String, HashMap<String, String>>) -> bool {
    sections.get("Data Source").and_then(|d| d.get("Enabled")).is_none_or(|v| v != "false")
}

/// Every enabled calendar, by name.
pub async fn calendars() -> Result<Vec<CalendarInfo>> {
    let bus = Connection::session().await?;
    let all = sources(&bus).await?;
    let names: HashMap<&str, &str> =
        all.iter().filter_map(|(uid, s)| Some((uid.as_str(), s.get("Data Source")?.get("DisplayName")?.as_str()))).collect();
    let mut out: Vec<CalendarInfo> = all
        .iter()
        .filter(|(_, s)| s.contains_key("Calendar") && enabled(s))
        .map(|(uid, s)| {
            let ds = s.get("Data Source");
            let parent = ds.and_then(|d| d.get("Parent")).filter(|p| !p.is_empty());
            CalendarInfo {
                uid: uid.clone(),
                name: ds.and_then(|d| d.get("DisplayName")).cloned().unwrap_or_else(|| uid.clone()),
                account: parent.and_then(|p| names.get(p.as_str())).map(|n| n.to_string()),
            }
        })
        .collect();
    out.sort_by(|a, b| a.account.cmp(&b.account).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

/// Opens a calendar or address book through its factory; returns a proxy on the opened object.
async fn open<'a>(bus: &'a Connection, kind: &str, method: &str, iface: &'a str, uid: &str) -> Result<zbus::Proxy<'a>> {
    let name = service(bus, kind).await?;
    let path = format!("/org/gnome/evolution/dataserver/{kind}Factory");
    let factory = zbus::Proxy::new(bus, name, path, format!("{PREFIX}{kind}Factory")).await?;
    let (object, owner): (String, String) = factory.call(method, &(uid,)).await.with_context(|| format!("{method} failed"))?;
    let proxy = zbus::Proxy::new(bus, owner, object, iface).await?;
    proxy.call_method("Open", &()).await.context("the calendar did not open")?;
    Ok(proxy)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub calendar: String,
    pub uid: String,
    pub title: String,
    pub location: Option<String>,
    pub description: Option<String>,
    /// For all-day events, midnight of the first day as if UTC (the watch reads it as local).
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub recurring: bool,
    /// Minutes before the start, for each alarm.
    pub alarms: Vec<i64>,
}

/// An ISO 8601 duration as alarms use it ("-PT15M", "-P1D"), in minutes.
fn duration_minutes(s: &str) -> Option<i64> {
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let rest = rest.strip_prefix('P')?;
    let mut minutes = 0i64;
    let mut number = String::new();
    let mut in_time = false;
    for c in rest.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => number.push(c),
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                minutes += match (unit, in_time) {
                    ('W', false) => n * 7 * 1440,
                    ('D', false) => n * 1440,
                    ('H', true) => n * 60,
                    ('M', true) => n,
                    ('S', true) => n / 60,
                    _ => return None,
                };
            }
        }
    }
    Some(sign * minutes)
}

/// Alarms relative to the start, as minutes before it.
fn alarms(event: &icalendar::Event) -> Vec<i64> {
    use icalendar::Component;
    let mut out: Vec<i64> = event
        .components()
        .iter()
        .filter(|c| c.component_kind() == "VALARM")
        .filter_map(|a| {
            let trigger = a.properties().get("TRIGGER")?;
            if trigger.params().get("RELATED").is_some_and(|r| r.value() == "END") {
                return None;
            }
            let before = -duration_minutes(trigger.value())?;
            (0..=7 * 1440).contains(&before).then_some(before)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn as_utc(d: Option<icalendar::DatePerhapsTime>) -> Option<(DateTime<Utc>, bool)> {
    use chrono::TimeZone;
    match d? {
        // Floating times are the phone's local time.
        icalendar::DatePerhapsTime::DateTime(icalendar::CalendarDateTime::Floating(t)) => {
            crate::host::Zone.from_local_datetime(&t).earliest().map(|t| (t.with_timezone(&Utc), false))
        }
        icalendar::DatePerhapsTime::DateTime(dt) => dt.try_into_utc().map(|t| (t, false)),
        icalendar::DatePerhapsTime::Date(d) => d.and_hms_opt(0, 0, 0).map(|t| (t.and_utc(), true)),
    }
}

/// Expands the iCalendar objects of one calendar into the occurrences inside the window.
pub fn expand(calendar: &str, objects: &[String], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Event> {
    use icalendar::{Component, EventLike};
    let mut events = Vec::new();
    let mut parsed: Vec<icalendar::Event> = Vec::new();
    for text in objects {
        // EDS names some zones by its own prefix ("/freeassociation.sourceforge.net/America/Chicago").
        let text = &text.replace("/freeassociation.sourceforge.net/Tzfile/", "").replace("/freeassociation.sourceforge.net/", "");
        // EDS gives bare VEVENTs; the parser wants a whole calendar.
        let wrapped = if text.trim_start().starts_with("BEGIN:VCALENDAR") {
            text.clone()
        } else {
            format!("BEGIN:VCALENDAR\r\n{text}\r\nEND:VCALENDAR\r\n")
        };
        let Ok(cal) = wrapped.parse::<icalendar::Calendar>() else { continue };
        for c in cal.components {
            if let icalendar::CalendarComponent::Event(e) = c {
                parsed.push(e);
            }
        }
    }
    // Detached occurrences replace their series' occurrence at the same start.
    let replaced: HashSet<(String, i64)> = parsed
        .iter()
        .filter_map(|e| {
            let when = e.get_recurrence_id();
            Some((e.get_uid()?.to_string(), as_utc(when)?.0.timestamp()))
        })
        .collect();
    for e in &parsed {
        let (Some(uid), Some((start, all_day))) = (e.get_uid(), as_utc(e.get_start())) else { continue };
        if e.property_value("STATUS") == Some("CANCELLED") {
            continue;
        }
        let end = as_utc(e.get_end()).map(|(t, _)| t).unwrap_or(if all_day { start + Duration::days(1) } else { start });
        let length = end - start;
        let is_override = e.properties().contains_key("RECURRENCE-ID");
        let recurring = e.properties().contains_key("RRULE") || e.properties().contains_key("RDATE");
        let mut starts = Vec::new();
        if recurring && !is_override {
            match e.get_recurrence() {
                Ok(set) => {
                    let tz = icalendar::Tz::UTC;
                    // An occurrence that started before the window may still be running in it.
                    let result = set.after((from - length).with_timezone(&tz)).before(to.with_timezone(&tz)).all(500);
                    starts.extend(result.dates.into_iter().map(|d| d.with_timezone(&Utc)));
                }
                Err(_) => starts.push(start),
            }
        } else {
            starts.push(start);
        }
        for s in starts {
            if !is_override && recurring && replaced.contains(&(uid.to_string(), s.timestamp())) {
                continue;
            }
            if s + length < from || s >= to {
                continue;
            }
            events.push(Event {
                calendar: calendar.to_string(),
                uid: uid.to_string(),
                title: e.get_summary().unwrap_or("Untitled").trim().to_string(),
                location: e.get_location().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()),
                description: e.get_description().map(|d| d.trim().to_string()).filter(|d| !d.is_empty()),
                start: s,
                end: s + length,
                all_day,
                recurring,
                alarms: alarms(e),
            });
        }
    }
    events
}

/// EDS's query for events touching the window.
fn window_query(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let f = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
    format!("(occur-in-time-range? (make-time \"{}\") (make-time \"{}\"))", f(from), f(to))
}

/// Events of the given calendars inside the window.
pub async fn events(calendars: &[String], from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Event>> {
    let bus = Connection::session().await?;
    let mut out = Vec::new();
    for uid in calendars {
        let cal = match open(&bus, "Calendar", "OpenCalendar", "org.gnome.evolution.dataserver.Calendar", uid).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(calendar = %uid, error = format!("{e:#}"), "calendar not read");
                continue;
            }
        };
        let objects: Vec<String> = match cal.call("GetObjectList", &(window_query(from, to),)).await {
            Ok(o) => o,
            // Some backends do not take the time query; everything is read then.
            Err(_) => cal.call("GetObjectList", &("#t",)).await.context("the calendar's events cannot be read")?,
        };
        out.extend(expand(uid, &objects, from, to));
    }
    out.sort_by_key(|e| e.start);
    Ok(out)
}

/// Digits only, the last ten, for comparing numbers written differently.
fn digits(number: &str) -> String {
    let d: String = number.chars().filter(char::is_ascii_digit).collect();
    d[d.len().saturating_sub(10)..].to_string()
}

/// The `FN` of the first vCard with a matching `TEL`.
fn name_in(vcards: &[String], number: &str) -> Option<String> {
    let want = digits(number);
    if want.len() < 4 {
        return None;
    }
    for card in vcards {
        let unfolded = card.replace("\r\n ", "").replace("\n ", "");
        let mut name = None;
        let mut matches = false;
        for line in unfolded.lines() {
            let Some((key, value)) = line.split_once(':') else { continue };
            let key = key.split(';').next().unwrap_or("").to_ascii_uppercase();
            match key.as_str() {
                "FN" => name = Some(value.trim().to_string()),
                "TEL" => matches |= digits(value) == want,
                _ => {}
            }
        }
        if matches {
            return name.filter(|n| !n.is_empty());
        }
    }
    None
}

/// The contact name for a phone number, from any address book.
pub async fn contact_name(number: &str) -> Result<Option<String>> {
    if digits(number).len() < 4 {
        return Ok(None);
    }
    let bus = Connection::session().await?;
    let books: Vec<String> =
        sources(&bus).await?.into_iter().filter(|(_, s)| s.contains_key("Address Book") && enabled(s)).map(|(uid, _)| uid).collect();
    let query = format!("(eqphone_national \"phone\" \"{}\")", number.replace('"', ""));
    for uid in books {
        let Ok(book) = open(&bus, "AddressBook", "OpenAddressBook", "org.gnome.evolution.dataserver.AddressBook", &uid).await else {
            continue;
        };
        let found: Vec<String> = match book.call("GetContactList", &(query.as_str(),)).await {
            Ok(f) => f,
            Err(_) => continue,
        };
        if let Some(name) = name_in(&found, number) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Midnight of a date as if UTC, how the watch takes all-day times.
#[allow(dead_code)]
pub fn date_as_utc(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn durations() {
        assert_eq!(duration_minutes("-PT15M"), Some(-15));
        assert_eq!(duration_minutes("-P1D"), Some(-1440));
        assert_eq!(duration_minutes("-P1DT2H"), Some(-1560));
        assert_eq!(duration_minutes("PT0S"), Some(0));
        assert_eq!(duration_minutes("junk"), None);
    }

    #[test]
    fn key_files() {
        let k = key_file("[Data Source]\nDisplayName=Personal\nEnabled=true\nParent=local-stub\n\n[Calendar]\nColor=#62a0ea\n");
        assert_eq!(k["Data Source"]["DisplayName"], "Personal");
        assert!(k.contains_key("Calendar"));
        assert!(enabled(&k));
    }

    #[test]
    fn recurring_events() {
        let weekly = "BEGIN:VEVENT\r\nUID:w1\r\nSUMMARY:Standup\r\nDTSTART:20261102T150000Z\r\nDTEND:20261102T151500Z\r\nRRULE:FREQ=WEEKLY;COUNT=10\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT";
        let moved = "BEGIN:VEVENT\r\nUID:w1\r\nSUMMARY:Standup (moved)\r\nRECURRENCE-ID:20261116T150000Z\r\nDTSTART:20261117T150000Z\r\nDTEND:20261117T151500Z\r\nEND:VEVENT";
        let zoned = "BEGIN:VEVENT\r\nUID:z1\r\nSUMMARY:Dentist\r\nDTSTART;TZID=/freeassociation.sourceforge.net/America/Chicago:20261118T090000\r\nDTEND;TZID=/freeassociation.sourceforge.net/America/Chicago:20261118T100000\r\nEND:VEVENT";
        let day = "BEGIN:VEVENT\r\nUID:d1\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261120\r\nDTEND;VALUE=DATE:20261121\r\nLOCATION:Home\r\nEND:VEVENT";
        let from = Utc.with_ymd_and_hms(2026, 11, 12, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 11, 26, 0, 0, 0).unwrap();
        let ev = expand("cal", &[weekly.into(), moved.into(), day.into(), zoned.into()], from, to);
        let dentist = ev.iter().find(|e| e.uid == "z1").expect("zoned event read");
        assert_eq!(dentist.start, Utc.with_ymd_and_hms(2026, 11, 18, 15, 0, 0).unwrap());
        let titles: Vec<_> = ev.iter().map(|e| (e.title.as_str(), e.start.format("%m-%d").to_string())).collect();
        // The 16th's occurrence is replaced by the moved one on the 17th.
        assert!(titles.contains(&("Standup (moved)", "11-17".to_string())), "{titles:?}");
        assert!(titles.contains(&("Standup", "11-23".to_string())), "{titles:?}");
        assert!(!titles.contains(&("Standup", "11-16".to_string())), "{titles:?}");
        let standup = ev.iter().find(|e| e.title == "Standup").unwrap();
        assert_eq!(standup.alarms, vec![10]);
        assert_eq!((standup.end - standup.start).num_minutes(), 15);
        let holiday = ev.iter().find(|e| e.uid == "d1").unwrap();
        assert!(holiday.all_day);
        assert_eq!(holiday.location.as_deref(), Some("Home"));
        assert_eq!(holiday.start, date_as_utc(NaiveDate::from_ymd_opt(2026, 11, 20).unwrap()));
    }

    #[test]
    fn contacts() {
        let cards = vec!["BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alex Doe\r\nTEL;TYPE=CELL:+1 (479) 555-0100\r\nEND:VCARD".to_string()];
        assert_eq!(name_in(&cards, "4795550100").as_deref(), Some("Alex Doe"));
        assert_eq!(name_in(&cards, "+14795550100").as_deref(), Some("Alex Doe"));
        assert_eq!(name_in(&cards, "4795550199"), None);
    }
}
