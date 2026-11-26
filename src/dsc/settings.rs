//! Settings (seed spec/05-data/config.schema.json): load with defaults,
//! save by three-way merge, atomically (CFG-01..04).

use serde_json::{Value, json};
use std::path::Path;

/// The user's location, merged as one unit (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Location {
    pub set: bool,
    pub lat: f64,
    pub lon: f64,
}

/// A staged live channel as persisted (CH-05).
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelConf {
    pub label: String,
    pub device: String,
    /// Centre tone, Hz.
    pub tone: f64,
}

/// The default lookup endpoint: the aprs.fi API (LKP-06).
pub const DEFAULT_ENDPOINT: &str =
    "https://api.aprs.fi/api/get?name={mmsi}&what=loc&apikey={apikey}&format=json";

/// The persisted settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Message directory; "" = the default per-user data directory.
    pub ndjson_dir: String,
    /// Last directory a message text was saved to.
    pub save_dir: String,
    pub default_tone: f64,
    pub location: Location,
    pub online_lookup: bool,
    pub lookup_endpoint: String,
    pub lookup_apikey: String,
    pub alert_bell: bool,
    pub channels: Vec<ChannelConf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            ndjson_dir: String::new(),
            save_dir: String::new(),
            default_tone: 800.0,
            location: Location::default(),
            online_lookup: false,
            lookup_endpoint: DEFAULT_ENDPOINT.into(),
            lookup_apikey: String::new(),
            alert_bell: true,
            channels: Vec::new(),
        }
    }
}

/// Three-way merge (04-support.md §4.8): where `mine` equals `base` I did not
/// change it, so the disk's value wins; where I changed it, mine wins.
pub fn merge(base: &Settings, mine: &Settings, disk: &Settings) -> Settings {
    fn pick<T: PartialEq + Clone>(base: &T, mine: &T, disk: &T) -> T {
        if mine == base {
            disk.clone()
        } else {
            mine.clone()
        }
    }
    Settings {
        ndjson_dir: pick(&base.ndjson_dir, &mine.ndjson_dir, &disk.ndjson_dir),
        save_dir: pick(&base.save_dir, &mine.save_dir, &disk.save_dir),
        default_tone: pick(&base.default_tone, &mine.default_tone, &disk.default_tone),
        location: pick(&base.location, &mine.location, &disk.location),
        online_lookup: pick(
            &base.online_lookup,
            &mine.online_lookup,
            &disk.online_lookup,
        ),
        lookup_endpoint: pick(
            &base.lookup_endpoint,
            &mine.lookup_endpoint,
            &disk.lookup_endpoint,
        ),
        lookup_apikey: pick(
            &base.lookup_apikey,
            &mine.lookup_apikey,
            &disk.lookup_apikey,
        ),
        alert_bell: pick(&base.alert_bell, &mine.alert_bell, &disk.alert_bell),
        channels: pick(&base.channels, &mine.channels, &disk.channels),
    }
}

