//! Online aircraft and route lookups (seed `04-support.md` §4.11, group
//! `lookup`): hexdb.io and adsbdb.com, each switched on separately, on a
//! worker thread at most 1 request per second per service, answers kept in
//! `lookup-cache.ndjson` for display only (never exported, F-H15). With
//! `dsc::lookup`, the only HFDL code that touches the network.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};

use super::msg::{AcftInfo, Airport, RouteInfo};

/// How long one request may take.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Cache ages (s): aircraft and not-found answers 24 h, routes 6 h.
pub const AIRCRAFT_AGE: i64 = 24 * 3600;
pub const ROUTE_AGE: i64 = 6 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Svc {
    Hexdb,
    Adsbdb,
}

impl Svc {
    pub fn name(self) -> &'static str {
        match self {
            Svc::Hexdb => "hexdb",
            Svc::Adsbdb => "adsbdb",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        match s {
            "hexdb" => Some(Svc::Hexdb),
            "adsbdb" => Some(Svc::Adsbdb),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum What {
    Aircraft,
    Route,
}

impl What {
    pub fn name(self) -> &'static str {
        match self {
            What::Aircraft => "aircraft",
            What::Route => "route",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        match s {
            "aircraft" => Some(What::Aircraft),
            "route" => Some(What::Route),
            _ => None,
        }
    }

    /// How long an answer stays fresh (s).
    pub fn age(self, found: bool) -> i64 {
        match (self, found) {
            (What::Route, true) => ROUTE_AGE,
            _ => AIRCRAFT_AGE,
        }
    }
}

/// The request URL: `{icao}` or `{callsign}` replaced.
pub fn url(template: &str, key: &str) -> String {
    let k: String = key
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    template.replace("{icao}", &k).replace("{callsign}", &k)
}

fn s(v: &Value, k: &str) -> Option<String> {
    v[k].as_str()
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v[k].as_f64()
        .or_else(|| v[k].as_str().and_then(|x| x.trim().parse().ok()))
}

/// An aircraft answer, None when it holds no aircraft.
pub fn parse_aircraft(svc: Svc, body: &str) -> Option<AcftInfo> {
    let v: Value = serde_json::from_str(body).ok()?;
    let (a, type_k, op_k) = match svc {
        Svc::Hexdb => (&v, "icao_type_code", "operator_flag_code"),
        Svc::Adsbdb => (
            &v["response"]["aircraft"],
            "icao_type",
            "registered_owner_operator_flag_code",
        ),
    };
    if !a.is_object() {
        return None;
    }
    // hexdb's v1 answer capitalizes its names
    let man = s(a, "manufacturer").or_else(|| s(a, "Manufacturer"));
    let type_ = match (man, s(a, "type").or_else(|| s(a, "Type"))) {
        (Some(m), Some(t)) if !t.starts_with(&m) => Some(format!("{m} {t}")),
        (_, Some(t)) => Some(t),
        (m, None) => m,
    };
    let info = AcftInfo {
        reg: s(a, "registration").or_else(|| s(a, "Registration")),
        icao_type: s(a, type_k).or_else(|| s(a, "ICAOTypeCode")),
        type_,
        operator: s(a, "registered_owner")
            .or_else(|| s(a, "RegisteredOwners"))
            .or_else(|| s(a, op_k)),
        airline: None,
        src: Some(svc.name().into()),
    };
    (info.reg.is_some() || info.icao_type.is_some() || info.type_.is_some()).then_some(info)
}

/// A route answer, None when it holds no route.
pub fn parse_route(svc: Svc, body: &str) -> Option<RouteInfo> {
    let v: Value = serde_json::from_str(body).ok()?;
    let airport = |a: &Value, icao: &str, iata: &str| -> Option<Airport> {
        if !a.is_object() {
            // hexdb's older answer: "route": "EGLL-OMDB"
            return None;
        }
        let ap = Airport {
            icao: s(a, icao),
            iata: s(a, iata),
            name: s(a, "name"),
            lat: num(a, "latitude"),
            lon: num(a, "longitude"),
        };
        (ap.icao.is_some() || ap.iata.is_some()).then_some(ap)
    };
    let (orig, dest) = match svc {
        Svc::Hexdb => {
            let (o, d) = (
                airport(&v["origin"], "icao", "iata"),
                airport(&v["destination"], "icao", "iata"),
            );
            if o.is_none() && d.is_none() {
                // `{"flight":"UAE1","route":"OMDB-EGLL"}`
                let r = s(&v, "route")?;
                let mut it = r.split('-').map(|c| Airport {
                    icao: Some(c.trim().to_string()),
                    ..Default::default()
                });
                (it.next(), it.next_back())
            } else {
                (o, d)
            }
        }
        Svc::Adsbdb => {
            let r = &v["response"]["flightroute"];
            (
                airport(&r["origin"], "icao_code", "iata_code"),
                airport(&r["destination"], "icao_code", "iata_code"),
            )
        }
    };
    (orig.is_some() || dest.is_some()).then(|| RouteInfo {
        src: svc.name().into(),
        orig,
        dest,
    })
}

/// One cached answer.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheEntry {
    pub t: i64,
    pub found: bool,
    pub data: Value,
}

type Key = (Svc, What, String);

/// The answers kept (`lookup-cache.ndjson`; the last line for a key wins).
#[derive(Debug, Default)]
pub struct Cache {
    path: Option<PathBuf>,
    map: HashMap<Key, CacheEntry>,
}

impl Cache {
    /// The cache of `path` (missing or bad lines skipped).
    pub fn open(path: PathBuf) -> Self {
        let mut map = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let (Some(svc), Some(what), Some(key), Some(t)) = (
                    v["svc"].as_str().and_then(Svc::from_name),
                    v["what"].as_str().and_then(What::from_name),
                    v["key"].as_str(),
                    v["t"].as_str().and_then(crate::dsc::time::parse_iso_utc),
                ) else {
                    continue;
                };
                map.insert(
                    (svc, what, key.to_string()),
                    CacheEntry {
                        t,
                        found: v["found"].as_bool().unwrap_or(false),
                        data: v["data"].clone(),
                    },
                );
            }
        }
        Self {
            path: Some(path),
            map,
        }
    }

