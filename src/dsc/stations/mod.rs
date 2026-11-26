//! Stations: MID -> country, built-in coast stations, station CSVs and the
//! position cache (seed spec/05-data "Station CSVs", ENR-03..06).

use std::sync::OnceLock;

/// A built-in coast station (source `builtin`).
#[derive(Debug, Clone, PartialEq)]
pub struct Builtin {
    pub mmsi: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// The built-in coast station of an identity, if the table has it.
///
/// The table (`data/builtin_stations.csv`) is a representative subset, as
/// spec/05-data allows; it holds the stations the seed's vectors name.
pub fn builtin(mmsi: &str) -> Option<&'static Builtin> {
    builtin_table().iter().find(|b| b.mmsi == mmsi)
}

fn builtin_table() -> &'static [Builtin] {
    static TABLE: OnceLock<Vec<Builtin>> = OnceLock::new();
    TABLE.get_or_init(|| {
        crate::dsc::csv::parse(include_str!("../data/builtin_stations.csv"))
            .into_iter()
            .skip(1)
            .filter_map(|row| {
                Some(Builtin {
                    mmsi: row.first()?.clone(),
                    name: row.get(1)?.clone(),
                    lat: row.get(5)?.parse().ok()?,
                    lon: row.get(6)?.parse().ok()?,
                })
            })
            .collect()
    })
}

use crate::dsc::record::{PosSource, StationInfo, StationPos};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What a station file says about one identity.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: Option<String>,
    pub callsign: Option<String>,
    pub country: Option<String>,
    pub kind: Option<String>,
    pub position: Option<StationPos>,
}

/// Station data: built-in coast stations, the user's CSVs and the
/// position cache. Not locked itself; the session keeps it under its own
/// lock (02-architecture.md rule 3).
#[derive(Debug, Default)]
pub struct Stations {
    entries: HashMap<String, Entry>,
    cache: Option<PathBuf>,
}

/// The files loaded from `<message dir>/stations/`, in order.
pub const FILES: [&str; 4] = [
    "ships.csv",
    "shore_stations.csv",
    "stations.csv",
    "positions_cache.csv",
];

impl Stations {
    /// Built-in data only; nothing is cached to disk.
    pub fn new() -> Stations {
        Stations::default()
    }

    /// Loads `<dir>/stations/*.csv`; positions learned later are appended
    /// to `<dir>/stations/positions_cache.csv`.
    pub fn load_dir(dir: &Path) -> Stations {
        let mut s = Stations::new();
        for f in FILES {
            if let Ok(text) = std::fs::read_to_string(dir.join("stations").join(f)) {
                s.load_csv(&text);
            }
        }
        s.cache = Some(dir.join("stations").join("positions_cache.csv"));
        s
    }

    /// Adds the rows of one CSV text; later rows replace earlier ones.
    pub fn load_csv(&mut self, text: &str) {
        let mut rows = crate::dsc::csv::parse(text).into_iter();
        let Some(header) = rows.next() else { return };
        let columns = Columns::from_header(&header);
        for row in rows {
            let field = |i: Option<usize>| {
                i.and_then(|i| row.get(i))
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                    .map(String::from)
            };
            let Some(mmsi) = field(columns.mmsi) else {
                continue;
            };
            let num = |i| {
                field(i)
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
            };
            let position = match (num(columns.lat), num(columns.lon)) {
                (Some(lat), Some(lon))
                    if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                {
                    Some(StationPos {
                        lat,
                        lon,
                        source: match field(columns.source).as_deref() {
                            Some("builtin") => PosSource::Builtin,
                            Some("dsc") => PosSource::Dsc,
                            Some("aprs.fi") => PosSource::AprsFi,
                            _ => PosSource::Csv,
                        },
                        time: field(columns.time)
                            .and_then(|t| t.parse::<i64>().ok())
                            .filter(|&t| t != 0),
                    })
                }
                _ => None,
            };
            let mut e = Entry {
                name: field(columns.name),
                callsign: field(columns.callsign),
                country: field(columns.country),
                kind: field(columns.kind),
                position,
            };
            if e.position.is_none()
                && let Some(old) = self.entries.get(&mmsi)
            {
                e.position = old.position;
                if old.name.is_some() {
                    e.name = old.name.clone();
                }
            }
            self.entries.insert(mmsi, e);
        }
    }

    /// What is known about an identity now, with the distance from `user`.
    pub fn info(&self, mmsi: &str, user: Option<(f64, f64)>) -> StationInfo {
        if mmsi.is_empty() {
            return StationInfo::default();
        }
        let entry = self.entries.get(mmsi);
        let built = builtin(mmsi);
        let name = entry
            .and_then(|e| e.name.clone())
            .or_else(|| built.map(|b| b.name.clone()));
        let country = entry
            .and_then(|e| e.country.clone())
            .or_else(|| mid_country(mmsi));
        let position = entry.and_then(|e| e.position).or_else(|| {
            built.map(|b| StationPos {
                lat: b.lat,
                lon: b.lon,
                source: PosSource::Builtin,
                time: None,
            })
        });
        let km = match (user, position) {
            (Some((ulat, ulon)), Some(p)) => {
                Some(crate::dsc::geo::distance_km(ulat, ulon, p.lat, p.lon))
            }
            _ => None,
        };
        StationInfo {
            name,
            country,
            position,
            km,
        }
    }

