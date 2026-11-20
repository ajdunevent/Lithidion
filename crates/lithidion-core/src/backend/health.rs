//! Health: keeping what the watch logs (section 17), summing it up for the Health tab, and
//! sending the watch its wearer's profile (BlobDB 7) and history (BlobDB 10, section 18).
//! History belongs to a profile, so it carries on across watches; for a minute or a session
//! two watches both logged, the fuller one is kept.

use super::apps::{lock, Db};
use super::phonedata::apply_keyed;
use super::session::Protocol;
use crate::host::Zone as Local;
use crate::settings::{self, HealthSettings};
use crate::store::{ProfileId, Store, WatchId};
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Timelike};
use pebble_protocol::blobdb::Database;
use pebble_protocol::datalog::{self, tag, Incoming};
use pebble_protocol::endpoint;
use pebble_protocol::health::{self, params, stats, Minute, Session};
use pebble_protocol::system::Version;
use std::collections::{BTreeSet, HashMap};

/// Sleep belonging to a day starts between 6 PM the evening before and 2 PM.
const NIGHT_FROM: i64 = -6 * 3600;
const NIGHT_TO: i64 = 14 * 3600;
/// Sleep with a shorter gap is one sleep.
const SLEEP_GAP: i64 = 3600;
const AVERAGE_DAYS: i64 = 30;
const TYPICAL_WEEKS: i64 = 7;
const TYPICAL_MIN_DAYS: usize = 2;
const TYPICAL_MIN_SLEEP: u32 = 1800;

/// What a watch's data logging session carries.
#[derive(Debug, Clone, Copy)]
struct Open {
    tag: Option<u32>,
    item_size: u16,
}

/// What came of a data logging message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Nothing,
    /// Health data was stored; `last` when the watch has no more to send.
    Health {
        last: bool,
    },
}

/// Data logging sessions of one connection. Health data is stored before it is acknowledged,
/// so the watch keeps what could not be stored; other apps' data is acknowledged and dropped,
/// as no app here reads it.
#[derive(Default)]
pub struct Receiver {
    sessions: HashMap<u8, Open>,
}

impl Receiver {
    /// Asks the watch to open its sessions again, as the phone has none.
    pub fn start(&mut self, protocol: &Protocol) {
        self.sessions.clear();
        let _ = protocol.send(endpoint::DATA_LOG, &datalog::report_open_sessions(&[]));
    }

    /// `watch` is `None` while nothing can be stored (recovery firmware): everything is refused
    /// and stays on the watch.
    pub fn handle(&mut self, protocol: &Protocol, db: &Db, watch: Option<(ProfileId, WatchId)>, payload: &[u8]) -> Received {
        let message = match datalog::parse(payload) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(error = %e, "data logging message not understood");
                return Received::Nothing;
            }
        };
        let reply = |session: u8, ok: bool| {
            let _ = protocol.send(endpoint::DATA_LOG, &if ok { datalog::ack(session) } else { datalog::nack(session) });
        };
        match message {
            Incoming::Open { session, app, tag, item_size, .. } => {
                let health = app.is_nil() && matches!(tag, tag::STEPS | tag::SLEEP | tag::OVERLAY | tag::HEART_RATE);
                tracing::debug!(session, %app, tag, item_size, "data logging session opened");
                self.sessions.insert(session, Open { tag: health.then_some(tag), item_size });
                reply(session, watch.is_some());
                Received::Nothing
            }
            Incoming::Data { session, items_left, payload, .. } => {
                let (Some(open), Some(watch)) = (self.sessions.get(&session).copied(), watch) else {
                    // Unknown to this connection: the watch sends it again once reopened.
                    reply(session, false);
                    return Received::Nothing;
                };
                let Some(t) = open.tag else {
                    reply(session, true);
                    return Received::Nothing;
                };
                let stored = store(&mut lock(db), watch.0, watch.1, t, &payload, open.item_size);
                match stored {
                    Ok(n) => {
                        tracing::debug!(session, tag = t, records = n, items_left, "health data stored");
                        reply(session, true);
                        Received::Health { last: items_left == 0 }
                    }
                    Err(e) => {
                        tracing::warn!(error = format!("{e:#}"), "health data not stored");
                        reply(session, false);
                        Received::Nothing
                    }
                }
            }
            Incoming::Close { session } => {
                self.sessions.remove(&session);
                reply(session, true);
                Received::Nothing
            }
            Incoming::Timeout { session } => {
                tracing::info!(session, "the watch missed an acknowledgement");
                Received::Nothing
            }
            Incoming::SendEnabled(_) => Received::Nothing,
        }
    }
}

fn store(store: &mut Store, profile: ProfileId, watch: WatchId, tag: u32, payload: &[u8], item_size: u16) -> Result<usize> {
    match tag {
        tag::STEPS => {
            let minutes = health::parse_minutes(payload, item_size);
            store.add_health_minutes(profile, watch, &minutes)?;
            Ok(minutes.len())
        }
        tag::SLEEP | tag::OVERLAY => {
            let sessions = health::parse_sessions(payload, item_size);
            store.add_health_sessions(profile, watch, &sessions)?;
            Ok(sessions.len())
        }
        // Heart rate measurements are in the minutes too; this tag's records are not kept.
        _ => Ok(0),
    }
}