    /// An answer younger than its age at `now`.
    pub fn fresh(&self, svc: Svc, what: What, key: &str, now: i64) -> Option<&CacheEntry> {
        self.map
            .get(&(svc, what, key.to_string()))
            .filter(|e| now - e.t < what.age(e.found))
    }

    /// Whether any of `svcs` answered `key` recently (found or not).
    pub fn answered(&self, svcs: &[Svc], what: What, key: &str, now: i64) -> bool {
        svcs.iter()
            .any(|&s| self.fresh(s, what, key, now).is_some())
    }

    /// The first of `svcs` (in order) with a found answer, at any age.
    pub fn found(&self, svcs: &[Svc], what: What, key: &str) -> Option<&Value> {
        svcs.iter().find_map(|&s| {
            self.map
                .get(&(s, what, key.to_string()))
                .filter(|e| e.found)
                .map(|e| &e.data)
        })
    }

    /// Keep an answer, appending it to the file.
    pub fn put(&mut self, svc: Svc, what: What, key: &str, t: i64, data: Option<Value>) {
        let e = CacheEntry {
            t,
            found: data.is_some(),
            data: data.unwrap_or(Value::Null),
        };
        if let Some(p) = &self.path {
            let mut line = json!({
                "svc": svc.name(),
                "what": what.name(),
                "key": key,
                "t": crate::dsc::time::iso_utc(t),
                "found": e.found,
            });
            if e.found {
                line["data"] = e.data.clone();
            }
            let _ = append(p, &line.to_string());
        }
        self.map.insert((svc, what, key.to_string()), e);
    }
}

fn append(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(format!("{line}\n").as_bytes())
}

/// What one service answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The data under `record.schema.json`'s `acft` / `route` names.
    Found(Value),
    NotFound,
    Failed(String),
}

/// A lookup to do: the services in order with their URL templates.
#[derive(Debug, Clone)]
pub struct Job {
    pub what: What,
    pub key: String,
    pub svcs: Vec<(Svc, String)>,
}

/// A finished lookup: each asked service's answer, in order.
#[derive(Debug, Clone)]
pub struct Done {
    pub what: What,
    pub key: String,
    pub answers: Vec<(Svc, Answer)>,
}

/// An HTTP GET: (status, body).
pub type Get = fn(&str) -> Result<(u16, String), String>;

/// The real GET (ureq, `TIMEOUT`).
pub fn http_get(u: &str) -> Result<(u16, String), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent("shellsdr")
        .build()
        .into();
    let mut resp = agent.get(u).call().map_err(|e| e.to_string())?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((resp.status().as_u16(), body))
}

