//! One record as one NDJSON line (seed spec/05-data/record.schema.json).

use crate::dsc::domain::identity::Identity;
use crate::dsc::message::{Category, Element, ElementKind, Eos, Format};
use crate::dsc::record::{PosSource, Record, StationInfo, StationPos, TimeSource};
use crate::dsc::time;
use serde_json::{Map, Value};

/// The record as one line, without the newline (STO-02). Writes the keys
/// of `x-writer-required` and every other key whose value is present;
/// never a key for an absent value.
pub fn to_line(r: &Record) -> String {
    let m = &r.message;
    let mut w = Obj::default();
    w.raw("v", r.version.to_string());
    w.str("id", &m.content_id);
    w.str("utc", &time::iso_utc(r.utc));
    w.str("off", &offset_text(r.offset));
    w.str("tsrc", r.time_source.name());
    if let Some(s) = &r.source_label {
        w.str("src", s);
    }
    if let Some(f) = &r.file {
        w.str("file", f);
    }
    w.str("fmt", format_name(m.format));
    if let Some(c) = m.category {
        w.str("cat", category_name(c));
    }
    identity(&mut w, "from", &m.from);
    identity(&mut w, "to", &m.to);
    identity(&mut w, "dmmsi", &m.distress_mmsi);
    symbol(&mut w, "nature", m.distress_nature);
    symbol(&mut w, "dtc", m.distress_telecommand);
    symbol(&mut w, "subc", m.subsequent_comm);
    if let Some(p) = m.position {
        let lq = p.lat_quad.to_string();
        let oq = p.lon_quad.to_string();
        w.raw(
            "pos",
            format!(
                "{{\"lat\":{},\"lon\":{},\"lq\":{},\"oq\":{}}}",
                coord(p.lat),
                coord(p.lon),
                q(&lq),
                q(&oq)
            ),
        );
    }
    if let Some(a) = m.area {
        w.raw(
            "area",
            format!(
                "{{\"lat\":{},\"lon\":{},\"dlat\":{},\"dlon\":{}}}",
                coord(f64::from(a.corner_lat)),
                coord(f64::from(a.corner_lon)),
                a.d_lat,
                a.d_lon
            ),
        );
    }
    if let Some((h, mi)) = m.event_time {
        w.str("etime", &format!("{h:02}:{mi:02}"));
    }
    symbol(&mut w, "tc1", m.tc1);
    symbol(&mut w, "tc2", m.tc2);
    if let Some(fc) = m.freq_channel {
        if let Some(e) = fc.rx {
            w.raw("rxe", element(&e));
        }
        if let Some(e) = fc.tx {
            w.raw("txe", element(&e));
        }
    }
    if m.position_refused {
        w.raw("posref", "true".into());
    }
    if let Some(n) = &m.number {
        w.str("num", n);
    }
    if m.format_unconfirmed {
        w.raw("fmtunc", "true".into());
    }
    if let Some((lat, lon)) = m.expansion.as_ref().and_then(|x| x.precise_position) {
        w.raw(
            "ppos",
            format!("{{\"lat\":{},\"lon\":{}}}", coord(lat), coord(lon)),
        );
    }
    if let Some(e) = m.eos {
        w.str("eos", eos_name(e));
    }
    if m.self_cancel {
        w.raw("selfcancel", "true".into());
    }
    if let Some((lat, lon)) = r.rx {
        w.raw(
            "rx",
            format!("{{\"lat\":{},\"lon\":{}}}", coord(lat), coord(lon)),
        );
    }
    if let Some(km) = r.pos_km {
        w.raw("pos_km", format!("{km:.3}"));
    }
    station(&mut w, "from_st", &r.from_st);
    station(&mut w, "to_st", &r.to_st);
    station(&mut w, "dmmsi_st", &r.dmmsi_st);
    w.raw("ecc_rx", m.ecc.received.map_or(-1, i32::from).to_string());
    w.raw("ecc_cc", m.ecc.computed.map_or(-1, i32::from).to_string());
    w.raw("ecc_ok", m.ecc.ok.to_string());
    w.raw("errs", m.garbled_count.to_string());
    if let Some(snr) = r.snr.filter(|v| v.is_finite()) {
        w.raw("snr", number(snr));
    }
    let sym: Vec<String> = r.symbols.iter().map(u8::to_string).collect();
    w.raw("sym", format!("[{}]", sym.join(",")));
    w.finish()
}

/// A line read back, or `None` if it is not a valid record (STO-04). All
/// message fields are re-derived from `sym`; only `x-stored` keys are taken
/// from the line (STO-05). Known keys of the wrong type count as absent.
pub fn from_line(line: &str) -> Option<Record> {
    let v: Value = serde_json::from_str(line).ok()?;
    let o = v.as_object()?;
    let symbols: Vec<u8> = o
        .get("sym")?
        .as_array()?
        .iter()
        .map(|x| x.as_u64().filter(|&n| n <= 255).map(|n| n as u8))
        .collect::<Option<_>>()?;
    if symbols.is_empty() {
        return None;
    }
    let utc = o
        .get("utc")
        .and_then(Value::as_str)
        .and_then(time::parse_iso_utc)
        .unwrap_or(0);
    let offset = o
        .get("off")
        .and_then(Value::as_str)
        .and_then(parse_offset)
        .unwrap_or(0);
    let tsrc = match o.get("tsrc").and_then(Value::as_str) {
        Some("filename") => TimeSource::Filename,
        Some("mtime") => TimeSource::Mtime,
        _ => TimeSource::Wallclock,
    };
    let mut r = Record::new(symbols, utc, offset, tsrc);
    r.version = match o.get("v").and_then(Value::as_u64) {
        Some(2) => 2,
        _ => 1,
    };
    let text = |k: &str| o.get(k).and_then(Value::as_str).map(String::from);
    r.source_label = text("src");
    r.file = text("file");
    r.rx = o.get("rx").and_then(lat_lon);
    r.pos_km = o.get("pos_km").and_then(Value::as_f64);
    r.from_st = o.get("from_st").and_then(read_station);
    r.to_st = o.get("to_st").and_then(read_station);
    r.dmmsi_st = o.get("dmmsi_st").and_then(read_station);
    Some(r)
}

