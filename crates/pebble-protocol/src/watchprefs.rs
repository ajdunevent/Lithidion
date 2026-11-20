//! The watch's own settings (section 20): records in BlobDB 12 keyed by name, as PebbleOS's
//! `settings_blob_db.c` allowlist and the official app's `WatchPrefEntity.kt` define them.
//! Values are little-endian; keys go out with their trailing NUL, and may come back without it.

use crate::hardware::AppPlatform;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    U8,
    U16,
    U32,
    /// From and to, hours and minutes.
    Schedule,
    /// Enabled, then the app's UUID (all ones for none).
    QuickLaunch,
}

/// Hardware a setting needs, beyond the settings database itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    Nothing,
    ColorBacklight,
    DynamicBacklight,
    Touch,
    Orientation,
    /// Quick View drawn over faces that do not leave room for it.
    PeekFit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pref {
    pub key: &'static str,
    pub kind: Kind,
    pub needs: Needs,
}

const fn pref(key: &'static str, kind: Kind) -> Pref {
    Pref { key, kind, needs: Needs::Nothing }
}

const fn pref_needing(key: &'static str, kind: Kind, needs: Needs) -> Pref {
    Pref { key, kind, needs }
}

pub const CLOCK_24H: &str = "clock24h";
pub const UNITS_WIND: &str = "unitsWind";
pub const UNITS_TEMPERATURE: &str = "unitsTemperature";
pub const SYSTEM_TEXT_SIZE: &str = "systemTextSize";
pub const NOTIFICATION_TEXT_SIZE: &str = "notifTextSize";

/// The settings Lithidion reads and writes.
pub const PREFS: &[Pref] = &[
    pref(CLOCK_24H, Kind::Bool),
    pref("timezoneSource", Kind::Bool),
    pref(UNITS_WIND, Kind::U8),
    pref(UNITS_TEMPERATURE, Kind::U8),
    pref(SYSTEM_TEXT_SIZE, Kind::U8),
    pref("stationaryMode", Kind::Bool),
    pref_needing("displayOrientationLeftHanded", Kind::Bool, Needs::Orientation),
    pref("lightEnabled", Kind::Bool),
    pref("lightAmbientSensorEnabled", Kind::Bool),
    pref("lightTimeoutMs", Kind::U32),
    pref("lightIntensity", Kind::U8),
    pref("lightMotion", Kind::Bool),
    pref_needing("lightTouch", Kind::U8, Needs::Touch),
    pref_needing("lightDynamicMode", Kind::U8, Needs::DynamicBacklight),
    pref("lightPreset", Kind::U8),
    pref_needing("lightColor", Kind::U32, Needs::ColorBacklight),
    pref_needing("lightColorDayNightEnabled", Kind::Bool, Needs::ColorBacklight),
    pref_needing("lightColorNight", Kind::U32, Needs::ColorBacklight),
    pref_needing("lightColorSunriseMinute", Kind::U16, Needs::ColorBacklight),
    pref_needing("lightColorSunsetMinute", Kind::U16, Needs::ColorBacklight),
    pref("qlUp", Kind::QuickLaunch),
    pref("qlDown", Kind::QuickLaunch),
    pref("qlSelect", Kind::QuickLaunch),
    pref("qlBack", Kind::QuickLaunch),
    pref("qlSingleClickUp", Kind::QuickLaunch),
    pref("qlSingleClickDown", Kind::QuickLaunch),
    pref("qlComboBackUp", Kind::QuickLaunch),
    pref("qlComboUpDown", Kind::QuickLaunch),
    pref("timelineQuickViewEnabled", Kind::Bool),
    pref("timelineQuickViewBeforeTimeMin", Kind::U16),
    pref_needing("timelineQuickViewWatchfaceFit", Kind::U8, Needs::PeekFit),
    pref("menuScrollWrapAround", Kind::Bool),
    pref("menuScrollVibeBehavior", Kind::U8),
    pref("musicShowVolumeControls", Kind::Bool),
    pref("musicShowProgressBar", Kind::Bool),
    pref("mask", Kind::U8),
    pref("dndInterruptionsMask", Kind::U8),
    pref("dndShowNotifications", Kind::U8),
    pref("vibeIntensity", Kind::U8),
    pref("vibeScoreNotifications", Kind::U8),
    pref("vibeScoreIncomingCalls", Kind::U8),
    pref("vibeScoreAlarms", Kind::U8),
    pref("dndManuallyEnabled", Kind::Bool),
    pref("dndSmartEnabled", Kind::Bool),
    pref("dndWeekdaySchedule", Kind::Schedule),
    pref("dndWeekdayScheduleEnabled", Kind::Bool),
    pref("dndWeekendSchedule", Kind::Schedule),
    pref("dndWeekendScheduleEnabled", Kind::Bool),
    pref("notifWindowTimeout", Kind::U32),
    pref(NOTIFICATION_TEXT_SIZE, Kind::U8),
    pref("notifDesignStyle", Kind::Bool),
    pref("notifVibeDelay", Kind::Bool),
    pref("notifBacklight", Kind::Bool),
    pref("notifDismissalSync", Kind::Bool),
    pref("dndMotionBacklight", Kind::Bool),
    pref("dndTouchBacklight", Kind::Bool),
    pref("dndNotificationBacklight", Kind::Bool),
    pref("dndAutoDismiss", Kind::Bool),
];

