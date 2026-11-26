//! dsqrust values shaped as the vectors give them (conformance/README.md
//! rule 4: names are the vocabulary).
#![allow(dead_code)]

use serde_json::{Value, json};
use shellsdr::dsc::domain::identity::Identity;
use shellsdr::dsc::message::*;

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

pub fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Routine => "routine",
        Severity::Safety => "safety",
        Severity::Urgency => "urgency",
        Severity::Distress => "distress",
    }
}

pub fn eos_name(e: Eos) -> &'static str {
    match e {
        Eos::Req => "req",
        Eos::Ack => "ack",
        Eos::Eos => "eos",
    }
}

fn identity(i: &Option<Identity>) -> Value {
    i.as_ref().map_or(
        Value::Null,
        |i| json!({"digits": i.digits, "kind": i.kind.name(), "mid": i.mid}),
    )
}

fn ecc(e: &Ecc) -> Value {
    json!({"received": e.received, "computed": e.computed, "ok": e.ok})
}

fn element(e: &Option<Element>) -> Value {
    e.as_ref().map_or(Value::Null, |e| {
        let kind = match e.kind {
            ElementKind::Freq => "freq",
            ElementKind::Hfmf => "hfmf",
            ElementKind::Vhf => "vhf",
            ElementKind::M586 => "m586",
            ElementKind::Unknown => "unknown",
        };
        json!({"kind": kind, "khz": e.khz, "seven_digit": e.seven_digit, "channel": e.channel, "vhf_mode": e.vhf_mode})
    })
}

fn expansion(x: &Option<Expansion>) -> Value {
    x.as_ref().map_or(Value::Null, |x| {
        json!({
            "lat_extra_min": x.lat_extra_min, "lon_extra_min": x.lon_extra_min,
            "precise_position": x.precise_position.map(|(lat, lon)| json!({"lat": lat, "lon": lon})),
            "position_source": x.position_source, "hdop": x.hdop, "datum": x.datum,
            "speed_kn": x.speed_kn, "course_deg": x.course_deg, "station_id": x.station_id,
            "area_lat_extra_min": x.area_lat_extra_min, "area_lon_extra_min": x.area_lon_extra_min,
            "area_dlat_extra_min": x.area_dlat_extra_min, "area_dlon_extra_min": x.area_dlon_extra_min,
            "area_speed_kn": x.area_speed_kn, "area_course_deg": x.area_course_deg,
            "persons_on_board": x.persons_on_board, "requested": x.requested, "unavailable": x.unavailable,
            "eos": x.eos.map(eos_name), "ecc": ecc(&x.ecc),
        })
    })
}

/// The parse suite's `expected` shape.
pub fn message(m: &Message) -> Value {
    json!({
        "format": format_name(m.format),
        "format_unconfirmed": m.format_unconfirmed,
        "category": m.category.map(category_name),
        "severity": severity_name(m.severity),
        "from": identity(&m.from),
        "to": identity(&m.to),
        "distress_mmsi": identity(&m.distress_mmsi),
        "distress_nature": m.distress_nature,
        "distress_telecommand": m.distress_telecommand,
        "subsequent_comm": m.subsequent_comm,
        "position": m.position.map(|p| json!({"lat": p.lat, "lon": p.lon,
            "lat_quad": p.lat_quad.to_string(), "lon_quad": p.lon_quad.to_string()})),
        "area": m.area.map(|a| json!({"corner_lat": a.corner_lat, "corner_lon": a.corner_lon,
            "lat_quad": a.lat_quad.to_string(), "lon_quad": a.lon_quad.to_string(), "d_lat": a.d_lat, "d_lon": a.d_lon})),
        "position_refused": m.position_refused,
        "event_time": m.event_time.map(|(h, mi)| format!("{h:02}:{mi:02}")),
        "tc1": m.tc1,
        "tc2": m.tc2,
        "freq_channel": m.freq_channel.map(|f| json!({"rx": element(&f.rx), "tx": element(&f.tx)})),
        "number": m.number,
        "duration_s": m.duration_s,
        "expansion": expansion(&m.expansion),
        "eos": m.eos.map(eos_name),
        "self_cancel": m.self_cancel,
        "ecc": ecc(&m.ecc),
        "garbled_count": m.garbled_count,
        "content_id": m.content_id,
    })
}

/// Symbols of a case input.
pub fn symbols(v: &Value) -> Option<Vec<u8>> {
    v.as_array()?
        .iter()
        .map(|x| x.as_u64().and_then(|n| u8::try_from(n).ok()))
        .collect()
}

