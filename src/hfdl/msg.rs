//! One HFDL message and the rules on it (HFDL seed `03-parser.md` §3.4,
//! §3.6, `04-support.md` §4.6, §4.7, §4.9, `spec/05-data/`): class,
//! severity and keywords, search terms, alerts, the aircraft view, and the
//! day-file record. A message is its parse tree (`{"lpdu": …}` or
//! `{"spdu": …}`, libacars / dumphfdl names) with its reception context.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{acars, apps, pdu, stations};
use crate::dsc::record::TimeSource;
use crate::dsc::time;

/// The default keyword lists (`03-parser.md` §3.6).
pub const EMERGENCY_KW: [&str; 7] = [
    "MAYDAY",
    "PAN PAN",
    "EMERGENCY",
    "SQUAWK 7700",
    "SQUAWK 7600",
    "SQUAWK 7500",
    "HIJACK",
];
pub const CAUTION_KW: [&str; 14] = [
    "FIRE",
    "SMOKE",
    "ENG FAIL",
    "ENGINE FAIL",
    "ENG SHUTDOWN",
    "ENGINE SHUTDOWN",
    "DIVERT",
    "DIVERSION",
    "MEDICAL",
    "BOMB",
    "DEPRESSURIZATION",
    "DECOMPRESSION",
    "FUEL LEAK",
    "LOW FUEL",
];

/// The keyword lists in use.
#[derive(Debug, Clone, PartialEq)]
pub struct Keywords {
    pub emergency: Vec<String>,
    pub caution: Vec<String>,
}

impl Default for Keywords {
    fn default() -> Self {
        Self {
            emergency: EMERGENCY_KW.iter().map(|s| s.to_string()).collect(),
            caution: CAUTION_KW.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// A message's severity (`03-parser.md` §3.4), lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Link,
    Routine,
    Caution,
    Emergency,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Severity::Link => "link",
            Severity::Routine => "routine",
            Severity::Caution => "caution",
            Severity::Emergency => "emergency",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "link" => Severity::Link,
            "routine" => Severity::Routine,
            "caution" => Severity::Caution,
            "emergency" => Severity::Emergency,
            _ => return None,
        })
    }

    /// The list's severity letter (`VIEW-04`).
    pub fn letter(self) -> char {
        match self {
            Severity::Link => '·',
            Severity::Routine => ' ',
            Severity::Caution => 'C',
            Severity::Emergency => 'E',
        }
    }
}

const LINK_CLASSES: [&str; 12] = [
    "squitter",
    "logon",
    "logon_confirm",
    "logon_resume",
    "logoff",
    "logon_denied",
    "systable",
    "systable_req",
    "perf",
    "freq",
    "echo",
    "media",
];

// ── the tree ───────────────────────────────────────────────────────────────

/// The LPDU or SPDU node.
pub fn node(m: &Value) -> &Value {
    m.get("lpdu")
        .or_else(|| m.get("spdu"))
        .unwrap_or(&Value::Null)
}

pub fn is_squitter(m: &Value) -> bool {
    m.get("spdu").is_some()
}

/// The ACARS block, if the message carries one.
pub fn acars_of(m: &Value) -> Option<&Value> {
    m.get("lpdu")?.get("hfnpdu")?.get("acars")
}

pub fn hfnpdu_of(m: &Value) -> Option<&Value> {
    m.get("lpdu")?.get("hfnpdu")
}

/// From an aircraft.
pub fn is_downlink(m: &Value) -> bool {
    m.get("lpdu")
        .is_some_and(|l| l["src"]["type"] == "Aircraft")
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Every (key, value) of a tree, depth first.
fn walk_items<'a>(v: &'a Value, out: &mut Vec<(&'a str, &'a Value)>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                out.push((k, x));
                walk_items(x, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                walk_items(x, out);
            }
        }
        _ => {}
    }
}

fn walk_strings(v: &Value) -> Vec<&str> {
    let mut it = Vec::new();
    walk_items(v, &mut it);
    it.into_iter().filter_map(|(_, x)| x.as_str()).collect()
}

fn cpdlc_of(m: &Value) -> Option<&Value> {
    acars_of(m)?.get("arinc622")?.get("cpdlc")
}

fn adsc_of(m: &Value) -> Option<&Value> {
    acars_of(m)?.get("arinc622")?.get("adsc")
}

/// The texts searched for keywords (§3.6).
pub fn texts(m: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(a) = acars_of(m) else {
        return out;
    };
    if let Some(t) = str_of(a, "msg_text").filter(|t| !t.is_empty()) {
        out.push(t.to_string());
    }
    for app in ["miam", "ohma"] {
        if let Some(x) = a.get(app) {
            out.extend(
                walk_strings(x)
                    .into_iter()
                    .filter(|s| s.chars().count() > 3)
                    .map(str::to_string),
            );
        }
    }
    if let Some(cp) = cpdlc_of(m) {
        let mut it = Vec::new();
        walk_items(cp, &mut it);
        out.extend(
            it.into_iter()
                .filter(|(k, _)| *k != "choice" && *k != "choice_label")
                .filter_map(|(_, v)| v.as_str().map(str::to_string)),
        );
    }
    out
}

/// Whether `kw` occurs in `text` (§3.6): case-insensitive, whole words,
/// its words separated by any white space.
pub fn keyword_in(text: &str, kw: &str) -> bool {
    let words: Vec<&[u8]> = kw.split_whitespace().map(str::as_bytes).collect();
    if words.is_empty() {
        return false;
    }
    let t = text.as_bytes();
    let alnum = |i: usize| t.get(i).is_some_and(|c| c.is_ascii_alphanumeric());
    'start: for s in 0..t.len() {
        if s > 0 && alnum(s - 1) {
            continue;
        }
        let mut p = s;
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                let q = p;
                while t.get(p).is_some_and(|c| c.is_ascii_whitespace()) {
                    p += 1;
                }
                if p == q {
                    continue 'start;
                }
            }
            if t.len() < p + w.len() || !t[p..p + w.len()].eq_ignore_ascii_case(w) {
                continue 'start;
            }
            p += w.len();
        }
        if !alnum(p) {
            return true;
        }
    }
    false
}