pub fn find(key: &str) -> Option<&'static Pref> {
    PREFS.iter().find(|p| p.key == key)
}

/// The key as it goes on the wire.
pub fn key_bytes(key: &str) -> Vec<u8> {
    let mut out = key.as_bytes().to_vec();
    out.push(0);
    out
}

/// A key from the wire, with or without its NUL.
pub fn key_from_bytes(bytes: &[u8]) -> Option<String> {
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Number(u32),
    Schedule { from: (u8, u8), to: (u8, u8) },
    QuickLaunch { enabled: bool, app: Option<Uuid> },
}

const NO_APP: Uuid = Uuid::from_bytes([0xFF; 16]);

pub fn encode(kind: Kind, value: Value) -> Option<Vec<u8>> {
    Some(match (kind, value) {
        (Kind::Bool, Value::Bool(b)) => vec![b as u8],
        (Kind::U8, Value::Number(n)) => vec![u8::try_from(n).ok()?],
        (Kind::U16, Value::Number(n)) => u16::try_from(n).ok()?.to_le_bytes().to_vec(),
        (Kind::U32, Value::Number(n)) => n.to_le_bytes().to_vec(),
        (Kind::Schedule, Value::Schedule { from, to }) => vec![from.0, from.1, to.0, to.1],
        (Kind::QuickLaunch, Value::QuickLaunch { enabled, app }) => {
            let mut out = vec![enabled as u8];
            out.extend_from_slice(app.unwrap_or(NO_APP).as_bytes());
            out
        }
        _ => return None,
    })
}

pub fn decode(kind: Kind, bytes: &[u8]) -> Option<Value> {
    Some(match kind {
        Kind::Bool => Value::Bool(*bytes.first()? != 0),
        Kind::U8 => Value::Number(*bytes.first()? as u32),
        Kind::U16 => Value::Number(u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?) as u32),
        Kind::U32 => Value::Number(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?)),
        Kind::Schedule => {
            let b = bytes.get(..4)?;
            Value::Schedule { from: (b[0], b[1]), to: (b[2], b[3]) }
        }
        Kind::QuickLaunch => {
            let enabled = *bytes.first()? != 0;
            let app = Uuid::from_slice(bytes.get(1..17)?).ok()?;
            Value::QuickLaunch { enabled, app: (app != NO_APP && !app.is_nil()).then_some(app) }
        }
    })
}

/// The hardware behind `needs`, by platform (PebbleOS board configs: emery is obelix, gabbro is
/// getafix, flint is asterix).
pub fn has(platform: AppPlatform, needs: Needs) -> bool {
    use AppPlatform::*;
    match needs {
        Needs::Nothing => true,
        Needs::ColorBacklight => platform == Emery,
        Needs::DynamicBacklight | Needs::Touch | Needs::PeekFit => matches!(platform, Emery | Gabbro),
        Needs::Orientation => matches!(platform, Emery | Gabbro | Flint),
    }
}

/// Text sizes on these platforms start one higher on the wire (LP `textSizeOffset`).
pub fn text_size_offset(platform: AppPlatform) -> u8 {
    matches!(platform, AppPlatform::Emery | AppPlatform::Gabbro) as u8
}

/// Wind and temperature units as the watch numbers them.
pub fn wind_units(imperial: bool) -> u32 {
    if imperial {
        2
    } else {
        1
    }
}

pub fn temperature_units(imperial: bool) -> u32 {
    if imperial {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for (kind, value) in [
            (Kind::Bool, Value::Bool(true)),
            (Kind::U8, Value::Number(15)),
            (Kind::U16, Value::Number(600)),
            (Kind::U32, Value::Number(180_000)),
            (Kind::Schedule, Value::Schedule { from: (22, 0), to: (7, 30) }),
            (Kind::QuickLaunch, Value::QuickLaunch { enabled: true, app: Some(Uuid::from_u128(7)) }),
            (Kind::QuickLaunch, Value::QuickLaunch { enabled: false, app: None }),
        ] {
            assert_eq!(decode(kind, &encode(kind, value).unwrap()), Some(value), "{kind:?}");
        }
        assert_eq!(encode(Kind::U32, Value::Number(3000)).unwrap(), vec![0xB8, 0x0B, 0, 0]);
        assert_eq!(encode(Kind::QuickLaunch, Value::QuickLaunch { enabled: false, app: None }).unwrap()[1..], [0xFF; 16]);
        assert_eq!(encode(Kind::U8, Value::Number(300)), None);
        assert_eq!(key_bytes("mask"), b"mask\0");
        assert_eq!(key_from_bytes(b"mask\0").as_deref(), Some("mask"));
        assert_eq!(key_from_bytes(b"mask").as_deref(), Some("mask"));
        assert!(PREFS.iter().all(|p| find(p.key) == Some(p)));
    }
}
