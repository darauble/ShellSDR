//! The HFDL message service (0.4.5, HFDL seed `02-architecture.md`): the
//! session's parser, the messages of the day in view, the day files, the
//! channel counters, the aircraft and ground-station views, alerts, search
//! and filters, and the detail text. The UI (`hfdl_app`, `ui::hfdl_view`)
//! drives it; frames come from the HFDL VFO's processor.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::Value;

use super::lookup::{self, Svc, What};
use super::msg::{
    self, AircraftTable, AlertKind, AlertMemory, AlertSettings, Keywords, Msg, Severity,
};
use super::{enrich, pdu, phy, rx::ChannelStatus, stations};
use crate::dsc::record::TimeSource;
use crate::dsc::time::{self, TimeZone};

/// The project name in saved file names.
pub const PROJECT: &str = "shellsdr-hfdl";
/// The built-in ground-station table's version.
pub const SYSTABLE_BUILT_IN: u32 = 52;

type Settings = serde_json::Map<String, Value>;

/// The settings of the Work Area's menus, kept in the VFO's `HFDL` table
/// next to the schema's fields (`config.schema.json`'s `hfdl` keys).
#[derive(Debug, Clone, PartialEq)]
pub struct HfdlPrefs {
    pub alert_sound: bool,
    pub alert_on_caution: bool,
    pub watch_alerts: bool,
    pub notifications: bool,
    pub watch: Vec<String>,
    pub keywords: Keywords,
    pub filter_link: bool,
    pub filter_joined: bool,
    /// The enrichment files (`~` allowed; empty = none).
    pub aircraft_file: String,
    pub airlines_file: String,
    pub airports_file: String,
    /// Online lookups, each off by default (`LKP-01`).
    pub lookup_hexdb: bool,
    pub lookup_adsbdb: bool,
    pub hexdb_aircraft_url: String,
    pub hexdb_route_url: String,
    pub adsbdb_aircraft_url: String,
    pub adsbdb_route_url: String,
}

/// The lookup endpoints' defaults (`config.schema.json`).
pub const HEXDB_AIRCRAFT_URL: &str = "https://hexdb.io/api/v2/aircraft/{icao}";
pub const HEXDB_ROUTE_URL: &str = "https://hexdb.io/api/v2/routes/{callsign}";
pub const ADSBDB_AIRCRAFT_URL: &str = "https://api.adsbdb.com/v0/aircraft/{icao}";
pub const ADSBDB_ROUTE_URL: &str = "https://api.adsbdb.com/v0/callsign/{callsign}";

impl Default for HfdlPrefs {
    fn default() -> Self {
        Self {
            alert_sound: true,
            alert_on_caution: true,
            watch_alerts: true,
            notifications: true,
            watch: Vec::new(),
            keywords: Keywords::default(),
            filter_link: true,
            filter_joined: true,
            aircraft_file: String::new(),
            airlines_file: String::new(),
            airports_file: String::new(),
            lookup_hexdb: false,
            lookup_adsbdb: false,
            hexdb_aircraft_url: HEXDB_AIRCRAFT_URL.into(),
            hexdb_route_url: HEXDB_ROUTE_URL.into(),
            adsbdb_aircraft_url: ADSBDB_AIRCRAFT_URL.into(),
            adsbdb_route_url: ADSBDB_ROUTE_URL.into(),
        }
    }
}

fn str_list(v: Option<&Value>) -> Option<Vec<String>> {
    Some(
        v?.as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

impl HfdlPrefs {
    /// From the VFO's settings table (missing or bad keys = defaults).
    pub fn from_settings(s: &Settings) -> Self {
        let d = Self::default();
        let b = |k: &str, def: bool| s.get(k).and_then(Value::as_bool).unwrap_or(def);
        let st = |k: &str, def: &str| {
            s.get(k)
                .and_then(Value::as_str)
                .map_or(def.to_string(), |v| v.trim().to_string())
        };
        // an endpoint must be http(s) and carry its placeholder
        let url = |k: &str, def: &str, ph: &str| {
            s.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| v.starts_with("http") && v.contains(ph))
                .unwrap_or(def)
                .to_string()
        };
        Self {
            alert_sound: b("alert_sound", d.alert_sound),
            alert_on_caution: b("alert_on_caution", d.alert_on_caution),
            watch_alerts: b("watch_alerts", d.watch_alerts),
            notifications: b("notifications", d.notifications),
            watch: str_list(s.get("watch")).unwrap_or_default(),
            keywords: Keywords {
                emergency: str_list(s.get("keywords_emergency")).unwrap_or(d.keywords.emergency),
                caution: str_list(s.get("keywords_caution")).unwrap_or(d.keywords.caution),
            },
            filter_link: b("filter_link", d.filter_link),
            filter_joined: b("filter_joined", d.filter_joined),
            aircraft_file: st("aircraft_file", ""),
            airlines_file: st("airlines_file", ""),
            airports_file: st("airports_file", ""),
            lookup_hexdb: b("lookup_hexdb", false),
            lookup_adsbdb: b("lookup_adsbdb", false),
            hexdb_aircraft_url: url("hexdb_aircraft_url", HEXDB_AIRCRAFT_URL, "{icao}"),
            hexdb_route_url: url("hexdb_route_url", HEXDB_ROUTE_URL, "{callsign}"),
            adsbdb_aircraft_url: url("adsbdb_aircraft_url", ADSBDB_AIRCRAFT_URL, "{icao}"),
            adsbdb_route_url: url("adsbdb_route_url", ADSBDB_ROUTE_URL, "{callsign}"),
        }
    }

    /// Into the VFO's settings table (other keys kept).
    pub fn write_to(&self, s: &mut Settings) {
        s.insert("alert_sound".into(), self.alert_sound.into());
        s.insert("alert_on_caution".into(), self.alert_on_caution.into());
        s.insert("watch_alerts".into(), self.watch_alerts.into());
        s.insert("notifications".into(), self.notifications.into());
        s.insert("watch".into(), self.watch.clone().into());
        s.insert(
            "keywords_emergency".into(),
            self.keywords.emergency.clone().into(),
        );
        s.insert(
            "keywords_caution".into(),
            self.keywords.caution.clone().into(),
        );
        s.insert("filter_link".into(), self.filter_link.into());
        s.insert("filter_joined".into(), self.filter_joined.into());
        s.insert("aircraft_file".into(), self.aircraft_file.clone().into());
        s.insert("airlines_file".into(), self.airlines_file.clone().into());
        s.insert("airports_file".into(), self.airports_file.clone().into());
        s.insert("lookup_hexdb".into(), self.lookup_hexdb.into());
        s.insert("lookup_adsbdb".into(), self.lookup_adsbdb.into());
        for (k, v) in [
            ("hexdb_aircraft_url", &self.hexdb_aircraft_url),
            ("hexdb_route_url", &self.hexdb_route_url),
            ("adsbdb_aircraft_url", &self.adsbdb_aircraft_url),
            ("adsbdb_route_url", &self.adsbdb_route_url),
        ] {
            s.insert(k.into(), v.clone().into());
        }
    }

    /// The enrichment files named.
    pub fn files(&self) -> enrich::Files {
        let o = |s: &String| Some(s.clone()).filter(|s| !s.trim().is_empty());
        enrich::Files {
            aircraft: o(&self.aircraft_file),
            airlines: o(&self.airlines_file),
            airports: o(&self.airports_file),
        }
    }

    /// The services switched on, in their order (hexdb first), with the
    /// endpoint of `what`.
    pub fn services(&self, what: What) -> Vec<(Svc, String)> {
        let mut v = Vec::new();
        if self.lookup_hexdb {
            v.push((
                Svc::Hexdb,
                match what {
                    What::Aircraft => self.hexdb_aircraft_url.clone(),
                    What::Route => self.hexdb_route_url.clone(),
                },
            ));
        }
        if self.lookup_adsbdb {
            v.push((
                Svc::Adsbdb,
                match what {
                    What::Aircraft => self.adsbdb_aircraft_url.clone(),
                    What::Route => self.adsbdb_route_url.clone(),
                },
            ));
        }
        v
    }

    fn alert_settings(&self) -> AlertSettings {
        AlertSettings {
            alert_on_caution: self.alert_on_caution,
            watch_alerts: self.watch_alerts,
            watch: self.watch.clone(),
        }
    }
}

/// A frame as the service receives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reception {
    pub freq_khz: u32,
    pub m1: u8,
    pub octets: Vec<u8>,
    /// UTC seconds of the frame's start.
    pub utc: f64,
    pub time_source: TimeSource,
    pub level_db: f32,
    pub noise_db: f32,
    pub ferr_hz: f32,
    /// The source label (`HFDL 8.9 MHz`).
    pub src: String,
    /// The recording it was played from, if any.
    pub file: Option<String>,
}

