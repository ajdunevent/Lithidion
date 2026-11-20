//! Health (section 18): the minute and session records PebbleOS logs (data logging tags 81, 83,
//! and 84), the profile and heart-rate settings the phone writes to BlobDB 7, the history it
//! writes back to BlobDB 10, and the sync request on endpoint 911. Little-endian throughout.

use crate::system::Version;
use crate::wire::{err, Reader, Result};

/// One minute of activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Minute {
    /// UTC.
    pub time: u32,
    pub steps: u8,
    pub orientation: u8,
    /// Vector magnitude counts: how much the watch moved.
    pub vmc: u16,
    pub light: u8,
    pub plugged_in: bool,
    pub active: bool,
    /// 1/1000 kcal.
    pub resting_cal: u16,
    pub active_cal: u16,
    pub distance_cm: u16,
    /// 0 when not measured.
    pub heart_rate: u8,
    pub heart_rate_weight: u16,
    pub heart_rate_zone: u8,
    /// 0 when not measured.
    pub spo2: u8,
}

const MINUTE_HEADER: usize = 9;

/// Minute records (tag 81). Each item is a header and up to 15 one-minute samples; samples
/// carry the fields of their version and possibly more appended, which are skipped. Items of
/// unknown versions are skipped.
pub fn parse_minutes(payload: &[u8], item_size: u16) -> Vec<Minute> {
    let mut out = Vec::new();
    if item_size as usize <= MINUTE_HEADER {
        return out;
    }
    for item in payload.chunks_exact(item_size as usize) {
        let _ = parse_minute_item(item, &mut out);
    }
    out
}

fn parse_minute_item(item: &[u8], out: &mut Vec<Minute>) -> Result<()> {
    let mut r = Reader::new(item);
    let version = r.u16_le()?;
    let start = r.u32_le()?;
    let _local_offset = r.u8()?;
    let sample_size = r.u8()? as usize;
    let count = r.u8()?;
    // Versions only append fields, and each newer layout is a superset; 4 lacks the flags.
    if !(5..=225).contains(&version) || sample_size < 6 {
        return err(format!("minute record version {version}"));
    }
    for i in 0..count as u32 {
        let mut s = Reader::new(r.bytes(sample_size)?);
        let mut m = Minute { time: start + i * 60, ..Minute::default() };
        m.steps = s.u8()?;
        m.orientation = s.u8()?;
        m.vmc = s.u16_le()?;
        m.light = s.u8()?;
        let flags = s.u8()?;
        m.plugged_in = flags & 1 != 0;
        m.active = flags & 2 != 0;
        if version >= 6 {
            m.resting_cal = s.u16_le()?;
            m.active_cal = s.u16_le()?;
            m.distance_cm = s.u16_le()?;
        }
        if version >= 7 {
            m.heart_rate = s.u8()?;
        }
        if version >= 12 {
            m.heart_rate_weight = s.u16_le()?;
        }
        if version >= 13 {
            m.heart_rate_zone = s.u8()?;
        }
        if version >= 14 {
            m.spo2 = s.u8()?;
        }
        out.push(m);
    }
    Ok(())
}

/// What an activity session was.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKind {
    Sleep = 1,
    DeepSleep = 2,
    Nap = 3,
    DeepNap = 4,
    Walk = 5,
    Run = 6,
    Workout = 7,
}

impl SessionKind {
    pub fn from_u16(v: u16) -> Option<SessionKind> {
        use SessionKind::*;
        Some(match v {
            1 => Sleep,
            2 => DeepSleep,
            3 => Nap,
            4 => DeepNap,
            5 => Walk,
            6 => Run,
            7 => Workout,
            _ => return None,
        })
    }

    pub fn is_sleep(self) -> bool {
        matches!(self, SessionKind::Sleep | SessionKind::DeepSleep | SessionKind::Nap | SessionKind::DeepNap)
    }

    /// Deep sleep lies within a Sleep or Nap session, so it is not added to the total.
    pub fn is_deep(self) -> bool {
        matches!(self, SessionKind::DeepSleep | SessionKind::DeepNap)
    }
}