/// Asks the watch to send what it has logged now.
pub fn request_sync(protocol: &Protocol, db: &Db, profile: ProfileId) {
    let latest = lock(db).health_span(profile).ok().flatten().map(|(_, last)| last);
    let since = latest.map(|t| (chrono::Utc::now().timestamp() - t).clamp(0, u32::MAX as i64) as u32).unwrap_or(u32::MAX);
    let _ = protocol.send(endpoint::HEALTH_SYNC, &health::sync_request(since));
}

/// What a connection knows about the watch's health features.
#[derive(Debug, Clone, Copy)]
pub struct Watch {
    pub id: WatchId,
    pub profile: ProfileId,
    pub firmware: Version,
    pub heart_rate: bool,
    /// The watch takes the profile and settings (`HealthInsights`).
    pub settings: bool,
}

/// Brings the watch's health settings in line with the phone's.
pub async fn sync_settings(protocol: &Protocol, db: &Db, watch: Watch) -> Result<()> {
    if !watch.settings {
        return Ok(());
    }
    let h = settings::health(&lock(db), watch.profile);
    let wanted = settings_values(&h, watch, crate::units::imperial(), Local::now().year());
    let changed = apply_keyed(protocol, db, watch.id, Database::HealthParams, wanted, |k| Some(k.as_bytes().to_vec())).await?;
    tracing::info!(changed, "health settings sent");
    Ok(())
}

fn settings_values(h: &HealthSettings, watch: Watch, imperial: bool, this_year: i32) -> HashMap<String, Vec<u8>> {
    let age = h.age(this_year);
    let profile = health::Profile {
        height_mm: h.height_mm,
        weight_dag: h.weight_dag,
        tracking: h.tracking,
        activity_insights: h.activity_insights,
        sleep_insights: h.sleep_insights,
        age,
        sex: match h.sex {
            settings::Sex::Female => health::Sex::Female,
            settings::Sex::Male => health::Sex::Male,
            settings::Sex::Other => health::Sex::Other,
        },
    };
    let mut out = HashMap::from([(params::ACTIVITY.to_string(), profile.encode()), (params::UNITS.to_string(), health::units(imperial))]);
    if watch.heart_rate {
        let interval = interval(h.heart_rate_interval, watch.firmware);
        out.insert(
            params::HEART_RATE_MONITOR.to_string(),
            health::heart_rate_monitor(h.heart_rate, interval, h.heart_rate_during_activities, watch.firmware),
        );
        out.insert(params::HEART_RATE.to_string(), health::heart_rate(age, 70, 100));
    }
    out
}

/// The interval to send: five minutes needs newer firmware, which falls back to ten.
fn interval(i: settings::Interval, firmware: Version) -> health::Interval {
    let wanted = match i {
        settings::Interval::Five => health::Interval::FiveMinutes,
        settings::Interval::Ten => health::Interval::TenMinutes,
        settings::Interval::Thirty => health::Interval::ThirtyMinutes,
        settings::Interval::Hour => health::Interval::Hour,
    };
    if firmware >= wanted.since() {
        wanted
    } else {
        health::Interval::TenMinutes
    }
}

pub fn five_minutes_supported(firmware: Version) -> bool {
    firmware >= health::Interval::FiveMinutes.since()
}

/// Sends the watch the past week, the typical day for each weekday, and the monthly averages,
/// for its Health app's comparisons and in case it lost its own.
pub async fn sync_stats(protocol: &Protocol, db: &Db, watch: Watch) -> Result<()> {
    let wanted = stats_values(&lock(db), watch.profile, watch.heart_rate, Local::now().date_naive())?;
    if wanted.is_empty() {
        return Ok(());
    }
    let changed = apply_keyed(protocol, db, watch.id, Database::HealthStats, wanted, |k| Some(k.as_bytes().to_vec())).await?;
    tracing::info!(changed, "health history sent");
    Ok(())
}

/// Local midnight starting `date`, as UTC seconds.
pub fn midnight(date: NaiveDate) -> i64 {
    let naive = date.and_hms_opt(0, 0, 0).expect("midnight exists");
    match Local.from_local_datetime(&naive).earliest() {
        Some(t) => t.timestamp(),
        // Midnight skipped by a clock change: the day starts an hour later.
        None => Local.from_utc_datetime(&naive).timestamp(),
    }
}

fn seconds_of_day(t: i64) -> u32 {
    Local.timestamp_opt(t, 0).single().map(|d| d.num_seconds_from_midnight()).unwrap_or(0)
}

fn weekday(date: NaiveDate) -> usize {
    date.weekday().num_days_from_monday() as usize
}

/// A day's activity.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub steps: u32,
    pub distance_m: u32,
    pub active_minutes: u32,
    pub active_kcal: u32,
    pub resting_kcal: u32,
    /// Minutes the watch recorded.
    pub minutes: u32,
}

pub fn totals<'a>(minutes: impl IntoIterator<Item = &'a Minute>) -> Totals {
    let (mut t, mut distance_cm, mut active_cal, mut resting_cal) = (Totals::default(), 0u64, 0u64, 0u64);
    for m in minutes {
        t.steps += m.steps as u32;
        t.active_minutes += m.active as u32;
        t.minutes += 1;
        distance_cm += m.distance_cm as u64;
        active_cal += m.active_cal as u64;
        resting_cal += m.resting_cal as u64;
    }
    t.distance_m = (distance_cm / 100) as u32;
    t.active_kcal = (active_cal / 1000) as u32;
    t.resting_kcal = (resting_cal / 1000) as u32;
    t
}