fn lat_lon(v: &Value) -> Option<(f64, f64)> {
    Some((v.get("lat")?.as_f64()?, v.get("lon")?.as_f64()?))
}

fn read_station(v: &Value) -> Option<StationInfo> {
    let o: &Map<String, Value> = v.as_object()?;
    let source = match o.get("psrc").and_then(Value::as_str) {
        Some("builtin") => PosSource::Builtin,
        Some("dsc") => PosSource::Dsc,
        Some("aprs.fi") => PosSource::AprsFi,
        _ => PosSource::Csv,
    };
    let position = lat_lon(v).map(|(lat, lon)| StationPos {
        lat,
        lon,
        source,
        time: o.get("ptime").and_then(Value::as_i64),
    });
    Some(StationInfo {
        name: o.get("name").and_then(Value::as_str).map(String::from),
        country: o.get("country").and_then(Value::as_str).map(String::from),
        position,
        km: o.get("km").and_then(Value::as_f64),
    })
}

/// `±HH:MM` to seconds.
pub fn parse_offset(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    if b.len() != 6 || b[3] != b':' || !(b[0] == b'+' || b[0] == b'-') {
        return None;
    }
    let h: i32 = s.get(1..3)?.parse().ok()?;
    let m: i32 = s.get(4..6)?.parse().ok()?;
    let secs = h * 3600 + m * 60;
    Some(if b[0] == b'-' { -secs } else { secs })
}

/// Local - UTC as `±HH:MM`.
pub fn offset_text(offset: i32) -> String {
    let (sign, o) = if offset < 0 {
        ('-', -offset)
    } else {
        ('+', offset)
    };
    format!("{sign}{:02}:{:02}", o / 3600, o / 60 % 60)
}

/// The schema's `fmt` vocabulary.
pub fn format_name(f: Format) -> &'static str {
    match f {
        Format::GeographicArea => "area",
        Format::Distress => "distress",
        Format::CommonInterest => "common",
        Format::AllShips => "allships",
        Format::Individual => "individual",
        Format::IndividualAuto => "individual_auto",
        Format::Vts => "vts",
        Format::Unknown => "unknown",
    }
}

/// The schema's `cat` vocabulary.
pub fn category_name(c: Category) -> &'static str {
    match c {
        Category::Routine => "routine",
        Category::Vts => "vts",
        Category::Acs => "acs",
        Category::Safety => "safety",
        Category::Urgency => "urgency",
        Category::Distress => "distress",
        Category::Unknown => "unknown",
    }
}

fn eos_name(e: Eos) -> &'static str {
    match e {
        Eos::Req => "req",
        Eos::Ack => "ack",
        Eos::Eos => "eos",
    }
}

fn identity(w: &mut Obj, key: &str, id: &Option<Identity>) {
    if let Some(id) = id {
        w.str(key, &id.digits);
    }
}

fn symbol(w: &mut Obj, key: &str, s: Option<u8>) {
    if let Some(s) = s {
        w.raw(key, s.to_string());
    }
}

fn element(e: &Element) -> String {
    let k = match e.kind {
        ElementKind::Freq => "freq",
        ElementKind::Hfmf => "hfmf",
        ElementKind::Vhf => "vhf",
        ElementKind::M586 => "m586",
        ElementKind::Unknown => "unknown",
    };
    let mut o = Obj::default();
    o.str("k", k);
    if let Some(khz) = e.khz {
        o.raw("khz", number(khz));
    }
    if let Some(ch) = e.channel {
        o.raw("ch", ch.to_string());
    }
    if let Some(m) = e.vhf_mode {
        o.raw("m", m.to_string());
    }
    o.finish()
}

fn station(w: &mut Obj, key: &str, st: &Option<StationInfo>) {
    let Some(st) = st else { return };
    let mut o = Obj::default();
    if let Some(n) = &st.name {
        o.str("name", n);
    }
    if let Some(c) = &st.country {
        o.str("country", c);
    }
    if let Some(p) = st.position {
        o.raw("lat", coord(p.lat));
        o.raw("lon", coord(p.lon));
        o.str("psrc", p.source.name());
        if let Some(t) = p.time {
            o.raw("ptime", t.to_string());
        }
    }
    if let Some(km) = st.km {
        o.raw("km", format!("{km:.3}"));
    }
    w.raw(key, o.finish());
}

/// A coordinate with 6 decimal places (F-09).
fn coord(v: f64) -> String {
    format!("{v:.6}")
}

/// A finite number in its shortest exact form.
fn number(v: f64) -> String {
    serde_json::Number::from_f64(v).map_or_else(|| "null".into(), |n| n.to_string())
}

fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// A JSON object written key by key, in insertion order.
#[derive(Default)]
struct Obj {
    out: String,
}

impl Obj {
    fn raw(&mut self, key: &str, value: String) {
        self.out.push(if self.out.is_empty() { '{' } else { ',' });
        self.out.push_str(&q(key));
        self.out.push(':');
        self.out.push_str(&value);
    }

    fn str(&mut self, key: &str, value: &str) {
        self.raw(key, q(value));
    }

    fn finish(mut self) -> String {
        if self.out.is_empty() {
            self.out.push('{');
        }
        self.out.push('}');
        self.out
    }
}