/// An activity session (tags 83 and 84): a sleep, a nap, a walk, a run, or a workout.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub kind: SessionKind,
    /// UTC.
    pub start: u32,
    pub seconds: u32,
    pub utc_to_local: i32,
    /// For walks, runs, and workouts from version 3 on.
    pub steps: u16,
    pub active_kcal: u16,
    pub resting_kcal: u16,
    pub distance_m: u16,
}

/// Session records. Bit 0 of the version set means fields are only ever appended; a record
/// with it clear, or of an unknown kind, is skipped.
pub fn parse_sessions(payload: &[u8], item_size: u16) -> Vec<Session> {
    if item_size == 0 {
        return Vec::new();
    }
    payload.chunks_exact(item_size as usize).filter_map(|item| parse_session(item).ok().flatten()).collect()
}

fn parse_session(item: &[u8]) -> Result<Option<Session>> {
    let mut r = Reader::new(item);
    let version = r.u16_le()?;
    let _size = r.u16_le()?;
    let Some(kind) = SessionKind::from_u16(r.u16_le()?) else { return Ok(None) };
    if version & 1 == 0 {
        return Ok(None);
    }
    let utc_to_local = r.u32_le()? as i32;
    let start = r.u32_le()?;
    let seconds = r.u32_le()?;
    let mut s = Session { kind, start, seconds, utc_to_local, steps: 0, active_kcal: 0, resting_kcal: 0, distance_m: 0 };
    if version >= 3 && !kind.is_sleep() {
        s.steps = r.u16_le()?;
        s.active_kcal = r.u16_le()?;
        s.resting_kcal = r.u16_le()?;
        s.distance_m = r.u16_le()?;
    }
    Ok(Some(s))
}

/// Asks the watch to send what it has logged now (endpoint 911).
pub fn sync_request(seconds_since_last: u32) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&seconds_since_last.to_le_bytes());
    out
}

/// The watch's answer to [`sync_request`], once it has sent everything: true for ACK.
pub fn parse_sync_response(payload: &[u8]) -> Option<bool> {
    match payload {
        [0x11, result, ..] => Some(*result == 1),
        _ => None,
    }
}

/// BlobDB 7 keys.
pub mod params {
    pub const ACTIVITY: &str = "activityPreferences";
    pub const HEART_RATE_MONITOR: &str = "hrmPreferences";
    pub const HEART_RATE: &str = "heartRatePreferences";
    pub const UNITS: &str = "unitsDistance";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sex {
    #[default]
    Female = 0,
    Male = 1,
    Other = 2,
}

/// The `activityPreferences` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub height_mm: u16,
    /// Decagrams (10 g).
    pub weight_dag: u16,
    pub tracking: bool,
    pub activity_insights: bool,
    pub sleep_insights: bool,
    pub age: u8,
    pub sex: Sex,
}

impl Profile {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(9);
        out.extend_from_slice(&self.height_mm.to_le_bytes());
        out.extend_from_slice(&self.weight_dag.to_le_bytes());
        out.extend([self.tracking as u8, self.activity_insights as u8, self.sleep_insights as u8, self.age, self.sex as u8]);
        out
    }
}

/// How often the heart rate is measured in the background, as the watch numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interval {
    FiveMinutes = 4,
    #[default]
    TenMinutes = 0,
    ThirtyMinutes = 1,
    Hour = 2,
}

impl Interval {
    /// The first firmware to offer it.
    pub fn since(self) -> Version {
        match self {
            Interval::FiveMinutes => Version { major: 4, minor: 39, patch: 0 },
            _ => Version { major: 0, minor: 0, patch: 0 },
        }
    }
}

/// The `hrmPreferences` value. It grew without a version, and the watch takes only the size it
/// knows: 1 byte before 4.9.146, 2 before 4.9.150, then 3.
pub fn heart_rate_monitor(enabled: bool, interval: Interval, during_activities: bool, firmware: Version) -> Vec<u8> {
    let mut out = vec![enabled as u8];
    if firmware >= (Version { major: 4, minor: 9, patch: 146 }) {
        out.push(interval as u8);
    }
    if firmware >= (Version { major: 4, minor: 9, patch: 150 }) {
        out.push(during_activities as u8);
    }
    out
}