/// The keywords of `list` found in the message's texts, in list order.
fn keyword_hits(texts: &[String], list: &[String]) -> Vec<String> {
    list.iter()
        .filter(|k| texts.iter().any(|t| keyword_in(t, k)))
        .cloned()
        .collect()
}

/// The class (§3.4).
pub fn class_of(m: &Value) -> &'static str {
    if is_squitter(m) {
        return "squitter";
    }
    let lp = node(m);
    if let Some(a) = acars_of(m) {
        let ar = a.get("arinc622");
        return if ar.is_some_and(|x| x.get("cpdlc").is_some()) {
            "cpdlc"
        } else if ar.is_some_and(|x| x.get("adsc").is_some()) {
            "adsc"
        } else if a.get("miam").is_some() {
            "miam"
        } else if a.get("media-adv").is_some() {
            "media"
        } else if a.get("ohma").is_some() {
            "ohma"
        } else {
            "acars"
        };
    }
    let ty = lp["type"]["id"].as_u64();
    match ty {
        Some(0x8F | 0xBF) => "logon",
        Some(0x4F) => "logon_resume",
        Some(0x9F | 0x5F) => "logon_confirm",
        Some(0x3F) => "logoff",
        Some(0x2F) => "logon_denied",
        Some(0x0D | 0x1D) => match lp["hfnpdu"]["type"]["id"].as_u64() {
            Some(0xD0) => "systable",
            Some(0xD1) => "perf",
            Some(0xD5) => "freq",
            Some(0xDE) => "echo",
            Some(0xD2) => "systable_req",
            _ => "other",
        },
        _ => "other",
    }
}

/// Class, severity and the matched keywords (emergency list first).
pub fn classify(m: &Value, kw: &Keywords) -> (&'static str, Severity, Vec<String>) {
    let cls = class_of(m);
    let down = is_downlink(m);
    let tx = texts(m);
    let emerg = keyword_hits(&tx, &kw.emergency);
    let caution = keyword_hits(&tx, &kw.caution);
    let cpdlc_emergency = down
        && cpdlc_of(m).is_some_and(|cp| {
            let mut it = Vec::new();
            walk_items(cp, &mut it);
            it.iter()
                .any(|(k, v)| *k == "choice" && matches!(v.as_str(), Some("dM55NULL" | "dM56NULL")))
        });
    let adsc_emergency = down
        && adsc_of(m)
            .and_then(|a| a["tags"].as_array())
            .is_some_and(|tags| {
                tags.iter().any(|t| {
                    t.as_object()
                        .and_then(|o| o.keys().next())
                        .map(String::as_str)
                        == Some("emerg_basic_report")
                })
            });
    let a = acars_of(m);
    let sev = if cpdlc_emergency || adsc_emergency || !emerg.is_empty() {
        Severity::Emergency
    } else if !caution.is_empty() {
        Severity::Caution
    } else if LINK_CLASSES.contains(&cls)
        || (cls == "acars"
            && a.is_some_and(|a| {
                matches!(str_of(a, "label"), Some("_d" | "Q0"))
                    && str_of(a, "msg_text").is_none_or(str::is_empty)
            }))
    {
        Severity::Link
    } else {
        Severity::Routine
    };
    let mut kws = emerg;
    kws.extend(caution);
    (cls, sev, kws)
}

/// The ICAO address and where it came from (`lpdu` or `table`).
pub fn icao_of(m: &Value) -> Option<(String, &'static str)> {
    let n = m.get("lpdu")?;
    if let Some(i) = n["ac_info"]["icao"].as_str() {
        return Some((i.to_string(), "lpdu"));
    }
    for side in ["src", "dst"] {
        let s = &n[side];
        if s["type"] == "Aircraft"
            && let Some(i) = s["ac_info"]["icao"].as_str()
        {
            return Some((i.to_string(), "table"));
        }
    }
    None
}

/// The ACARS registration, dots trimmed.
pub fn registration(m: &Value) -> Option<String> {
    let r = str_of(acars_of(m)?, "reg")?.trim_matches('.').trim();
    (!r.is_empty()).then(|| r.to_string())
}