    pub fn entry(&self, mmsi: &str) -> Option<&Entry> {
        self.entries.get(mmsi)
    }

    /// Records an observed position (from DSC or a lookup) and appends it
    /// to the position cache (ENR-04, ENR-06). Coordinates keep 6 decimal
    /// places, as in the file, so a reload gives the same.
    pub fn observe(
        &mut self,
        mmsi: &str,
        name: Option<&str>,
        pos: StationPos,
    ) -> Result<(), String> {
        let clean = |v: &str| v.replace([',', '\n', '\r'], " ");
        let round6 = |v: f64| format!("{v:.6}").parse::<f64>().unwrap_or(v);
        let pos = StationPos {
            lat: round6(pos.lat),
            lon: round6(pos.lon),
            ..pos
        };
        let e = self.entries.entry(mmsi.to_string()).or_default();
        e.position = Some(pos);
        if let Some(n) = name.map(clean).filter(|n| !n.trim().is_empty()) {
            e.name = Some(n);
        }
        let Some(path) = &self.cache else {
            return Ok(());
        };
        let country = e
            .country
            .clone()
            .or_else(|| mid_country(mmsi))
            .unwrap_or_default();
        let line = format!(
            "{},{},,{},,{:.6},{:.6},{},{}\n",
            clean(mmsi),
            name.map(clean).unwrap_or_default(),
            clean(&country),
            pos.lat,
            pos.lon,
            pos.source.name(),
            pos.time.unwrap_or(0)
        );
        append_line(path, &line)
    }

    /// Whether the identity's position is fresh for a message at `utc`
    /// (LKP-02): from a user CSV, or observed within 1 h.
    pub fn is_fresh(&self, mmsi: &str, utc: i64) -> bool {
        match self.entries.get(mmsi).and_then(|e| e.position) {
            Some(p) if p.source == PosSource::Csv => true,
            Some(StationPos { time: Some(t), .. }) => (utc - t).abs() <= 3600,
            _ => false,
        }
    }
}

const CACHE_HEADER: &str = "mmsi,name,callsign,country,type,lat,lon,pos_source,pos_time\n";

/// Appends a line under a lock; a new file gets the header first.
fn append_line(path: &Path, line: &str) -> Result<(), String> {
    use std::io::Write;
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(err)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(err)?;
    f.lock().map_err(err)?;
    let new = f.metadata().map(|m| m.len() == 0).unwrap_or(false);
    let text = if new {
        format!("{CACHE_HEADER}{line}")
    } else {
        line.to_string()
    };
    let r = f.write_all(text.as_bytes());
    let _ = f.unlock();
    r.map_err(err)
}

fn mid_country(mmsi: &str) -> Option<String> {
    crate::dsc::domain::identity::classify(mmsi)
        .mid
        .and_then(crate::dsc::domain::mid::administrations)
}

/// Where each known column is in a CSV.
#[derive(Default)]
struct Columns {
    mmsi: Option<usize>,
    name: Option<usize>,
    callsign: Option<usize>,
    country: Option<usize>,
    kind: Option<usize>,
    lat: Option<usize>,
    lon: Option<usize>,
    source: Option<usize>,
    time: Option<usize>,
}

