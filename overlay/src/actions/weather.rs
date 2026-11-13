//! `weather`: Open-Meteo (no key) through curl: the place from the geocoding API, then the
//! forecast; both bounded (8 s each, 512 KiB). The card shows the current temperature and
//! conditions, today's high and low, and the next hours, in the overlay's language.

use serde_json::Value;

pub const GEOCODE: &str = "https://geocoding-api.open-meteo.com/v1/search";
pub const FORECAST: &str = "https://api.open-meteo.com/v1/forecast";
pub const BODY_MAX: usize = 512 * 1024;
pub const HOURS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    /// Region and country, for the card's second line.
    pub region: String,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hour {
    /// "14:00"
    pub time: String,
    pub temperature: f64,
    pub code: i64,
    pub day: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    pub place: Place,
    pub temperature: f64,
    pub feels_like: Option<f64>,
    pub code: i64,
    pub day: bool,
    pub wind_kmh: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub rain_chance: Option<i64>,
    pub hours: Vec<Hour>,
}

/// Percent-encoding for a query value.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn geocode_url(location: &str, language: &str) -> String {
    format!("{GEOCODE}?name={}&count=1&language={}&format=json", encode(location.trim()), encode(language))
}

pub fn forecast_url(p: &Place) -> String {
    format!(
        "{FORECAST}?latitude={:.4}&longitude={:.4}&current=temperature_2m,apparent_temperature,weather_code,wind_speed_10m,is_day\
         &hourly=temperature_2m,weather_code,is_day&daily=temperature_2m_max,temperature_2m_min,precipitation_probability_max\
         &timezone=auto&forecast_days=2",
        p.latitude, p.longitude
    )
}

fn short(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

pub fn parse_place(json: &[u8]) -> Option<Place> {
    let v: Value = serde_json::from_slice(json).ok()?;
    let r = v["results"].as_array()?.first()?;
    let latitude = r["latitude"].as_f64().filter(|l| (-90.0..=90.0).contains(l))?;
    let longitude = r["longitude"].as_f64().filter(|l| (-180.0..=180.0).contains(l))?;
    // The country is enough to tell places apart on a small card.
    let region = r["country"].as_str().filter(|c| Some(*c) != r["name"].as_str()).map(|c| short(c, 60)).unwrap_or_default();
    Some(Place { name: short(r["name"].as_str()?, 80), region, latitude, longitude })
}

pub fn parse_forecast(json: &[u8], place: Place) -> Option<Weather> {
    let v: Value = serde_json::from_slice(json).ok()?;
    let c = &v["current"];
    let temperature = c["temperature_2m"].as_f64()?;
    let code = c["weather_code"].as_i64().unwrap_or(-1);
    let day = c["is_day"].as_i64().is_none_or(|d| d != 0);
    let first = |k: &str| v["daily"][k].as_array().and_then(|a| a.first()).and_then(Value::as_f64);
    // The next hours: from the current hour on.
    let now = c["time"].as_str().unwrap_or("");
    let hour_now = now.get(..13).unwrap_or(""); // "2026-11-13T14"
    let times = v["hourly"]["time"].as_array().cloned().unwrap_or_default();
    let temps = v["hourly"]["temperature_2m"].as_array().cloned().unwrap_or_default();
    let codes = v["hourly"]["weather_code"].as_array().cloned().unwrap_or_default();
    let days = v["hourly"]["is_day"].as_array().cloned().unwrap_or_default();
    let start = times.iter().position(|t| t.as_str().is_some_and(|t| t.get(..13).unwrap_or("") > hour_now)).unwrap_or(times.len());
    let mut hours = vec![];
    for i in start..times.len() {
        if hours.len() == HOURS {
            break;
        }
        let (Some(t), Some(temp)) = (times[i].as_str(), temps.get(i).and_then(Value::as_f64)) else { continue };
        hours.push(Hour {
            time: t.get(11..16).unwrap_or(t).to_string(),
            temperature: temp,
            code: codes.get(i).and_then(Value::as_i64).unwrap_or(-1),
            day: days.get(i).and_then(Value::as_i64).is_none_or(|d| d != 0),
        });
    }
    Some(Weather {
        place,
        temperature,
        feels_like: c["apparent_temperature"].as_f64(),
        code,
        day,
        wind_kmh: c["wind_speed_10m"].as_f64(),
        high: first("temperature_2m_max"),
        low: first("temperature_2m_min"),
        rain_chance: v["daily"]["precipitation_probability_max"].as_array().and_then(|a| a.first()).and_then(Value::as_i64),
        hours,
    })
}

/// WMO weather code → the words shown (a translation key).
pub fn condition(code: i64) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 | 66 | 67 => "Freezing rain",
        61 | 63 | 65 => "Rain",
        71 | 73 | 75 | 77 => "Snow",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95 | 96 | 99 => "Thunderstorm",
        _ => "Unknown weather",
    }
}

