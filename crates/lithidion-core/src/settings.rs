//! App settings, kept in the store's `settings` table.

use crate::appstore::Source;
use crate::backend::weather::{Place, Provider};
use crate::store::{ProfileId, Store, WatchId};
use anyhow::Result;

const RUN_IN_BACKGROUND: &str = "run_in_background";
const START_AT_LOGIN: &str = "start_at_login";
const ACTIVE_WATCH: &str = "active_watch";

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub run_in_background: bool,
    pub start_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { run_in_background: true, start_at_login: false }
    }
}

fn flag(store: &Store, key: &str, default: bool) -> Result<bool> {
    Ok(store.setting(key)?.map(|v| v == "1").unwrap_or(default))
}

impl Settings {
    pub fn load(store: &Store) -> Result<Settings> {
        let d = Settings::default();
        Ok(Settings {
            run_in_background: flag(store, RUN_IN_BACKGROUND, d.run_in_background)?,
            start_at_login: flag(store, START_AT_LOGIN, d.start_at_login)?,
        })
    }

    pub fn save(&self, store: &Store) -> Result<()> {
        store.set_setting(RUN_IN_BACKGROUND, if self.run_in_background { "1" } else { "0" })?;
        store.set_setting(START_AT_LOGIN, if self.start_at_login { "1" } else { "0" })?;
        Ok(())
    }
}

/// The settings the window shows, for the active profile.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct View {
    pub general: Settings,
    pub units: crate::units::Units,
    pub launch_with_chooser: bool,
    pub pictures: bool,
    pub catalog_sources: Vec<Source>,
    pub calendars: Vec<String>,
    pub weather: WeatherSettings,
    /// Apps allowed the network.
    pub network: Vec<uuid::Uuid>,
}

pub fn view(store: &Store) -> View {
    let profile = store.profile().unwrap_or(0);
    let network = store
        .setting_keys("network:")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|key| {
            let rest = key.strip_prefix("network:")?;
            let (app, p) = rest.split_once(":p").map(|(a, p)| (a, p.parse().ok())).unwrap_or((rest, Some(0)));
            (p == Some(profile)).then(|| app.parse().ok()).flatten()
        })
        .filter(|app| network_allowed(store, profile, *app))
        .collect();
    View {
        general: Settings::load(store).unwrap_or_default(),
        units: crate::units::stored(store),
        launch_with_chooser: launch_with_chooser(store),
        pictures: pictures(store),
        catalog_sources: catalog_sources(store),
        calendars: calendars(store),
        weather: weather(store),
        network,
    }
}

/// `key` for one profile: `<key>:p<id>`, or `key` itself before any profile exists.
pub fn profile_key(key: &str, profile: ProfileId) -> String {
    if profile == 0 {
        key.to_string()
    } else {
        format!("{key}:p{profile}")
    }
}

/// `key` for the active profile.
fn scoped(store: &Store, key: &str) -> String {
    profile_key(key, store.profile().unwrap_or(0))
}

/// The active profile's active watch.
pub fn active_watch(store: &Store) -> Result<Option<WatchId>> {
    Ok(store.setting(&scoped(store, ACTIVE_WATCH))?.and_then(|v| v.parse().ok()))
}

pub fn set_active_watch(store: &Store, id: Option<WatchId>) -> Result<()> {
    let key = scoped(store, ACTIVE_WATCH);
    match id {
        Some(id) => store.set_setting(&key, &id.to_string()),
        None => store.clear_setting(&key),
    }
}

const CALENDARS: &str = "calendars";
const WEATHER_PROVIDER: &str = "weather_provider";
const WEATHER_HERE: &str = "weather_here";
const WEATHER_PLACES: &str = "weather_places";