/// The flight ID from ACARS, else the HFNPDU.
pub fn flight(m: &Value) -> Option<String> {
    let a = acars_of(m).and_then(|a| str_of(a, "flight")).map(str::trim);
    let h = hfnpdu_of(m)
        .and_then(|h| str_of(h, "flight_id"))
        .map(str::trim);
    a.filter(|s| !s.is_empty())
        .or(h.filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// The ground station of the message (source of an uplink or squitter,
/// destination of a downlink).
pub fn gs_id(m: &Value) -> Option<u8> {
    let n = node(m);
    let g = if n["src"]["type"] == "Ground station" {
        &n["src"]
    } else {
        &n["dst"]
    };
    g["id"].as_u64().map(|x| x as u8)
}

/// The aircraft ID of an LPDU.
pub fn ac_id(m: &Value) -> Option<u8> {
    let n = m.get("lpdu")?;
    let a = if n["src"]["type"] == "Aircraft" {
        &n["src"]
    } else {
        &n["dst"]
    };
    (a["type"] == "Aircraft")
        .then(|| a["id"].as_u64().map(|x| x as u8))
        .flatten()
}

/// Whether the message is a watched aircraft's (§4.9).
pub fn watched(m: &Value, watch: &[String]) -> bool {
    let ids: Vec<String> = [
        icao_of(m).map(|x| x.0),
        registration(m),
        acars_of(m)
            .and_then(|a| str_of(a, "flight"))
            .map(|s| s.trim().to_string()),
        hfnpdu_of(m)
            .and_then(|h| str_of(h, "flight_id"))
            .map(|s| s.trim().to_string()),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .map(|s| s.to_ascii_uppercase())
    .collect();
    watch
        .iter()
        .map(|w| w.trim().to_ascii_uppercase())
        .any(|w| !w.is_empty() && ids.contains(&w))
}

/// A reported position (`pdu-layers.md` §9).
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
    /// `perf`, `freq` or `adsc`.
    pub src: &'static str,
    /// hh:mm:ss UTC as reported (ADS-C: mm:ss past the hour).
    pub time: Option<String>,
}

fn valid(lat: f64, lon: f64) -> bool {
    lat.abs() <= 90.0 && lon.abs() <= 180.0
}

/// The position a downlink reports: performance or frequency data, else
/// an ADS-C report.
pub fn position(m: &Value) -> Option<Position> {
    if !is_downlink(m) {
        return None;
    }
    if let Some(h) = hfnpdu_of(m)
        && let Some(p) = h.get("pos")
    {
        let (lat, lon) = (p["lat"].as_f64()?, p["lon"].as_f64()?);
        if !valid(lat, lon) {
            return None;
        }
        let ty = h["type"]["id"].as_u64();
        let t = h.get("time").or_else(|| h.get("utc_time")).map(|t| {
            format!(
                "{:02}:{:02}:{:02}",
                t["hour"].as_u64().unwrap_or(0),
                t["min"].as_u64().unwrap_or(0),
                t["sec"].as_u64().unwrap_or(0)
            )
        });
        return Some(Position {
            lat,
            lon,
            src: if ty == Some(0xD5) { "freq" } else { "perf" },
            time: t,
        });
    }
    let (lat, lon, ts) = apps::adsc::position(adsc_of(m)?)?;
    Some(Position {
        lat,
        lon,
        src: "adsc",
        time: Some(format!(
            "xx:{:02}:{:02}",
            (ts as u32 / 60) % 60,
            ts as u32 % 60
        )),
    })
}

/// The application's search name.
fn app_name(cls: &str) -> Option<&'static str> {
    Some(match cls {
        "cpdlc" => "cpdlc",
        "adsc" => "ads-c",
        "miam" => "miam",
        "media" => "media advisory",
        "ohma" => "ohma",
        _ => return None,
    })
}

/// The search terms of a message (`spec/07-ui/search-terms.md`) without
/// its enrichment.
pub fn search_terms(m: &Value, freq_khz: u32, file: Option<&str>, kw: &Keywords) -> Vec<String> {
    let mut t = vec![freq_khz.to_string()];
    t.push(if is_downlink(m) { "downlink" } else { "uplink" }.into());
    let (cls, sev, kws) = classify(m, kw);
    t.push(cls.into());
    if sev >= Severity::Caution {
        t.push(sev.name().into());
    }
    t.extend(kws);
    let n = node(m);
    if let Some(g) = gs_id(m) {
        t.push(g.to_string());
        if let Some(s) = stations::station(g) {
            t.push(s.name.clone());
        }
    }
    let ac = if n["src"]["type"] == "Aircraft" {
        &n["src"]
    } else {
        &n["dst"]
    };
    if ac["type"] == "Aircraft" {
        if let Some(id) = ac["id"].as_u64() {
            t.push(id.to_string());
        }
        if let Some(i) = ac["ac_info"]["icao"].as_str() {
            t.push(i.into());
        }
    }
    if let Some(i) = n["ac_info"]["icao"].as_str() {
        t.push(i.into());
    }
    if let Some(a) = acars_of(m) {
        if let Some(r) = registration(m) {
            t.push(r);
        }
        if let Some(f) = str_of(a, "flight").map(str::trim).filter(|s| !s.is_empty()) {
            t.push(f.into());
        }
        if let Some(l) = str_of(a, "label") {
            t.push(l.into());
            if let Some(s) = str_of(a, "sublabel") {
                t.push(format!("{l}/{s}"));
            }
        }
        if let Some(x) = str_of(a, "mfi") {
            t.push(x.into());
        }
        if let Some(n) = str_of(a, "msg_num").filter(|s| !s.is_empty()) {
            t.push(format!("{n}{}", str_of(a, "msg_num_seq").unwrap_or("")));
        }
        if let Some(x) = str_of(a, "msg_text").filter(|s| !s.is_empty()) {
            t.push(x.into());
        }
        if let Some(n) = app_name(cls) {
            t.push(n.into());
        }
        if let Some(cp) = cpdlc_of(m) {
            let mut it = Vec::new();
            walk_items(cp, &mut it);
            t.extend(
                it.into_iter()
                    .filter(|(k, _)| *k == "choice_label")
                    .filter_map(|(_, v)| v.as_str().map(str::to_string)),
            );
        }
        if let Some(ad) = adsc_of(m).and_then(|a| a["tags"].as_array()) {
            for tag in ad {
                if let Some(o) = tag.as_object() {
                    for (k, v) in o {
                        t.push(k.clone());
                        if let Some(n) = v.get("name").and_then(Value::as_str) {
                            t.push(n.into());
                        }
                    }
                }
            }
        }
        for app in ["miam", "ohma"] {
            if let Some(x) = a.get(app) {
                t.extend(walk_strings(x).into_iter().map(str::to_string));
            }
        }
    }
    if let Some(f) = hfnpdu_of(m)
        .and_then(|h| str_of(h, "flight_id"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        t.push(f.into());
    }
    if let Some(f) = file {
        t.push(f.into());
    }
    t
}

/// Whether every word of `query` is a substring of one of `terms`
/// (case-insensitive ASCII).
pub fn matches(query: &str, terms: &[String]) -> bool {
    let low: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    query.split_whitespace().all(|w| {
        let w = w.to_ascii_lowercase();
        low.iter().any(|t| t.contains(&w))
    })
}

// ── alerts ─────────────────────────────────────────────────────────────────

/// An alert's kind (§4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Emergency,
    Caution,
    Watch,
}

impl AlertKind {
    pub fn name(self) -> &'static str {
        match self {
            AlertKind::Emergency => "emergency",
            AlertKind::Caution => "caution",
            AlertKind::Watch => "watch",
        }
    }
}

