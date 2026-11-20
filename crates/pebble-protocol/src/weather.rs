//! Weather locations for the watch's Weather app (section 14): BlobDB 5, keyed by a UUID per
//! place. Little-endian. Watches with `WeatherDbV4` take the v4 record, others v3.

/// Conditions, as the watch knows them.
pub mod kind {
    pub const PARTLY_CLOUDY: u8 = 0;
    pub const CLOUDY: u8 = 1;
    pub const LIGHT_SNOW: u8 = 2;
    pub const LIGHT_RAIN: u8 = 3;
    pub const HEAVY_RAIN: u8 = 4;
    pub const HEAVY_SNOW: u8 = 5;
    pub const GENERIC: u8 = 6;
    pub const SUN: u8 = 7;
    pub const RAIN_AND_SNOW: u8 = 8;
    pub const UNKNOWN: u8 = 255;
}

pub const DAYS: usize = 7;
pub const HOURS: usize = 24;

/// Daily forecast: high, low, conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub high: i16,
    pub low: i16,
    pub kind: u8,
}

/// The v4 additions; `None` fields go as the watch's "unknown" values.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extra {
    pub feels_like: Option<i16>,
    pub uv_x10: Option<i16>,
    pub precip_percent: Option<i16>,
    pub wind_speed: Option<u16>,
    pub wind_direction: Option<u16>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Index 0 is today.
    pub daily: Vec<Day>,
    /// Today's hours 0 to 23: conditions and temperature.
    pub hourly: Vec<(u8, i8)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub current_temp: i16,
    pub current_kind: u8,
    pub today_high: i16,
    pub today_low: i16,
    pub tomorrow_kind: u8,
    pub tomorrow_high: i16,
    pub tomorrow_low: i16,
    /// Unix seconds.
    pub updated: u32,
    pub is_current_location: bool,
    pub name: String,
    /// "Partly cloudy".
    pub phrase: String,
}

/// The watch's sentinel for an unknown temperature.
const TEMP_UNKNOWN: i16 = i16::MAX;

fn put_strings(out: &mut Vec<u8>, strings: &[&str]) {
    let cut = |s: &str| {
        let mut end = s.len().min(63);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    };
    let strings: Vec<String> = strings.iter().map(|s| cut(s)).collect();
    let total: usize = strings.iter().map(|s| s.len() + 2).sum();
    out.extend_from_slice(&(total as u16).to_le_bytes());
    for s in &strings {
        out.extend_from_slice(&(s.len() as u16).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
}

impl Record {
    fn prefix(&self, version: u8, out: &mut Vec<u8>) {
        out.push(version);
        out.extend_from_slice(&self.current_temp.to_le_bytes());
        out.push(self.current_kind);
        out.extend_from_slice(&self.today_high.to_le_bytes());
        out.extend_from_slice(&self.today_low.to_le_bytes());
        out.push(self.tomorrow_kind);
        out.extend_from_slice(&self.tomorrow_high.to_le_bytes());
        out.extend_from_slice(&self.tomorrow_low.to_le_bytes());
        out.extend_from_slice(&self.updated.to_le_bytes());
        out.push(self.is_current_location as u8);
    }

    /// The legacy record.
    pub fn encode_v3(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.prefix(3, &mut out);
        put_strings(&mut out, &[&self.name, &self.phrase]);
        out
    }

    /// The v4.0 record: the v3 fields, today's details, the week, and today's hours.
    pub fn encode_v4(&self, extra: &Extra) -> Vec<u8> {
        let mut out = Vec::new();
        self.prefix(4, &mut out);
        out.push(0); // minor version
        out.extend_from_slice(&extra.feels_like.unwrap_or(TEMP_UNKNOWN).to_le_bytes());
        out.extend_from_slice(&extra.uv_x10.unwrap_or(-1).to_le_bytes());
        out.extend_from_slice(&extra.precip_percent.unwrap_or(-1).to_le_bytes());
        out.extend_from_slice(&extra.wind_speed.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&extra.wind_direction.unwrap_or(0xFFFF).to_le_bytes());
        let coord = |c: Option<f64>| c.map(|v| (v * 100.0).round().clamp(-32767.0, 32767.0) as i16).unwrap_or(i16::MIN);
        out.extend_from_slice(&coord(extra.latitude).to_le_bytes());
        out.extend_from_slice(&coord(extra.longitude).to_le_bytes());
        let days = &extra.daily[..extra.daily.len().min(DAYS)];
        out.push(days.len() as u8);
        for i in 0..DAYS {
            let d = days.get(i).copied().unwrap_or(Day { high: TEMP_UNKNOWN, low: TEMP_UNKNOWN, kind: kind::UNKNOWN });
            out.extend_from_slice(&d.high.to_le_bytes());
            out.extend_from_slice(&d.low.to_le_bytes());
            out.push(d.kind);
        }
        let hours = if extra.hourly.len() >= HOURS { &extra.hourly[..HOURS] } else { &[][..] };
        out.push(hours.len() as u8);
        for i in 0..HOURS {
            out.push(hours.get(i).map_or(kind::UNKNOWN, |h| h.0));
        }
        for i in 0..HOURS {
            out.push(hours.get(i).map_or(0, |h| h.1) as u8);
        }
        put_strings(&mut out, &[&self.name, &self.phrase]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        Record {
            current_temp: 38,
            current_kind: kind::SUN,
            today_high: 52,
            today_low: 30,
            tomorrow_kind: kind::CLOUDY,
            tomorrow_high: 50,
            tomorrow_low: 28,
            updated: 1,
            is_current_location: true,
            name: "Bentonville".into(),
            phrase: "Clear".into(),
        }
    }

    #[test]
    fn layouts() {
        let v3 = record().encode_v3();
        // 18-byte fixed part, then the strings block: total, then each with its length.
        assert_eq!(v3.len(), 18 + 2 + 2 + 11 + 2 + 5);
        assert_eq!(&v3[..3], &[3, 38, 0]);
        assert_eq!(&v3[18..22], &[20, 0, 11, 0]);
        let v4 = record().encode_v4(&Extra {
            daily: vec![Day { high: 52, low: 30, kind: kind::SUN }],
            hourly: vec![(kind::SUN, 5); 24],
            ..Default::default()
        });
        // v4.0 fixed part: 18 + 1 + 14 + 1 + 35 + 1 + 48 = 118.
        assert_eq!(v4.len(), 118 + 2 + 2 + 11 + 2 + 5);
        assert_eq!(v4[0], 4);
        assert_eq!(v4[18], 0);
        assert_eq!(&v4[19..21], &i16::MAX.to_le_bytes());
        assert_eq!(v4[33], 1); // one day
        assert_eq!(&v4[34..39], &[52, 0, 30, 0, 7]);
        assert_eq!(v4[69], 24); // hours
    }
}