/// The calendars sent to the watch, by EDS source UID; none until picked.
pub fn calendars(store: &Store) -> Vec<String> {
    store.setting(&scoped(store, CALENDARS)).ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

pub fn set_calendars(store: &Store, uids: &[String]) -> Result<()> {
    store.set_setting(&scoped(store, CALENDARS), &serde_json::to_string(uids)?)
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct WeatherSettings {
    pub provider: Provider,
    /// The phone's location comes first.
    pub here: bool,
    pub places: Vec<Place>,
}

pub fn weather(store: &Store) -> WeatherSettings {
    WeatherSettings {
        provider: Provider::from_key(&store.setting(&scoped(store, WEATHER_PROVIDER)).ok().flatten().unwrap_or_default()),
        here: flag(store, &scoped(store, WEATHER_HERE), true).unwrap_or(true),
        places: store
            .setting(&scoped(store, WEATHER_PLACES))
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default(),
    }
}

pub fn set_weather(store: &Store, w: &WeatherSettings) -> Result<()> {
    store.set_setting(&scoped(store, WEATHER_PROVIDER), w.provider.key())?;
    store.set_setting(&scoped(store, WEATHER_HERE), if w.here { "1" } else { "0" })?;
    store.set_setting(&scoped(store, WEATHER_PLACES), &serde_json::to_string(&w.places)?)
}

/// A profile's health details and settings. Sent to its watches as their
/// `activityPreferences`, `hrmPreferences`, and `heartRatePreferences`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HealthSettings {
    pub tracking: bool,
    pub activity_insights: bool,
    pub sleep_insights: bool,
    pub height_mm: u16,
    /// Decagrams (10 g), as the watch keeps it.
    pub weight_dag: u16,
    /// Unset: the watch's default age, 30.
    pub birth_year: Option<i32>,
    pub sex: Sex,
    pub heart_rate: bool,
    pub heart_rate_interval: Interval,
    pub heart_rate_during_activities: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sex {
    #[default]
    Female,
    Male,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    Five,
    #[default]
    Ten,
    Thirty,
    Hour,
}

/// The watch's own defaults, but with tracking on.
impl Default for HealthSettings {
    fn default() -> Self {
        HealthSettings {
            tracking: true,
            activity_insights: false,
            sleep_insights: false,
            height_mm: 1620,
            weight_dag: 7539,
            birth_year: None,
            sex: Sex::Female,
            heart_rate: true,
            heart_rate_interval: Interval::Ten,
            heart_rate_during_activities: false,
        }
    }
}

impl HealthSettings {
    /// Takes the wearer's details from `other`: height, weight, birth year, and sex.
    pub fn set_details(&mut self, other: &HealthSettings) {
        self.height_mm = other.height_mm;
        self.weight_dag = other.weight_dag;
        self.birth_year = other.birth_year;
        self.sex = other.sex;
    }

    pub fn age(&self, this_year: i32) -> u8 {
        self.birth_year.map(|y| (this_year - y).clamp(1, 120) as u8).unwrap_or(30)
    }
}

pub fn health(store: &Store, profile: ProfileId) -> HealthSettings {
    store.setting(&profile_key(HEALTH, profile)).ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

pub fn set_health(store: &Store, profile: ProfileId, h: &HealthSettings) -> Result<()> {
    store.set_setting(&profile_key(HEALTH, profile), &serde_json::to_string(h)?)
}

const HEALTH: &str = "health";

const CATALOG_SOURCES: &str = "catalog_sources";

/// The stores the Catalog shows, in `Source::ALL` order; never empty.
pub fn catalog_sources(store: &Store) -> Vec<Source> {
    let chosen: Vec<Source> = match store.setting(CATALOG_SOURCES).ok().flatten() {
        Some(v) => Source::ALL.into_iter().filter(|s| v.split(',').any(|k| k == s.key())).collect(),
        None => Source::ALL.to_vec(),
    };
    if chosen.is_empty() {
        Source::ALL.to_vec()
    } else {
        chosen
    }
}

pub fn set_catalog_sources(store: &Store, sources: &[Source]) -> Result<()> {
    let keys: Vec<&str> = sources.iter().map(|s| s.key()).collect();
    store.set_setting(CATALOG_SOURCES, &keys.join(","))
}

const LAUNCH_PROFILE: &str = "launch_profile";
const DICTATION: &str = "dictation";
const PICTURES: &str = "notification_pictures";

/// Whether startup shows the profile chooser ("Select") instead of the last profile used.
pub fn launch_with_chooser(store: &Store) -> bool {
    store.setting(LAUNCH_PROFILE).ok().flatten().as_deref() == Some("select")
}

pub fn set_launch_with_chooser(store: &Store, choose: bool) -> Result<()> {
    store.set_setting(LAUNCH_PROFILE, if choose { "select" } else { "last" })
}

/// Dictation for the watch, transcribed on this device. Off until turned on, as it needs a
/// download.
pub fn dictation(store: &Store) -> bool {
    store.setting(DICTATION).ok().flatten().as_deref() == Some("on")
}

pub fn set_dictation(store: &Store, on: bool) -> Result<()> {
    store.set_setting(DICTATION, if on { "on" } else { "off" })
}

/// Pictures in notifications, for watches that show them. Off until turned on.
pub fn pictures(store: &Store) -> bool {
    store.setting(PICTURES).ok().flatten().as_deref() == Some("on")
}

pub fn set_pictures(store: &Store, on: bool) -> Result<()> {
    store.set_setting(PICTURES, if on { "on" } else { "off" })
}

/// The watch's clock: as this device shows time, or fixed at 12 or 24 hours.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Clock {
    #[default]
    System,
    Twelve,
    TwentyFour,
}

impl Clock {
    /// Whether the watch should show 24-hour time.
    pub fn twenty_four(self) -> bool {
        match self {
            Clock::System => !crate::host::twelve_hour_clock().unwrap_or_else(crate::host::locale_twelve_hour),
            Clock::Twelve => false,
            Clock::TwentyFour => true,
        }
    }
}

/// Whether the watch has shown it keeps a settings database (seen during a session).
pub fn watch_has_settings(store: &Store, watch: WatchId) -> bool {
    store.setting(&format!("watch_settings_db:{watch}")).ok().flatten().is_some_and(|v| v == "1")
}

pub fn set_watch_has_settings(store: &Store, watch: WatchId) -> Result<()> {
    store.set_setting(&format!("watch_settings_db:{watch}"), "1")
}

pub fn watch_clock(store: &Store, watch: WatchId) -> Clock {
    match store.setting(&format!("watch_clock:{watch}")).ok().flatten().as_deref() {
        Some("12") => Clock::Twelve,
        Some("24") => Clock::TwentyFour,
        _ => Clock::System,
    }
}

pub fn set_watch_clock(store: &Store, watch: WatchId, clock: Clock) -> Result<()> {
    let value = match clock {
        Clock::System => "system",
        Clock::Twelve => "12",
        Clock::TwentyFour => "24",
    };
    store.set_setting(&format!("watch_clock:{watch}"), value)
}

/// Whether an app's script may use the network; off until allowed.
pub fn network_allowed(store: &Store, profile: ProfileId, app: uuid::Uuid) -> bool {
    store.setting(&profile_key(&format!("network:{app}"), profile)).ok().flatten().is_some_and(|v| v == "1")
}

pub fn set_network_allowed(store: &Store, profile: ProfileId, app: uuid::Uuid, allowed: bool) -> Result<()> {
    let key = profile_key(&format!("network:{app}"), profile);
    if allowed {
        store.set_setting(&key, "1")
    } else {
        store.clear_setting(&key)
    }
}

/// The profile's own seed for scripts' account tokens, made the first time it is asked for.
pub fn account_seed(store: &Store, profile: ProfileId) -> Result<String> {
    let key = profile_key("account_seed", profile);
    if let Some(seed) = store.setting(&key)? {
        return Ok(seed);
    }
    let seed = uuid::Uuid::new_v4().simple().to_string();
    store.set_setting(&key, &seed)?;
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_lists_the_active_profiles_network_apps() {
        let store = Store::in_memory();
        let (a, b) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
        set_network_allowed(&store, 0, a, true).unwrap();
        set_network_allowed(&store, 2, b, true).unwrap();
        assert_eq!(view(&store).network, [a]);
        store.set_profile(2).unwrap();
        assert_eq!(view(&store).network, [b]);
        set_network_allowed(&store, 2, b, false).unwrap();
        assert!(view(&store).network.is_empty());
        assert_eq!(view(&store).general, Settings::default());
    }
}