/// The alert settings.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertSettings {
    pub alert_on_caution: bool,
    pub watch_alerts: bool,
    pub watch: Vec<String>,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self {
            alert_on_caution: true,
            watch_alerts: true,
            watch: Vec::new(),
        }
    }
}

/// What was alerted lately (§4.7: once per message or aircraft in 30 min).
#[derive(Debug, Default)]
pub struct AlertMemory {
    recent: HashMap<String, f64>,
}

const ALERT_MEMORY_S: f64 = 1800.0;

impl AlertMemory {
    /// The alert `m` (of severity `sev`) raises at `now` (s), if any.
    pub fn alert(
        &mut self,
        m: &Value,
        sev: Severity,
        s: &AlertSettings,
        now: f64,
    ) -> Option<AlertKind> {
        let kind = if sev == Severity::Emergency {
            AlertKind::Emergency
        } else if sev == Severity::Caution && s.alert_on_caution {
            AlertKind::Caution
        } else if s.watch_alerts && watched(m, &s.watch) {
            AlertKind::Watch
        } else {
            return None;
        };
        let key = match acars_of(m) {
            Some(a) => format!(
                "acars\u{1}{}\u{1}{}\u{1}{}",
                str_of(a, "reg").unwrap_or(""),
                str_of(a, "label").unwrap_or(""),
                str_of(a, "msg_num").unwrap_or("")
            ),
            None => format!(
                "ac\u{1}{}\u{1}{}",
                icao_of(m).map(|x| x.0).unwrap_or_default(),
                kind.name()
            ),
        };
        if let Some(&t) = self.recent.get(&key)
            && now - t < ALERT_MEMORY_S
        {
            return None;
        }
        self.recent.retain(|_, t| now - *t < ALERT_MEMORY_S);
        self.recent.insert(key, now);
        Some(kind)
    }
}

// ── aircraft ───────────────────────────────────────────────────────────────

/// One aircraft of the view (§4.9).
#[derive(Debug, Clone, PartialEq)]
pub struct Aircraft {
    pub icao: Option<String>,
    pub registration: Option<String>,
    pub flight: Option<String>,
    pub messages: u32,
    pub first_t: f64,
    pub last_t: f64,
    pub last_freq_khz: u32,
    pub last_gs: Option<u8>,
    /// The last valid reported position and its time (s).
    pub position: Option<(Position, f64)>,
    /// The latest enrichment (`04-support.md` §4.10).
    pub acft: Option<AcftInfo>,
}

/// The aircraft heard, keyed by ICAO address, else registration.
#[derive(Debug, Default)]
pub struct AircraftTable {
    by_key: HashMap<String, Aircraft>,
    alias: HashMap<String, String>,
}

impl AircraftTable {
    /// Count a message at `t` (s) on `freq_khz`.
    pub fn add(&mut self, m: &Value, t: f64, freq_khz: u32) {
        if is_squitter(m) {
            return;
        }
        let icao = icao_of(m).map(|x| x.0);
        let reg = registration(m);
        let Some(key) = icao
            .clone()
            .or_else(|| reg.as_ref().and_then(|r| self.alias.get(r).cloned()))
            .or_else(|| reg.clone())
        else {
            return;
        };
        if let (Some(i), Some(r)) = (&icao, &reg) {
            if r != i
                && let Some(old) = self.by_key.remove(r)
            {
                self.by_key.entry(i.clone()).or_insert(old);
            }
            self.alias.insert(r.clone(), i.clone());
        }
        let e = self.by_key.entry(key).or_insert(Aircraft {
            icao: None,
            registration: None,
            flight: None,
            messages: 0,
            first_t: t,
            last_t: t,
            last_freq_khz: freq_khz,
            last_gs: None,
            position: None,
            acft: None,
        });
        if icao.is_some() {
            e.icao = icao;
        }
        if reg.is_some() {
            e.registration = reg;
        }
        if let Some(f) = flight(m) {
            e.flight = Some(f);
        }
        e.messages += 1;
        e.last_t = t;
        e.last_freq_khz = freq_khz;
        e.last_gs = gs_id(m).or(e.last_gs);
        if let Some(p) = position(m) {
            e.position = Some((p, t));
        }
    }

    /// Note what is known about the aircraft of message `m`.
    pub fn enrich(&mut self, m: &Value, acft: Option<&AcftInfo>) {
        let Some(a) = acft else { return };
        let key = icao_of(m).map(|x| x.0).or_else(|| {
            let r = registration(m)?;
            Some(self.alias.get(&r).cloned().unwrap_or(r))
        });
        if let Some(e) = key.and_then(|k| self.by_key.get_mut(&k)) {
            e.acft = Some(a.clone());
        }
    }

    /// Every aircraft, by (ICAO address, registration).
    pub fn list(&self) -> Vec<&Aircraft> {
        let mut v: Vec<&Aircraft> = self.by_key.values().collect();
        v.sort_by(|a, b| {
            (
                a.icao.as_deref().unwrap_or(""),
                a.registration.as_deref().unwrap_or(""),
            )
                .cmp(&(
                    b.icao.as_deref().unwrap_or(""),
                    b.registration.as_deref().unwrap_or(""),
                ))
        });
        v
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    pub fn clear(&mut self) {
        self.by_key.clear();
        self.alias.clear();
    }
}

// ── the message and its record ─────────────────────────────────────────────

/// What the session's state contributed (`03-parser.md` §3.3, stored).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stored {
    /// The joined ACARS text, on the block that completed a message.
    pub joined: Option<String>,
    /// A complete system table: (version, octets).
    pub systable: Option<(u32, Vec<u8>)>,
}