/// One sleep: consecutive sleep sessions with less than an hour between them.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sleep {
    pub start: i64,
    pub end: i64,
    /// Seconds asleep; deep sleep is part of it.
    pub total: u32,
    pub deep: u32,
}

/// The sleep of the night before a day, and naps on it.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Night {
    pub sleeps: Vec<Sleep>,
    pub total: u32,
    pub deep: u32,
}

impl Night {
    pub fn start(&self) -> i64 {
        self.sleeps[0].start
    }

    pub fn end(&self) -> i64 {
        self.sleeps[self.sleeps.len() - 1].end
    }
}

fn group_sleep<'a>(sessions: impl IntoIterator<Item = &'a Session>) -> Vec<Sleep> {
    let mut sleep: Vec<&Session> = sessions.into_iter().filter(|s| s.kind.is_sleep()).collect();
    sleep.sort_by_key(|s| s.start);
    // Each sleep, with its sessions' spans; overlapping spans (two watches) count once.
    let mut groups: Vec<SleepGroup> = Vec::new();
    for s in sleep {
        let span = (s.start as i64, s.start as i64 + s.seconds as i64);
        match groups.last_mut() {
            Some(g) if span.0 <= g.1 + SLEEP_GAP => {
                g.1 = g.1.max(span.1);
                if s.kind.is_deep() {
                    g.3.push(span)
                } else {
                    g.2.push(span)
                }
            }
            _ => {
                let (light, deep) = if s.kind.is_deep() { (vec![], vec![span]) } else { (vec![span], vec![]) };
                groups.push((span.0, span.1, light, deep));
            }
        }
    }
    groups.into_iter().map(|(start, end, light, deep)| Sleep { start, end, total: covered(light), deep: covered(deep) }).collect()
}

/// Start, end, and the light and deep sleep spans of one sleep.
type SleepGroup = (i64, i64, Vec<(i64, i64)>, Vec<(i64, i64)>);

/// Seconds covered by spans, overlaps counted once.
fn covered(mut spans: Vec<(i64, i64)>) -> u32 {
    spans.sort_unstable();
    let (mut total, mut reach) = (0i64, i64::MIN);
    for (a, b) in spans {
        let a = a.max(reach);
        if b > a {
            total += b - a;
            reach = b;
        }
    }
    total as u32
}

/// `sessions` should cover the night's window.
pub fn night(sessions: &[Session], date: NaiveDate) -> Option<Night> {
    let day = midnight(date);
    let (from, to) = (day + NIGHT_FROM, day + NIGHT_TO);
    let sleeps = group_sleep(sessions.iter().filter(|s| (from..to).contains(&(s.start as i64))));
    if sleeps.is_empty() {
        return None;
    }
    let (total, deep) = (sleeps.iter().map(|s| s.total).sum(), sleeps.iter().map(|s| s.deep).sum());
    Some(Night { sleeps, total, deep })
}

/// Heart rate over a day.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeartRate {
    pub latest: Option<(i64, u8)>,
    pub low: Option<u8>,
    pub high: Option<u8>,
    /// From the lowest readings while asleep.
    pub resting: Option<u8>,
}

fn heart_rate(today: &[Minute], night: Option<&Night>, overnight: &[Minute]) -> HeartRate {
    let readings: Vec<&Minute> = today.iter().filter(|m| m.heart_rate > 0).collect();
    let mut asleep: Vec<u8> = match night {
        Some(n) => overnight
            .iter()
            .filter(|m| m.heart_rate > 0 && n.sleeps.iter().any(|s| (s.start..s.end).contains(&(m.time as i64))))
            .map(|m| m.heart_rate)
            .collect(),
        None => Vec::new(),
    };
    asleep.sort_unstable();
    let resting = (asleep.len() >= 10).then(|| {
        let lowest = &asleep[..5];
        (lowest.iter().map(|&b| b as u32).sum::<u32>() as f64 / 5.0).round() as u8
    });
    HeartRate {
        latest: readings.last().map(|m| (m.time as i64, m.heart_rate)),
        low: readings.iter().map(|m| m.heart_rate).min(),
        high: readings.iter().map(|m| m.heart_rate).max(),
        resting,
    }
}

/// One day of the week view.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub date: NaiveDate,
    pub steps: u32,
    pub sleep: u32,
}

/// What the Health tab shows for one profile.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct View {
    pub profile: ProfileId,
    pub today: Totals,
    pub heart_rate: HeartRate,
    pub night: Option<Night>,
    /// The last seven days, today last.
    pub week: Vec<Day>,
    /// Walks, runs, and workouts of the last seven days, latest first.
    pub activities: Vec<Session>,
    /// The first and last minute kept.
    pub span: Option<(i64, i64)>,
    pub settings: HealthSettings,
    pub has_heart_rate: bool,
    pub five_minutes: bool,
    /// Whether the watch takes the profile and settings.
    pub settable: bool,
}