/// The `heartRatePreferences` value: resting and elevated rates, and the maximum and zone
/// thresholds from the age (220 - age, then 50, 70, and 85% of the heart rate reserve).
pub fn heart_rate(age: u8, resting: u8, elevated: u8) -> Vec<u8> {
    let max = (220 - age.clamp(1, 120) as i32).max(elevated as i32);
    let reserve = (max - resting as i32).max(0);
    let zone = |percent: i32| (resting as i32 + (reserve * percent + 50) / 100).min(255) as u8;
    vec![resting, elevated, max as u8, zone(50), zone(70), zone(85)]
}

pub fn units(imperial: bool) -> Vec<u8> {
    vec![imperial as u8]
}

/// BlobDB 10: history and typical values the phone keeps for the watch.
pub mod stats {
    const VERSION: u32 = 1;
    pub const AVERAGE_STEPS: &str = "average_dailySteps";
    pub const AVERAGE_SLEEP: &str = "average_sleepDuration";
    /// Monday first.
    pub const WEEKDAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    /// Steps per 15 minutes of a typical day.
    pub const SLOTS: usize = 96;
    pub const UNKNOWN_SLOT: u16 = 0xFFFF;

    pub fn movement_key(weekday: usize) -> String {
        format!("{}_movementData", WEEKDAYS[weekday])
    }

    pub fn sleep_key(weekday: usize) -> String {
        format!("{}_sleepData", WEEKDAYS[weekday])
    }

    pub fn typical_steps_key(weekday: usize) -> String {
        format!("{}_steps", WEEKDAYS[weekday])
    }

    pub fn heart_rate_zones_key(weekday: usize) -> String {
        format!("{}_heartRateZoneData", WEEKDAYS[weekday])
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    pub fn average(value: u32) -> Vec<u8> {
        value.to_le_bytes().to_vec()
    }

    /// A finished day; `day_start` is local midnight as UTC. The watch ignores days older than
    /// six days.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Movement {
        pub day_start: u32,
        pub steps: u32,
        pub active_kcal: u32,
        pub resting_kcal: u32,
        pub distance_m: u32,
        pub active_seconds: u32,
    }

    impl Movement {
        pub fn encode(&self) -> Vec<u8> {
            words(&[VERSION, self.day_start, self.steps, self.active_kcal, self.resting_kcal, self.distance_m, self.active_seconds])
        }
    }

