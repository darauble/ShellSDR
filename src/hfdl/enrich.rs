//! Enrichment files (seed `spec/05-data/README.md`, group `enrich`): the
//! aircraft, airline and airport CSV files the user names. Columns are
//! found by header name; other columns are ignored; a row without its key
//! is skipped. The files are read on a worker thread (the aircraft file
//! can be 100 MB).

use std::collections::HashMap;
use std::path::Path;

use super::msg::{AcftInfo, Airport};

/// One row of the aircraft file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AircraftRow {
    pub registration: Option<String>,
    pub typecode: Option<String>,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub operator: Option<String>,
    pub owner: Option<String>,
}

/// The loaded files.
#[derive(Debug, Default)]
pub struct Tables {
    /// By ICAO address (upper-case hex).
    pub aircraft: HashMap<String, AircraftRow>,
    /// Registration (upper-case, no dots or dashes) to ICAO address.
    pub by_reg: HashMap<String, String>,
    pub airlines_icao: HashMap<String, String>,
    pub airlines_iata: HashMap<String, String>,
    /// By ICAO code (`ident` and `gps_code`).
    pub airports: HashMap<String, Airport>,
}

fn norm_reg(r: &str) -> String {
    r.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A field's value: trimmed, empty and OpenFlights' `\N` as none.
fn val(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s != "\\N" && s != "-").then(|| s.to_string())
}

impl Tables {
    /// The aircraft known by `icao`, else by registration (`04-support.md`
    /// §4.10), as enrichment with `src: file`.
    pub fn aircraft_info(&self, icao: Option<&str>, reg: Option<&str>) -> Option<AcftInfo> {
        let row = icao
            .and_then(|i| self.aircraft.get(&i.to_ascii_uppercase()))
            .or_else(|| {
                let i = self.by_reg.get(&norm_reg(reg?))?;
                self.aircraft.get(i)
            })?;
        let type_ = match (&row.manufacturer, &row.model) {
            (Some(m), Some(x)) if !x.starts_with(m.as_str()) => Some(format!("{m} {x}")),
            (_, Some(x)) => Some(x.clone()),
            (Some(m), None) => Some(m.clone()),
            _ => None,
        };
        Some(AcftInfo {
            reg: row.registration.clone(),
            icao_type: row.typecode.clone(),
            type_,
            operator: row.operator.clone().or_else(|| row.owner.clone()),
            airline: None,
            src: Some("file".into()),
        })
    }

    /// The airline of a flight ID: its first 3 letters as an ICAO code,
    /// else its first 2 characters as an IATA code.
    pub fn airline(&self, flight: &str) -> Option<String> {
        let f = flight.trim().to_ascii_uppercase();
        let icao: String = f.chars().take(3).collect();
        if icao.len() == 3
            && icao.chars().all(|c| c.is_ascii_alphabetic())
            && let Some(n) = self.airlines_icao.get(&icao)
        {
            return Some(n.clone());
        }
        let iata: String = f.chars().take(2).collect();
        self.airlines_iata.get(&iata).cloned()
    }

    /// The airport of an ICAO code.
    pub fn airport(&self, icao: &str) -> Option<Airport> {
        self.airports
            .get(&icao.trim().to_ascii_uppercase())
            .cloned()
    }
}

/// Every record of a CSV text after the header, as fields, given to `f`
/// with the header's lower-cased names. RFC 4180 quoting; a file whose
/// header starts with `'` quotes with `'` (OpenSky's newer exports).
pub fn each_row(text: &str, mut f: impl FnMut(&HashMap<String, usize>, &[String])) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let q = if text.starts_with('\'') { '\'' } else { '"' };
    let mut header: Option<HashMap<String, usize>> = None;
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut end_row = |row: &mut Vec<String>, field: &mut String| {
        row.push(std::mem::take(field));
        let r = std::mem::take(row);
        if r.len() == 1 && r[0].trim().is_empty() {
            return;
        }
        match &header {
            None => {
                header = Some(
                    r.iter()
                        .enumerate()
                        .map(|(i, n)| (n.trim().to_ascii_lowercase(), i))
                        .collect(),
                );
            }
            Some(h) => f(h, &r),
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\r' if !quoted => {}
            c if c == q && quoted && chars.peek() == Some(&q) => {
                chars.next();
                field.push(q);
            }
            c if c == q => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\n' if !quoted => end_row(&mut row, &mut field),
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        end_row(&mut row, &mut field);
    }
}

fn col(h: &HashMap<String, usize>, r: &[String], name: &str) -> Option<String> {
    h.get(name).and_then(|&i| r.get(i)).and_then(|s| val(s))
}

