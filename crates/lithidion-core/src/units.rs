//! Units: the system's (from the locale), metric, or imperial, as chosen in Preferences. Used
//! for what Lithidion shows and what it sends the watch (weather, distance).

use crate::store::Store;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    System = 0,
    Metric = 1,
    Imperial = 2,
}

impl Units {
    pub const ALL: [Units; 3] = [Units::System, Units::Metric, Units::Imperial];

    pub fn key(self) -> &'static str {
        match self {
            Units::System => "system",
            Units::Metric => "metric",
            Units::Imperial => "imperial",
        }
    }

    fn from_key(key: &str) -> Units {
        Units::ALL.into_iter().find(|u| u.key() == key).unwrap_or(Units::System)
    }
}

const KEY: &str = "units";
static CHOSEN: AtomicU8 = AtomicU8::new(Units::System as u8);

pub fn stored(store: &Store) -> Units {
    store.setting(KEY).ok().flatten().map(|v| Units::from_key(&v)).unwrap_or(Units::System)
}

/// Reads the choice; at startup.
pub fn load(store: &Store) {
    use_units(stored(store));
}

/// Takes the choice as given, where the database is not at hand.
pub fn use_units(u: Units) {
    CHOSEN.store(u as u8, Ordering::Relaxed);
}

pub fn set(store: &Store, u: Units) -> anyhow::Result<()> {
    CHOSEN.store(u as u8, Ordering::Relaxed);
    store.set_setting(KEY, u.key())
}

pub fn chosen() -> Units {
    match CHOSEN.load(Ordering::Relaxed) {
        1 => Units::Metric,
        2 => Units::Imperial,
        _ => Units::System,
    }
}

pub fn imperial() -> bool {
    match chosen() {
        Units::Metric => false,
        Units::Imperial => true,
        Units::System => locale_imperial(),
    }
}

/// US, Liberia, and Myanmar locales measure in imperial units.
fn locale_imperial() -> bool {
    let locale =
        ["LC_ALL", "LC_MEASUREMENT", "LANG"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty())).unwrap_or_default();
    let territory = locale.split(['.', '@']).next().unwrap_or("").split('_').nth(1).unwrap_or("");
    matches!(territory, "US" | "LR" | "MM")
}
