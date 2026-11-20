//! Weather for the watch (PROTOCOL.md section 14): forecasts from the provider the user picks,
//! for the current location (through the location portal) and saved places.

use crate::host::Zone as Local;
use crate::net;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{NaiveDate, Timelike, Utc};
use pebble_protocol::weather::{kind, Day};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    OpenMeteo,
    MetNorway,
    NationalWeatherService,
    Core,
}

impl Provider {
    pub const ALL: [Provider; 4] = [Provider::OpenMeteo, Provider::MetNorway, Provider::NationalWeatherService, Provider::Core];

    pub fn key(self) -> &'static str {
        match self {
            Provider::OpenMeteo => "open-meteo",
            Provider::MetNorway => "met-norway",
            Provider::NationalWeatherService => "nws",
            Provider::Core => "core",
        }
    }

    pub fn from_key(k: &str) -> Provider {
        Provider::ALL.into_iter().find(|p| p.key() == k).unwrap_or(Provider::OpenMeteo)
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::OpenMeteo => "Open-Meteo",
            Provider::MetNorway => "MET Norway (Yr)",
            Provider::NationalWeatherService => "US National Weather Service",
            Provider::Core => "Core Devices",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            Provider::OpenMeteo | Provider::MetNorway | Provider::Core => "Worldwide",
            Provider::NationalWeatherService => "US Only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// A forecast in the user's units (°F and mph, or °C and km/h).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Forecast {
    pub temp: f64,
    pub feels_like: Option<f64>,
    pub kind: u8,
    pub phrase: String,
    pub wind_speed: Option<f64>,
    pub wind_direction: Option<f64>,
    pub uv: Option<f64>,
    pub precip_percent: Option<f64>,
    /// Index 0 is today.
    pub days: Vec<Day>,
    /// Today's hours 0 to 23, or empty.
    pub hours: Vec<(u8, i8)>,
    pub utc_offset_minutes: Option<i32>,
}

/// WMO weather codes (Open-Meteo).
fn wmo(code: i64) -> (u8, &'static str) {
    match code {
        0 => (kind::SUN, "Clear"),
        1 => (kind::SUN, "Mostly clear"),
        2 => (kind::PARTLY_CLOUDY, "Partly cloudy"),
        3 => (kind::CLOUDY, "Overcast"),
        45 | 48 => (kind::GENERIC, "Fog"),
        51 | 53 | 55 => (kind::LIGHT_RAIN, "Drizzle"),
        56 | 57 | 66 | 67 => (kind::RAIN_AND_SNOW, "Freezing rain"),
        61 | 63 | 80 | 81 => (kind::LIGHT_RAIN, "Rain"),
        65 | 82 => (kind::HEAVY_RAIN, "Heavy rain"),
        71 | 73 | 77 | 85 => (kind::LIGHT_SNOW, "Snow"),
        75 | 86 => (kind::HEAVY_SNOW, "Heavy snow"),
        95..=99 => (kind::HEAVY_RAIN, "Thunderstorms"),
        _ => (kind::GENERIC, ""),
    }
}

/// Words in a forecast ("Chance Light Rain", "heavysnowshowers_day") to conditions.
fn from_words(text: &str) -> u8 {
    let t = text.to_lowercase();
    let has = |w: &str| t.contains(w);
    if has("sleet") || has("freezing") || (has("rain") && has("snow")) {
        kind::RAIN_AND_SNOW
    } else if has("snow") || has("flurr") || has("blizzard") {
        if has("heavy") || has("blizzard") {
            kind::HEAVY_SNOW
        } else {
            kind::LIGHT_SNOW
        }
    } else if has("thunder") || has("storm") || (has("heavy") && has("rain")) {
        kind::HEAVY_RAIN
    } else if has("rain") || has("shower") || has("drizzle") {
        kind::LIGHT_RAIN
    } else if has("partly") || has("fair") || has("mostly sunny") || has("mostly clear") {
        kind::PARTLY_CLOUDY
    } else if has("cloud") || has("overcast") {
        kind::CLOUDY
    } else if has("sun") || has("clear") {
        kind::SUN
    } else {
        kind::GENERIC
    }
}

/// The official app's mapping of The Weather Company's icon codes, which Core's service uses.
fn icon_code(code: i64) -> u8 {
    match code {
        0..=4 | 12 | 37..=40 | 47 => kind::HEAVY_RAIN,
        5..=8 | 10 | 13 | 14 | 18 | 35 | 46 => kind::LIGHT_SNOW,
        9 | 11 | 45 => kind::LIGHT_RAIN,
        15..=17 | 41..=43 => kind::HEAVY_SNOW,
        19..=22 | 26..=28 => kind::CLOUDY,
        29 | 30 => kind::PARTLY_CLOUDY,
        31..=34 | 36 => kind::SUN,
        _ => kind::GENERIC,
    }
}