fn read(path: &Path) -> Result<String, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// Read the aircraft file into `t`; the number of aircraft.
pub fn load_aircraft(t: &mut Tables, text: &str) -> Result<usize, String> {
    let mut ok = false;
    each_row(text, |h, r| {
        if !h.contains_key("icao24") {
            return;
        }
        ok = true;
        let Some(icao) = col(h, r, "icao24").map(|s| s.to_ascii_uppercase()) else {
            return;
        };
        if icao.len() != 6 || !icao.bytes().all(|b| b.is_ascii_hexdigit()) {
            return;
        }
        let row = AircraftRow {
            registration: col(h, r, "registration"),
            typecode: col(h, r, "typecode"),
            model: col(h, r, "model"),
            manufacturer: col(h, r, "manufacturername"),
            operator: col(h, r, "operator"),
            owner: col(h, r, "owner"),
        };
        if row == AircraftRow::default() {
            return;
        }
        if let Some(reg) = &row.registration {
            t.by_reg.insert(norm_reg(reg), icao.clone());
        }
        t.aircraft.insert(icao, row);
    });
    if !ok && t.aircraft.is_empty() {
        return Err("no icao24 column".into());
    }
    Ok(t.aircraft.len())
}

/// Read the airlines file into `t`; the number of airlines. Active
/// airlines win a shared code.
pub fn load_airlines(t: &mut Tables, text: &str) -> Result<usize, String> {
    let mut ok = false;
    let mut active: HashMap<String, bool> = HashMap::new();
    each_row(text, |h, r| {
        if !h.contains_key("icao") || !h.contains_key("name") {
            return;
        }
        ok = true;
        let Some(name) = col(h, r, "name") else {
            return;
        };
        let act = col(h, r, "active").is_none_or(|a| a.eq_ignore_ascii_case("y"));
        let mut put = |map: &mut HashMap<String, String>, code: String, tag: &str| {
            let k = format!("{tag}{code}");
            let was = active.get(&k).copied();
            if was.is_none() || (act && was == Some(false)) {
                map.insert(code, name.clone());
                active.insert(k, act);
            }
        };
        if let Some(c) = col(h, r, "icao").filter(|c| c.len() == 3) {
            put(&mut t.airlines_icao, c.to_ascii_uppercase(), "i");
        }
        if let Some(c) = col(h, r, "iata").filter(|c| c.len() == 2) {
            put(&mut t.airlines_iata, c.to_ascii_uppercase(), "a");
        }
    });
    if !ok {
        return Err("no icao and name columns".into());
    }
    Ok(t.airlines_icao.len())
}

/// Read the airports file into `t`; the number of airports.
pub fn load_airports(t: &mut Tables, text: &str) -> Result<usize, String> {
    let mut ok = false;
    let mut n = 0;
    each_row(text, |h, r| {
        if !h.contains_key("ident") && !h.contains_key("gps_code") {
            return;
        }
        ok = true;
        let num = |k: &str| col(h, r, k).and_then(|s| s.parse::<f64>().ok());
        let keys: Vec<String> = ["gps_code", "ident"]
            .iter()
            .filter_map(|k| col(h, r, k))
            .map(|s| s.to_ascii_uppercase())
            .filter(|s| s.len() == 4 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
            .collect();
        let Some(first) = keys.first().cloned() else {
            return;
        };
        let a = Airport {
            icao: Some(first),
            iata: col(h, r, "iata_code"),
            name: col(h, r, "name"),
            lat: num("latitude_deg"),
            lon: num("longitude_deg"),
        };
        n += 1;
        for k in keys {
            t.airports.entry(k).or_insert_with(|| a.clone());
        }
    });
    if !ok {
        return Err("no ident or gps_code column".into());
    }
    Ok(n)
}

/// The files to load: (aircraft, airlines, airports), each optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Files {
    pub aircraft: Option<String>,
    pub airlines: Option<String>,
    pub airports: Option<String>,
}