/// `watch`: the profile's active watch, for what it can do.
pub fn view(store: &Store, profile: ProfileId, watch: Option<&crate::store::Watch>, settable: bool) -> Result<View> {
    let today = Local::now().date_naive();
    let week_start = today - Duration::days(6);
    let (from, to) = (midnight(week_start), midnight(today + Duration::days(1)));
    let minutes = store.health_minutes(profile, from + NIGHT_FROM, to)?;
    let sessions = store.health_sessions(profile, from + NIGHT_FROM - 86_400, to)?;
    let mut week = Vec::new();
    for i in 0..7 {
        let date = week_start + Duration::days(i);
        let (a, b) = (midnight(date), midnight(date + Duration::days(1)));
        let steps = totals(minutes.iter().filter(|m| (a..b).contains(&(m.time as i64)))).steps;
        week.push(Day { date, steps, sleep: night(&sessions, date).map(|n| n.total).unwrap_or(0) });
    }
    let start = midnight(today);
    let today_minutes: Vec<Minute> = minutes.iter().filter(|m| m.time as i64 >= start).copied().collect();
    let last_night = night(&sessions, today);
    let mut activities: Vec<Session> = sessions.iter().filter(|s| !s.kind.is_sleep() && s.start as i64 >= from).copied().collect();
    activities.reverse();
    let platform = watch.and_then(|w| w.hardware).and_then(|h| pebble_protocol::hardware::HardwarePlatform::from_number(h).app_platform);
    let firmware = watch.and_then(|w| w.firmware.as_deref()).and_then(pebble_protocol::system::parse_version);
    Ok(View {
        profile,
        today: totals(&today_minutes),
        heart_rate: heart_rate(&today_minutes, last_night.as_ref(), &minutes),
        night: last_night,
        week,
        activities,
        span: store.health_span(profile)?,
        settings: settings::health(store, profile),
        has_heart_rate: watch.is_some_and(|w| pebble_protocol::hardware::has_heart_rate(w.model, platform)),
        five_minutes: firmware.is_some_and(five_minutes_supported),
        settable,
    })
}

/// How far back the Health tab's charts go.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Week,
    Month,
    Year,
}

/// Days in the month chart: five rows of six.
pub const MONTH_DAYS: i64 = 30;

/// The bars of a chart: days, or for a year, months with their daily averages (each dated the
/// first of its month).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Trend {
    pub profile: ProfileId,
    pub range: Range,
    pub bars: Vec<Day>,
}

fn days(store: &Store, profile: ProfileId, first: NaiveDate, last: NaiveDate) -> Result<Vec<Day>> {
    let count = (last - first).num_days() + 1;
    let dates: Vec<NaiveDate> = (0..count).map(|i| first + Duration::days(i)).collect();
    let bounds: Vec<i64> = dates.iter().map(|d| midnight(*d)).chain([midnight(last + Duration::days(1))]).collect();
    let steps = store.health_steps(profile, &bounds)?;
    let sessions = store.health_sessions(profile, bounds[0] + NIGHT_FROM, bounds[bounds.len() - 1] + NIGHT_TO)?;
    Ok(dates
        .into_iter()
        .zip(steps)
        .map(|(date, steps)| Day { date, steps, sleep: night(&sessions, date).map(|n| n.total).unwrap_or(0) })
        .collect())
}

pub fn trend(store: &Store, profile: ProfileId, range: Range) -> Result<Trend> {
    let today = Local::now().date_naive();
    let bars = match range {
        Range::Week => days(store, profile, today - Duration::days(6), today)?,
        Range::Month => days(store, profile, today - Duration::days(MONTH_DAYS - 1), today)?,
        Range::Year => {
            let this_month = today.with_day(1).expect("the first exists");
            let first = this_month.checked_sub_months(chrono::Months::new(11)).expect("in range");
            let all = days(store, profile, first, today)?;
            // Averages over the days that have any: a day without the watch is not a day of rest.
            let mean = |v: Vec<u32>| v.iter().sum::<u32>().checked_div(v.len() as u32).unwrap_or(0);
            (0..12)
                .map(|i| {
                    let month = first.checked_add_months(chrono::Months::new(i)).expect("in range");
                    let of = all.iter().filter(|d| d.date.year() == month.year() && d.date.month() == month.month());
                    Day {
                        date: month,
                        steps: mean(of.clone().map(|d| d.steps).filter(|&s| s > 0).collect()),
                        sleep: mean(of.map(|d| d.sleep).filter(|&s| s > 0).collect()),
                    }
                })
                .collect()
        }
    };
    Ok(Trend { profile, range, bars })
}

/// A stretch of sleep, light or deep.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepBlock {
    pub start: i64,
    pub end: i64,
    pub deep: bool,
}

/// One day hour by hour.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct DayDetail {
    pub profile: ProfileId,
    pub date: NaiveDate,
    /// Local midnight at its start and end.
    pub bounds: (i64, i64),
    pub totals: Totals,
    /// Steps in each hour of the clock.
    pub hourly: [u32; 24],
    pub heart_rate: Vec<(i64, u8)>,
    pub spo2: Vec<(i64, u8)>,
    /// The night before the day.
    pub night: Option<Night>,
    pub sleep: Vec<SleepBlock>,
}

