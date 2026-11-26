//! Feeds each case's `input` to the matching function of dsqrust and
//! returns the result shaped like `expected`. `None`: no adapter yet.

use serde_json::Value;

use serde_json::json;

pub fn actual(suite: &str, case: &Value, suite_json: &Value) -> Option<Value> {
    let input = &case["input"];
    match suite {
        "codewords" => codewords(input),
        "audio-decode" => audio_decode(input),
        "detail" => detail(input),
        "mmsi-lookup" => mmsi_lookup(input),
        "alerts" => alerts(input),
        "search" => search(input, suite_json),
        "list-row" => list_row(input),
        "file-name-time" => file_name_time(input),
        "config-merge" => {
            let f = |k: &str| crate::shape::settings(&input[k]);
            Some(crate::shape::settings_json(
                &shellsdr::dsc::settings::merge(&f("base")?, &f("mine")?, &f("disk")?),
            ))
        }
        "ndjson-write" => {
            let line = shellsdr::dsc::store::ndjson::to_line(&crate::shape::record(input)?);
            // "the line MUST NOT contain a newline": an unparsable line fails.
            serde_json::from_str::<Value>(&line)
                .ok()
                .filter(|_| !line.contains('\n'))
                .map(|v| json!({"line": v}))
                .or(Some(json!({"line": line})))
        }
        "ndjson-read" => Some(
            match shellsdr::dsc::store::ndjson::from_line(input["line"].as_str()?) {
                Some(r) => json!({"accepted": true, "record": crate::shape::record_json(&r)}),
                None => json!({"accepted": false}),
            },
        ),
        "distance" => {
            let f = |k: &str| input[k].as_f64();
            let km =
                shellsdr::dsc::geo::distance_km(f("lat1")?, f("lon1")?, f("lat2")?, f("lon2")?);
            Some(json!({"km": km, "nm": shellsdr::dsc::geo::km_to_nm(km)}))
        }
        "parse-location" => Some(
            match shellsdr::dsc::geo::parse_location(input["text"].as_str()?) {
                Some((lat, lon)) => json!({"valid": true, "lat": lat, "lon": lon}),
                None => json!({"valid": false}),
            },
        ),
        "iso8601" => {
            let (utc, off) = (
                input["utc_epoch"].as_i64()?,
                input["utc_offset"].as_i64()? as i32,
            );
            Some(
                json!({"utc": shellsdr::dsc::time::iso_utc(utc), "local": shellsdr::dsc::time::iso_local(utc, off)}),
            )
        }
        "day-file" => {
            let (utc, off) = (
                input["utc_epoch"].as_i64()?,
                input["utc_offset"].as_i64()? as i32,
            );
            Some(
                json!({"local_date": shellsdr::dsc::time::local_date(utc, off),
                        "file_name": shellsdr::dsc::time::day_file_name(utc, off)}),
            )
        }
        "search-terms" => Some(
            json!({"terms": shellsdr::dsc::presentation::search_terms(&crate::shape::record(&input["record"])?)}),
        ),
        "parse" => Some(crate::shape::message(
            &shellsdr::dsc::decoder::parser::parse(&crate::shape::symbols(&input["symbols"])?),
        )),
        _ => None,
    }
}

fn codewords(input: &Value) -> Option<Value> {
    let code = shellsdr::dsc::domain::codeword::encode(input["symbol"].as_u64()? as u8);
    let bits: String = (0..10)
        .map(|i| if code >> i & 1 == 1 { '1' } else { '0' })
        .collect();
    Some(json!({"codeword": code, "bits_in_transmission_order": bits}))
}