/// A channel's state (`06-behaviour.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    Silent,
    Activity,
    Decoding,
}

impl ChannelState {
    pub fn name(self) -> &'static str {
        match self {
            ChannelState::Silent => "silent",
            ChannelState::Activity => "activity",
            ChannelState::Decoding => "decoding",
        }
    }
}

/// One channel's counters (`03-parser.md` §3.5) and live state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChannelInfo {
    pub status: Option<ChannelStatus>,
    pub frames_ok: u32,
    pub frames_err: u32,
    pub messages: u32,
    /// UTC of the last frame with a good FCS, and of the last one at all.
    pub last_ok: Option<f64>,
    pub last_frame: Option<f64>,
}

impl ChannelInfo {
    /// The state at `now` (UTC s).
    pub fn state(&self, now: f64) -> ChannelState {
        if self.status.is_some_and(|s| s.decoding) || self.last_ok.is_some_and(|t| now - t < 3.0) {
            ChannelState::Decoding
        } else if self.last_frame.is_some_and(|t| now - t < 32.0) {
            ChannelState::Activity
        } else {
            ChannelState::Silent
        }
    }
}

/// One message in view.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub seq: u64,
    pub msg: Msg,
    terms: Vec<String>,
    /// A later block completed its message (the "hide joined" filter).
    pub joined_later: bool,
    /// The message is a watched aircraft's.
    pub watched: bool,
    pub subject: String,
}

/// The view's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Messages,
    Aircraft,
    Stations,
}

impl Tab {
    pub fn next(self) -> Self {
        match self {
            Tab::Messages => Tab::Aircraft,
            Tab::Aircraft => Tab::Stations,
            Tab::Stations => Tab::Messages,
        }
    }
}

/// What the Report takes (spec 20.3, 27): accepted since it last took
/// them.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Reported {
    /// Messages by severity (`Severity as usize`: link, routine, caution,
    /// emergency), whatever their day.
    pub by_severity: [u64; 4],
    /// Frames that failed their FCS.
    pub bad_frames: u64,
    /// Alerts raised: their kind and a line for the log.
    pub alerts: Vec<(AlertKind, String)>,
    /// Aircraft heard (downlinks): ICAO address, else registration.
    pub aircraft: std::collections::BTreeSet<String>,
    /// Ground stations heard (uplinks and squitters): their ID.
    pub stations: std::collections::BTreeSet<u8>,
}

/// The active alert (`ALR-02`).
#[derive(Debug, Clone, PartialEq)]
pub struct AlertInfo {
    pub seq: u64,
    pub kind: AlertKind,
    pub summary: String,
}

/// Side effects for the UI.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Effects {
    pub bell: bool,
}

enum WriteJob {
    Line(PathBuf, String),
    Barrier(Sender<()>),
}

/// A ground station as heard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Heard {
    pub last_utc: f64,
    pub last_freq_khz: u32,
    /// Frequencies (kHz) heard in its squitters.
    pub freqs: Vec<u32>,
}

/// The service.
pub struct HfdlService {
    dir: PathBuf,
    tz: TimeZone,
    prefs: HfdlPrefs,
    location: Option<(f64, f64)>,
    parser: pdu::Parser,
    entries: Vec<Entry>,
    /// Indices of `entries` that pass the search and filters.
    rows: Vec<usize>,
    selected: Option<usize>,
    follow: bool,
    query: String,
    alerts_only: bool,
    pub tab: Tab,
    aircraft: AircraftTable,
    heard: HashMap<u8, Heard>,
    channels: BTreeMap<u32, ChannelInfo>,
    alert_mem: AlertMemory,
    /// The reception clock (UTC s): the recording's time during playback.
    clock: f64,
    alert: Option<AlertInfo>,
    alerts_count: u32,
    reported: Reported,
    systable_on_air: Option<u32>,
    next_seq: u64,
    day: String,
    offset: u64,
    own_lines: HashMap<String, usize>,
    writer: Sender<WriteJob>,
    write_errors: Receiver<String>,
    notices: VecDeque<String>,
    effects: Effects,
    notifier: fn(&str, &str, bool),
    /// The enrichment files, and the worker loading them.
    tables: enrich::Tables,
    tables_rx: Option<Receiver<(enrich::Tables, Vec<String>)>>,
    /// The online lookups: cache, worker, keys asked, records waiting.
    cache: lookup::Cache,
    lookup_get: lookup::Get,
    worker: Option<lookup::Worker>,
    inflight: HashSet<(What, String)>,
    pending: Vec<Pending>,
}

/// A record waiting for its lookups before it is written (`LKP-03`).
struct Pending {
    seq: u64,
    msg: Msg,
    waits: Vec<(What, String)>,
    failure: Option<String>,
}

