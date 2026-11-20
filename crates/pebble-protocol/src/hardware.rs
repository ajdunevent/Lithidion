//! Hardware platforms, app platforms, and model numbers (section 8).

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppPlatform {
    Aplite,
    Basalt,
    Chalk,
    Diorite,
    Emery,
    Flint,
    Gabbro,
}

impl AppPlatform {
    pub fn codename(self) -> &'static str {
        match self {
            AppPlatform::Aplite => "aplite",
            AppPlatform::Basalt => "basalt",
            AppPlatform::Chalk => "chalk",
            AppPlatform::Diorite => "diorite",
            AppPlatform::Emery => "emery",
            AppPlatform::Flint => "flint",
            AppPlatform::Gabbro => "gabbro",
        }
    }

    pub fn from_codename(name: &str) -> Option<AppPlatform> {
        [
            AppPlatform::Aplite,
            AppPlatform::Basalt,
            AppPlatform::Chalk,
            AppPlatform::Diorite,
            AppPlatform::Emery,
            AppPlatform::Flint,
            AppPlatform::Gabbro,
        ]
        .into_iter()
        .find(|p| p.codename() == name)
    }

    /// Platforms whose apps run here, best first.
    pub fn compatible(self) -> &'static [AppPlatform] {
        use AppPlatform::*;
        match self {
            Aplite => &[Aplite],
            Basalt => &[Basalt, Aplite],
            Chalk => &[Chalk],
            Diorite => &[Diorite, Aplite],
            Emery => &[Emery, Basalt, Diorite, Aplite],
            Flint => &[Flint, Diorite, Aplite],
            Gabbro => &[Gabbro, Chalk],
        }
    }

    /// The watch family, for a watch whose exact model is unknown.
    pub fn family_name(self) -> &'static str {
        match self {
            AppPlatform::Aplite => "Pebble",
            AppPlatform::Basalt => "Pebble Time",
            AppPlatform::Chalk => "Pebble Time Round",
            AppPlatform::Diorite => "Pebble 2",
            AppPlatform::Emery => "Pebble Time 2",
            AppPlatform::Flint => "Pebble 2 Duo",
            AppPlatform::Gabbro => "Pebble Round 2",
        }
    }

    /// Whether pairing starts with a write to the pairing trigger (section 3).
    pub fn needs_pairing_trigger(self) -> bool {
        !matches!(self, AppPlatform::Basalt | AppPlatform::Chalk)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwarePlatform {
    pub number: u8,
    pub revision: &'static str,
    pub app_platform: Option<AppPlatform>,
}

const HARDWARE: &[(u8, &str, AppPlatform)] = &[
    (1, "ev1", AppPlatform::Aplite),
    (2, "ev2", AppPlatform::Aplite),
    (3, "ev2_3", AppPlatform::Aplite),
    (4, "ev2_4", AppPlatform::Aplite),
    (5, "v1_5", AppPlatform::Aplite),
    (6, "v2_0", AppPlatform::Aplite),
    (7, "snowy_evt2", AppPlatform::Basalt),
    (8, "snowy_dvt", AppPlatform::Basalt),
    (9, "spalding_evt", AppPlatform::Chalk),
    (10, "snowy_s3", AppPlatform::Basalt),
    (11, "spalding", AppPlatform::Chalk),
    (12, "silk_evt", AppPlatform::Diorite),
    (13, "robert_evt", AppPlatform::Emery),
    (14, "silk", AppPlatform::Diorite),
    (15, "asterix", AppPlatform::Flint),
    (16, "obelix_evt", AppPlatform::Emery),
    (17, "obelix_dvt", AppPlatform::Emery),
    (18, "obelix_pvt", AppPlatform::Emery),
    (19, "getafix_evt", AppPlatform::Gabbro),
    (20, "getafix_dvt", AppPlatform::Gabbro),
    (21, "getafix_dvt2", AppPlatform::Gabbro),
    (242, "qemu_gabbro", AppPlatform::Gabbro),
    (243, "obelix_bb2", AppPlatform::Emery),
    (244, "obelix_bb", AppPlatform::Emery),
    (245, "qemu_emery", AppPlatform::Emery),
    (246, "qemu_flint", AppPlatform::Flint),
    (247, "robert_bb2", AppPlatform::Emery),
    (248, "silk_bb2", AppPlatform::Diorite),
    (249, "robert_bb", AppPlatform::Emery),
    (250, "silk_bb", AppPlatform::Diorite),
    (251, "spalding_bb2", AppPlatform::Chalk),
    (253, "snowy_bb2", AppPlatform::Basalt),
    (254, "bb2", AppPlatform::Aplite),
    (255, "bigboard", AppPlatform::Aplite),
];

impl HardwarePlatform {
    pub fn from_number(number: u8) -> HardwarePlatform {
        match HARDWARE.iter().find(|(n, _, _)| *n == number) {
            Some(&(number, revision, app)) => HardwarePlatform { number, revision, app_platform: Some(app) },
            None => HardwarePlatform { number, revision: "unknown", app_platform: None },
        }
    }
}

/// Model name and finish for a model number from the factory registry or a scan record.
pub fn model(number: i32) -> Option<(&'static str, &'static str)> {
    Some(match number {
        1 => ("Pebble", "Black"),
        2 => ("Pebble", "White"),
        3 => ("Pebble", "Red"),
        4 => ("Pebble", "Orange"),
        5 => ("Pebble", "Pink"),
        6 => ("Pebble Steel", "Silver"),
        7 => ("Pebble Steel", "Gunmetal"),
        8 => ("Pebble", "Fly Blue"),
        9 => ("Pebble", "Fresh Green"),
        10 => ("Pebble", "Hot Pink"),
        11 => ("Pebble Time", "White"),
        12 => ("Pebble Time", "Black"),
        13 => ("Pebble Time", "Red"),
        14 => ("Pebble Time Steel", "Silver"),
        15 => ("Pebble Time Steel", "Black"),
        16 => ("Pebble Time Steel", "Gold"),
        17 | 19 => ("Pebble Time Round", "Silver"),
        18 | 20 => ("Pebble Time Round", "Black"),
        21 => ("Pebble Time Round", "Rose Gold"),
        22 => ("Pebble Time Round", "Silver Rainbow"),
        23 => ("Pebble Time Round", "Black Rainbow"),
        24 => ("Pebble 2 SE", "Black Charcoal"),
        25 => ("Pebble 2 HR", "Black Charcoal"),
        26 => ("Pebble 2 SE", "White/Gray"),
        27 => ("Pebble 2 HR", "Sorbet Green"),
        28 => ("Pebble 2 HR", "Charcoal Red"),
        29 => ("Pebble 2 HR", "White Gray"),
        30 => ("Pebble 2 HR", "White Turquoise"),
        31 => ("Pebble Time 2", "Black"),
        32 => ("Pebble Time 2", "Silver"),
        33 => ("Pebble Time 2", "Gold"),
        34 => ("Pebble 2 Duo", "Black"),
        35 => ("Pebble 2 Duo", "White"),
        36 => ("Pebble Time 2", "Black/Gray"),
        37 => ("Pebble Time 2", "Black/Red"),
        38 => ("Pebble Time 2", "Silver/Blue"),
        39 => ("Pebble Time 2", "Silver/Gray"),
        40 => ("Pebble Round 2", "Black"),
        41 | 43 => ("Pebble Round 2", "Silver"),
        42 => ("Pebble Round 2", "Gold"),
        _ => return None,
    })
}

/// The name scripts see for a model (`Pebble.getActiveWatchInfo().model`), as the Pebble app
/// gave it.
pub fn js_model(number: i32) -> &'static str {
    match number {
        1 => "pebble_black",
        2 => "pebble_white",
        3 => "pebble_red",
        4 => "pebble_orange",
        5 => "pebble_pink",
        6 => "pebble_steel_silver",
        7 => "pebble_steel_gunmetal",
        8 => "pebble_fly_blue",
        9 => "pebble_fresh_green",
        10 => "pebble_hot_pink",
        11 => "pebble_time_white",
        12 => "pebble_time_black",
        13 => "pebble_time_red",
        14 => "pebble_time_steel_silver",
        15 => "pebble_time_steel_black",
        16 => "pebble_time_steel_gold",
        17 => "pebble_time_round_silver",
        18 => "pebble_time_round_black",
        19 => "pebble_time_round_silver_20",
        20 => "pebble_time_round_black_20",
        21 => "pebble_time_round_rose_gold",
        22 => "pebble_time_round_silver_rainbow",
        23 => "pebble_time_round_black_rainbow",
        24 => "pebble_2_se_black_charcoal",
        25 => "pebble_2_hr_black_charcoal",
        26 => "pebble_2_se_white_gray",
        27 => "pebble_2_hr_charcoal_sorbet_green",
        28 => "pebble_2_hr_charcoal_red",
        29 => "pebble_2_hr_white_gray",
        30 => "pebble_2_hr_white_turquoise",
        31 => "pebble_time_2_black",
        32 => "pebble_time_2_silver",
        33 => "pebble_time_2_gold",
        34 => "pebble_2_duo_black",
        35 => "pebble_2_duo_white",
        36 => "pebble_time_2_black_gray",
        37 => "pebble_time_2_black_red",
        38 => "pebble_time_2_silver_blue",
        39 => "pebble_time_2_silver_gray",
        40 => "pebble_round_2_black",
        41 => "pebble_round_2_silver",
        42 => "pebble_round_2_gold",
        43 => "pebble_round_2_silver_14",
        _ => "unknown_unknown",
    }
}

/// Whether the watch has a heart rate sensor: the Pebble 2 HR and the Time 2. Without a known
/// model, the Time 2's platform (Emery) says so.
pub fn has_heart_rate(model: Option<i32>, platform: Option<AppPlatform>) -> bool {
    match model {
        Some(m) if self::model(m).is_some() => matches!(m, 25 | 27..=33 | 36..=39),
        _ => platform == Some(AppPlatform::Emery),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obelix_is_emery() {
        let hw = HardwarePlatform::from_number(18);
        assert_eq!(hw.revision, "obelix_pvt");
        assert_eq!(hw.app_platform, Some(AppPlatform::Emery));
        assert_eq!(AppPlatform::Emery.compatible()[1], AppPlatform::Basalt);
        assert_eq!(HardwarePlatform::from_number(99).app_platform, None);
    }

    #[test]
    fn models() {
        assert_eq!(model(37), Some(("Pebble Time 2", "Black/Red")));
        assert_eq!(model(-1), None);
        assert!(has_heart_rate(Some(25), None));
        assert!(!has_heart_rate(Some(24), Some(AppPlatform::Diorite)));
        assert!(!has_heart_rate(Some(34), None));
        assert!(has_heart_rate(None, Some(AppPlatform::Emery)));
    }
}