/// An airport of a route (`record.schema.json` `$defs/airport`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Airport {
    pub icao: Option<String>,
    pub iata: Option<String>,
    pub name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

impl Airport {
    fn to_json(&self, rx: Option<(f64, f64)>) -> Value {
        let mut o = serde_json::Map::new();
        for (k, v) in [
            ("icao", &self.icao),
            ("iata", &self.iata),
            ("name", &self.name),
        ] {
            if let Some(v) = v {
                o.insert(k.into(), json!(v));
            }
        }
        if let (Some(la), Some(lo)) = (self.lat, self.lon) {
            o.insert("lat".into(), coord(la));
            o.insert("lon".into(), coord(lo));
            if let Some((rl, ro)) = rx {
                let km = crate::dsc::geo::distance_km(rl, ro, la, lo);
                o.insert("km".into(), json!(round(km, 1)));
            }
        }
        Value::Object(o)
    }

    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v[k].as_str().map(str::to_string);
        let a = Self {
            icao: s("icao"),
            iata: s("iata"),
            name: s("name"),
            lat: v["lat"].as_f64(),
            lon: v["lon"].as_f64(),
        };
        (a != Self::default()).then_some(a)
    }

    /// `EGLL/LHR London Heathrow`.
    pub fn label(&self) -> String {
        let codes: Vec<&str> = [&self.icao, &self.iata]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        let mut s = codes.join("/");
        if let Some(n) = &self.name {
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(n);
        }
        s
    }

    /// Its distance from `rx` (km).
    pub fn km(&self, rx: Option<(f64, f64)>) -> Option<f64> {
        let (rl, ro) = rx?;
        Some(crate::dsc::geo::distance_km(rl, ro, self.lat?, self.lon?))
    }
}

/// What was known about the aircraft at reception (`04-support.md`
/// §4.10, record key `acft`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AcftInfo {
    pub reg: Option<String>,
    pub icao_type: Option<String>,
    pub type_: Option<String>,
    pub operator: Option<String>,
    pub airline: Option<String>,
    /// `file`, `hexdb` or `adsbdb` (none when only the airline is known).
    pub src: Option<String>,
}

impl AcftInfo {
    pub fn to_json(&self) -> Value {
        let mut o = serde_json::Map::new();
        for (k, v) in [
            ("reg", &self.reg),
            ("icao_type", &self.icao_type),
            ("type", &self.type_),
            ("operator", &self.operator),
            ("airline", &self.airline),
            ("src", &self.src),
        ] {
            if let Some(v) = v {
                o.insert(k.into(), json!(v));
            }
        }
        Value::Object(o)
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v[k].as_str().map(str::to_string);
        let a = Self {
            reg: s("reg"),
            icao_type: s("icao_type"),
            type_: s("type"),
            operator: s("operator"),
            airline: s("airline"),
            src: s("src"),
        };
        (a != Self::default()).then_some(a)
    }

    /// The search terms it adds (`search-terms.md`).
    pub fn terms(&self) -> Vec<String> {
        [&self.icao_type, &self.type_, &self.operator, &self.airline]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

/// The flight's route at reception (record key `route`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouteInfo {
    /// `hexdb`, `adsbdb` or `cpdlc`.
    pub src: String,
    pub orig: Option<Airport>,
    pub dest: Option<Airport>,
}

impl RouteInfo {
    pub fn to_json(&self, rx: Option<(f64, f64)>) -> Value {
        let mut o = json!({"src": self.src});
        if let Some(a) = &self.orig {
            o["orig"] = a.to_json(rx);
        }
        if let Some(a) = &self.dest {
            o["dest"] = a.to_json(rx);
        }
        o
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            src: v["src"].as_str()?.to_string(),
            orig: Airport::from_json(&v["orig"]),
            dest: Airport::from_json(&v["dest"]),
        })
    }

    /// The search terms it adds: the airports' codes and names.
    pub fn terms(&self) -> Vec<String> {
        [&self.orig, &self.dest]
            .into_iter()
            .flatten()
            .flat_map(|a| [&a.icao, &a.iata, &a.name])
            .flatten()
            .cloned()
            .collect()
    }
}

/// The CPDLC departure and destination airports in a message, if any.
pub fn cpdlc_airports(m: &Value) -> (Option<String>, Option<String>) {
    fn find(v: &Value, key: &str) -> Option<String> {
        match v {
            Value::Object(o) => {
                if let Some(s) = o.get(key).and_then(Value::as_str) {
                    return Some(s.trim().to_string()).filter(|s| !s.is_empty());
                }
                o.values().find_map(|x| find(x, key))
            }
            Value::Array(a) => a.iter().find_map(|x| find(x, key)),
            _ => None,
        }
    }
    if classify(m, &Keywords::default()).0 != "cpdlc" {
        return (None, None);
    }
    (find(m, "airport_dep"), find(m, "airport_dst"))
}

/// One message with its reception.
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub tree: Value,
    /// The SPDU or the LPDU with its FCS.
    pub pdu: Vec<u8>,
    /// UTC, seconds (milliseconds kept).
    pub utc: f64,
    /// Local offset at reception (s).
    pub off: i32,
    pub tsrc: TimeSource,
    pub freq_khz: u32,
    pub m1: Option<u8>,
    pub lvl: Option<f32>,
    pub noise: Option<f32>,
    pub ferr: Option<f32>,
    pub src: Option<String>,
    pub file: Option<String>,
    /// The user's location at reception.
    pub rx: Option<(f64, f64)>,
    /// Enrichment at reception (`04-support.md` §4.10).
    pub acft: Option<AcftInfo>,
    pub route: Option<RouteInfo>,
    pub stored: Stored,
    pub class: &'static str,
    pub sev: Severity,
    pub kw: Vec<String>,
}