fn f(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn round16(v: f64) -> i16 {
    v.round().clamp(-999.0, 999.0) as i16
}

fn round8(v: f64) -> i8 {
    v.round().clamp(-127.0, 127.0) as i8
}

async fn get_json(url: &str) -> Result<Value> {
    let response = net::client()
        .get(url)
        .header("Accept", "application/json, application/geo+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("the weather service cannot be reached")?;
    if !response.status().is_success() {
        bail!("the weather service answered {}", response.status());
    }
    response.json().await.context("the weather service's answer cannot be read")
}

async fn open_meteo(p: &Place, imperial: bool) -> Result<Forecast> {
    let (t, w) = if imperial { ("fahrenheit", "mph") } else { ("celsius", "kmh") };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current=temperature_2m,apparent_temperature,weather_code,wind_speed_10m,wind_direction_10m\
         &hourly=temperature_2m,weather_code&daily=weather_code,temperature_2m_max,temperature_2m_min,uv_index_max,precipitation_probability_max\
         &timezone=auto&forecast_days=7&temperature_unit={t}&wind_speed_unit={w}",
        p.latitude, p.longitude
    );
    let v = get_json(&url).await?;
    let cur = &v["current"];
    let (k, phrase) = wmo(cur["weather_code"].as_i64().unwrap_or(-1));
    let daily = &v["daily"];
    let n = daily["time"].as_array().map_or(0, Vec::len);
    let days = (0..n)
        .map(|i| Day {
            high: round16(f(&daily["temperature_2m_max"][i]).unwrap_or(0.0)),
            low: round16(f(&daily["temperature_2m_min"][i]).unwrap_or(0.0)),
            kind: wmo(daily["weather_code"][i].as_i64().unwrap_or(-1)).0,
        })
        .collect();
    let hourly = &v["hourly"];
    let hours = (0..24)
        .map_while(|i| Some((wmo(hourly["weather_code"][i].as_i64()?).0, round8(f(&hourly["temperature_2m"][i])?))))
        .collect::<Vec<_>>();
    Ok(Forecast {
        temp: f(&cur["temperature_2m"]).ok_or_else(|| anyhow!("no current temperature"))?,
        feels_like: f(&cur["apparent_temperature"]),
        kind: k,
        phrase: phrase.to_string(),
        wind_speed: f(&cur["wind_speed_10m"]),
        wind_direction: f(&cur["wind_direction_10m"]),
        uv: f(&daily["uv_index_max"][0]),
        precip_percent: f(&daily["precipitation_probability_max"][0]),
        days,
        hours: if hours.len() == 24 { hours } else { Vec::new() },
        utc_offset_minutes: v["utc_offset_seconds"].as_i64().map(|s| (s / 60) as i32),
    })
}

async fn met_norway(p: &Place, imperial: bool) -> Result<Forecast> {
    // MET asks for at most four decimals.
    let url = format!("https://api.met.no/weatherapi/locationforecast/2.0/compact?lat={:.4}&lon={:.4}", p.latitude, p.longitude);
    let v = get_json(&url).await?;
    let series = v["properties"]["timeseries"].as_array().ok_or_else(|| anyhow!("no forecast"))?;
    let temp = |c: f64| if imperial { c * 9.0 / 5.0 + 32.0 } else { c };
    let speed = |ms: f64| if imperial { ms * 2.236_94 } else { ms * 3.6 };
    let first = series.first().ok_or_else(|| anyhow!("no forecast"))?;
    let now = &first["data"]["instant"]["details"];
    let symbol = |e: &Value| {
        e["data"]["next_1_hours"]["summary"]["symbol_code"]
            .as_str()
            .or_else(|| e["data"]["next_6_hours"]["summary"]["symbol_code"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let current_symbol = symbol(first);
    // Days by the phone's local date.
    let mut days: Vec<(NaiveDate, f64, f64, u8)> = Vec::new();
    for e in series {
        let Some(t) = e["time"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()) else { continue };
        let local = t.with_timezone(&Local);
        let Some(c) = f(&e["data"]["instant"]["details"]["air_temperature"]) else { continue };
        let date = local.date_naive();
        match days.iter_mut().find(|d| d.0 == date) {
            Some(d) => {
                d.1 = d.1.max(c);
                d.2 = d.2.min(c);
                if local.hour() == 12 {
                    d.3 = from_words(&symbol(e));
                }
            }
            None => days.push((date, c, c, from_words(&symbol(e)))),
        }
    }
    let phrase = current_symbol.split('_').next().unwrap_or("").to_string();
    Ok(Forecast {
        temp: temp(f(&now["air_temperature"]).ok_or_else(|| anyhow!("no current temperature"))?),
        feels_like: None,
        kind: from_words(&current_symbol),
        phrase: describe_symbol(&phrase),
        wind_speed: f(&now["wind_speed"]).map(speed),
        wind_direction: f(&now["wind_from_direction"]),
        uv: f(&now["ultraviolet_index_clear_sky"]),
        precip_percent: None,
        days: days.into_iter().take(7).map(|(_, hi, lo, k)| Day { high: round16(temp(hi)), low: round16(temp(lo)), kind: k }).collect(),
        hours: Vec::new(),
        utc_offset_minutes: None,
    })
}

/// "partlycloudy" → "Partly cloudy".
fn describe_symbol(s: &str) -> String {
    let words = [
        ("clearsky", "Clear"),
        ("fair", "Fair"),
        ("partlycloudy", "Partly cloudy"),
        ("cloudy", "Cloudy"),
        ("fog", "Fog"),
        ("lightrainshowers", "Light showers"),
        ("heavyrainshowers", "Heavy showers"),
        ("rainshowers", "Showers"),
        ("lightrain", "Light rain"),
        ("heavyrain", "Heavy rain"),
        ("rain", "Rain"),
        ("lightsnow", "Light snow"),
        ("heavysnow", "Heavy snow"),
        ("snow", "Snow"),
        ("sleet", "Sleet"),
    ];
    words.iter().find(|(k, _)| *k == s).map(|(_, v)| v.to_string()).unwrap_or_else(|| {
        let mut c = s.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    })
}

async fn nws(p: &Place, imperial: bool) -> Result<Forecast> {
    let points = get_json(&format!("https://api.weather.gov/points/{:.4},{:.4}", p.latitude, p.longitude))
        .await
        .context("the National Weather Service covers only the United States")?;
    let props = &points["properties"];
    let daily = get_json(props["forecast"].as_str().ok_or_else(|| anyhow!("no forecast for this place"))?).await?;
    let hourly = get_json(props["forecastHourly"].as_str().ok_or_else(|| anyhow!("no forecast for this place"))?).await?;
    let temp = |v: f64, unit: &str| match (unit, imperial) {
        ("F", false) => (v - 32.0) * 5.0 / 9.0,
        ("C", true) => v * 9.0 / 5.0 + 32.0,
        _ => v,
    };
    let hours = hourly["properties"]["periods"].as_array().ok_or_else(|| anyhow!("no hourly forecast"))?;
    let now = hours.first().ok_or_else(|| anyhow!("no hourly forecast"))?;
    let unit = now["temperatureUnit"].as_str().unwrap_or("F");
    let wind = now["windSpeed"].as_str().and_then(|w| w.split_whitespace().next()?.parse::<f64>().ok());
    let periods = daily["properties"]["periods"].as_array().cloned().unwrap_or_default();
    let mut days: Vec<Day> = Vec::new();
    let mut i = 0;
    while i < periods.len() && days.len() < 7 {
        let pd = &periods[i];
        let t = temp(f(&pd["temperature"]).unwrap_or(0.0), pd["temperatureUnit"].as_str().unwrap_or("F"));
        if pd["isDaytime"].as_bool().unwrap_or(true) {
            let night = periods.get(i + 1).filter(|n| !n["isDaytime"].as_bool().unwrap_or(true));
            let low = night.map_or(t, |n| temp(f(&n["temperature"]).unwrap_or(0.0), n["temperatureUnit"].as_str().unwrap_or("F")));
            days.push(Day { high: round16(t), low: round16(low), kind: from_words(pd["shortForecast"].as_str().unwrap_or("")) });
            i += 2;
        } else {
            // Tonight first: today's high is past.
            days.push(Day { high: round16(t), low: round16(t), kind: from_words(pd["shortForecast"].as_str().unwrap_or("")) });
            i += 1;
        }
    }
    let phrase = now["shortForecast"].as_str().unwrap_or("").to_string();
    let dir = now["windDirection"].as_str().and_then(compass);
    Ok(Forecast {
        temp: temp(f(&now["temperature"]).ok_or_else(|| anyhow!("no current temperature"))?, unit),
        feels_like: None,
        kind: from_words(&phrase),
        phrase,
        wind_speed: wind.map(|mph| if imperial { mph } else { mph * 1.609_34 }),
        wind_direction: dir,
        uv: None,
        precip_percent: f(&now["probabilityOfPrecipitation"]["value"]),
        days,
        hours: Vec::new(),
        utc_offset_minutes: None,
    })
}

fn compass(d: &str) -> Option<f64> {
    const POINTS: [&str; 16] = ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];
    POINTS.iter().position(|p| *p == d).map(|i| i as f64 * 22.5)
}

async fn core(p: &Place, imperial: bool) -> Result<Forecast> {
    let units = if imperial { "e" } else { "m" };
    let v =
        get_json(&format!("https://weather-api.repebble.com/api/v1/geocode/{}/{}?language=en-US&units={units}", p.latitude, p.longitude))
            .await?;
    let obs = &v["conditions"]["data"]["observation"];
    let temps = if imperial { &obs["imperial"] } else { &obs["metric"] };
    let forecasts = v["fcstdaily7"]["data"]["forecasts"].as_array().cloned().unwrap_or_default();
    let days = forecasts
        .iter()
        .map(|d| {
            let code = d["day"]["icon_code"].as_i64().or_else(|| d["night"]["icon_code"].as_i64()).unwrap_or(-1);
            let low = f(&d["min_temp"]).unwrap_or(0.0);
            Day { high: round16(f(&d["max_temp"]).unwrap_or(low)), low: round16(low), kind: icon_code(code) }
        })
        .collect();
    let today = forecasts.first();
    let hours: Vec<(u8, i8)> = v["fcsthourly"]["data"]["hours"]
        .as_array()
        .map(|h| h.iter().take(24).filter_map(|e| Some((icon_code(e["icon_code"].as_i64()?), round8(f(&e["temp"])?)))).collect())
        .unwrap_or_default();
    Ok(Forecast {
        temp: f(&temps["temp"]).ok_or_else(|| anyhow!("no current temperature"))?,
        feels_like: f(&temps["feels_like"]),
        kind: icon_code(obs["icon_code"].as_i64().unwrap_or(-1)),
        phrase: obs["phrase_22char"].as_str().unwrap_or("").to_string(),
        wind_speed: f(&obs["wspd"]),
        wind_direction: f(&obs["wdir"]),
        uv: today.and_then(|t| f(&t["uv_index"])),
        precip_percent: today.and_then(|t| f(&t["pop"])),
        days,
        hours: if hours.len() == 24 { hours } else { Vec::new() },
        utc_offset_minutes: None,
    })
}

pub async fn forecast(provider: Provider, place: &Place, imperial: bool) -> Result<Forecast> {
    match provider {
        Provider::OpenMeteo => open_meteo(place, imperial).await,
        Provider::MetNorway => met_norway(place, imperial).await,
        Provider::NationalWeatherService => nws(place, imperial).await,
        Provider::Core => core(place, imperial).await,
    }
}

/// Places matching a name, through Open-Meteo's geocoding (no account).
pub async fn search(name: String) -> Result<Vec<Place>> {
    let url = reqwest::Url::parse_with_params(
        "https://geocoding-api.open-meteo.com/v1/search",
        &[("name", name.as_str()), ("count", "10"), ("language", "en")],
    )?;
    let v = get_json(url.as_str()).await?;
    Ok(v["results"]
        .as_array()
        .map(|r| {
            r.iter()
                .filter_map(|p| {
                    let parts: Vec<&str> =
                        ["name", "admin1", "country"].iter().filter_map(|k| p[*k].as_str()).filter(|s| !s.is_empty()).collect();
                    Some(Place { name: parts.join(", "), latitude: f(&p["latitude"])?, longitude: f(&p["longitude"])? })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// A position from the location portal.
pub struct Fix {
    pub latitude: f64,
    pub longitude: f64,
    /// Metres.
    pub accuracy: f64,
    pub description: Option<String>,
}

/// The device's location through the location portal: to the street when `exact`, else to the
/// city.
pub async fn locate(exact: bool) -> Result<Fix> {
    use ashpd::desktop::location::{Accuracy, CreateSessionOptions, LocationProxy};
    use futures::StreamExt;
    let proxy = LocationProxy::new().await.context("the location portal is not available")?;
    let accuracy = if exact { Accuracy::Exact } else { Accuracy::City };
    let session = proxy.create_session(CreateSessionOptions::default().set_accuracy(accuracy)).await?;
    let mut updates = proxy.receive_location_updated().await?;
    proxy.start(&session, None, Default::default()).await.context("location was not allowed")?;
    let location = tokio::time::timeout(Duration::from_secs(60), updates.next())
        .await
        .map_err(|_| anyhow!("the location did not arrive"))?
        .ok_or_else(|| anyhow!("the location did not arrive"))?;
    let _ = session.close().await;
    Ok(Fix {
        latitude: location.latitude(),
        longitude: location.longitude(),
        accuracy: location.accuracy(),
        description: location.description().filter(|d| !d.is_empty()).map(str::to_string),
    })
}

/// The device's location, city-accurate.
pub async fn current_location() -> Result<Place> {
    let fix = locate(false).await?;
    Ok(Place { name: fix.description.unwrap_or_else(|| "Current Location".to_string()), latitude: fix.latitude, longitude: fix.longitude })
}

/// The watch's record for a place.
pub fn record(place: &Place, current: bool, f: &Forecast) -> (pebble_protocol::weather::Record, pebble_protocol::weather::Extra) {
    use pebble_protocol::weather::{Extra, Record};
    let today = f.days.first().copied().unwrap_or(Day { high: round16(f.temp), low: round16(f.temp), kind: f.kind });
    let tomorrow = f.days.get(1).copied().unwrap_or(today);
    let record = Record {
        current_temp: round16(f.temp),
        current_kind: f.kind,
        today_high: today.high,
        today_low: today.low,
        tomorrow_kind: tomorrow.kind,
        tomorrow_high: tomorrow.high,
        tomorrow_low: tomorrow.low,
        updated: Utc::now().timestamp() as u32,
        is_current_location: current,
        name: place.name.split(',').next().unwrap_or(&place.name).trim().to_string(),
        phrase: f.phrase.clone(),
    };
    let extra = Extra {
        feels_like: f.feels_like.map(round16),
        uv_x10: f.uv.map(|u| (u * 10.0).round() as i16),
        precip_percent: f.precip_percent.map(|p| p.round() as i16),
        wind_speed: f.wind_speed.map(|w| w.round().max(0.0) as u16),
        wind_direction: f.wind_direction.map(|d| (d.round() as u16) % 360),
        latitude: Some(place.latitude),
        longitude: Some(place.longitude),
        daily: f.days.clone(),
        hourly: f.hours.clone(),
    };
    (record, extra)
}

/// The watch's key for a place: fixed for the current location, else from the place.
pub fn key(place: Option<&Place>) -> uuid::Uuid {
    const NS: uuid::Uuid = uuid::Uuid::from_u128(0x5c0b_8e63_3c43_4c0c_9a8d_2f1e_6d2b_7a10);
    match place {
        None => uuid::Uuid::new_v5(&NS, b"current-location"),
        Some(p) => uuid::Uuid::new_v5(&NS, format!("{:.3},{:.3}", p.latitude, p.longitude).as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions() {
        assert_eq!(wmo(0).0, kind::SUN);
        assert_eq!(wmo(65).0, kind::HEAVY_RAIN);
        assert_eq!(from_words("Chance Light Rain"), kind::LIGHT_RAIN);
        assert_eq!(from_words("heavysnowshowers_day"), kind::HEAVY_SNOW);
        assert_eq!(from_words("Mostly Sunny"), kind::PARTLY_CLOUDY);
        assert_eq!(from_words("sleet"), kind::RAIN_AND_SNOW);
        assert_eq!(icon_code(32), kind::SUN);
        assert_eq!(compass("SE"), Some(135.0));
        assert_eq!(describe_symbol("partlycloudy"), "Partly cloudy");
    }

    /// Asks every provider for real: `cargo test -- --ignored live_weather`.
    #[tokio::test]
    #[ignore]
    async fn live_weather() {
        let p = Place { name: "Bentonville, Arkansas, United States".into(), latitude: 36.37, longitude: -94.21 };
        for provider in Provider::ALL {
            let f = forecast(provider, &p, true).await.unwrap_or_else(|e| panic!("{}: {e:#}", provider.name()));
            assert!(f.days.len() >= 5, "{}: {} days", provider.name(), f.days.len());
            assert!(f.temp > -60.0 && f.temp < 130.0, "{}: {}", provider.name(), f.temp);
            eprintln!("{}: {:.0}° {} kind {} days {} hours {}", provider.name(), f.temp, f.phrase, f.kind, f.days.len(), f.hours.len());
        }
        let found = search("Bentonville".into()).await.unwrap();
        assert!(found.iter().any(|p| p.name.contains("Arkansas")));
    }
}