/// Load the named files (`~` expanded); the tables and one line per file
/// for the user.
pub fn load(files: &Files) -> (Tables, Vec<String>) {
    let mut t = Tables::default();
    let mut notes = Vec::new();
    type Loader = fn(&mut Tables, &str) -> Result<usize, String>;
    let jobs: [(&Option<String>, &str, &str, Loader); 3] = [
        (&files.aircraft, "aircraft file", "aircraft", load_aircraft),
        (&files.airlines, "airlines file", "airlines", load_airlines),
        (&files.airports, "airports file", "airports", load_airports),
    ];
    for (p, what, unit, f) in jobs {
        let Some(p) = p.as_deref().filter(|p| !p.trim().is_empty()) else {
            continue;
        };
        let path = std::path::PathBuf::from(crate::app::expand_home(p.trim()));
        match read(&path).and_then(|text| f(&mut t, &text)) {
            Ok(n) => notes.push(format!("{what}: {n} {unit}")),
            Err(e) => notes.push(format!("{what} not read: {e}")),
        }
    }
    (t, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aircraft_by_icao_then_registration() {
        let mut t = Tables::default();
        let csv = "icao24,registration,manufacturericao,manufacturername,model,typecode,operator,owner\n\
                   4010ee,G-VRAY,AIRBUS,Airbus,A330-343,A333,Virgin Atlantic,\n\
                   a1b2c3,N873FD,,Boeing,777-FS2,B77L,,Federal Express\n\
                   zzzzzz,BAD,,,,,,\n\
                   ,NOKEY,,,,,,\n";
        assert_eq!(load_aircraft(&mut t, csv), Ok(2));
        let a = t.aircraft_info(Some("4010EE"), None).unwrap();
        assert_eq!(a.reg.as_deref(), Some("G-VRAY"));
        assert_eq!(a.icao_type.as_deref(), Some("A333"));
        assert_eq!(a.type_.as_deref(), Some("Airbus A330-343"));
        assert_eq!(a.operator.as_deref(), Some("Virgin Atlantic"));
        assert_eq!(a.src.as_deref(), Some("file"));
        let b = t.aircraft_info(None, Some(".N873FD")).unwrap();
        assert_eq!(b.operator.as_deref(), Some("Federal Express"));
        assert!(t.aircraft_info(Some("000000"), Some("X")).is_none());
    }

    #[test]
    fn single_quoted_opensky_export() {
        let mut t = Tables::default();
        let csv =
            "'icao24','registration','typecode','model'\n'4b1814','HB-JNA','B77W','777-3DE ER'\n";
        assert_eq!(load_aircraft(&mut t, csv), Ok(1));
        assert_eq!(
            t.aircraft_info(Some("4b1814"), None)
                .unwrap()
                .icao_type
                .as_deref(),
            Some("B77W")
        );
    }

    #[test]
    fn airlines_icao_then_iata_active_first() {
        let mut t = Tables::default();
        let csv = "id,name,alias,iata,icao,callsign,country,active\n\
                   1,Old Etihad,\\N,EY,XXX,,,N\n\
                   2,Etihad Airways,\\N,EY,ETD,ETIHAD,United Arab Emirates,Y\n\
                   3,Emirates,\\N,EK,UAE,EMIRATES,United Arab Emirates,Y\n\
                   4,\"Air \"\"Arabia\"\"\",\\N,G9,ABY,ARABIA,,Y\n";
        assert_eq!(load_airlines(&mut t, csv), Ok(4));
        assert_eq!(t.airline("UAE123").as_deref(), Some("Emirates"));
        assert_eq!(t.airline("EY03XA").as_deref(), Some("Etihad Airways"));
        assert_eq!(t.airline("G90605").as_deref(), Some("Air \"Arabia\""));
        assert_eq!(t.airline("ZZ1"), None);
    }

    #[test]
    fn airports_by_ident_and_gps_code() {
        let mut t = Tables::default();
        let csv = "\u{feff}id,ident,type,name,latitude_deg,longitude_deg,iso_country,municipality,gps_code,iata_code\n\
                   1,EGLL,large_airport,London Heathrow Airport,51.4706,-0.461941,GB,London,EGLL,LHR\n\
                   2,K00A,heliport,Some Heli,40.0,-75.0,US,X,00A,\n\
                   3,US-0001,small_airport,\"Field, North\",10,20,US,,KXYZ,\n";
        assert_eq!(load_airports(&mut t, csv), Ok(3));
        let a = t.airport("egll").unwrap();
        assert_eq!(a.iata.as_deref(), Some("LHR"));
        assert_eq!(a.lat, Some(51.4706));
        assert_eq!(
            t.airport("KXYZ").unwrap().name.as_deref(),
            Some("Field, North")
        );
    }

    #[test]
    fn a_file_without_its_key_column_is_refused() {
        let mut t = Tables::default();
        assert!(load_aircraft(&mut t, "a,b\n1,2\n").is_err());
        assert!(load_airlines(&mut t, "a,b\n1,2\n").is_err());
        assert!(load_airports(&mut t, "a,b\n1,2\n").is_err());
        let (_, notes) = load(&Files {
            aircraft: Some("/nonexistent/aircraft.csv".into()),
            ..Default::default()
        });
        assert!(notes[0].starts_with("aircraft file not read"), "{notes:?}");
    }
}
