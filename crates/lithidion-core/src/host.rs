//! What the machine Lithidion runs on says about itself over the system bus: its time zone
//! (timedated, followed as it changes) and what kind of device it is (hostnamed).

use chrono::{DateTime, FixedOffset, MappedLocalTime, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{OnceLock, RwLock};

static NOUN: OnceLock<&'static str> = OnceLock::new();
/// The zone timedated reports; `None` leaves it to chrono's `Local`.
static ZONE: RwLock<Option<chrono_tz::Tz>> = RwLock::new(None);
/// The desktop's clock format: 0 unknown, 12, or 24.
static CLOCK: AtomicU8 = AtomicU8::new(0);

/// GNOME's and Phosh's clock format setting.
pub const CLOCK_NAMESPACE: &str = "org.gnome.desktop.interface";
pub const CLOCK_KEY: &str = "clock-format";

/// Takes a `clock-format` value, `12h` or `24h`.
pub fn set_clock_format(value: &str) {
    let hours = match value {
        "12h" => 12,
        "24h" => 24,
        _ => 0,
    };
    CLOCK.store(hours, Ordering::Relaxed);
}

/// Whether the desktop shows a 12-hour clock; `None` when it does not say, and the locale decides.
pub fn twelve_hour_clock() -> Option<bool> {
    match CLOCK.load(Ordering::Relaxed) {
        12 => Some(true),
        24 => Some(false),
        _ => None,
    }
}

/// Whether the locale's own time format (`LC_TIME`) is 12-hour.
pub fn locale_twelve_hour() -> bool {
    glib::DateTime::new(&glib::TimeZone::utc(), 2001, 2, 3, 14, 0, 0.0)
        .ok()
        .and_then(|t| t.format("%X").ok())
        .is_some_and(|s| !s.contains("14"))
}

/// The clock format through the settings portal, else from GSettings where the schema is
/// installed (outside Flatpak).
pub async fn read_clock_format() -> Option<String> {
    if let Ok(s) = ashpd::desktop::settings::Settings::new().await {
        if let Ok(v) = s.read::<String>(CLOCK_NAMESPACE, CLOCK_KEY).await {
            return Some(v);
        }
    }
    // Without dconf, GSettings answers from memory with the schema's default.
    if glib::prelude::ObjectExt::type_(&gio::SettingsBackend::default()).name() == "GMemorySettingsBackend" {
        return None;
    }
    let schema = gio::SettingsSchemaSource::default()?.lookup(CLOCK_NAMESPACE, true)?;
    use gio::prelude::SettingsExt;
    schema.has_key(CLOCK_KEY).then(|| gio::Settings::new(CLOCK_NAMESPACE).string(CLOCK_KEY).to_string())
}

/// Reads both and remembers them.
pub fn detect() {
    let (zone, chassis) = glib::MainContext::new().block_on(async {
        let Ok(bus) = zbus::Connection::system().await else { return (None, None) };
        let get = |service: &'static str, path: &'static str, property: &'static str| {
            let bus = bus.clone();
            async move {
                let proxy = zbus::Proxy::new(&bus, service, path, service).await.ok()?;
                proxy.get_property::<String>(property).await.ok()
            }
        };
        let zone = get("org.freedesktop.timedate1", "/org/freedesktop/timedate1", "Timezone").await;
        let chassis = get("org.freedesktop.hostname1", "/org/freedesktop/hostname1", "Chassis").await;
        bus.close().await.ok();
        (zone, chassis)
    });
    let clock = glib::MainContext::new().block_on(read_clock_format());
    if let Some(c) = &clock {
        set_clock_format(c);
    }
    tracing::info!(clock = ?clock, "clock format");
    use_time_zone(zone);
    let noun = noun_for(chassis.as_deref().unwrap_or(""));
    let _ = NOUN.set(noun);
    tracing::info!(chassis = ?chassis, noun, "device");
}