pub fn day(store: &Store, profile: ProfileId, date: NaiveDate) -> Result<DayDetail> {
    let bounds = (midnight(date), midnight(date + Duration::days(1)));
    let minutes = store.health_minutes(profile, bounds.0, bounds.1)?;
    let sessions = store.health_sessions(profile, bounds.0 + NIGHT_FROM, bounds.0 + NIGHT_TO)?;
    let mut hourly = [0u32; 24];
    for m in &minutes {
        if let Some(t) = Local.timestamp_opt(m.time as i64, 0).single() {
            hourly[t.hour() as usize] += m.steps as u32;
        }
    }
    let readings = |f: fn(&Minute) -> u8| minutes.iter().filter(|m| f(m) > 0).map(|m| (m.time as i64, f(m))).collect();
    let mut sleep: Vec<SleepBlock> = sessions
        .iter()
        .filter(|s| s.kind.is_sleep())
        .map(|s| SleepBlock { start: s.start as i64, end: s.start as i64 + s.seconds as i64, deep: s.kind.is_deep() })
        .collect();
    // Light first, so deep is drawn over it.
    sleep.sort_by_key(|b| (b.deep, b.start));
    Ok(DayDetail {
        profile,
        date,
        bounds,
        totals: totals(&minutes),
        hourly,
        heart_rate: readings(|m| m.heart_rate),
        spo2: readings(|m| m.spo2),
        night: night(&sessions, date),
        sleep,
    })
}