impl Msg {
    /// A message of `tree` with its derived fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tree: Value,
        pdu: Vec<u8>,
        utc: f64,
        off: i32,
        tsrc: TimeSource,
        freq_khz: u32,
        stored: Stored,
        kw: &Keywords,
    ) -> Self {
        let (class, sev, kws) = classify(&tree, kw);
        Self {
            tree,
            pdu,
            utc,
            off,
            tsrc,
            freq_khz,
            m1: None,
            lvl: None,
            noise: None,
            ferr: None,
            src: None,
            file: None,
            rx: None,
            acft: None,
            route: None,
            stored,
            class,
            sev,
            kw: kws,
        }
    }

    /// Classify again (keyword lists changed).
    pub fn reclassify(&mut self, kw: &Keywords) {
        let (c, s, k) = classify(&self.tree, kw);
        self.class = c;
        self.sev = s;
        self.kw = k;
    }

    /// The ACARS reassembly status.
    pub fn reasm(&self) -> Option<&str> {
        acars_of(&self.tree).and_then(|a| str_of(a, "reassembly"))
    }

    /// The ACARS reassembly key (registration, label, message number).
    pub fn acars_key(&self) -> Option<String> {
        let a = acars_of(&self.tree)?;
        Some(format!(
            "{}\u{1}{}\u{1}{}",
            str_of(a, "reg").unwrap_or(""),
            str_of(a, "label").unwrap_or(""),
            str_of(a, "msg_num").unwrap_or("")
        ))
    }

    /// The distance from the user's location to the reported position (km).
    pub fn pos_km(&self) -> Option<f64> {
        let (la, lo) = self.rx?;
        let p = position(&self.tree)?;
        Some(crate::dsc::geo::distance_km(la, lo, p.lat, p.lon))
    }

    /// The ground station's distance from the user's location (km).
    pub fn gs_km(&self) -> Option<f64> {
        let (la, lo) = self.rx?;
        let s = stations::station(gs_id(&self.tree)?)?;
        Some(crate::dsc::geo::distance_km(la, lo, s.lat, s.lon))
    }
}

fn round(x: f64, d: i32) -> f64 {
    let k = 10f64.powi(d);
    (x * k).round() / k
}

/// `YYYY-MM-DDTHH:MM:SS[.mmm]Z`.
pub fn iso_utc_ms(utc: f64) -> String {
    let s = utc.floor();
    let ms = ((utc - s) * 1000.0).round() as i64;
    let (s, ms) = if ms >= 1000 { (s + 1.0, 0) } else { (s, ms) };
    let base = time::iso_utc(s as i64);
    if ms == 0 {
        base
    } else {
        format!("{}.{ms:03}Z", base.trim_end_matches('Z'))
    }
}