/// Inside Flatpak, `/etc/localtime` can say UTC while the host keeps another zone, so the zone
/// timedated names is used when there is one.
fn use_time_zone(zone: Option<String>) {
    match zone.as_deref().map(str::parse::<chrono_tz::Tz>) {
        Some(Ok(tz)) => set_zone(tz),
        other => tracing::info!(zone = ?zone, parsed = other.is_some(), "time zone left to the system"),
    }
}

pub fn set_zone(tz: chrono_tz::Tz) {
    if let Ok(mut z) = ZONE.write() {
        *z = Some(tz);
    }
}

/// The zone's name, when timedated gave one.
pub fn zone_name() -> Option<String> {
    ZONE.read().ok().and_then(|z| *z).map(|tz| tz.name().to_string())
}

fn zone() -> Option<chrono_tz::Tz> {
    ZONE.read().ok().and_then(|z| *z)
}

/// The device's local time, following time zone changes as they happen. Used in place of
/// chrono's `Local`, which reads the zone from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone;

impl Zone {
    pub fn now() -> DateTime<Zone> {
        Utc::now().with_timezone(&Zone)
    }
}

impl TimeZone for Zone {
    type Offset = FixedOffset;

    fn from_offset(_: &FixedOffset) -> Zone {
        Zone
    }

    fn offset_from_local_date(&self, local: &NaiveDate) -> MappedLocalTime<FixedOffset> {
        self.offset_from_local_datetime(&local.and_hms_opt(0, 0, 0).expect("midnight exists"))
    }

    fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> MappedLocalTime<FixedOffset> {
        local_offset(zone(), local)
    }

    fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
        self.offset_from_utc_datetime(&utc.and_hms_opt(0, 0, 0).expect("midnight exists"))
    }

    fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
        utc_offset(zone(), utc)
    }
}

fn local_offset(tz: Option<chrono_tz::Tz>, local: &NaiveDateTime) -> MappedLocalTime<FixedOffset> {
    match tz {
        Some(tz) => tz.offset_from_local_datetime(local).map(|o| o.fix()),
        None => chrono::Local.offset_from_local_datetime(local),
    }
}

fn utc_offset(tz: Option<chrono_tz::Tz>, utc: &NaiveDateTime) -> FixedOffset {
    match tz {
        Some(tz) => tz.offset_from_utc_datetime(utc).fix(),
        None => chrono::Local.offset_from_utc_datetime(utc),
    }
}

fn noun_for(chassis: &str) -> &'static str {
    match chassis {
        "handset" => "phone",
        "tablet" => "tablet",
        "laptop" | "convertible" => "laptop",
        "desktop" | "server" => "computer",
        _ => "device",
    }
}

/// "phone", "tablet", "laptop", "computer", or "device".
pub fn noun() -> &'static str {
    NOUN.get().copied().unwrap_or("device")
}

/// The noun for titles: "Phone".
pub fn title() -> String {
    let n = noun();
    n[..1].to_uppercase() + &n[1..]
}

#[cfg(test)]
mod tests {
    #[test]
    fn nouns() {
        assert_eq!(super::noun_for("handset"), "phone");
        assert_eq!(super::noun_for("convertible"), "laptop");
        assert_eq!(super::noun_for(""), "device");
        assert_eq!(super::noun(), "device");
        assert_eq!(super::title(), "Device");
    }

    #[test]
    fn offsets_follow_the_zone() {
        // The global zone is not touched: other tests read it.
        let t = chrono::DateTime::parse_from_rfc3339("2026-11-15T15:00:00Z").unwrap().naive_utc();
        assert_eq!(super::utc_offset(Some(chrono_tz::America::Chicago), &t).local_minus_utc(), -6 * 3600);
        assert_eq!(super::utc_offset(Some(chrono_tz::Asia::Tokyo), &t).local_minus_utc(), 9 * 3600);
        let midnight = t.date().and_hms_opt(0, 0, 0).unwrap();
        assert_eq!(super::local_offset(Some(chrono_tz::Asia::Tokyo), &midnight).single().unwrap().local_minus_utc(), 9 * 3600);
    }
}