/// WMO weather code → a symbolic icon name (Adwaita/Breeze/COSMIC all have these).
pub fn icon(code: i64, day: bool) -> &'static str {
    match code {
        0 | 1 if day => "weather-clear-symbolic",
        0 | 1 => "weather-clear-night-symbolic",
        2 if day => "weather-few-clouds-symbolic",
        2 => "weather-few-clouds-night-symbolic",
        3 => "weather-overcast-symbolic",
        45 | 48 => "weather-fog-symbolic",
        51..=67 | 80..=82 => "weather-showers-symbolic",
        71..=77 | 85 | 86 => "weather-snow-symbolic",
        95..=99 => "weather-storm-symbolic",
        _ => "weather-severe-alert-symbolic",
    }
}

pub fn degrees(t: f64) -> String {
    let r = t.round();
    // no "-0"
    format!("{} °C", if r == 0.0 { 0.0 } else { r })
}

/// A one-line summary in English, kept with the exchange so a follow-up ("and tomorrow?")
/// has the numbers.
pub fn summary(w: &Weather) -> String {
    let mut s = format!("Weather in {}: {}, {}", w.place.name, degrees(w.temperature), condition(w.code).to_lowercase());
    if let (Some(h), Some(l)) = (w.high, w.low) {
        s.push_str(&format!("; today {} to {}", degrees(l), degrees(h)));
    }
    if let Some(r) = w.rain_chance {
        s.push_str(&format!(", rain chance {r} %"));
    }
    if !w.hours.is_empty() {
        let next: Vec<String> = w.hours.iter().map(|h| format!("{} {}", h.time, degrees(h.temperature))).collect();
        s.push_str(&format!("; next hours: {}", next.join(", ")));
    }
    s
}

/// A place when none was said: the `weather_location` setting, else the city of the system's
/// time zone (`/etc/localtime` → `Europe/Berlin` → "Berlin").
pub fn default_location(setting: &str, localtime_target: Option<&str>) -> Option<String> {
    if !setting.trim().is_empty() {
        return Some(setting.trim().to_string());
    }
    let target = localtime_target?;
    let zone = target.split("zoneinfo/").nth(1)?;
    let city = zone.rsplit('/').next()?;
    if city.is_empty() || !zone.contains('/') || zone.starts_with("Etc/") {
        return None;
    }
    Some(city.replace('_', " "))
}

/// curl, no shell: https only, bounded in time and size.
pub fn curl_argv(url: &str) -> Vec<String> {
    ["curl", "-fsS", "--proto", "=https", "--max-time", "8", "--max-filesize", &BODY_MAX.to_string(), "-A", "maprisma-dictation", url].iter().map(|s| s.to_string()).collect()
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let argv = curl_argv(url);
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut out = vec![];
    child.stdout.take().expect("piped").take(BODY_MAX as u64 + 1).read_to_end(&mut out).map_err(|e| e.to_string())?;
    let mut err = String::new();
    let _ = child.stderr.take().expect("piped").take(4096).read_to_string(&mut err);
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() || out.len() > BODY_MAX {
        return Err(err.lines().last().unwrap_or("curl failed").trim().chars().take(200).collect());
    }
    Ok(out)
}

pub enum LookupError {
    NoPlace(String),
    NoLocation,
    Network(String),
}

