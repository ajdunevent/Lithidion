//! Dates and times as the user's locale writes them (`LC_TIME`), through GLib, in the device's
//! time zone as Lithidion follows it.

use chrono::{Datelike, NaiveDate};

fn zone() -> glib::TimeZone {
    crate::host::zone_name().and_then(|z| glib::TimeZone::from_identifier(Some(&z))).unwrap_or_else(glib::TimeZone::local)
}

fn at(unix: i64) -> Option<glib::DateTime> {
    glib::DateTime::from_unix_utc(unix).ok()?.to_timezone(&zone()).ok()
}

fn on(date: NaiveDate) -> Option<glib::DateTime> {
    glib::DateTime::new(&zone(), date.year(), date.month() as i32, date.day() as i32, 12, 0, 0.0).ok()
}

fn format(t: Option<glib::DateTime>, f: &str) -> String {
    t.and_then(|t| t.format(f).ok()).map(|s| s.to_string()).unwrap_or_default()
}

/// The locale's short date, as `11/15/2026` or `15/11/2026`.
pub fn short_date(unix: i64) -> String {
    format(at(unix), "%x")
}

/// The desktop's clock setting, else the locale's own time format.
fn twelve_hour() -> bool {
    crate::host::twelve_hour_clock().unwrap_or_else(crate::host::locale_twelve_hour)
}

/// Hours and minutes, 12-hour where the locale's own time format is.
pub fn time(unix: i64) -> String {
    format(at(unix), if twelve_hour() { "%-l:%M %p" } else { "%H:%M" })
}

/// As `time`, without AM or PM, as watchfaces show it.
pub fn watch_time(unix: i64) -> String {
    format(at(unix), if twelve_hour() { "%-l:%M" } else { "%H:%M" })
}

pub fn weekday(date: NaiveDate) -> String {
    format(on(date), "%A")
}

pub fn weekday_short(date: NaiveDate) -> String {
    format(on(date), "%a")
}

/// Whether the locale's short date puts the month before the day.
fn month_first() -> bool {
    let probe = format(glib::DateTime::new(&glib::TimeZone::utc(), 2001, 2, 3, 12, 0, 0.0).ok(), "%x");
    match (probe.find('2'), probe.find('3')) {
        (Some(m), Some(d)) => m < d,
        _ => false,
    }
}

/// A weekday, day, and month, in the locale's order and names: `Sun Nov 15` or `Sun 15 Nov`.
pub fn day_and_month(unix: i64) -> String {
    format(at(unix), if month_first() { "%a %b %-d" } else { "%a %-d %b" })
}

/// A day and month in the locale's order: `Nov 15` or `15 Nov`.
pub fn month_day(date: NaiveDate) -> String {
    format(on(date), if month_first() { "%b %-d" } else { "%-d %b" })
}

/// A month's abbreviated name.
pub fn month_short(date: NaiveDate) -> String {
    format(on(date), "%b")
}

/// A month's name and year.
pub fn month_year(date: NaiveDate) -> String {
    format(on(date), "%B %Y")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_locale(name: &str, f: impl FnOnce()) {
        let c = std::ffi::CString::new(name).unwrap();
        // Tests run on threads of their own, but `setlocale` is process-wide.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let set = unsafe { !setlocale(LC_ALL, c.as_ptr()).is_null() };
        if set {
            f();
        } else {
            panic!("{name} is not installed");
        }
        unsafe { setlocale(LC_ALL, c"C".as_ptr()) };
    }

    /// glibc's value.
    const LC_ALL: i32 = 6;

    extern "C" {
        fn setlocale(category: i32, locale: *const std::ffi::c_char) -> *const std::ffi::c_char;
    }

    /// Needs the three locales: `LOCPATH` may point at ones built with `localedef`.

    #[test]
    #[ignore]
    fn dates_follow_the_locale() {
        crate::host::set_zone(chrono_tz::America::Chicago);
        let t = 1_763_236_800; // 2025-11-15 20:00 UTC, 14:00 in Chicago
        with_locale("en_US.UTF-8", || {
            assert_eq!(short_date(t), "11/15/2025");
            assert_eq!(time(t), "2:00 PM");
            assert_eq!(watch_time(t), "2:00");
            assert_eq!(day_and_month(t), "Sat Nov 15");
        });
        with_locale("en_GB.UTF-8", || {
            assert_eq!(short_date(t), "15/11/25");
            assert_eq!(time(t), "14:00");
            assert_eq!(day_and_month(t), "Sat 15 Nov");
        });
        with_locale("de_DE.UTF-8", || {
            assert_eq!(short_date(t), "15.11.2025");
            assert_eq!(weekday(NaiveDate::from_ymd_opt(2025, 11, 15).unwrap()), "Samstag");
        });
    }
}