/// Circular mean of times of day, so 23:30 and 00:30 average to midnight.
fn mean_time_of_day(values: &[u32]) -> u32 {
    let (mut x, mut y) = (0.0f64, 0.0f64);
    for &v in values {
        let a = v as f64 / 86_400.0 * std::f64::consts::TAU;
        x += a.cos();
        y += a.sin();
    }
    let secs = (y.atan2(x) / std::f64::consts::TAU * 86_400.0).round() as i64;
    secs.rem_euclid(86_400) as u32
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TypicalSleep {
    total: u32,
    deep: u32,
    fell_asleep: u32,
    woke: u32,
}

fn typical_sleep(sessions: &[Session], today: NaiveDate) -> HashMap<usize, TypicalSleep> {
    let mut nights: HashMap<usize, Vec<(u32, u32, u32, u32)>> = HashMap::new();
    for back in 1..=TYPICAL_WEEKS * 7 {
        let date = today - Duration::days(back);
        let Some(n) = night(sessions, date) else { continue };
        let long: Vec<&Sleep> = n.sleeps.iter().filter(|s| s.total >= TYPICAL_MIN_SLEEP).collect();
        let (Some(first), Some(last)) = (long.first(), long.last()) else { continue };
        nights.entry(weekday(date)).or_default().push((
            long.iter().map(|s| s.total).sum(),
            long.iter().map(|s| s.deep).sum(),
            seconds_of_day(first.start),
            seconds_of_day(last.end),
        ));
    }
    nights
        .into_iter()
        .filter(|(_, n)| n.len() >= TYPICAL_MIN_DAYS)
        .map(|(w, n)| {
            let count = n.len() as u64;
            let typical = TypicalSleep {
                total: (n.iter().map(|x| x.0 as u64).sum::<u64>() / count) as u32,
                deep: (n.iter().map(|x| x.1 as u64).sum::<u64>() / count) as u32,
                fell_asleep: mean_time_of_day(&n.iter().map(|x| x.2).collect::<Vec<_>>()),
                woke: mean_time_of_day(&n.iter().map(|x| x.3).collect::<Vec<_>>()),
            };
            (w, typical)
        })
        .collect()
}

/// Per weekday: steps per slot, the days with data in each slot, and the days with any data.
type SlotSums = HashMap<usize, (Vec<u64>, Vec<BTreeSet<i64>>, BTreeSet<i64>)>;

/// Steps per quarter hour on a typical day of each weekday, from the weeks before today.
fn typical_steps(minutes: &[Minute], today: NaiveDate) -> HashMap<usize, [u16; stats::SLOTS]> {
    let mut sums: SlotSums = HashMap::new();
    for back in 1..=TYPICAL_WEEKS * 7 {
        let date = today - Duration::days(back);
        let (a, b) = (midnight(date), midnight(date + Duration::days(1)));
        let (lo, hi) = (minutes.partition_point(|m| (m.time as i64) < a), minutes.partition_point(|m| (m.time as i64) < b));
        if lo == hi {
            continue;
        }
        let entry =
            sums.entry(weekday(date)).or_insert_with(|| (vec![0; stats::SLOTS], vec![BTreeSet::new(); stats::SLOTS], BTreeSet::new()));
        entry.2.insert(a);
        for m in &minutes[lo..hi] {
            let slot = (((m.time as i64 - a) / 900) as usize).min(stats::SLOTS - 1);
            entry.0[slot] += m.steps as u64;
            entry.1[slot].insert(a);
        }
    }
    sums.into_iter()
        .filter(|(_, (_, _, days))| days.len() >= TYPICAL_MIN_DAYS)
        .map(|(w, (steps, days, _))| {
            let mut slots = [stats::UNKNOWN_SLOT; stats::SLOTS];
            for (i, slot) in slots.iter_mut().enumerate() {
                if !days[i].is_empty() {
                    *slot = (steps[i] / days[i].len() as u64).min(0xFFFE) as u16;
                }
            }
            (w, slots)
        })
        .collect()
}

/// The BlobDB 10 values: the six days before today, today's typical sleep (with no day, so the
/// watch keeps counting today itself), each weekday's typical steps, and the averages.
fn stats_values(store: &Store, profile: ProfileId, heart_rate: bool, today: NaiveDate) -> Result<HashMap<String, Vec<u8>>> {
    let start = midnight(today);
    let from = midnight(today - Duration::days(TYPICAL_WEEKS * 7));
    let minutes = store.health_minutes(profile, from, start)?;
    if minutes.is_empty() {
        return Ok(HashMap::new());
    }
    let sessions = store.health_sessions(profile, from + NIGHT_FROM, start + NIGHT_TO)?;
    let sleep_typicals = typical_sleep(&sessions, today);
    let mut out = HashMap::new();
    let day_minutes = |date: NaiveDate| {
        let (a, b) = (midnight(date), midnight(date + Duration::days(1)));
        let (lo, hi) = (minutes.partition_point(|m| (m.time as i64) < a), minutes.partition_point(|m| (m.time as i64) < b));
        &minutes[lo..hi]
    };
    for back in 1..=6 {
        let date = today - Duration::days(back);
        let day = midnight(date) as u32;
        let w = weekday(date);
        let mins = day_minutes(date);
        let t = totals(mins);
        let movement = stats::Movement {
            day_start: day,
            steps: t.steps,
            active_kcal: t.active_kcal,
            resting_kcal: t.resting_kcal,
            distance_m: t.distance_m,
            active_seconds: t.active_minutes * 60,
        };
        out.insert(stats::movement_key(w), movement.encode());
        let n = night(&sessions, date);
        let typ = sleep_typicals.get(&w).copied().unwrap_or_default();
        let sleep = stats::Sleep {
            day_start: day,
            total: n.as_ref().map(|n| n.total).unwrap_or(0),
            deep: n.as_ref().map(|n| n.deep).unwrap_or(0),
            fell_asleep: n.as_ref().map(|n| seconds_of_day(n.start())).unwrap_or(0),
            woke: n.as_ref().map(|n| seconds_of_day(n.end())).unwrap_or(0),
            typical_total: typ.total,
            typical_deep: typ.deep,
            typical_fell_asleep: typ.fell_asleep,
            typical_woke: typ.woke,
        };
        out.insert(stats::sleep_key(w), sleep.encode());
        if heart_rate {
            let mut zones = [0u32; 3];
            for m in mins {
                if (1..=3).contains(&m.heart_rate_zone) {
                    zones[m.heart_rate_zone as usize - 1] += 1;
                }
            }
            out.insert(stats::heart_rate_zones_key(w), stats::heart_rate_zones(day, zones));
        }
    }
    let typ = sleep_typicals.get(&weekday(today)).copied().unwrap_or_default();
    let today_sleep = stats::Sleep {
        typical_total: typ.total,
        typical_deep: typ.deep,
        typical_fell_asleep: typ.fell_asleep,
        typical_woke: typ.woke,
        ..Default::default()
    };
    out.insert(stats::sleep_key(weekday(today)), today_sleep.encode());
    for (w, slots) in typical_steps(&minutes, today) {
        out.insert(stats::typical_steps_key(w), stats::typical_steps(&slots));
    }
    let (mut steps, mut step_days, mut sleep, mut sleep_days) = (0u64, 0u64, 0u64, 0u64);
    for back in 1..=AVERAGE_DAYS {
        let date = today - Duration::days(back);
        let mins = day_minutes(date);
        if !mins.is_empty() {
            steps += totals(mins).steps as u64;
            step_days += 1;
        }
        if let Some(n) = night(&sessions, date) {
            sleep += n.total as u64;
            sleep_days += 1;
        }
    }
    out.insert(stats::AVERAGE_STEPS.to_string(), stats::average(steps.checked_div(step_days).unwrap_or(0) as u32));
    out.insert(stats::AVERAGE_SLEEP.to_string(), stats::average(sleep.checked_div(sleep_days).unwrap_or(0) as u32));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_protocol::health::SessionKind;

    fn minute(time: i64, steps: u8) -> Minute {
        Minute { time: time as u32, steps, active: steps > 50, distance_cm: 70 * steps as u16, ..Minute::default() }
    }

    fn session(kind: SessionKind, start: i64, seconds: u32) -> Session {
        Session { kind, start: start as u32, seconds, utc_to_local: 0, steps: 0, active_kcal: 0, resting_kcal: 0, distance_m: 0 }
    }

    #[test]
    fn nights_group_sleep_and_count_deep_once() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let day = midnight(date);
        let sessions = [
            session(SessionKind::Sleep, day - 3600, 4 * 3600),
            session(SessionKind::DeepSleep, day, 3600),
            // 30 minutes awake, then more sleep: the same night.
            session(SessionKind::Sleep, day + 3 * 3600 + 1800, 3 * 3600),
            // The evening after belongs to the next day.
            session(SessionKind::Sleep, day + 20 * 3600, 3600),
            session(SessionKind::Walk, day + 8 * 3600, 600),
        ];
        let n = night(&sessions, date).unwrap();
        assert_eq!(n.sleeps.len(), 1);
        assert_eq!(n.total, 7 * 3600);
        assert_eq!(n.deep, 3600);
        assert_eq!(n.start(), day - 3600);
        assert_eq!(n.end(), day + 6 * 3600 + 1800);
        assert_eq!(night(&sessions, date + Duration::days(1)).unwrap().total, 3600);
        assert!(night(&sessions, date - Duration::days(1)).is_none());
    }

    #[test]
    fn two_watches_overlapping_sleep_count_once() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let day = midnight(date);
        let sessions = [session(SessionKind::Sleep, day - 3600, 4 * 3600), session(SessionKind::Sleep, day - 1800, 4 * 3600)];
        assert_eq!(night(&sessions, date).unwrap().total, 4 * 3600 + 1800);
    }

    #[test]
    fn totals_and_circular_mean() {
        let t = totals(&[minute(0, 100), minute(60, 20)]);
        assert_eq!((t.steps, t.active_minutes, t.distance_m, t.minutes), (120, 1, 84, 2));
        assert_eq!(mean_time_of_day(&[23 * 3600 + 1800, 1800]), 0);
        assert_eq!(mean_time_of_day(&[7 * 3600, 8 * 3600]), 7 * 3600 + 1800);
    }

    #[test]
    fn stats_for_the_watch() {
        let mut store = Store::in_memory();
        let profile = store.add_profile("AJ", 0).unwrap();
        store.set_profile(profile).unwrap();
        let watch = store.add_watch("24:A3:0A:D4:7B:24", 0).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        // 10 steps a minute 9:00-9:59 on each of the last 14 days, and a night before each.
        let mut minutes = Vec::new();
        let mut sessions = Vec::new();
        for back in 1..=14 {
            let day = midnight(today - Duration::days(back));
            minutes.extend((0..60).map(|i| minute(day + 9 * 3600 + i * 60, 10)));
            sessions.push(session(SessionKind::Sleep, day - 2 * 3600, 8 * 3600));
        }
        store.add_health_minutes(profile, watch, &minutes).unwrap();
        store.add_health_sessions(profile, watch, &sessions).unwrap();
        let v = stats_values(&store, profile, true, today).unwrap();
        // 6 days × (movement, sleep, zones) + today's sleep + 7 typicals + 2 averages.
        assert_eq!(v.len(), 6 * 3 + 1 + 7 + 2);
        assert_eq!(v[stats::AVERAGE_STEPS], 600u32.to_le_bytes());
        assert_eq!(v[stats::AVERAGE_SLEEP], (8 * 3600u32).to_le_bytes());
        let yesterday = today - Duration::days(1);
        let movement = &v[&stats::movement_key(weekday(yesterday))];
        assert_eq!(&movement[4..8], (midnight(yesterday) as u32).to_le_bytes());
        assert_eq!(&movement[8..12], 600u32.to_le_bytes());
        let typical = &v[&stats::typical_steps_key(weekday(yesterday))];
        // 9:00-9:15 is slot 36: 150 steps; 8:45 had none on any day.
        assert_eq!(&typical[72..74], 150u16.to_le_bytes());
        assert_eq!(&typical[70..72], stats::UNKNOWN_SLOT.to_le_bytes());
        let today_sleep = &v[&stats::sleep_key(weekday(today))];
        assert_eq!(&today_sleep[4..8], [0; 4]);
        assert_eq!(&today_sleep[24..28], (8 * 3600u32).to_le_bytes());
        assert_eq!(&today_sleep[32..36], (22 * 3600u32).to_le_bytes());
        // Nothing logged: nothing to send.
        assert!(stats_values(&store, profile + 1, false, today).unwrap().is_empty());
    }

    #[test]
    fn settings_follow_the_firmware() {
        let h = HealthSettings { birth_year: Some(1990), heart_rate_interval: settings::Interval::Five, ..HealthSettings::default() };
        let watch = Watch { id: 1, profile: 1, firmware: Version { major: 4, minor: 9, patch: 150 }, heart_rate: true, settings: true };
        let v = settings_values(&h, watch, true, 2026);
        assert_eq!(v[params::ACTIVITY][7], 36);
        assert_eq!(v[params::UNITS], [1]);
        // Five minutes needs 4.39: ten minutes instead.
        assert_eq!(v[params::HEART_RATE_MONITOR], [1, 0, 0]);
        assert_eq!(v[params::HEART_RATE][2], 184);
        let no_sensor = settings_values(&h, Watch { heart_rate: false, ..watch }, false, 2026);
        assert_eq!(no_sensor.len(), 2);
    }

    #[test]
    fn receiver_stores_then_acknowledges() {
        use pebble_protocol::frame::encode;
        let db: Db = std::sync::Arc::new(std::sync::Mutex::new(Store::in_memory()));
        let (protocol, mut out, _tx) = Protocol::loopback();
        let mut r = Receiver::default();
        r.start(&protocol);
        assert_eq!(out.try_recv().unwrap(), encode(endpoint::DATA_LOG, &[0x84]));
        let mut open = vec![0x01, 3];
        open.extend_from_slice(&[0; 16]);
        open.extend_from_slice(&0u32.to_le_bytes());
        open.extend_from_slice(&tag::STEPS.to_le_bytes());
        open.push(0);
        open.extend_from_slice(&20u16.to_le_bytes());
        assert_eq!(r.handle(&protocol, &db, Some((1, 1)), &open), Received::Nothing);
        assert_eq!(out.try_recv().unwrap(), encode(endpoint::DATA_LOG, &[0x85, 3]));
        // One v5 minute: header, then steps, orientation, vmc, light, flags.
        let mut item = 5u16.to_le_bytes().to_vec();
        item.extend_from_slice(&600u32.to_le_bytes());
        item.extend([0, 6, 1, 42, 0, 0, 0, 0, 2]);
        item.resize(20, 0);
        let mut data = vec![0x02, 3, 0, 0, 0, 0, 0, 0, 0, 0];
        data.extend(&item);
        assert_eq!(r.handle(&protocol, &db, Some((1, 1)), &data), Received::Health { last: true });
        assert_eq!(out.try_recv().unwrap(), encode(endpoint::DATA_LOG, &[0x85, 3]));
        assert_eq!(lock(&db).health_minutes(1, 0, 1000).unwrap()[0].steps, 42);
        // Another session's data, unknown here, is refused so the watch keeps it.
        data[1] = 4;
        assert_eq!(r.handle(&protocol, &db, Some((1, 1)), &data), Received::Nothing);
        assert_eq!(out.try_recv().unwrap(), encode(endpoint::DATA_LOG, &[0x86, 4]));
        // Without PebbleOS nothing is taken.
        assert_eq!(r.handle(&protocol, &db, None, &open), Received::Nothing);
        assert_eq!(out.try_recv().unwrap(), encode(endpoint::DATA_LOG, &[0x86, 3]));
    }

    #[test]
    fn view_of_a_new_watch() {
        let store = Store::in_memory();
        let id = store.add_watch("24:A3:0A:D4:7B:24", 0).unwrap();
        let watch = store.watch(id).unwrap().unwrap();
        let v = view(&store, 0, Some(&watch), true).unwrap();
        assert!(!v.has_heart_rate);
        assert!(view(&store, 0, None, true).is_ok());
        assert_eq!(v.week.len(), 7);
        assert!(v.night.is_none());
    }

    #[test]
    fn trends_and_a_day() {
        use pebble_protocol::health::SessionKind;
        let mut store = Store::in_memory();
        let today = Local::now().date_naive();
        let noon = midnight(today) + 12 * 3600;
        let mut m = minute(noon, 40);
        m.heart_rate = 70;
        let mut earlier = minute(noon - 3 * 86_400, 90);
        earlier.spo2 = 97;
        store.add_health_minutes(1, 7, &[m, earlier]).unwrap();
        let night = session(SessionKind::Sleep, midnight(today) - 3600, 7 * 3600);
        let deep = session(SessionKind::DeepSleep, midnight(today), 3600);
        store.add_health_sessions(1, 7, &[night, deep]).unwrap();

        let month = trend(&store, 1, Range::Month).unwrap();
        assert_eq!(month.bars.len(), MONTH_DAYS as usize);
        assert_eq!(month.bars.last().map(|d| (d.date, d.steps, d.sleep)), Some((today, 40, 7 * 3600)));
        assert_eq!(month.bars[MONTH_DAYS as usize - 4].steps, 90);
        let year = trend(&store, 1, Range::Year).unwrap();
        assert_eq!(year.bars.len(), 12);
        assert_eq!(year.bars[11].date, today.with_day(1).unwrap());
        // Averaged over the days with steps, whatever day of the month it is.
        let this_month: Vec<u32> = month.bars.iter().filter(|d| d.date.month() == today.month() && d.steps > 0).map(|d| d.steps).collect();
        assert_eq!(year.bars[11].steps, this_month.iter().sum::<u32>() / this_month.len() as u32);
        assert_eq!(trend(&store, 1, Range::Week).unwrap().bars.len(), 7);

        let d = day(&store, 1, today).unwrap();
        assert_eq!(d.hourly[12], 40);
        assert_eq!(d.heart_rate, vec![(noon, 70)]);
        assert!(d.spo2.is_empty());
        assert_eq!(d.sleep.iter().map(|b| b.deep).collect::<Vec<_>>(), [false, true]);
        assert_eq!(d.night.map(|n| (n.total, n.deep)), Some((7 * 3600, 3600)));
        assert_eq!(day(&store, 1, today - Duration::days(3)).unwrap().spo2.len(), 1);
    }

    #[test]
    fn minutes_keep_the_higher_count() {
        let mut store = Store::in_memory();
        store.add_health_minutes(1, 7, &[minute(60, 30)]).unwrap();
        // Another watch of the same profile logged the same minute with fewer steps.
        store.add_health_minutes(1, 8, &[minute(60, 10), minute(120, 5)]).unwrap();
        let m = store.health_minutes(1, 0, 1000).unwrap();
        assert_eq!(m.iter().map(|m| m.steps).collect::<Vec<_>>(), [30, 5]);
        assert_eq!(store.health_span(1).unwrap(), Some((60, 120)));
        store.clear_health(1, Some(100)).unwrap();
        assert_eq!(store.health_span(1).unwrap(), Some((120, 120)));
        store.clear_health(1, None).unwrap();
        assert_eq!(store.health_span(1).unwrap(), None);
    }
}