fn station_info(v: &Value) -> Option<shellsdr::dsc::record::StationInfo> {
    use shellsdr::dsc::record::*;
    let o = v.as_object()?;
    let source = match v["pos_source"].as_str() {
        Some("builtin") => PosSource::Builtin,
        Some("dsc") => PosSource::Dsc,
        Some("aprs.fi") => PosSource::AprsFi,
        _ => PosSource::Csv,
    };
    let position = (v["has_pos"].as_bool() == Some(true)).then(|| StationPos {
        lat: v["lat"].as_f64().unwrap_or_default(),
        lon: v["lon"].as_f64().unwrap_or_default(),
        source,
        time: v["pos_time"].as_i64(),
    });
    let _ = o;
    Some(StationInfo {
        name: v["name"].as_str().map(String::from),
        country: v["country"].as_str().map(String::from),
        position,
        km: v["dist_km"].as_f64(),
    })
}

/// A case `record` (the shared input shape of the presentation, alerts and
/// ndjson-write suites) as a dsqrust Record. Message fields come from the
/// symbols, not from `parsed`.
pub fn record(v: &Value) -> Option<shellsdr::dsc::record::Record> {
    use shellsdr::dsc::record::*;
    let tsrc = match v["time_source"].as_str()? {
        "filename" => TimeSource::Filename,
        "mtime" => TimeSource::Mtime,
        _ => TimeSource::Wallclock,
    };
    let mut r = Record::new(
        symbols(&v["symbols"])?,
        v["utc_epoch"].as_i64()?,
        v["utc_offset"].as_i64()? as i32,
        tsrc,
    );
    r.version = v["schema_version"].as_u64().unwrap_or(1) as u8;
    r.source_label = v["source_label"].as_str().map(String::from);
    r.file = v["source_file"].as_str().map(String::from);
    r.rx = v["rx_location"]
        .as_object()
        .and_then(|o| Some((o.get("lat")?.as_f64()?, o.get("lon")?.as_f64()?)));
    r.pos_km = v["position_dist_km"].as_f64();
    r.snr = v["snr_db"].as_f64();
    r.from_st = station_info(&v["from_info"]);
    r.to_st = station_info(&v["to_info"]);
    r.dmmsi_st = station_info(&v["distress_info"]);
    Some(r)
}

fn station_info_json(st: &Option<shellsdr::dsc::record::StationInfo>) -> Value {
    st.as_ref().map_or(Value::Null, |s| {
        let p = s.position;
        json!({"name": s.name, "country": s.country, "has_pos": p.is_some(),
            "lat": p.map(|p| p.lat), "lon": p.map(|p| p.lon), "pos_source": p.map(|p| p.source.name()),
            "pos_time": p.and_then(|p| p.time), "dist_km": s.km})
    })
}

/// A dsqrust Record in the vectors' record shape (ndjson-read).
pub fn record_json(r: &shellsdr::dsc::record::Record) -> Value {
    json!({
        "symbols": r.symbols, "schema_version": r.version, "utc_epoch": r.utc, "utc_offset": r.offset,
        "time_source": r.time_source.name(), "source_label": r.source_label.clone().unwrap_or_default(),
        "source_file": r.file, "rx_location": r.rx.map(|(lat, lon)| json!({"lat": lat, "lon": lon})),
        "position_dist_km": r.pos_km, "snr_db": r.snr, "from_info": station_info_json(&r.from_st),
        "to_info": station_info_json(&r.to_st), "distress_info": station_info_json(&r.dmmsi_st),
        "parsed": message(&r.message),
    })
}

/// The config-merge suite's settings shape.
pub fn settings(v: &Value) -> Option<shellsdr::dsc::settings::Settings> {
    use shellsdr::dsc::settings::*;
    let loc = &v["location"];
    Some(Settings {
        ndjson_dir: v["ndjson_dir"].as_str()?.into(),
        save_dir: v["save_dir"].as_str()?.into(),
        default_tone: v["default_tone"].as_f64()?,
        location: Location {
            set: loc["set"].as_bool()?,
            lat: loc["lat"].as_f64()?,
            lon: loc["lon"].as_f64()?,
        },
        online_lookup: v["online_lookup"].as_bool()?,
        lookup_endpoint: v["lookup_endpoint"].as_str()?.into(),
        lookup_apikey: v["lookup_apikey"].as_str()?.into(),
        alert_bell: v["alert_bell"].as_bool()?,
        channels: v["channels"]
            .as_array()?
            .iter()
            .map(|c| {
                Some(ChannelConf {
                    label: c["label"].as_str()?.into(),
                    device: c["device"].as_str()?.into(),
                    tone: c["tone"].as_f64()?,
                })
            })
            .collect::<Option<_>>()?,
    })
}

pub fn settings_json(s: &shellsdr::dsc::settings::Settings) -> Value {
    json!({
        "ndjson_dir": s.ndjson_dir, "save_dir": s.save_dir, "default_tone": s.default_tone,
        "online_lookup": s.online_lookup, "lookup_endpoint": s.lookup_endpoint, "lookup_apikey": s.lookup_apikey,
        "alert_bell": s.alert_bell,
        "location": {"set": s.location.set, "lat": s.location.lat, "lon": s.location.lon},
        "channels": s.channels.iter().map(|c| json!({"label": c.label, "device": c.device, "tone": c.tone})).collect::<Vec<_>>(),
    })
}