    /// A night, with the typical values for its weekday. Times of day are seconds after local
    /// midnight. A `day_start` of 0 makes the watch keep only the typical values.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Sleep {
        pub day_start: u32,
        pub total: u32,
        pub deep: u32,
        pub fell_asleep: u32,
        pub woke: u32,
        pub typical_total: u32,
        pub typical_deep: u32,
        pub typical_fell_asleep: u32,
        pub typical_woke: u32,
    }

    impl Sleep {
        pub fn encode(&self) -> Vec<u8> {
            words(&[
                VERSION,
                self.day_start,
                self.total,
                self.deep,
                self.fell_asleep,
                self.woke,
                self.typical_total,
                self.typical_deep,
                self.typical_fell_asleep,
                self.typical_woke,
            ])
        }
    }

    pub fn heart_rate_zones(day_start: u32, minutes: [u32; 3]) -> Vec<u8> {
        words(&[VERSION, day_start, 3, minutes[0], minutes[1], minutes[2]])
    }

    pub fn typical_steps(slots: &[u16; SLOTS]) -> Vec<u8> {
        slots.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minute_item(version: u16, sample: &[u8], count: u8, item_size: usize) -> Vec<u8> {
        let mut out = version.to_le_bytes().to_vec();
        out.extend_from_slice(&1000u32.to_le_bytes());
        out.extend([0, sample.len() as u8, count]);
        for _ in 0..count {
            out.extend_from_slice(sample);
        }
        out.resize(item_size, 0);
        out
    }

    #[test]
    fn minutes_of_each_version() {
        // v13: steps, orientation, vmc, light, flags, resting, active, distance, hr, weight, zone.
        let sample = [12, 3, 0x10, 0, 9, 0b10, 0x20, 0, 0x30, 0, 0x40, 0, 72, 0x64, 0, 2];
        let mut payload = minute_item(13, &sample, 2, 249);
        // An unknown layout is skipped, and v14 gains the SpO2 byte, plus one appended field.
        payload.extend(minute_item(4, &[1; 5], 1, 249));
        let mut v14 = sample.to_vec();
        v14.extend([97, 3, 0xEE]);
        payload.extend(minute_item(14, &v14, 1, 249));
        let m = parse_minutes(&payload, 249);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1].time, 1060);
        assert_eq!(
            m[0],
            Minute {
                time: 1000,
                steps: 12,
                orientation: 3,
                vmc: 0x10,
                light: 9,
                plugged_in: false,
                active: true,
                resting_cal: 0x20,
                active_cal: 0x30,
                distance_cm: 0x40,
                heart_rate: 72,
                heart_rate_weight: 100,
                heart_rate_zone: 2,
                spo2: 0,
            }
        );
        assert_eq!(m[2].spo2, 97);
        // v6 has no heart rate.
        let v6 = minute_item(6, &[5, 0, 0, 0, 0, 1, 1, 0, 2, 0, 3, 0], 1, 30);
        assert_eq!(parse_minutes(&v6, 30)[0].heart_rate, 0);
        assert!(parse_minutes(&v6, 0).is_empty());
    }

    fn session_item(version: u16, kind: u16, extra: &[u8]) -> Vec<u8> {
        let mut out = version.to_le_bytes().to_vec();
        out.extend_from_slice(&30u16.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(-21600i32).to_le_bytes());
        out.extend_from_slice(&5000u32.to_le_bytes());
        out.extend_from_slice(&1800u32.to_le_bytes());
        out.extend_from_slice(extra);
        out.resize(26, 0);
        out
    }

    #[test]
    fn sessions() {
        let walk = [0xE8, 3, 50, 0, 20, 0, 0x20, 3];
        let mut payload = session_item(3, 5, &walk);
        payload.extend(session_item(3, 1, &[]));
        payload.extend(session_item(2, 2, &[])); // bit 0 clear
        payload.extend(session_item(3, 99, &[]));
        let s = parse_sessions(&payload, 26);
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0],
            Session {
                kind: SessionKind::Walk,
                start: 5000,
                seconds: 1800,
                utc_to_local: -21600,
                steps: 1000,
                active_kcal: 50,
                resting_kcal: 20,
                distance_m: 800
            }
        );
        assert_eq!(s[1].kind, SessionKind::Sleep);
        assert_eq!(s[1].steps, 0);
    }

    #[test]
    fn params_and_stats() {
        let p = Profile {
            height_mm: 1800,
            weight_dag: 8000,
            tracking: true,
            activity_insights: false,
            sleep_insights: true,
            age: 40,
            sex: Sex::Male,
        };
        assert_eq!(p.encode(), [0x08, 0x07, 0x40, 0x1F, 1, 0, 1, 40, 1]);
        let v = |minor, patch| Version { major: 4, minor, patch };
        assert_eq!(heart_rate_monitor(true, Interval::Hour, true, v(9, 0)), [1]);
        assert_eq!(heart_rate_monitor(true, Interval::Hour, true, v(9, 146)), [1, 2]);
        assert_eq!(heart_rate_monitor(true, Interval::Hour, true, v(9, 150)), [1, 2, 1]);
        // The firmware's defaults for age 30.
        assert_eq!(heart_rate(30, 70, 100), [70, 100, 190, 130, 154, 172]);
        assert_eq!(sync_request(60), [1, 60, 0, 0, 0]);
        assert_eq!(parse_sync_response(&[0x11, 1]), Some(true));
        let m = stats::Movement { day_start: 1, steps: 2, ..Default::default() }.encode();
        assert_eq!(m.len(), 28);
        assert_eq!(&m[..12], [1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(stats::Sleep::default().encode().len(), 40);
        assert_eq!(stats::typical_steps(&[stats::UNKNOWN_SLOT; stats::SLOTS]).len(), 192);
        assert_eq!(stats::sleep_key(6), "sunday_sleepData");
    }
}