impl Columns {
    fn from_header(header: &[String]) -> Columns {
        let mut c = Columns::default();
        let mut known = false;
        for (i, h) in header.iter().enumerate() {
            let slot = match h.trim().to_ascii_lowercase().as_str() {
                "mmsi" => &mut c.mmsi,
                "name" | "shipname" => &mut c.name,
                "callsign" | "call" => &mut c.callsign,
                "country" | "flag" => &mut c.country,
                "type" | "shiptype" => &mut c.kind,
                "lat" | "latitude" => &mut c.lat,
                "lon" | "lng" | "longitude" => &mut c.lon,
                "pos_source" | "source" => &mut c.source,
                "pos_time" => &mut c.time,
                _ => continue,
            };
            known = true;
            slot.get_or_insert(i);
        }
        if known {
            c
        } else {
            // The order of the position cache.
            Columns {
                mmsi: Some(0),
                name: Some(1),
                callsign: Some(2),
                country: Some(3),
                kind: Some(4),
                lat: Some(5),
                lon: Some(6),
                source: Some(7),
                time: Some(8),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("dsqrust-stations-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("stations")).unwrap();
        d
    }

    fn pos(s: &Stations, mmsi: &str) -> Option<(f64, f64, PosSource, Option<i64>)> {
        s.info(mmsi, None)
            .position
            .map(|p| (p.lat, p.lon, p.source, p.time))
    }

    #[test]
    fn sc_enr_06_cache_reload_last_line_wins() {
        let d = temp_dir("enr06");
        std::fs::write(
            d.join("stations/positions_cache.csv"),
            "mmsi,name,callsign,country,type,lat,lon,pos_source,pos_time\n\
             258999119,,,Norway,,60.000000,5.000000,dsc,1792498000\n\
             258999119,,,Norway,,61.000000,6.000000,dsc,1792498971\n",
        )
        .unwrap();
        let s = Stations::load_dir(&d);
        assert_eq!(
            pos(&s, "258999119"),
            Some((61.0, 6.0, PosSource::Dsc, Some(1792498971)))
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn headers_are_case_insensitive_with_aliases_in_any_order() {
        let mut s = Stations::new();
        s.load_csv("LNG,Latitude,MMSI,ShipName,Flag\n25.5,54.25,477231700,TEST VESSEL,Hong Kong\n");
        let i = s.info("477231700", None);
        assert_eq!(i.name.as_deref(), Some("TEST VESSEL"));
        assert_eq!(i.country.as_deref(), Some("Hong Kong"));
        assert_eq!(
            pos(&s, "477231700"),
            Some((54.25, 25.5, PosSource::Csv, None))
        );
    }

    #[test]
    fn without_known_column_names_columns_go_by_position() {
        let mut s = Stations::new();
        s.load_csv("a,b,c,d,e,f,g,h,i\n477231700,X,,,,1.5,2.5,dsc,100\n");
        assert_eq!(
            pos(&s, "477231700"),
            Some((1.5, 2.5, PosSource::Dsc, Some(100)))
        );
        assert_eq!(s.info("477231700", None).name.as_deref(), Some("X"));
    }

    #[test]
    fn a_row_without_a_position_keeps_the_position_and_name_known_before() {
        let mut s = Stations::new();
        s.load_csv("mmsi,name,lat,lon\n477231700,OLD,1,2\n");
        s.load_csv("mmsi,name,callsign\n477231700,NEW,VRAB2\n,NOBODY,X\n");
        let e = s.entry("477231700").unwrap();
        assert_eq!(e.name.as_deref(), Some("OLD"));
        assert_eq!(e.callsign.as_deref(), Some("VRAB2"));
        assert_eq!(pos(&s, "477231700").map(|p| (p.0, p.1)), Some((1.0, 2.0)));
    }

    #[test]
    fn country_comes_from_the_mid_and_builtin_coast_stations_are_known() {
        let s = Stations::new();
        assert_eq!(s.info("258999119", None).country.as_deref(), Some("Norway"));
        let coast = s.info("002570000", Some((58.66, 6.66)));
        assert_eq!(coast.name.as_deref(), Some("Rogaland Radio"));
        assert_eq!(
            pos(&s, "002570000"),
            Some((58.66, 5.66, PosSource::Builtin, None))
        );
        assert!(coast.km.unwrap() > 50.0 && coast.km.unwrap() < 60.0);
        assert_eq!(s.info("", None), StationInfo::default());
    }

    #[test]
    fn observed_positions_are_appended_to_the_cache_with_a_header() {
        let d = temp_dir("observe");
        let mut s = Stations::load_dir(&d);
        let p = StationPos {
            lat: 62.4666666666,
            lon: 6.2166666666,
            source: PosSource::Dsc,
            time: Some(1792498971),
        };
        s.observe("258999119", None, p).unwrap();
        let q = StationPos {
            lat: 1.0,
            lon: 2.0,
            source: PosSource::AprsFi,
            time: Some(5),
        };
        s.observe("477231700", Some("A, B\nC"), q).unwrap();
        let text = std::fs::read_to_string(d.join("stations/positions_cache.csv")).unwrap();
        assert_eq!(
            text,
            "mmsi,name,callsign,country,type,lat,lon,pos_source,pos_time\n\
             258999119,,,Norway,,62.466667,6.216667,dsc,1792498971\n\
             477231700,A  B C,,China (People's Republic of) - Hong Kong (Special Administrative Region of China),,1.000000,2.000000,aprs.fi,5\n"
        );
        assert_eq!(pos(&s, "258999119").map(|p| p.2), Some(PosSource::Dsc));
        assert_eq!(s.info("477231700", None).name.as_deref(), Some("A  B C"));
        // Reloading gives the same.
        assert_eq!(
            pos(&Stations::load_dir(&d), "258999119"),
            Some((62.466667, 6.216667, PosSource::Dsc, Some(1792498971)))
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn fresh_means_a_user_csv_or_observed_within_an_hour() {
        let mut s = Stations::new();
        s.load_csv("mmsi,lat,lon\n111111111,1,2\n");
        s.load_csv("mmsi,lat,lon,pos_source,pos_time\n222222222,1,2,dsc,10000\n");
        assert!(s.is_fresh("111111111", 99_999_999));
        assert!(s.is_fresh("222222222", 10000 + 600));
        assert!(!s.is_fresh("222222222", 10000 + 3601));
        assert!(!s.is_fresh("333333333", 10000));
    }
}