fn parse_iso_utc_ms(s: &str) -> Option<f64> {
    match s.split_once('.') {
        None => time::parse_iso_utc(s).map(|x| x as f64),
        Some((a, b)) => {
            let frac = b.strip_suffix('Z')?;
            if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let base = time::parse_iso_utc(&format!("{a}Z"))? as f64;
            Some(base + frac.parse::<f64>().ok()? / 10f64.powi(frac.len() as i32))
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty()
        || !s.len().is_multiple_of(2)
        || !s
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn coord(x: f64) -> Value {
    json!(round(x, 7))
}

/// The day-file line of a message (`record.schema.json`), without the
/// newline: the writer-required keys, every present value, `pdu` last.
pub fn to_line(m: &Msg) -> String {
    let mut kv: Vec<(&str, Value)> = vec![
        ("v", json!(1)),
        (
            "kind",
            json!(if is_squitter(&m.tree) {
                "squitter"
            } else {
                "lpdu"
            }),
        ),
        ("utc", json!(iso_utc_ms(m.utc))),
        ("off", json!(crate::dsc::store::ndjson::offset_text(m.off))),
        ("tsrc", json!(m.tsrc.name())),
    ];
    if let Some(s) = &m.src {
        kv.push(("src", json!(s)));
    }
    if let Some(f) = &m.file {
        kv.push(("file", json!(f)));
    }
    kv.push(("freq", json!(m.freq_khz)));
    if let Some(x) = m.m1 {
        kv.push(("m1", json!(x)));
    }
    for (k, x) in [("lvl", m.lvl), ("noise", m.noise), ("ferr", m.ferr)] {
        if let Some(x) = x.filter(|x| x.is_finite()) {
            kv.push((k, json!(round(f64::from(x), 1))));
        }
    }
    let t = &m.tree;
    if !is_squitter(t) {
        kv.push(("dir", json!(if is_downlink(t) { "down" } else { "up" })));
    }
    if let Some(g) = gs_id(t) {
        kv.push(("gs", json!(g)));
    }
    if let Some(a) = ac_id(t) {
        kv.push(("ac", json!(a)));
    }
    if let Some((i, src)) = icao_of(t) {
        kv.push(("icao", json!(i)));
        kv.push(("icao_src", json!(src)));
    }
    if let Some(r) = m.reasm() {
        kv.push(("reasm", json!(r.replace(' ', "_"))));
    }
    if let Some(j) = &m.stored.joined {
        kv.push(("joined", json!(j)));
    }
    if let Some((v, o)) = &m.stored.systable {
        kv.push(("systable", json!(hex(o))));
        kv.push(("systable_v", json!(v)));
    }
    kv.push(("class", json!(m.class)));
    kv.push(("sev", json!(m.sev.name())));
    if !m.kw.is_empty() {
        kv.push(("kw", json!(m.kw)));
    }
    if let Some(a) = acars_of(t) {
        if let Some(r) = registration(t) {
            kv.push(("reg", json!(r)));
        }
        if let Some(l) = str_of(a, "label") {
            kv.push(("label", json!(l)));
        }
        if let Some(x) = str_of(a, "msg_text").filter(|s| !s.is_empty()) {
            kv.push(("text", json!(x)));
        }
    }
    if let Some(f) = flight(t) {
        kv.push(("flight", json!(f)));
    }
    if let Some(p) = position(t) {
        let mut o = json!({"lat": round(p.lat, 7), "lon": round(p.lon, 7), "src": p.src});
        if let Some(tm) = p.time {
            o["time"] = json!(tm);
        }
        kv.push(("pos", o));
    }
    if let Some((la, lo)) = m.rx {
        kv.push(("rx", json!({"lat": coord(la), "lon": coord(lo)})));
        if let Some(k) = m.pos_km() {
            kv.push(("pos_km", json!(round(k, 3))));
        }
    }
    if let Some(s) = gs_id(t).and_then(stations::station) {
        let mut o = json!({"name": s.name, "lat": coord(s.lat), "lon": coord(s.lon)});
        if let Some(k) = m.gs_km() {
            o["km"] = json!(round(k, 1));
        }
        kv.push(("gs_st", o));
    }
    if let Some(a) = &m.acft {
        kv.push(("acft", a.to_json()));
    }
    if let Some(r) = &m.route {
        kv.push(("route", r.to_json(m.rx)));
    }
    if !m.pdu.is_empty() {
        kv.push(("pdu", json!(hex(&m.pdu))));
    }
    let body: Vec<String> = kv
        .into_iter()
        .map(|(k, v)| format!("{}:{}", json!(k), v))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// Whether a day-file line is valid (`spec/05-data/README.md`); its
/// object.
pub fn valid_line(line: &str) -> Option<serde_json::Map<String, Value>> {
    let v: Value = serde_json::from_str(line).ok()?;
    let o = v.as_object()?;
    o.get("kind")?.as_str()?;
    o.get("freq")?.as_f64()?;
    unhex(o.get("pdu")?.as_str()?)?;
    Some(o.clone())
}

/// A message from a day-file line, re-parsed with its stored context; None
/// for an invalid line.
pub fn from_line(line: &str, kw: &Keywords) -> Option<Msg> {
    let o = valid_line(line)?;
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    let n = |k: &str| o.get(k).and_then(Value::as_f64);
    let pdu = unhex(s("pdu")?)?;
    let freq_khz = n("freq")? as u32;
    let utc = s("utc").and_then(parse_iso_utc_ms).unwrap_or(0.0);
    let off = s("off")
        .and_then(crate::dsc::store::ndjson::parse_offset)
        .unwrap_or(0);
    let tsrc = match s("tsrc") {
        Some("filename") => TimeSource::Filename,
        Some("mtime") => TimeSource::Mtime,
        _ => TimeSource::Wallclock,
    };
    let lpdu = s("kind") == Some("lpdu");
    let down = s("dir") == Some("down");
    let byte = |k: &str| n(k).map(|x| x as u8);
    let parsed = pdu::Parser::stored(
        lpdu,
        &pdu,
        down,
        byte("gs").unwrap_or(0),
        byte("ac").unwrap_or(0),
        freq_khz,
        0.0,
    );
    let mut stored = Stored {
        joined: s("joined").map(str::to_string),
        systable: None,
    };
    if let (Some(st), Some(v)) = (s("systable").and_then(unhex), n("systable_v")) {
        stored.systable = Some((v as u32, st));
    }
    let key = if lpdu { "lpdu" } else { "spdu" };
    let mut tree = match parsed {
        Some(d) => json!({ key: d.tree }),
        None => json!({ key: {"err": true} }),
    };
    if lpdu {
        apply_stored(&mut tree, &o, &stored, utc);
    }
    let mut m = Msg::new(tree, pdu, utc, off, tsrc, freq_khz, stored, kw);
    m.m1 = n("m1").map(|x| x as u8);
    m.lvl = n("lvl").map(|x| x as f32);
    m.noise = n("noise").map(|x| x as f32);
    m.ferr = n("ferr").map(|x| x as f32);
    m.src = s("src").map(str::to_string);
    m.file = s("file").map(str::to_string);
    if let Some(rx) = o.get("rx") {
        m.rx = rx["lat"].as_f64().zip(rx["lon"].as_f64());
    }
    m.acft = o.get("acft").and_then(AcftInfo::from_json);
    m.route = o.get("route").and_then(RouteInfo::from_json);
    Some(m)
}

/// Put a record's stored context into its re-parsed tree: the ICAO address
/// from the table, the reassembly status, the joined text and what the
/// application makes of it, the complete system table.
fn apply_stored(tree: &mut Value, o: &serde_json::Map<String, Value>, st: &Stored, t: f64) {
    let lp = &mut tree["lpdu"];
    if o.get("icao_src").and_then(Value::as_str) == Some("table")
        && let Some(i) = o.get("icao").and_then(Value::as_str)
    {
        let side = if lp["src"]["type"] == "Aircraft" {
            "src"
        } else {
            "dst"
        };
        if lp[side].is_object() {
            lp[side]["ac_info"] = json!({"icao": i});
        }
    }
    if let Some((v, octets)) = &st.systable
        && lp["hfnpdu"].is_object()
    {
        lp["hfnpdu"]["systable_complete"] = pdu::systable_decode(octets, *v);
    }
    let down = lp["src"]["type"] == "Aircraft";
    let Some(a) = lp
        .get_mut("hfnpdu")
        .and_then(|h| h.get_mut("acars"))
        .filter(|a| a.is_object())
    else {
        return;
    };
    if let Some(r) = o.get("reasm").and_then(Value::as_str) {
        a["reassembly"] = json!(r.replace('_', " "));
    }
    if let Some(j) = &st.joined {
        a["msg_text"] = json!(j);
        let reg = a["reg"].as_str().unwrap_or("").to_string();
        let label = a["label"].as_str().unwrap_or("").to_string();
        let dir = if down { apps::Dir::Down } else { apps::Dir::Up };
        if let Some((k, v)) = apps::decode(&reg, &label, j, dir, None, t) {
            a[k] = v;
        }
    }
}

/// The joined text a decoded block carries (its reassembly completed it).
pub fn joined_of(tree: &Value) -> Option<String> {
    let a = acars_of(tree)?;
    (str_of(a, "reassembly") == Some(acars::Reasm::Complete.name()))
        .then(|| str_of(a, "msg_text").map(str::to_string))
        .flatten()
}

/// The subject column (`spec/07-ui/README.md` §Subject).
pub fn subject(m: &Msg) -> String {
    let t = &m.tree;
    let first_line = |s: &str| {
        s.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    };
    if m.sev >= Severity::Caution {
        for text in texts(t) {
            if m.kw.iter().any(|k| keyword_in(&text, k)) {
                let line = text
                    .lines()
                    .find(|l| m.kw.iter().any(|k| keyword_in(l, k)))
                    .unwrap_or(&text)
                    .trim()
                    .to_string();
                return line;
            }
        }
    }
    let a = acars_of(t);
    match m.class {
        "cpdlc" => {
            if let Some(cp) = cpdlc_of(t) {
                let mut it = Vec::new();
                walk_items(cp, &mut it);
                if let Some(l) = it
                    .into_iter()
                    .find(|(k, _)| *k == "choice_label")
                    .and_then(|(_, v)| v.as_str())
                {
                    return l.to_string();
                }
            }
        }
        "adsc" => {
            if let Some(tags) = adsc_of(t).and_then(|a| a["tags"].as_array()) {
                let mut names: Vec<String> = tags
                    .iter()
                    .filter_map(|x| x.as_object())
                    .flat_map(|o| o.iter())
                    .map(|(k, v)| {
                        v.get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| k.replace('_', " "))
                    })
                    .collect();
                if let Some(i) = names
                    .iter()
                    .position(|n| n.to_ascii_lowercase().contains("emerg"))
                {
                    let e = names.remove(i);
                    names.insert(0, e);
                }
                if !names.is_empty() {
                    return names.join(", ");
                }
            }
        }
        "media" => {
            if let Some(c) = a
                .and_then(|a| a.get("media-adv"))
                .map(|x| &x["current_link"])
            {
                let tm = &c["time"];
                return format!(
                    "{} link {} {:02}:{:02}:{:02}",
                    c["descr"].as_str().unwrap_or("?"),
                    if c["established"] == json!(true) {
                        "established"
                    } else {
                        "lost"
                    },
                    tm["hour"].as_u64().unwrap_or(0),
                    tm["min"].as_u64().unwrap_or(0),
                    tm["sec"].as_u64().unwrap_or(0)
                );
            }
        }
        _ => {}
    }
    if let Some(x) = a.and_then(|a| str_of(a, "msg_text")).and_then(first_line) {
        return x;
    }
    if let Some(p) = position(t)
        && matches!(m.class, "perf" | "freq" | "logon")
    {
        return format!(
            "Position {:.2} {} {:.2} {}",
            p.lat.abs(),
            if p.lat < 0.0 { 'S' } else { 'N' },
            p.lon.abs(),
            if p.lon < 0.0 { 'W' } else { 'E' }
        );
    }
    let n = node(t);
    match m.class {
        "squitter" => {
            let ids: Vec<String> = n["gs_status"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|g| g["gs"]["id"].as_u64())
                        .map(|x| x.to_string())
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "Squitter: GS {}; systable {}",
                ids.join(", "),
                n["systable_version"].as_u64().unwrap_or(0)
            )
        }
        c if c.starts_with("logo") => {
            let name = n["type"]["name"].as_str().unwrap_or(c);
            match icao_of(t) {
                Some((i, _)) => format!("{name} {i}"),
                None => name.to_string(),
            }
        }
        "systable" => {
            let h = &n["hfnpdu"];
            format!(
                "System table {} part {}/{}",
                h["version"].as_u64().unwrap_or(0),
                h["systable_partial"]["part_num"].as_u64().unwrap_or(0),
                h["systable_partial"]["parts_cnt"].as_u64().unwrap_or(0)
            )
        }
        c => c.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_match_whole_words_across_white_space() {
        assert!(keyword_in("ENG 1 FIRE LOOP B FAULT", "FIRE"));
        assert!(!keyword_in("CHECK FIREWALL SEAL", "FIRE"));
        assert!(keyword_in("PAN   PAN MEDICAL", "PAN PAN"));
        assert!(keyword_in("smoke in cabin", "SMOKE"));
        assert!(!keyword_in("PANPAN", "PAN PAN"));
        assert!(!keyword_in("PAN-PAN", "PAN PAN"));
        assert!(keyword_in("x/FIRE.", "FIRE"));
    }

    #[test]
    fn times_keep_milliseconds() {
        assert_eq!(iso_utc_ms(1_795_600_689.0), "2026-11-25T09:58:09Z");
        assert_eq!(iso_utc_ms(1_795_600_689.25), "2026-11-25T09:58:09.250Z");
        assert_eq!(
            parse_iso_utc_ms("2026-11-25T09:58:09.250Z"),
            Some(1_795_600_689.25)
        );
        assert_eq!(
            parse_iso_utc_ms("2026-11-25T09:58:09Z"),
            Some(1_795_600_689.0)
        );
    }
}