fn mmsi_lookup(input: &Value) -> Option<Value> {
    use shellsdr::dsc::domain::{identity, mid};
    let id = identity::classify(input["mmsi"].as_str()?);
    // The vectors give an unknown country as "".
    let country = id.mid.and_then(mid::administrations).unwrap_or_default();
    let builtin = shellsdr::dsc::stations::builtin(&id.digits).map(|b| b.name.clone());
    Some(
        json!({"country": country, "kind": id.kind.name(), "mid": id.mid, "is_coast": id.is_coast(),
        "builtin_station": builtin}),
    )
}

fn alerts(input: &Value) -> Option<Value> {
    let r = crate::shape::record(&input["record"])?;
    let alert = shellsdr::dsc::alert::decide(&r.message, input["alert_on_urgency"].as_bool()?);
    if !alert {
        return Some(json!({"alert": false}));
    }
    let (summary, body) = shellsdr::dsc::presentation::alert_text(&r);
    Some(json!({"alert": true, "summary": summary, "body": body}))
}

fn list_row(input: &Value) -> Option<Value> {
    let l = shellsdr::dsc::presentation::list_row(&crate::shape::record(input)?);
    Some(
        json!({"date": l.date, "time": l.time, "src": l.source, "fmt": l.format, "cat": l.category,
        "from": l.from, "to": l.to, "subj": l.subject, "ec": l.ecc, "sev": l.severity.to_string()}),
    )
}

fn search(input: &Value, suite_json: &Value) -> Option<Value> {
    use shellsdr::dsc::presentation::{distress_only, matches, search_terms};
    let query = input["query"].as_str()?;
    let only = input["distress_only"].as_bool()?;
    let mut ids = Vec::new();
    for m in suite_json["messages"].as_array()? {
        let r = crate::shape::record(&m["record"])?;
        if (!only || distress_only(&r)) && matches(query, &search_terms(&r)) {
            ids.push(m["id"].clone());
        }
    }
    Some(json!({"matching_ids": ids}))
}

/// Rule 7: the case's time zone is the local zone.
pub fn zone(input: &Value) -> Option<shellsdr::dsc::time::TimeZone> {
    shellsdr::dsc::time::TimeZone::get(input["time_zone"].as_str()?).ok()
}

fn file_name_time(input: &Value) -> Option<Value> {
    use shellsdr::dsc::time::*;
    Some(
        match base_time_from_name(input["file_name"].as_str()?, &zone(input)?) {
            Some((utc, off)) => json!({"found": true, "utc_epoch": utc, "utc_offset": off,
            "utc_iso": iso_utc(utc), "local_iso": iso_local(utc, off)}),
            None => json!({"found": false}),
        },
    )
}

/// Decode a WAV file of the conformance directory, as a WAV channel does.
fn audio_decode(input: &Value) -> Option<Value> {
    let path = crate::suites::conformance_dir().join(input["file"].as_str()?);
    let tz = zone(input)?;
    let found =
        shellsdr::dsc::decoder::decode_file(&path, input["center_tone_hz"].as_f64()?, false, &tz)
            .ok()?;
    let messages: Vec<Value> = found
        .iter()
        .map(|d| {
            json!({"symbols": d.symbols, "parsed": crate::shape::message(&shellsdr::dsc::decoder::parser::parse(&d.symbols)),
                   "utc_epoch": d.utc, "utc_offset": d.offset, "time_source": d.time_source.name()})
        })
        .collect();
    Some(json!({"message_count": messages.len(), "messages": messages}))
}

/// The detail view's fields (informative suite).
fn detail(input: &Value) -> Option<Value> {
    use shellsdr::dsc::presentation::detail;
    let r = crate::shape::record(&input["record"])?;
    let loc = &input["current_location"];
    let location = loc["lat"].as_f64().zip(loc["lon"].as_f64());
    let stations = shellsdr::dsc::stations::Stations::new();
    let fields: Vec<Value> = detail::fields(
        &r,
        &detail::Current {
            location,
            stations: &stations,
        },
    )
    .into_iter()
    .map(|(label, value)| json!({"label": label, "value": value}))
    .collect();
    Some(json!({"fields": fields}))
}