fn desktop_notify(summary: &str, body: &str, critical: bool) {
    let _ = crate::dsc::notify::desktop(summary, body, critical);
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(format!("{line}\n").as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

impl HfdlService {
    /// The service for `dir`, viewing the local day of `now`.
    pub fn new(
        dir: PathBuf,
        tz: TimeZone,
        prefs: HfdlPrefs,
        location: Option<(f64, f64)>,
        now: i64,
    ) -> Self {
        let (writer, jobs) = unbounded::<WriteJob>();
        let (err_tx, write_errors) = unbounded::<String>();
        let _ = std::thread::Builder::new()
            .name("hfdl-writer".into())
            .spawn(move || {
                for job in jobs {
                    match job {
                        WriteJob::Line(path, line) => {
                            if let Err(e) = append_line(&path, &line) {
                                let _ = err_tx.send(format!("cannot write the message: {e}"));
                            }
                        }
                        WriteJob::Barrier(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        let mut s = Self {
            dir,
            tz,
            prefs,
            location,
            parser: pdu::Parser::default(),
            entries: Vec::new(),
            rows: Vec::new(),
            selected: None,
            follow: true,
            query: String::new(),
            alerts_only: false,
            tab: Tab::Messages,
            aircraft: AircraftTable::default(),
            heard: HashMap::new(),
            channels: BTreeMap::new(),
            alert_mem: AlertMemory::default(),
            clock: 0.0,
            alert: None,
            alerts_count: 0,
            reported: Reported::default(),
            systable_on_air: None,
            next_seq: 1,
            day: String::new(),
            offset: 0,
            own_lines: HashMap::new(),
            writer,
            write_errors,
            notices: VecDeque::new(),
            effects: Effects::default(),
            notifier: desktop_notify,
            tables: enrich::Tables::default(),
            tables_rx: None,
            cache: lookup::Cache::default(),
            lookup_get: lookup::http_get,
            worker: None,
            inflight: HashSet::new(),
            pending: Vec::new(),
        };
        s.cache = lookup::Cache::open(s.cache_path());
        s.load_tables();
        s.open_day(now);
        s
    }

    /// The lookup cache file, next to the day files.
    pub fn cache_path(&self) -> PathBuf {
        self.dir.join("lookup-cache.ndjson")
    }

    /// Replace the HTTP GET of the lookups (tests).
    pub fn set_lookup_get(&mut self, get: lookup::Get) {
        self.lookup_get = get;
        self.worker = None;
    }

    /// Read the enrichment files on a worker thread.
    fn load_tables(&mut self) {
        let files = self.prefs.files();
        if files == enrich::Files::default() {
            self.tables = enrich::Tables::default();
            self.tables_rx = None;
            return;
        }
        let (tx, rx) = unbounded();
        self.tables_rx = Some(rx);
        let _ = std::thread::Builder::new()
            .name("hfdl-enrich".into())
            .spawn(move || {
                let _ = tx.send(enrich::load(&files));
            });
    }

    /// Wait until the enrichment files are read (tests, start-up).
    pub fn wait_tables(&mut self) {
        if let Some(rx) = self.tables_rx.take()
            && let Ok((t, notes)) = rx.recv_timeout(Duration::from_secs(60))
        {
            self.tables = t;
            for n in notes {
                self.notice(n);
            }
        }
    }

    /// The enrichment of a message at reception (`04-support.md` §4.10):
    /// aircraft from the file, else the lookup cache; airline from the
    /// flight ID; route from the lookup cache, else CPDLC airports.
    fn enrich(&self, m: &mut Msg) {
        let t = &m.tree;
        if msg::is_squitter(t) {
            return;
        }
        let icao = msg::icao_of(t).map(|x| x.0);
        let reg = msg::registration(t);
        let flight = msg::flight(t);
        let svcs: Vec<Svc> = [Svc::Hexdb, Svc::Adsbdb]
            .into_iter()
            .filter(|s| match s {
                Svc::Hexdb => self.prefs.lookup_hexdb,
                Svc::Adsbdb => self.prefs.lookup_adsbdb,
            })
            .collect();
        let mut acft = self
            .tables
            .aircraft_info(icao.as_deref(), reg.as_deref())
            .or_else(|| {
                let d = self.cache.found(&svcs, What::Aircraft, icao.as_deref()?)?;
                msg::AcftInfo::from_json(d)
            });
        if let Some(name) = flight.as_deref().and_then(|f| self.tables.airline(f)) {
            acft.get_or_insert_with(Default::default).airline = Some(name);
        }
        m.acft = acft;
        let fill = |a: &mut msg::Airport| {
            if let Some(f) = a.icao.as_deref().and_then(|c| self.tables.airport(c)) {
                a.iata = a.iata.take().or(f.iata);
                a.name = a.name.take().or(f.name);
                a.lat = a.lat.or(f.lat);
                a.lon = a.lon.or(f.lon);
            }
        };
        let mut route = flight
            .as_deref()
            .and_then(|f| self.cache.found(&svcs, What::Route, f))
            .and_then(msg::RouteInfo::from_json);
        if route.is_none() {
            let (dep, dst) = msg::cpdlc_airports(t);
            if dep.is_some() || dst.is_some() {
                let ap = |c: Option<String>| {
                    c.map(|c| msg::Airport {
                        icao: Some(c),
                        ..Default::default()
                    })
                };
                route = Some(msg::RouteInfo {
                    src: "cpdlc".into(),
                    orig: ap(dep),
                    dest: ap(dst),
                });
            }
        }
        if let Some(r) = route.as_mut() {
            r.orig.as_mut().map(fill);
            r.dest.as_mut().map(fill);
        }
        m.route = route;
    }

    /// The lookups a downlink message needs: keys with no fresh answer.
    fn lookups_needed(&self, m: &Msg, now: i64) -> Vec<(What, String)> {
        let t = &m.tree;
        if !(self.prefs.lookup_hexdb || self.prefs.lookup_adsbdb)
            || msg::is_squitter(t)
            || !msg::is_downlink(t)
        {
            return Vec::new();
        }
        let svcs: Vec<Svc> = self
            .prefs
            .services(What::Aircraft)
            .into_iter()
            .map(|x| x.0)
            .collect();
        let mut v = Vec::new();
        if let Some((icao, _)) = msg::icao_of(t)
            && m.acft.as_ref().and_then(|a| a.src.as_deref()) != Some("file")
            && !self.cache.answered(&svcs, What::Aircraft, &icao, now)
        {
            v.push((What::Aircraft, icao));
        }
        if let Some(f) = msg::flight(t)
            && !self.cache.answered(&svcs, What::Route, &f, now)
        {
            v.push((What::Route, f));
        }
        v
    }

    /// Ask for the keys not already being asked.
    fn request(&mut self, keys: &[(What, String)]) {
        for (what, key) in keys {
            if !self.inflight.insert((*what, key.clone())) {
                continue;
            }
            let get = self.lookup_get;
            let w = self
                .worker
                .get_or_insert_with(|| lookup::Worker::start(get));
            w.send(lookup::Job {
                what: *what,
                key: key.clone(),
                svcs: self.prefs.services(*what),
            });
        }
    }

    /// A lookup ended: cache its answers, then write the records it
    /// completes, with their enrichment (`LKP-03`, `LKP-04`).
    fn finish_lookup(&mut self, d: lookup::Done, now: i64) {
        let mut failures = Vec::new();
        for (svc, a) in &d.answers {
            match a {
                lookup::Answer::Found(v) => {
                    self.cache.put(*svc, d.what, &d.key, now, Some(v.clone()))
                }
                lookup::Answer::NotFound => self.cache.put(*svc, d.what, &d.key, now, None),
                lookup::Answer::Failed(e) => failures.push(e.clone()),
            }
        }
        let all_failed = !d.answers.is_empty() && failures.len() == d.answers.len();
        let key = (d.what, d.key.clone());
        self.inflight.remove(&key);
        let mut ready = Vec::new();
        for (i, p) in self.pending.iter_mut().enumerate() {
            if let Some(j) = p.waits.iter().position(|k| *k == key) {
                p.waits.remove(j);
                if all_failed && p.failure.is_none() {
                    p.failure = Some(failures.join("; "));
                }
                if p.waits.is_empty() {
                    ready.push(i);
                }
            }
        }
        for i in ready.into_iter().rev() {
            let mut p = self.pending.remove(i);
            self.enrich(&mut p.msg);
            if let Some(e) = &p.failure {
                self.notice(format!(
                    "online lookup for {} failed: {e}",
                    aircraft_label(&p.msg)
                ));
            }
            self.write(&p.msg);
            self.update_entry(p.seq, &p.msg);
        }
    }

    /// Show a message's later enrichment in the view.
    fn update_entry(&mut self, seq: u64, m: &Msg) {
        if let Some(e) = self.entries.iter_mut().rev().find(|e| e.seq == seq) {
            e.msg.acft = m.acft.clone();
            e.msg.route = m.route.clone();
            e.terms = terms_of(&e.msg, &self.prefs.keywords);
            self.aircraft.enrich(&m.tree, m.acft.as_ref());
        }
    }

    /// Records still waiting for a lookup: written now without its result
    /// (quitting, `LKP-04`).
    fn write_pending(&mut self) {
        for p in std::mem::take(&mut self.pending) {
            self.write(&p.msg);
        }
        self.inflight.clear();
    }

    /// Whether lookups are running.
    pub fn lookup_pending(&self) -> bool {
        !self.inflight.is_empty()
    }

    /// Replace the desktop notifier (tests).
    pub fn set_notifier(&mut self, f: fn(&str, &str, bool)) {
        self.notifier = f;
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn day(&self) -> &str {
        &self.day
    }

    pub fn day_file(&self) -> PathBuf {
        self.dir.join(format!("{}.ndjson", self.day))
    }

    pub fn prefs(&self) -> &HfdlPrefs {
        &self.prefs
    }

    pub fn location(&self) -> Option<(f64, f64)> {
        self.location
    }

    /// New menu settings: keyword lists reclassify, filters re-apply.
    pub fn set_prefs(&mut self, prefs: HfdlPrefs) {
        let kw_changed = prefs.keywords != self.prefs.keywords;
        let watch_changed = prefs.watch != self.prefs.watch;
        let files_changed = prefs.files() != self.prefs.files();
        self.prefs = prefs;
        if files_changed {
            self.load_tables();
        }
        if kw_changed || watch_changed {
            let kw = self.prefs.keywords.clone();
            for e in self.entries.iter_mut() {
                if kw_changed {
                    e.msg.reclassify(&kw);
                    e.terms = terms_of(&e.msg, &kw);
                    e.subject = msg::subject(&e.msg);
                }
                e.watched = msg::watched(&e.msg.tree, &self.prefs.watch);
            }
        }
        self.refilter();
    }

    pub fn set_location(&mut self, loc: Option<(f64, f64)>) {
        self.location = loc;
    }

    /// Another message folder: the view reloads.
    pub fn set_dir(&mut self, dir: PathBuf, now: i64) {
        if dir == self.dir {
            return;
        }
        self.flush();
        self.dir = dir;
        self.cache = lookup::Cache::open(self.cache_path());
        self.own_lines.clear();
        self.open_day(now);
    }

    fn offset_at(&self, utc: i64) -> i32 {
        time::offset_at(&self.tz, utc)
    }

    fn local_date(&self, utc: i64) -> String {
        time::local_date(utc, self.offset_at(utc))
    }

    fn open_day(&mut self, now: i64) {
        self.day = self.local_date(now);
        self.offset = 0;
        self.entries.clear();
        self.rows.clear();
        self.selected = None;
        self.aircraft.clear();
        self.heard.clear();
        self.follow_file();
    }

    /// The periodic work (about once a second): midnight, the day file
    /// grown by others, writer errors.
    pub fn tick(&mut self, now: i64) {
        if self.local_date(now) != self.day {
            self.flush();
            self.own_lines.clear();
            self.open_day(now);
        } else {
            self.follow_file();
        }
        while let Ok(e) = self.write_errors.try_recv() {
            self.notice(e);
        }
        if let Some(rx) = &self.tables_rx
            && let Ok((t, notes)) = rx.try_recv()
        {
            self.tables = t;
            self.tables_rx = None;
            for n in notes {
                self.notice(n);
            }
        }
        let done: Vec<lookup::Done> = self
            .worker
            .as_ref()
            .map(|w| w.done.try_iter().collect())
            .unwrap_or_default();
        for d in done {
            self.finish_lookup(d, now);
        }
    }

    /// Read the whole lines appended to today's file since the last read.
    fn follow_file(&mut self) {
        let path = self.day_file();
        let Ok(mut f) = std::fs::File::open(&path) else {
            return;
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
            self.entries.clear();
            self.aircraft.clear();
            self.rows.clear();
            self.selected = None;
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        }
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else {
            return;
        };
        self.offset += end as u64 + 1;
        for line in bytes[..end].split(|&b| b == b'\n') {
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Some(n) = self.own_lines.get_mut(line) {
                *n -= 1;
                if *n == 0 {
                    self.own_lines.remove(line);
                }
                continue;
            }
            if let Some(m) = msg::from_line(line, &self.prefs.keywords) {
                self.add(m);
            }
        }
    }

    /// Frames decoded by the VFO, in any order: parsed by end time, then
    /// frequency (`03-parser.md` §3.1).
    pub fn accept(&mut self, mut frames: Vec<Reception>, now: i64) {
        frames.sort_by(|a, b| {
            let end = |r: &Reception| r.utc + frame_seconds(r.m1);
            end(a).total_cmp(&end(b)).then(a.freq_khz.cmp(&b.freq_khz))
        });
        for r in frames {
            self.accept_one(r, now);
        }
    }

    fn accept_one(&mut self, r: Reception, now: i64) {
        let ch = self.channels.entry(r.freq_khz).or_default();
        ch.last_frame = Some(r.utc);
        let decoded = self.parser.frame(&r.octets, r.freq_khz, r.utc);
        let ch = self.channels.entry(r.freq_khz).or_default();
        if decoded.is_empty() {
            ch.frames_err += 1;
            self.reported.bad_frames += 1;
            return;
        }
        ch.frames_ok += 1;
        ch.last_ok = Some(r.utc);
        ch.messages += decoded.len() as u32;
        for d in decoded {
            let tree = serde_json::json!({ d.key: d.tree });
            let stored = msg::Stored {
                joined: msg::joined_of(&tree),
                systable: d.systable.clone(),
            };
            let off = self.offset_at(r.utc.floor() as i64);
            let mut m = Msg::new(
                tree,
                d.octets,
                r.utc,
                off,
                r.time_source,
                r.freq_khz,
                stored,
                &self.prefs.keywords,
            );
            m.m1 = Some(r.m1);
            m.lvl = Some(r.level_db);
            m.noise = Some(r.noise_db);
            m.ferr = Some(r.ferr_hz);
            m.src = Some(r.src.clone());
            m.file = r.file.clone();
            m.rx = self.location;
            self.enrich(&mut m);
            self.reported.by_severity[m.sev as usize] += 1;
            if msg::is_downlink(&m.tree) {
                if let Some(id) = msg::icao_of(&m.tree)
                    .map(|(i, _)| i.to_ascii_uppercase())
                    .or_else(|| msg::registration(&m.tree))
                {
                    self.reported.aircraft.insert(id);
                }
            } else if let Some(g) = msg::gs_id(&m.tree) {
                self.reported.stations.insert(g);
            }
            if let Some((v, _)) = &m.stored.systable
                && *v > SYSTABLE_BUILT_IN
            {
                self.notice(format!(
                    "system table {v} received ({SYSTABLE_BUILT_IN} built in)"
                ));
            }
            // written now, or after its lookups (`LKP-03`)
            let waits = self.lookups_needed(&m, now);
            let seq = if waits.is_empty() {
                self.write(&m);
                0
            } else {
                self.request(&waits);
                let seq = self.next_seq;
                self.pending.push(Pending {
                    seq,
                    msg: m.clone(),
                    waits,
                    failure: None,
                });
                seq
            };
            let utc_day = self.local_date(m.utc.floor() as i64);
            if utc_day != self.day {
                if seq != 0 {
                    // not in view: keep the sequence numbers apart
                    self.next_seq += 1;
                }
                continue;
            }
            let seq = self.add(m);
            self.decide_alert(seq, now);
        }
    }

    /// Put a message into the view; its sequence number.
    fn add(&mut self, m: Msg) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.aircraft.add(&m.tree, m.utc, m.freq_khz);
        self.aircraft.enrich(&m.tree, m.acft.as_ref());
        if msg::is_squitter(&m.tree) {
            let n = msg::node(&m.tree);
            if let Some(g) = msg::gs_id(&m.tree) {
                let h = self.heard.entry(g).or_default();
                h.last_utc = m.utc;
                h.last_freq_khz = m.freq_khz;
                for st in n["gs_status"].as_array().into_iter().flatten() {
                    if st["gs"]["id"].as_u64() == Some(u64::from(g)) {
                        h.freqs = st["freqs"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|f| f["freq"].as_f64())
                            .map(|f| f.round() as u32)
                            .collect();
                    }
                }
            }
            if let Some(v) = n["systable_version"].as_u64().map(|v| v as u32)
                && v > SYSTABLE_BUILT_IN
                && self.systable_on_air.is_none_or(|x| x < v)
            {
                self.systable_on_air = Some(v);
                self.notice(format!(
                    "system table {v} on the air, {SYSTABLE_BUILT_IN} in use"
                ));
            }
        }
        // a completed message hides its earlier blocks
        if m.reasm() == Some("complete")
            && let Some(key) = m.acars_key()
        {
            let since = m.utc - 1260.0;
            for e in self.entries.iter_mut().rev() {
                if e.msg.utc < since {
                    break;
                }
                if e.msg.reasm() == Some("in progress") && e.msg.acars_key().as_ref() == Some(&key)
                {
                    e.joined_later = true;
                }
            }
            if self.prefs.filter_joined {
                self.refilter_keep_selection();
            }
        }
        let kw = &self.prefs.keywords;
        let e = Entry {
            seq,
            terms: terms_of(&m, kw),
            joined_later: false,
            watched: msg::watched(&m.tree, &self.prefs.watch),
            subject: msg::subject(&m),
            msg: m,
        };
        let pass = self.passes(&e);
        // keep the time order (another writer's lines may be older)
        let pos = self
            .entries
            .iter()
            .rposition(|x| x.msg.utc <= e.msg.utc)
            .map_or(0, |i| i + 1);
        if pos == self.entries.len() {
            self.entries.push(e);
            if pass {
                self.rows.push(pos);
                if self.follow {
                    self.selected = Some(self.rows.len() - 1);
                }
            }
        } else {
            self.entries.insert(pos, e);
            self.refilter_keep_selection();
        }
        seq
    }

    fn decide_alert(&mut self, seq: u64, _now: i64) {
        let Some(e) = self.entries.iter().rev().find(|e| e.seq == seq) else {
            return;
        };
        let set = self.prefs.alert_settings();
        let Some(kind) = self
            .alert_mem
            .alert(&e.msg.tree, e.msg.sev, &set, e.msg.utc)
        else {
            return;
        };
        let who = aircraft_label(&e.msg);
        self.reported.alerts.push((
            kind,
            format!(
                "{} {who} on {} kHz: {}",
                kind.name().to_ascii_uppercase(),
                e.msg.freq_khz,
                e.subject
            ),
        ));
        let summary = format!(
            "{} {} {}: {}",
            kind.name().to_ascii_uppercase(),
            time::local_time(e.msg.utc.floor() as i64, e.msg.off),
            who,
            e.subject
        );
        if self.prefs.notifications {
            (self.notifier)(
                &format!("HFDL {}", kind.name()),
                &summary,
                kind == AlertKind::Emergency,
            );
        }
        self.effects.bell |= self.prefs.alert_sound;
        self.alerts_count += 1;
        self.alert = Some(AlertInfo { seq, kind, summary });
    }

    fn write(&mut self, m: &Msg) {
        let line = msg::to_line(m);
        let path = self
            .dir
            .join(time::day_file_name(m.utc.floor() as i64, m.off));
        if path == self.day_file() {
            *self.own_lines.entry(line.clone()).or_insert(0) += 1;
        }
        let _ = self.writer.send(WriteJob::Line(path, line));
    }

    /// The channels' live states from the processor.
    pub fn set_status(&mut self, st: Vec<ChannelStatus>) {
        for s in st {
            self.channels.entry(s.freq_khz).or_default().status = Some(s);
        }
    }

    /// The instant the signal being decoded stands for (UTC s).
    pub fn set_clock(&mut self, t: f64) {
        self.clock = t;
    }

    /// The reception clock; `fallback` before any reception.
    pub fn clock(&self, fallback: f64) -> f64 {
        if self.clock > 0.0 {
            self.clock
        } else {
            fallback
        }
    }

    /// Forget the channels (the VFO was retuned or stopped).
    pub fn clear_channels(&mut self) {
        self.channels.clear();
    }

    pub fn channels(&self) -> &BTreeMap<u32, ChannelInfo> {
        &self.channels
    }

    /// Wait until every queued line is on disk (or failed).
    pub fn flush(&mut self) {
        self.write_pending();
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.writer.send(WriteJob::Barrier(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }

    // ── the list ───────────────────────────────────────────────────────────

    fn passes(&self, e: &Entry) -> bool {
        (!self.prefs.filter_link || e.msg.sev != Severity::Link)
            && (!self.alerts_only || e.msg.sev >= Severity::Caution || e.watched)
            && (!self.prefs.filter_joined || !e.joined_later)
            && msg::matches(&self.query, &e.terms)
    }

    fn refilter(&mut self) {
        self.rows = (0..self.entries.len())
            .filter(|&i| self.passes(&self.entries[i]))
            .collect();
        self.selected = if self.follow {
            self.rows.len().checked_sub(1)
        } else {
            self.selected
                .map(|s| s.min(self.rows.len().saturating_sub(1)))
                .filter(|_| !self.rows.is_empty())
        };
    }

    fn refilter_keep_selection(&mut self) {
        let seq = self.selected_entry().map(|e| e.seq);
        self.refilter();
        if !self.follow
            && let Some(seq) = seq
            && let Some(r) = self.rows.iter().position(|&i| self.entries[i].seq == seq)
        {
            self.selected = Some(r);
        }
    }

    /// Messages in view after search and filters.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// All messages of the day in view.
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn row(&self, r: usize) -> Option<&Entry> {
        self.rows.get(r).map(|&i| &self.entries[i])
    }

    pub fn selected_row(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.selected.and_then(|r| self.row(r))
    }

    pub fn select_row(&mut self, r: usize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = Some(r.min(self.rows.len() - 1));
        self.follow = self.selected == Some(self.rows.len() - 1) && self.follow;
    }

    pub fn move_by(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.selected.unwrap_or(self.rows.len() - 1) as isize;
        let n = (cur + d).clamp(0, self.rows.len() as isize - 1) as usize;
        self.selected = Some(n);
        if d < 0 {
            self.follow = false;
        }
    }

    pub fn first(&mut self) {
        self.follow = false;
        if !self.rows.is_empty() {
            self.selected = Some(0);
        }
    }

    pub fn last(&mut self) {
        if !self.rows.is_empty() {
            self.selected = Some(self.rows.len() - 1);
        }
    }

    pub fn follow(&self) -> bool {
        self.follow
    }

    pub fn set_follow(&mut self, on: bool) {
        self.follow = on;
        if on {
            self.last();
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.trim().to_string();
        self.refilter_keep_selection();
    }

    pub fn alerts_only(&self) -> bool {
        self.alerts_only
    }

    pub fn set_alerts_only(&mut self, on: bool) {
        self.alerts_only = on;
        self.refilter_keep_selection();
    }

    pub fn clear_filters(&mut self) {
        self.query.clear();
        self.alerts_only = false;
        self.refilter_keep_selection();
    }

    /// "message x of n" (`VIEW-09`).
    pub fn position_text(&self) -> String {
        match self.selected {
            Some(r) => format!("{} of {}", r + 1, self.rows.len()),
            None => format!("{} of {}", 0, self.rows.len()),
        }
    }

    // ── aircraft and ground stations ───────────────────────────────────────

    /// The aircraft view: watched first, then by last heard (newest first).
    pub fn aircraft(&self) -> Vec<&msg::Aircraft> {
        let mut v = self.aircraft.list();
        let watched = |a: &msg::Aircraft| {
            [&a.icao, &a.registration, &a.flight]
                .into_iter()
                .flatten()
                .any(|x| self.prefs.watch.iter().any(|w| w.eq_ignore_ascii_case(x)))
        };
        v.sort_by(|a, b| {
            watched(b)
                .cmp(&watched(a))
                .then(b.last_t.total_cmp(&a.last_t))
        });
        v
    }

    pub fn aircraft_count(&self) -> usize {
        self.aircraft.len()
    }

    pub fn is_watched(&self, a: &msg::Aircraft) -> bool {
        [&a.icao, &a.registration, &a.flight]
            .into_iter()
            .flatten()
            .any(|x| self.prefs.watch.iter().any(|w| w.eq_ignore_ascii_case(x)))
    }

    /// Ground stations as heard.
    pub fn heard(&self, id: u8) -> Option<&Heard> {
        self.heard.get(&id)
    }

    pub fn systable_on_air(&self) -> Option<u32> {
        self.systable_on_air
    }

    /// The identity to watch for the selected message (ICAO address, else
    /// registration, else flight).
    pub fn selected_identity(&self) -> Option<String> {
        let m = &self.selected_entry()?.msg;
        msg::icao_of(&m.tree)
            .map(|x| x.0)
            .or_else(|| msg::registration(&m.tree))
            .or_else(|| msg::flight(&m.tree))
    }

    // ── alerts and notices ─────────────────────────────────────────────────

    pub fn alert(&self) -> Option<&AlertInfo> {
        self.alert.as_ref()
    }

    /// What was accepted since the Report last asked.
    pub fn take_reported(&mut self) -> Reported {
        std::mem::take(&mut self.reported)
    }

    pub fn alerts_count(&self) -> u32 {
        self.alerts_count
    }

    pub fn acknowledge_alert(&mut self) {
        self.alert = None;
    }

    pub fn notice(&mut self, text: String) {
        self.notices.push_back(text);
        while self.notices.len() > 8 {
            self.notices.pop_front();
        }
    }

    pub fn take_notice(&mut self) -> Option<String> {
        self.notices.pop_front()
    }

    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut self.effects)
    }

    // ── detail ─────────────────────────────────────────────────────────────

    /// The selected message's detail as (label, value) lines; an empty
    /// label continues the previous value (`VIEW-05`).
    pub fn detail_fields(&self) -> Option<Vec<(String, String)>> {
        Some(detail(&self.selected_entry()?.msg, self.location))
    }

    /// The detail as saved and copied (`EXP-01`).
    pub fn detail_text(&self) -> Option<String> {
        let f = self.detail_fields()?;
        let w = f.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
        let mut out = String::new();
        for (k, v) in f {
            if k.is_empty() && v.is_empty() {
                out.push('\n');
            } else if k.is_empty() {
                out.push_str(&format!("{:w$}   {v}\n", ""));
            } else {
                out.push_str(&format!("{k:w$} : {v}\n"));
            }
        }
        Some(out)
    }

    /// Where the selected message is saved by default.
    pub fn default_save_path(&self) -> Option<PathBuf> {
        let m = &self.selected_entry()?.msg;
        let stamp = time::iso_utc(m.utc.floor() as i64)
            .replace(['-', ':'], "")
            .trim_end_matches('Z')
            .to_string();
        let who = aircraft_label(m).replace(|c: char| !c.is_ascii_alphanumeric(), "");
        Some(
            self.dir
                .join(format!("{PROJECT}_{stamp}Z_{}_{who}.txt", m.freq_khz)),
        )
    }

    pub fn save_selected(&self, path: &Path) -> Result<(), String> {
        let text = self.detail_text().ok_or("no message selected")?;
        if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

impl std::fmt::Debug for HfdlService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HfdlService")
            .field("dir", &self.dir)
            .field("day", &self.day)
            .field("total", &self.total())
            .finish_non_exhaustive()
    }
}

impl Drop for HfdlService {
    fn drop(&mut self) {
        self.flush();
    }
}

fn terms_of(m: &Msg, kw: &Keywords) -> Vec<String> {
    let mut t = msg::search_terms(&m.tree, m.freq_khz, m.file.as_deref(), kw);
    if let Some(s) = &m.src {
        t.push(s.clone());
    }
    if let Some(a) = &m.acft {
        t.extend(a.terms());
    }
    if let Some(r) = &m.route {
        t.extend(r.terms());
    }
    t
}

/// A frame's length (s) for its M1 mode (`physical-layer.md` §2).
pub fn frame_seconds(m1: u8) -> f64 {
    let p = phy::phy();
    let segs = p.modes.get(m1 as usize).map_or(72, |m| m.segments);
    let syms = phy::PREKEY
        + 2 * phy::A_LEN
        + phy::M1_LEN
        + phy::M2_LEN
        + phy::T_PREAMBLE * phy::T_LEN
        + segs * (phy::DATA_SEG + phy::T_LEN);
    syms as f64 / phy::SYMBOL_RATE
}

/// The aircraft column: registration, else flight, else ICAO address, else
/// AC ID.
pub fn aircraft_label(m: &Msg) -> String {
    let t = &m.tree;
    msg::registration(t)
        .or_else(|| msg::flight(t))
        .or_else(|| msg::icao_of(t).map(|x| x.0))
        .or_else(|| msg::ac_id(t).map(|a| format!("#{a}")))
        .unwrap_or_else(|| "--".into())
}

/// The type column: the class, for ACARS with its label and sublabel.
pub fn type_label(m: &Msg) -> String {
    let a = msg::acars_of(&m.tree);
    let label = a.and_then(|a| a["label"].as_str());
    let sub = a.and_then(|a| a["sublabel"].as_str());
    match (m.class, label) {
        ("acars", Some(l)) => match sub {
            Some(s) => format!("ACARS {l}/{s}"),
            None => format!("ACARS {l}"),
        },
        ("adsc", _) => "ADS-C".into(),
        ("cpdlc", _) => "CPDLC".into(),
        ("miam", _) => "MIAM".into(),
        ("media", _) => "MEDIA ADV".into(),
        ("ohma", _) => "OHMA".into(),
        ("squitter", _) => "SQUITTER".into(),
        ("perf", _) => "PERF DATA".into(),
        ("freq", _) => "FREQ DATA".into(),
        ("systable", _) => "SYSTABLE".into(),
        ("systable_req", _) => "SYSTABLE REQ".into(),
        ("logon", _) => "LOGON".into(),
        ("logon_confirm", _) => "LOGON CONF".into(),
        ("logon_resume", _) => "LOGON RES".into(),
        ("logoff", _) => "LOGOFF".into(),
        ("logon_denied", _) => "LOGON DENIED".into(),
        ("echo", _) => "ECHO".into(),
        (c, _) => c.to_ascii_uppercase(),
    }
}

fn latlon(lat: f64, lon: f64) -> String {
    format!(
        "{:.3} {} {:.3} {}",
        lat.abs(),
        if lat < 0.0 { 'S' } else { 'N' },
        lon.abs(),
        if lon < 0.0 { 'W' } else { 'E' }
    )
}

fn km_nm(km: f64) -> String {
    format!("{km:.0} km ({:.0} NM)", crate::dsc::geo::km_to_nm(km))
}

/// The detail of a message (`spec/07-ui/README.md` §Detail).
pub fn detail(m: &Msg, location: Option<(f64, f64)>) -> Vec<(String, String)> {
    let mut f: Vec<(String, String)> = Vec::new();
    let mut add = |k: &str, v: String| f.push((k.to_string(), v));
    let t = &m.tree;
    let rx = m.rx.or(location);
    // summary
    let sev = match m.sev {
        Severity::Emergency | Severity::Caution => {
            let why = if m.kw.is_empty() {
                match m.class {
                    "cpdlc" => "CPDLC MAYDAY / PAN PAN".to_string(),
                    "adsc" => "ADS-C emergency report".to_string(),
                    _ => String::new(),
                }
            } else {
                format!("keyword {}", m.kw.join(", "))
            };
            format!("{}: {why}", m.sev.name().to_ascii_uppercase())
        }
        s => s.name().to_string(),
    };
    add("Severity", sev);
    add("Type", type_label(m));
    let subj = msg::subject(m);
    add("Subject", subj);
    // aircraft
    let icao = msg::icao_of(t);
    let reg = msg::registration(t);
    if icao.is_some() || reg.is_some() || msg::ac_id(t).is_some() {
        let mut v = Vec::new();
        if let Some(r) = &reg {
            v.push(r.clone());
        }
        if let Some((i, src)) = &icao {
            v.push(format!(
                "ICAO {i}{}",
                if *src == "table" {
                    " (AC ID table)"
                } else {
                    ""
                }
            ));
        }
        if let Some(a) = msg::ac_id(t) {
            v.push(format!("AC ID {a}"));
        }
        add("Aircraft", v.join("  "));
    }
    if let Some(fl) = msg::flight(t) {
        add("Flight", fl);
    }
    if let Some(a) = &m.acft {
        let model: Vec<&str> = [&a.icao_type, &a.type_]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        let src = a
            .src
            .as_deref()
            .map(|s| format!("  [{s}]"))
            .unwrap_or_default();
        if !model.is_empty() {
            add("Model", format!("{}{src}", model.join("  ")));
        }
        if let Some(o) = &a.operator {
            add("Operator", o.clone());
        }
        if let Some(l) = &a.airline {
            add("Airline", l.clone());
        }
    }
    if let Some(r) = &m.route {
        for (k, ap) in [("From", &r.orig), ("To", &r.dest)] {
            if let Some(ap) = ap {
                let src = if k == "From" {
                    format!("  [{}]", r.src)
                } else {
                    String::new()
                };
                add(k, format!("{}{src}", ap.label()));
                if let Some(km) = ap.km(rx) {
                    add("", km_nm(km));
                }
            }
        }
    }
    if let Some(p) = msg::position(t) {
        add("Position", latlon(p.lat, p.lon));
        add(
            "",
            format!(
                "[{}{}]",
                match p.src {
                    "perf" => "performance data",
                    "freq" => "frequency data",
                    _ => "ADS-C",
                },
                p.time.map(|x| format!(" {x}")).unwrap_or_default()
            ),
        );
        if let Some((la, lo)) = rx {
            add(
                "",
                km_nm(crate::dsc::geo::distance_km(la, lo, p.lat, p.lon)),
            );
        }
    }
    // ground station
    if let Some(g) = msg::gs_id(t) {
        let mut v = format!("{g}");
        if let Some(s) = stations::station(g) {
            v.push_str(&format!(" {}", s.name));
            add("GS", v);
            if let Some((la, lo)) = rx {
                add(
                    "",
                    format!(
                        "{}, {} kHz",
                        km_nm(crate::dsc::geo::distance_km(la, lo, s.lat, s.lon)),
                        m.freq_khz
                    ),
                );
            } else {
                add("", format!("{}, {} kHz", latlon(s.lat, s.lon), m.freq_khz));
            }
        } else {
            add("GS", v);
        }
    }
    // HFDL
    let n = msg::node(t);
    add(
        "Direction",
        if msg::is_squitter(t) {
            "squitter (uplink)".into()
        } else if msg::is_downlink(t) {
            "downlink (aircraft to ground)".into()
        } else {
            "uplink (ground to aircraft)".into()
        },
    );
    if let Some(ty) = n["type"]["name"].as_str() {
        add("LPDU", ty.to_string());
    }
    if let Some(h) = msg::hfnpdu_of(t)
        && let Some(ty) = h["type"]["name"].as_str()
    {
        add("HFNPDU", ty.to_string());
    }
    // ACARS
    if let Some(a) = msg::acars_of(t) {
        let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
        let mut v = format!("mode {} label {}", s("mode"), s("label"));
        if !s("sublabel").is_empty() {
            v.push_str(&format!("/{}", s("sublabel")));
        }
        v.push_str(&format!(" blk {}", s("blk_id")));
        if !s("msg_num").is_empty() {
            v.push_str(&format!(" msg {}{}", s("msg_num"), s("msg_num_seq")));
        }
        if a["crc_ok"] == Value::Bool(false) {
            v.push_str(" CRC BAD");
        }
        add("ACARS", v);
        if !s("reassembly").is_empty() {
            add("Reasm", s("reassembly"));
        }
        let text = s("msg_text");
        if !text.is_empty() {
            let mut first = true;
            for line in text.lines() {
                add(if first { "Text" } else { "" }, line.to_string());
                first = false;
            }
        }
        // application
        for (k, label) in [
            ("arinc622", "ARINC 622"),
            ("miam", "MIAM"),
            ("media-adv", "Media adv"),
            ("ohma", "OHMA"),
        ] {
            if let Some(x) = a.get(k) {
                add(label, String::new());
                for (p, v) in flatten(x, "") {
                    add("", format!("{p}: {v}"));
                }
            }
        }
    } else if !msg::is_squitter(t) {
        if let Some(h) = msg::hfnpdu_of(t) {
            for (p, v) in flatten(h, "") {
                if p != "type.name" && p != "type.id" && p != "err" {
                    add("", format!("{p}: {v}"));
                }
            }
        }
    } else {
        for (p, v) in flatten(n, "") {
            if p != "err" {
                add("", format!("{p}: {v}"));
            }
        }
    }
    // reception
    add(
        "UTC",
        format!("{} [{}]", msg::iso_utc_ms(m.utc), m.tsrc.name()),
    );
    add("Local", time::iso_local(m.utc.floor() as i64, m.off));
    let mut sig = format!("{} kHz", m.freq_khz);
    if let Some(mode) = m.m1.and_then(|i| phy::phy().modes.get(i as usize)) {
        sig.push_str(&format!(
            "  {} bps {}",
            mode.bit_rate,
            if mode.slots == 1 { "S" } else { "D" }
        ));
    }
    if let (Some(l), Some(n)) = (m.lvl, m.noise) {
        sig.push_str(&format!("  {l:.1} dBFS, SNR {:.1} dB", l - n));
    }
    if let Some(e) = m.ferr {
        sig.push_str(&format!("  {e:+.1} Hz"));
    }
    add("Signal", sig);
    if let Some(s) = &m.src {
        add("Source", s.clone());
    }
    if let Some(fl) = &m.file {
        add("File", fl.clone());
    }
    if let Some((la, lo)) = m.rx {
        add("Receiver", latlon(la, lo));
    }
    // raw
    let mut first = true;
    for c in m.pdu.chunks(16) {
        add(
            if first { "Raw" } else { "" },
            c.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
        first = false;
    }
    f
}

/// A tree's leaves as (path, value) lines, `err` flags left out.
fn flatten(v: &Value, prefix: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if k == "err" && x == &Value::Bool(false) {
                    continue;
                }
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                out.extend(flatten(x, &p));
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                out.extend(flatten(x, &format!("{prefix}[{i}]")));
            }
        }
        Value::String(s) => out.push((prefix.to_string(), s.clone())),
        x => out.push((prefix.to_string(), x.to_string())),
    }
    out
}

/// Copy text to the clipboard (as DSC's).
pub fn copy_to_clipboard(text: &str) -> Result<&'static str, String> {
    crate::dsc::service::copy_to_clipboard(text)
}

#[cfg(test)]
/// Tests: an uplink frame carrying an ACARS block with `text`.
pub fn acars_frame(text: &str, label: &str) -> Vec<u8> {
    // ACARS block: mode, reg, ack, label, block id, STX, text, ETX, CRC, DEL
    let mut blk = vec![b'2'];
    blk.extend(b".RA7382");
    blk.push(0x15);
    blk.extend(label.as_bytes());
    blk.push(b'A');
    blk.push(0x02);
    blk.extend(text.as_bytes());
    blk.push(0x03);
    let crc = crate::hfdl::acars::crc16_reflected(&blk, 0);
    blk.extend(crc.to_le_bytes());
    blk.push(0x7F);
    // HFNPDU enveloped data, LPDU unnumbered data
    let mut lpdu = vec![0x0D, 0xFF, 0xFF, 0x01];
    lpdu.extend(&blk);
    let fcs = crate::hfdl::acars::crc16_reflected(&lpdu, 0xFFFF) ^ 0xFFFF;
    lpdu.extend(fcs.to_le_bytes());
    // uplink MPDU from GS 7: one aircraft (AC ID 23) with one LPDU
    let mut h = vec![0x01, 7, 23, 0x10, (lpdu.len() - 1) as u8];
    let fcs = crate::hfdl::acars::crc16_reflected(&h, 0xFFFF) ^ 0xFFFF;
    h.extend(fcs.to_le_bytes());
    h.extend(lpdu);
    h
}

/// A downlink ACARS frame from AC ID 23 to GS 7 with `flight` (tests).
#[cfg(test)]
pub fn acars_down_frame(text: &str, label: &str, flight: &str) -> Vec<u8> {
    let mut blk = vec![b'2'];
    blk.extend(b".RA7382");
    blk.push(0x15);
    blk.extend(label.as_bytes());
    blk.push(b'1');
    blk.push(0x02);
    blk.extend(b"M01A");
    blk.extend(format!("{flight:<6}").as_bytes());
    blk.extend(text.as_bytes());
    blk.push(0x03);
    let crc = crate::hfdl::acars::crc16_reflected(&blk, 0);
    blk.extend(crc.to_le_bytes());
    blk.push(0x7F);
    let mut lpdu = vec![0x0D, 0xFF, 0xFF, 0x01];
    lpdu.extend(&blk);
    let fcs = crate::hfdl::acars::crc16_reflected(&lpdu, 0xFFFF) ^ 0xFFFF;
    lpdu.extend(fcs.to_le_bytes());
    // downlink MPDU: one LPDU, GS 7, AC ID 23
    let mut h = vec![0x03 | (1 << 2), 7, 23, 0, 0, 0, (lpdu.len() - 1) as u8];
    let fcs = crate::hfdl::acars::crc16_reflected(&h, 0xFFFF) ^ 0xFFFF;
    h.extend(fcs.to_le_bytes());
    h.extend(lpdu);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_takes_messages_planes_and_bad_frames_once() {
        let d = Dir::new("reported");
        let mut s = HfdlService::new(d.0.clone(), utc_tz(), HfdlPrefs::default(), None, NOW);
        s.set_notifier(quiet);
        let mut bad = acars_down_frame("POS", "H1", "UAE123");
        bad[3] ^= 1;
        s.accept(
            vec![
                rx(acars_down_frame("POS", "H1", "UAE123"), NOW as f64),
                rx(acars_down_frame("POS2", "H1", "UAE123"), NOW as f64 + 5.0),
                rx(bad, NOW as f64 + 9.0),
            ],
            NOW,
        );
        let r = s.take_reported();
        assert_eq!(r.by_severity.iter().sum::<u64>(), 2);
        assert_eq!(r.bad_frames, 1);
        // no ICAO address in the frame: the registration
        assert_eq!(r.aircraft.iter().collect::<Vec<_>>(), ["RA7382"]);
        assert!(r.stations.is_empty(), "a downlink is not the station heard");
        assert_eq!(s.take_reported(), Reported::default());
    }

    #[test]
    fn a_downlink_test_frame_parses() {
        let mut p = pdu::Parser::default();
        let d = p.frame(&acars_down_frame("POS N55", "H1", "UAE123"), 8942, 0.0);
        assert_eq!(d.len(), 1);
        let tree = serde_json::json!({ d[0].key: d[0].tree });
        assert!(msg::is_downlink(&tree), "{tree}");
        assert_eq!(msg::flight(&tree).as_deref(), Some("UAE123"), "{tree}");
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("shellsdr-hfdl-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn quiet(_: &str, _: &str, _: bool) {}

    fn utc_tz() -> TimeZone {
        TimeZone::get("UTC").unwrap()
    }

    use super::acars_frame;

    fn rx(octets: Vec<u8>, utc: f64) -> Reception {
        Reception {
            freq_khz: 8942,
            m1: 1,
            octets,
            utc,
            time_source: TimeSource::Wallclock,
            level_db: -30.0,
            noise_db: -50.0,
            ferr_hz: 0.5,
            src: "HFDL 8.9 MHz".into(),
            file: None,
        }
    }

    const NOW: i64 = 1_795_600_689;

    fn service(d: &Dir) -> HfdlService {
        let mut s = HfdlService::new(d.0.clone(), utc_tz(), HfdlPrefs::default(), None, NOW);
        s.set_notifier(quiet);
        s
    }

    fn wait_for(s: &mut HfdlService, what: impl Fn(&HfdlService) -> bool) {
        let t0 = std::time::Instant::now();
        while !what(s) {
            assert!(t0.elapsed() < Duration::from_secs(10), "timed out");
            s.tick(NOW);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn day_lines(s: &mut HfdlService) -> Vec<Value> {
        s.flush();
        std::fs::read_to_string(s.day_file())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn messages_are_enriched_from_the_files_and_keep_it() {
        let d = Dir::new("enrich");
        std::fs::write(
            d.0.join("aircraft.csv"),
            "icao24,registration,manufacturername,model,typecode,operator\n\
             1520a0,RA-7382,Sukhoi,Superjet 100-95,SU95,Aeroflot\n",
        )
        .unwrap();
        std::fs::write(
            d.0.join("airlines.csv"),
            "id,name,alias,iata,icao,callsign,country,active\n1,Emirates,,EK,UAE,EMIRATES,AE,Y\n",
        )
        .unwrap();
        let prefs = HfdlPrefs {
            aircraft_file: d.0.join("aircraft.csv").display().to_string(),
            airlines_file: d.0.join("airlines.csv").display().to_string(),
            ..Default::default()
        };
        let mut s = HfdlService::new(d.0.clone(), utc_tz(), prefs, None, NOW);
        s.set_notifier(quiet);
        s.wait_tables();
        let notes: Vec<String> = std::iter::from_fn(|| s.take_notice()).collect();
        assert!(
            notes.contains(&"aircraft file: 1 aircraft".to_string()),
            "{notes:?}"
        );
        s.accept(
            vec![rx(acars_down_frame("POS", "H1", "UAE123"), NOW as f64)],
            NOW,
        );
        let a = s.selected_entry().unwrap().msg.acft.clone().unwrap();
        assert_eq!(a.icao_type.as_deref(), Some("SU95"));
        assert_eq!(a.operator.as_deref(), Some("Aeroflot"));
        assert_eq!(a.airline.as_deref(), Some("Emirates"));
        assert_eq!(
            s.aircraft()[0].acft.as_ref().unwrap().icao_type.as_deref(),
            Some("SU95")
        );
        // searchable, stored, and read back as it was at reception
        s.set_query("superjet emirates");
        assert_eq!(s.len(), 1);
        let lines = day_lines(&mut s);
        assert_eq!(lines[0]["acft"]["src"], "file");
        assert_eq!(lines[0]["acft"]["airline"], "Emirates");
        let mut other = HfdlService::new(d.0.clone(), utc_tz(), HfdlPrefs::default(), None, NOW);
        other.set_notifier(quiet);
        assert_eq!(other.row(0).unwrap().msg.acft, Some(a));
        let text = other.detail_text().unwrap();
        assert!(text.contains("SU95"), "{text}");
        assert!(text.contains("Emirates"), "{text}");
    }

    const ROUTE: &str = r#"{"response":{"flightroute":{"origin":{"iata_code":"DXB","icao_code":"OMDB","latitude":25.25,"longitude":55.36,"name":"Dubai"},"destination":{"iata_code":"LHR","icao_code":"EGLL","latitude":51.47,"longitude":-0.46,"name":"Heathrow"}}}}"#;

    fn fake_get(u: &str) -> Result<(u16, String), String> {
        if u.contains("DOWN") {
            return Err("connection refused".into());
        }
        if u.contains("SLOW") {
            std::thread::sleep(Duration::from_secs(3));
        }
        if u.starts_with("https://adsbdb/") {
            Ok((200, ROUTE.into()))
        } else {
            Ok((404, String::new()))
        }
    }

    fn lookup_service(d: &Dir) -> HfdlService {
        let prefs = HfdlPrefs {
            lookup_hexdb: true,
            lookup_adsbdb: true,
            hexdb_route_url: "https://hexdb/{callsign}".into(),
            adsbdb_route_url: "https://adsbdb/{callsign}".into(),
            ..Default::default()
        };
        let mut s = HfdlService::new(d.0.clone(), utc_tz(), prefs, Some((54.7, 25.3)), NOW);
        s.set_notifier(quiet);
        s.set_lookup_get(fake_get);
        s
    }

    #[test]
    fn a_record_is_written_after_its_lookup_with_the_answer() {
        let d = Dir::new("lookup");
        let mut s = lookup_service(&d);
        s.accept(
            vec![rx(acars_down_frame("POS", "H1", "UAE123"), NOW as f64)],
            NOW,
        );
        // shown at once, written after the lookup
        assert_eq!(s.total(), 1);
        assert!(s.lookup_pending());
        wait_for(&mut s, |s| !s.lookup_pending());
        let lines = day_lines(&mut s);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["route"]["src"], "adsbdb");
        assert_eq!(lines[0]["route"]["orig"]["icao"], "OMDB");
        assert!(lines[0]["route"]["dest"]["km"].as_f64().unwrap() > 1000.0);
        let e = s.selected_entry().unwrap();
        assert_eq!(
            e.msg
                .route
                .as_ref()
                .unwrap()
                .dest
                .as_ref()
                .unwrap()
                .iata
                .as_deref(),
            Some("LHR")
        );
        assert!(e.terms.iter().any(|t| t == "Heathrow"));
        // both answers cached; the next message is not asked again
        let cache = std::fs::read_to_string(s.cache_path()).unwrap();
        assert_eq!(cache.lines().count(), 2, "{cache}");
        s.accept(
            vec![rx(
                acars_down_frame("POS2", "H1", "UAE123"),
                NOW as f64 + 5.0,
            )],
            NOW,
        );
        assert!(!s.lookup_pending());
        let lines = day_lines(&mut s);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["route"]["dest"]["icao"], "EGLL");
    }

    #[test]
    fn a_failed_lookup_is_told_once_and_the_record_kept() {
        let d = Dir::new("lookup-fail");
        let mut s = lookup_service(&d);
        s.accept(
            vec![rx(acars_down_frame("POS", "H1", "DOWN1"), NOW as f64)],
            NOW,
        );
        wait_for(&mut s, |s| !s.lookup_pending());
        let notes: Vec<String> = std::iter::from_fn(|| s.take_notice()).collect();
        assert_eq!(
            notes
                .iter()
                .filter(|n| n.contains("lookup") && n.contains("failed"))
                .count(),
            1,
            "{notes:?}"
        );
        let lines = day_lines(&mut s);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].get("route").is_none());
        // a failure is not cached: asked again next time
        assert!(!s.cache_path().exists());
    }

    #[test]
    fn quitting_writes_records_still_waiting() {
        let d = Dir::new("lookup-quit");
        let mut s = lookup_service(&d);
        s.accept(
            vec![rx(acars_down_frame("POS", "H1", "SLOW1"), NOW as f64)],
            NOW,
        );
        assert!(s.lookup_pending());
        let t0 = std::time::Instant::now();
        let lines = day_lines(&mut s);
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].get("route").is_none());
    }

    #[test]
    fn lookups_are_off_by_default() {
        let d = Dir::new("lookup-off");
        let mut s = service(&d);
        s.set_lookup_get(fake_get);
        s.accept(
            vec![rx(acars_down_frame("POS", "H1", "UAE123"), NOW as f64)],
            NOW,
        );
        assert!(!s.lookup_pending());
        assert_eq!(day_lines(&mut s).len(), 1);
    }

    #[test]
    fn a_frame_is_parsed_shown_written_and_read_back() {
        let d = Dir::new("write");
        let mut s = service(&d);
        let f = acars_frame("ENG 1 FIRE LOOP B FAULT", "H1");
        s.accept(vec![rx(f, NOW as f64)], NOW);
        assert_eq!(s.total(), 1, "{:?}", s.channels());
        let e = s.selected_entry().unwrap();
        assert_eq!(e.msg.sev, Severity::Caution);
        assert_eq!(e.subject, "ENG 1 FIRE LOOP B FAULT");
        assert_eq!(s.alert().map(|a| a.kind), Some(AlertKind::Caution));
        assert!(s.take_effects().bell);
        s.flush();
        let text = std::fs::read_to_string(s.day_file()).unwrap();
        assert_eq!(text.lines().count(), 1);
        // the follower does not show our own line twice
        s.tick(NOW);
        assert_eq!(s.total(), 1);
        // another service reads it back
        let s2 = service(&d);
        assert_eq!(s2.total(), 1);
        assert_eq!(
            s2.selected_entry().unwrap().subject,
            "ENG 1 FIRE LOOP B FAULT"
        );
        assert_eq!(s.channels()[&8942].frames_ok, 1);
    }

    #[test]
    fn a_bad_frame_counts_as_an_error() {
        let d = Dir::new("bad");
        let mut s = service(&d);
        let mut f = acars_frame("X", "H1");
        f[3] ^= 0xFF;
        s.accept(vec![rx(f, NOW as f64)], NOW);
        assert_eq!(s.total(), 0);
        assert_eq!(s.channels()[&8942].frames_err, 1);
    }

    #[test]
    fn link_traffic_is_hidden_and_search_filters() {
        let d = Dir::new("filter");
        let mut s = service(&d);
        s.accept(
            vec![
                rx(acars_frame("", "_d"), NOW as f64),
                rx(acars_frame("REQPOS037B", "H1"), NOW as f64 + 1.0),
                rx(acars_frame("PLEASE DIVERT TO EINN", "C1"), NOW as f64 + 2.0),
            ],
            NOW,
        );
        assert_eq!(s.total(), 3);
        assert_eq!(s.len(), 2, "the empty _d is link traffic");
        s.set_query("divert");
        assert_eq!(s.len(), 1);
        s.clear_filters();
        s.set_alerts_only(true);
        assert_eq!(s.len(), 1);
        let mut p = s.prefs().clone();
        p.filter_link = false;
        s.set_alerts_only(false);
        s.set_prefs(p);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn prefs_round_trip_through_the_settings_table() {
        let mut p = HfdlPrefs {
            watch: vec!["RA73824".into()],
            alert_sound: false,
            ..Default::default()
        };
        p.keywords.caution.push("ICING".into());
        let mut s = Settings::new();
        p.write_to(&mut s);
        assert_eq!(HfdlPrefs::from_settings(&s), p);
        assert_eq!(
            HfdlPrefs::from_settings(&Settings::new()),
            HfdlPrefs::default()
        );
    }

    #[test]
    fn the_detail_has_its_sections_and_saves() {
        let d = Dir::new("detail");
        let mut s = service(&d);
        s.accept(
            vec![rx(acars_frame("ENG 1 FIRE LOOP B FAULT", "H1"), NOW as f64)],
            NOW,
        );
        let t = s.detail_text().unwrap();
        for k in [
            "Severity",
            "CAUTION: keyword FIRE",
            "Aircraft",
            "RA7382",
            "GS",
            "Raw",
            "Signal",
        ] {
            assert!(t.contains(k), "{k} in\n{t}");
        }
        let p = s.default_save_path().unwrap();
        s.save_selected(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), t);
    }

    #[test]
    fn frame_lengths_follow_the_mode() {
        // a single slot frame fits its 2.46 s slot
        assert!(
            (2.0..2.46).contains(&frame_seconds(0)),
            "{}",
            frame_seconds(0)
        );
        assert!(frame_seconds(4) > 4.0);
    }
}