/// Reads settings from JSON text. Never fails (CFG-04): unreadable text
/// gives the defaults; a missing key or one of the wrong type gives that
/// key's default; unknown keys are ignored.
pub fn from_json(text: &str) -> Settings {
    let d = Settings::default();
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(text) else {
        return d;
    };
    let text = |k: &str, def: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map_or_else(|| def.to_string(), String::from)
    };
    let flag = |k: &str, def: bool| match o.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) if n.as_i64() == Some(0) => false,
        Some(Value::Number(n)) if n.as_i64() == Some(1) => true,
        _ => def,
    };
    let num = |k: &str, ok: &dyn Fn(f64) -> bool, def: f64| {
        o.get(k)
            .and_then(Value::as_f64)
            .filter(|v| ok(*v))
            .unwrap_or(def)
    };
    let default_tone = num("default_tone", &|t| t > 10.0, d.default_tone);
    let channels = o
        .get("channels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|c| {
                    let label = c.get("channel_label").and_then(Value::as_str).unwrap_or("");
                    let device = c.get("device").and_then(Value::as_str).unwrap_or("");
                    if label.is_empty() && device.is_empty() {
                        return None;
                    }
                    let tone = c
                        .get("tone")
                        .and_then(Value::as_f64)
                        .filter(|t| *t > 10.0)
                        .unwrap_or(default_tone);
                    Some(ChannelConf {
                        label: label.into(),
                        device: device.into(),
                        tone,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Settings {
        ndjson_dir: text("ndjson_dir", &d.ndjson_dir),
        save_dir: text("save_dir", &d.save_dir),
        default_tone,
        location: Location {
            set: flag("has_location", false),
            lat: num("user_lat", &|v| (-90.0..=90.0).contains(&v), 0.0),
            lon: num("user_lon", &|v| (-180.0..=180.0).contains(&v), 0.0),
        },
        online_lookup: flag("online_lookup", d.online_lookup),
        lookup_endpoint: text("lookup_endpoint", &d.lookup_endpoint),
        lookup_apikey: clean_key(&text("lookup_apikey", "")),
        alert_bell: flag("alert_bell", d.alert_bell),
        channels,
    }
}

/// An API key without surrounding whitespace and control characters.
pub fn clean_key(key: &str) -> String {
    key.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

impl Settings {
    /// The endpoint to use: the setting, or the default when empty.
    pub fn endpoint(&self) -> &str {
        if self.lookup_endpoint.trim().is_empty() {
            DEFAULT_ENDPOINT
        } else {
            &self.lookup_endpoint
        }
    }
}

/// The settings as JSON text (full precision, booleans as true/false).
pub fn to_json(s: &Settings) -> String {
    let channels: Vec<Value> = s
        .channels
        .iter()
        .map(|c| json!({"channel_label": c.label, "device": c.device, "tone": c.tone}))
        .collect();
    let v = json!({
        "ndjson_dir": s.ndjson_dir,
        "save_dir": s.save_dir,
        "default_tone": s.default_tone,
        "has_location": s.location.set,
        "user_lat": s.location.lat,
        "user_lon": s.location.lon,
        "online_lookup": s.online_lookup,
        "lookup_endpoint": s.lookup_endpoint,
        "lookup_apikey": s.lookup_apikey,
        "alert_bell": s.alert_bell,
        "channels": channels,
    });
    serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
}

/// Loads a settings file; a missing or unreadable file gives the defaults.
pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .map(|t| from_json(&t))
        .unwrap_or_default()
}

/// Saves `mine` by the three-way merge with `base` and the file's current
/// content, atomically (CFG-02, CFG-03). Returns what was written, which
/// is the new `base`.
pub fn save(path: &Path, base: &Settings, mine: &Settings) -> Result<Settings, String> {
    let merged = merge(base, mine, &load(path));
    let dir = path.parent().ok_or("settings path has no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".config.json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, to_json(&merged)).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("dsqrust-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // SC-CFG-04, row by row.
    #[test]
    fn unreadable_json_gives_defaults() {
        assert_eq!(from_json("{ not json"), Settings::default());
        assert_eq!(from_json("[1,2]"), Settings::default());
        assert_eq!(
            load(std::path::Path::new("/nonexistent/dsqrust/config.json")),
            Settings::default()
        );
    }

    #[test]
    fn zero_and_one_are_accepted_as_booleans() {
        let s =
            from_json(r#"{"alert_bell": 0, "has_location": 1, "user_lat": 1.25, "user_lon": 2.5}"#);
        assert!(!s.alert_bell);
        assert_eq!(
            s.location,
            Location {
                set: true,
                lat: 1.25,
                lon: 2.5
            }
        );
    }

    #[test]
    fn a_key_inside_a_string_is_not_a_key() {
        let s = from_json(r#"{"save_dir": "x \"alert_bell\": 0", "alert_bell": true}"#);
        assert!(s.alert_bell);
        assert_eq!(s.save_dir, "x \"alert_bell\": 0");
    }

    #[test]
    fn a_wrong_type_gives_that_keys_default() {
        let s = from_json(r#"{"user_lat": "bad", "default_tone": 812.5}"#);
        assert_eq!(s.location.lat, 0.0);
        assert_eq!(s.default_tone, 812.5);
    }

    #[test]
    fn channel_items_that_are_not_objects_are_skipped() {
        let s = from_json(
            r#"{"channels": [{"channel_label": "a", "device": "d", "tone": 1234.5}, 7, {}]}"#,
        );
        assert_eq!(
            s.channels,
            vec![ChannelConf {
                label: "a".into(),
                device: "d".into(),
                tone: 1234.5
            }]
        );
    }

    #[test]
    fn a_channel_without_a_tone_gets_the_default_tone() {
        let s = from_json(
            r#"{"default_tone": 1700, "channels": [{"channel_label": "a", "device": "d"}]}"#,
        );
        assert_eq!(s.channels[0].tone, 1700.0);
    }

    #[test]
    fn the_api_key_loses_whitespace_and_control_characters() {
        assert_eq!(
            from_json("{\"lookup_apikey\": \" ab\\u0007c\\n \"}").lookup_apikey,
            "abc"
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn saving_and_loading_again_gives_the_same_values_exactly() {
        let mut s = Settings::default();
        s.ndjson_dir = "/tmp/x \"y\"".into();
        s.default_tone = 812.123456789012;
        s.location = Location {
            set: true,
            lat: 54.687157,
            lon: -25.279652000000001,
        };
        s.online_lookup = true;
        s.lookup_apikey = "k".into();
        s.alert_bell = false;
        s.channels = vec![ChannelConf {
            label: "8414.5 kHz".into(),
            device: "a.monitor".into(),
            tone: 800.5,
        }];
        assert_eq!(from_json(&to_json(&s)), s);
    }

    #[test]
    fn save_merges_with_what_another_copy_saved_and_writes_atomically() {
        let d = temp_dir("merge");
        let path = d.join("dsqrust/config.json");
        let base = Settings::default();
        // Another copy changed the tone on disk.
        let mut theirs = base.clone();
        theirs.default_tone = 1700.0;
        save(&path, &base, &theirs).unwrap();
        // This copy changed only the bell.
        let mut mine = base.clone();
        mine.alert_bell = false;
        let written = save(&path, &base, &mine).unwrap();
        assert_eq!(written.default_tone, 1700.0);
        assert!(!written.alert_bell);
        assert_eq!(load(&path), written);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file is left");
        std::fs::remove_dir_all(d).unwrap();
    }
}