/// Ask one service.
pub fn ask(get: Get, svc: Svc, what: What, template: &str, key: &str) -> Answer {
    match get(&url(template, key)) {
        Err(e) => Answer::Failed(format!("{}: no answer ({e})", svc.name())),
        Ok((404, _)) => Answer::NotFound,
        Ok((st, body)) if (200..300).contains(&st) => {
            let data = match what {
                What::Aircraft => parse_aircraft(svc, &body).map(|a| a.to_json()),
                What::Route => parse_route(svc, &body).map(|r| r.to_json(None)),
            };
            data.map_or(Answer::NotFound, Answer::Found)
        }
        Ok((st, _)) => Answer::Failed(format!("{}: HTTP {st}", svc.name())),
    }
}

/// The worker: jobs in, answers out, 1 request per second per service.
pub struct Worker {
    jobs: Sender<Job>,
    pub done: Receiver<Done>,
}

impl Worker {
    pub fn start(get: Get) -> Self {
        let (jobs, job_rx) = unbounded::<Job>();
        let (done_tx, done) = unbounded::<Done>();
        let _ = std::thread::Builder::new()
            .name("hfdl-lookup".into())
            .spawn(move || {
                let mut last: HashMap<Svc, Instant> = HashMap::new();
                for job in job_rx {
                    let mut answers = Vec::new();
                    for (svc, template) in &job.svcs {
                        if let Some(t) = last.get(svc) {
                            let wait = Duration::from_secs(1).saturating_sub(t.elapsed());
                            std::thread::sleep(wait);
                        }
                        last.insert(*svc, Instant::now());
                        let a = ask(get, *svc, job.what, template, &job.key);
                        let found = matches!(a, Answer::Found(_));
                        answers.push((*svc, a));
                        if found {
                            break;
                        }
                    }
                    let d = Done {
                        what: job.what,
                        key: job.key,
                        answers,
                    };
                    if done_tx.send(d).is_err() {
                        break;
                    }
                }
            });
        Self { jobs, done }
    }

    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEXDB_AC: &str = r#"{"ModeS":"4010EE","Registration":"G-VRAY","Manufacturer":"Airbus","ICAOTypeCode":"A333","Type":"A330-343","RegisteredOwners":"Virgin Atlantic Airways","OperatorFlagCode":"VIR"}"#;
    const HEXDB_AC_V2: &str = r#"{"registration":"G-VRAY","icao_type_code":"A333","type":"A330-343","manufacturer":"Airbus","registered_owner":"Virgin Atlantic Airways","operator_flag_code":"VIR"}"#;
    const ADSBDB_AC: &str = r#"{"response":{"aircraft":{"type":"A330 343","icao_type":"A333","manufacturer":"Airbus","mode_s":"4010EE","registration":"G-VRAY","registered_owner_country_iso_name":"GB","registered_owner":"Virgin Atlantic Airways","registered_owner_operator_flag_code":"VIR"}}}"#;
    const ADSBDB_ROUTE: &str = r#"{"response":{"flightroute":{"callsign":"VIR3N","origin":{"iata_code":"LHR","icao_code":"EGLL","latitude":51.4706,"longitude":-0.461941,"name":"London Heathrow Airport"},"destination":{"iata_code":"JFK","icao_code":"KJFK","latitude":40.639801,"longitude":-73.7789,"name":"John F Kennedy International Airport"}}}}"#;
    const HEXDB_ROUTE: &str = r#"{"flight":"VIR3N","route":"EGLL-KJFK","updatetime":1700000000}"#;

    #[test]
    fn urls_fill_their_key() {
        assert_eq!(
            url("https://hexdb.io/api/v2/aircraft/{icao}", "4010EE"),
            "https://hexdb.io/api/v2/aircraft/4010EE"
        );
        assert_eq!(url("https://x/{callsign}", "A B"), "https://x/A%20B");
    }