/// The whole lookup (blocking; up to about 16 s).
pub fn lookup(location: &str, language: &str, setting: &str) -> Result<Weather, LookupError> {
    let location = if location.trim().is_empty() {
        let target = std::fs::read_link("/etc/localtime").ok().map(|p| p.to_string_lossy().into_owned());
        default_location(setting, target.as_deref()).ok_or(LookupError::NoLocation)?
    } else {
        location.trim().to_string()
    };
    let places = fetch(&geocode_url(&location, language)).map_err(LookupError::Network)?;
    let place = parse_place(&places).ok_or_else(|| LookupError::NoPlace(location.clone()))?;
    let forecast = fetch(&forecast_url(&place)).map_err(LookupError::Network)?;
    parse_forecast(&forecast, place).ok_or_else(|| LookupError::Network("unexpected forecast".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLACES: &str = r#"{"results":[{"id":2950159,"name":"Berlin","latitude":52.52437,"longitude":13.41053,"elevation":74.0,"feature_code":"PPLC","country_code":"DE","admin1":"Berlin","timezone":"Europe/Berlin","population":3426354,"country":"Deutschland"}],"generationtime_ms":0.5}"#;

    fn forecast() -> String {
        let times: Vec<String> = (0..48).map(|h| format!("\"2026-11-{:02}T{:02}:00\"", 13 + h / 24, h % 24)).collect();
        let temps: Vec<String> = (0..48).map(|h| format!("{:.1}", 5.0 + (h % 24) as f64 * 0.25)).collect();
        let codes: Vec<String> = (0..48).map(|h| if h % 24 < 16 { "3".into() } else { "61".into() }).collect();
        let days: Vec<String> = (0..48).map(|h| if (7..17).contains(&(h % 24)) { "1".into() } else { "0".into() }).collect();
        format!(
            r#"{{"latitude":52.52,"longitude":13.419998,"timezone":"Europe/Berlin","current_units":{{"temperature_2m":"°C"}},
            "current":{{"time":"2026-11-13T14:15","interval":900,"temperature_2m":8.4,"apparent_temperature":5.1,"weather_code":3,"wind_speed_10m":14.2,"is_day":1}},
            "hourly":{{"time":[{}],"temperature_2m":[{}],"weather_code":[{}],"is_day":[{}]}},
            "daily":{{"time":["2026-11-13","2026-11-14"],"temperature_2m_max":[10.2,9.0],"temperature_2m_min":[3.6,2.1],"precipitation_probability_max":[45,80]}}}}"#,
            times.join(","),
            temps.join(","),
            codes.join(","),
            days.join(",")
        )
    }

    #[test]
    fn parsing_open_meteo() {
        let place = parse_place(PLACES.as_bytes()).unwrap();
        assert_eq!(place.name, "Berlin");
        assert_eq!(place.region, "Deutschland");
        assert!((place.latitude - 52.524).abs() < 0.01);
        assert!(forecast_url(&place).contains("latitude=52.5244&longitude=13.4105"));
        let w = parse_forecast(forecast().as_bytes(), place).unwrap();
        assert_eq!(w.temperature, 8.4);
        assert_eq!(w.code, 3);
        assert_eq!(condition(w.code), "Overcast");
        assert_eq!((w.high, w.low, w.rain_chance), (Some(10.2), Some(3.6), Some(45)));
        assert_eq!(w.hours.len(), HOURS);
        assert_eq!(w.hours[0].time, "15:00");
        assert_eq!(w.hours[1].code, 61);
        assert!(!w.hours[2].day);
        let s = summary(&w);
        assert!(s.starts_with("Weather in Berlin: 8 °C, overcast; today 4 °C to 10 °C, rain chance 45 %; next hours: 15:00 9 °C"), "{s}");
        // nothing found, broken JSON
        assert_eq!(parse_place(br#"{"generationtime_ms":0.3}"#), None);
        assert_eq!(parse_place(b"<html>"), None);
        assert!(parse_forecast(b"{}", parse_place(PLACES.as_bytes()).unwrap()).is_none());
    }

    #[test]
    fn urls_words_and_defaults() {
        assert_eq!(geocode_url("São Paulo", "pt"), "https://geocoding-api.open-meteo.com/v1/search?name=S%C3%A3o%20Paulo&count=1&language=pt&format=json");
        assert_eq!(geocode_url("a&b=c", "de"), "https://geocoding-api.open-meteo.com/v1/search?name=a%26b%3Dc&count=1&language=de&format=json");
        assert_eq!(condition(95), "Thunderstorm");
        assert_eq!(condition(1234), "Unknown weather");
        assert_eq!(icon(0, false), "weather-clear-night-symbolic");
        assert_eq!(icon(63, true), "weather-showers-symbolic");
        assert_eq!(degrees(-0.3), "0 °C");
        assert_eq!(degrees(-2.6), "-3 °C");
        assert_eq!(default_location(" Hamburg ", None).as_deref(), Some("Hamburg"));
        assert_eq!(default_location("", Some("/usr/share/zoneinfo/Europe/Berlin")).as_deref(), Some("Berlin"));
        assert_eq!(default_location("", Some("../usr/share/zoneinfo/America/New_York")).as_deref(), Some("New York"));
        assert_eq!(default_location("", Some("/usr/share/zoneinfo/UTC")), None);
        assert_eq!(default_location("", Some("/usr/share/zoneinfo/Etc/GMT+1")), None);
        assert_eq!(default_location("", None), None);
        let argv = curl_argv("https://x");
        assert!(argv.windows(2).any(|w| w == ["--proto", "=https"]) && argv.last().unwrap() == "https://x");
    }
}