    #[test]
    fn aircraft_answers_of_both_services() {
        for (svc, body) in [
            (Svc::Hexdb, HEXDB_AC),
            (Svc::Hexdb, HEXDB_AC_V2),
            (Svc::Adsbdb, ADSBDB_AC),
        ] {
            let a = parse_aircraft(svc, body).unwrap();
            assert_eq!(a.reg.as_deref(), Some("G-VRAY"), "{svc:?}");
            assert_eq!(a.icao_type.as_deref(), Some("A333"));
            assert_eq!(a.operator.as_deref(), Some("Virgin Atlantic Airways"));
            assert_eq!(a.src.as_deref(), Some(svc.name()));
            assert!(a.type_.unwrap().starts_with("Airbus A330"));
        }
        assert!(parse_aircraft(Svc::Adsbdb, r#"{"response":"unknown aircraft"}"#).is_none());
        assert!(parse_aircraft(Svc::Hexdb, "not json").is_none());
    }

    #[test]
    fn route_answers_of_both_services() {
        let r = parse_route(Svc::Adsbdb, ADSBDB_ROUTE).unwrap();
        assert_eq!(r.orig.as_ref().unwrap().icao.as_deref(), Some("EGLL"));
        assert_eq!(r.dest.as_ref().unwrap().iata.as_deref(), Some("JFK"));
        assert_eq!(r.dest.as_ref().unwrap().lat, Some(40.639801));
        let h = parse_route(Svc::Hexdb, HEXDB_ROUTE).unwrap();
        assert_eq!(h.orig.unwrap().icao.as_deref(), Some("EGLL"));
        assert_eq!(h.dest.unwrap().icao.as_deref(), Some("KJFK"));
        assert!(parse_route(Svc::Adsbdb, r#"{"response":"unknown callsign"}"#).is_none());
    }

    fn fake(u: &str) -> Result<(u16, String), String> {
        if u.contains("down") {
            Err("connection refused".into())
        } else if u.contains("hexdb") && u.ends_with("4010EE") {
            Ok((404, "{}".into()))
        } else if u.contains("adsbdb") && u.ends_with("4010EE") {
            Ok((200, ADSBDB_AC.into()))
        } else if u.ends_with("500") {
            Ok((500, "oops".into()))
        } else {
            Ok((404, "".into()))
        }
    }

    #[test]
    fn answers_of_a_request() {
        let t = "https://hexdb/{icao}";
        assert_eq!(
            ask(fake, Svc::Hexdb, What::Aircraft, t, "4010EE"),
            Answer::NotFound
        );
        assert!(matches!(
            ask(
                fake,
                Svc::Adsbdb,
                What::Aircraft,
                "https://adsbdb/{icao}",
                "4010EE"
            ),
            Answer::Found(_)
        ));
        assert!(matches!(
            ask(fake, Svc::Hexdb, What::Aircraft, "https://down/{icao}", "1"),
            Answer::Failed(e) if e.contains("no answer")
        ));
        assert!(matches!(
            ask(fake, Svc::Hexdb, What::Aircraft, t, "500"),
            Answer::Failed(e) if e.contains("HTTP 500")
        ));
    }

    #[test]
    fn the_worker_tries_the_next_service_after_not_found() {
        let w = Worker::start(fake);
        w.send(Job {
            what: What::Aircraft,
            key: "4010EE".into(),
            svcs: vec![
                (Svc::Hexdb, "https://hexdb/{icao}".into()),
                (Svc::Adsbdb, "https://adsbdb/{icao}".into()),
            ],
        });
        let d = w.done.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(d.answers.len(), 2);
        assert_eq!(d.answers[0], (Svc::Hexdb, Answer::NotFound));
        assert!(matches!(&d.answers[1], (Svc::Adsbdb, Answer::Found(v)) if v["reg"] == "G-VRAY"));
    }

    #[test]
    fn the_cache_keeps_answers_with_their_ages() {
        let dir = std::env::temp_dir().join(format!("shellsdr-lkp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("lookup-cache.ndjson");
        let t0 = 1_795_600_000;
        {
            let mut c = Cache::open(p.clone());
            c.put(
                Svc::Hexdb,
                What::Aircraft,
                "4010EE",
                t0,
                Some(json!({"reg": "G-VRAY"})),
            );
            c.put(
                Svc::Hexdb,
                What::Route,
                "VIR3N",
                t0,
                Some(json!({"src": "hexdb"})),
            );
            c.put(Svc::Adsbdb, What::Aircraft, "000001", t0, None);
        }
        let c = Cache::open(p);
        assert!(
            c.fresh(Svc::Hexdb, What::Aircraft, "4010EE", t0 + 23 * 3600)
                .is_some()
        );
        assert!(
            c.fresh(Svc::Hexdb, What::Aircraft, "4010EE", t0 + 25 * 3600)
                .is_none()
        );
        assert!(
            c.fresh(Svc::Hexdb, What::Route, "VIR3N", t0 + 5 * 3600)
                .is_some()
        );
        assert!(
            c.fresh(Svc::Hexdb, What::Route, "VIR3N", t0 + 7 * 3600)
                .is_none()
        );
        // not found is cached for 24 h
        assert!(c.answered(
            &[Svc::Hexdb, Svc::Adsbdb],
            What::Aircraft,
            "000001",
            t0 + 3600
        ));
        assert!(c.found(&[Svc::Adsbdb], What::Aircraft, "000001").is_none());
        // a stale answer still shows
        assert_eq!(
            c.found(&[Svc::Adsbdb, Svc::Hexdb], What::Aircraft, "4010EE")
                .unwrap()["reg"],
            "G-VRAY"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
