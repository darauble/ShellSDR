//! ADS-C (HFDL seed `applications.md` §2), in libacars' JSON names.

use serde_json::{Map, Value, json};

use super::Dir;

/// MSB-first bit reader.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    fn read(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.b[self.pos / 8];
            v = (v << 1) | ((byte >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        v
    }
}

fn signed(v: u32, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((v as i64) << shift) >> shift
}

fn coord(v: u32) -> f64 {
    (180.0 - 90.0 / 2f64.powi(19)) * signed(v, 21) as f64 / 0xFFFFF as f64
}

fn angle(v: u32, bits: u32, lsb_exp: i32, max: f64) -> f64 {
    let r = (180.0 - 90.0 / 2f64.powi(lsb_exp)) * signed(v, bits) as f64 / max;
    if r < 0.0 { r + 360.0 } else { r }
}

fn alt(v: u32) -> i64 {
    signed(v, 16) * 4
}

const ACCURACY: [f64; 8] = [-1.0, 30.0, 15.0, 8.0, 4.0, 1.0, 0.25, 0.05];

fn basic_report(b: &[u8]) -> Option<(Value, usize)> {
    if b.len() < 10 {
        return None;
    }
    let mut r = Bits::new(b);
    let lat = coord(r.read(21));
    let lon = coord(r.read(21));
    let a = alt(r.read(16));
    let ts = r.read(15) as f64 * 0.125;
    let f = r.read(7);
    Some((
        json!({"lat": lat, "lon": lon, "alt": a, "ts_sec": ts, "pos_accuracy_nm": ACCURACY[((f >> 1) & 7) as usize],
               "nav_redundancy": f & 1 == 1, "tcas_avail": (f >> 4) & 1 == 1}),
        10,
    ))
}

/// Parse one downlink tag's body: (json key, value, octets used).
fn downlink_tag(tag: u8, b: &[u8]) -> Option<(&'static str, Value, usize)> {
    let need = |n: usize| if b.len() < n { None } else { Some(()) };
    Some(match tag {
        3 => {
            need(1)?;
            ("ack", json!({"contract_num": b[0]}), 1)
        }
        4 => {
            need(2)?;
            if b[1] > 13 {
                return None;
            }
            let mut v = json!({"contract_req_num": b[0], "reason": b[1]});
            let mut used = 2;
            if matches!(b[1], 1 | 2 | 7) {
                need(3)?;
                v[if b[1] == 7 { "err_tag" } else { "err_octet" }] = json!(b[2]);
                used = 3;
            }
            ("nack", v, used)
        }
        5 => {
            need(2)?;
            let (n, mut p) = (b[1] as usize, 2usize);
            let mut groups = Vec::new();
            for i in 0..n {
                if b.len() < p + 2 {
                    return None;
                }
                let (gtag, f) = (b[p], b[p + 1]);
                let unrec = f & 0x80 != 0;
                let whole = f & 0x40 != 0;
                let cause = if unrec {
                    "group_unrecognized"
                } else if whole {
                    "group_unavailable"
                } else {
                    "params_unavailable"
                };
                let mut g = json!({"noncomp_tag": gtag, "noncomp_cause": cause});
                p += 2;
                if !unrec && !whole {
                    let cnt = (f & 0x0F) as usize;
                    let octets = cnt / 2 + cnt % 2;
                    if b.len() < p + octets {
                        return None;
                    }
                    let params: Vec<u8> = (0..cnt)
                        .map(|k| (b[p + k / 2] >> if k % 2 == 0 { 4 } else { 0 }) & 0x0F)
                        .collect();
                    g["params"] = json!(params);
                    p += octets;
                }
                groups.push(g);
                if p == b.len() && i + 1 < n {
                    return None;
                }
            }
            (
                "noncomp_notify",
                json!({"contract_req_num": b[0], "msg_groups": groups}),
                p,
            )
        }
        6 => ("cancel_emergency_mode", json!({}), 0),
        7 | 9 | 10 | 18 | 19 | 20 => {
            let (v, n) = basic_report(b)?;
            let key = match tag {
                7 => "basic_report",
                9 => "emerg_basic_report",
                10 => "lat_dev_change_event",
                18 => "vspd_chg_event",
                19 => "alt_range_event",
                _ => "wpt_change_event",
            };
            (key, v, n)
        }
        12 => {
            need(6)?;
            let mut r = Bits::new(b);
            let id: String = (0..8)
                .map(|_| {
                    let c = r.read(6) as u8;
                    (if c & 0x20 == 0 { c + 0x40 } else { c }) as char
                })
                .collect();
            ("flight_id", json!({"flight_id": id}), 6)
        }
        13 => {
            need(17)?;
            let mut r = Bits::new(b);
            let (la, lo, al, eta) = (
                coord(r.read(21)),
                coord(r.read(21)),
                alt(r.read(16)),
                r.read(14),
            );
            let (la2, lo2, al2) = (coord(r.read(21)), coord(r.read(21)), alt(r.read(16)));
            (
                "predicted_route",
                json!({"next_wpt": {"lat": la, "lon": lo, "alt": al, "eta_sec": eta},
                                      "next_next_wpt": {"lat": la2, "lon": lo2, "alt": al2}}),
                17,
            )
        }
        14 | 15 => {
            need(5)?;
            let mut r = Bits::new(b);
            let inv = r.read(1) == 1;
            let hdg = angle(r.read(12), 12, 10, 0x7FF as f64);
            let spd = r.read(13) as f64 / 2.0;
            let vs = signed(r.read(12), 12) * 16;
            if tag == 14 {
                (
                    "earth_ref_data",
                    json!({"true_trk_deg": hdg, "true_trk_valid": !inv, "gnd_spd_kts": spd, "vspd_ftmin": vs}),
                    5,
                )
            } else {
                (
                    "air_ref_data",
                    json!({"true_hdg_deg": hdg, "true_hdg_valid": !inv, "spd_mach": spd / 1000.0, "vspd_ftmin": vs as f64}),
                    5,
                )
            }
        }
        16 => {
            need(4)?;
            let mut r = Bits::new(b);
            let ws = r.read(9) as f64 / 2.0;
            let inv = r.read(1) == 1;
            let wd = angle(r.read(9), 9, 7, 0xFF as f64);
            let t = (512.0 - 256.0 / 2f64.powi(10)) * signed(r.read(12), 12) as f64 / 0x7FF as f64;
            (
                "meteo_data",
                json!({"wind_spd_kts": ws, "wind_dir_true_deg": wd, "wind_dir_valid": !inv, "temp_c": t}),
                4,
            )
        }
        17 => {
            need(3)?;
            (
                "airframe_id",
                json!({"icao_id": ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32}),
                3,
            )
        }
        22 => {
            need(8)?;
            let mut r = Bits::new(b);
            let d = r.read(16) as f64 / 8.0;
            let inv = r.read(1) == 1;
            let trk = angle(r.read(12), 12, 10, 0x7FF as f64);
            let (al, eta) = (alt(r.read(16)), r.read(14));
            (
                "intermediate_projection",
                json!({"dist_nm": d, "true_trk_deg": trk, "true_trk_valid": !inv, "alt": al, "eta_sec": eta}),
                8,
            )
        }
        23 => {
            need(9)?;
            let mut r = Bits::new(b);
            let (la, lo, al, eta) = (
                coord(r.read(21)),
                coord(r.read(21)),
                alt(r.read(16)),
                r.read(14),
            );
            (
                "fixed_projection",
                json!({"lat": la, "lon": lo, "alt": al, "eta_sec": eta}),
                9,
            )
        }
        _ => return None,
    })
}

fn request_tag(tag: u8, b: &[u8]) -> Option<(&'static str, Value, usize)> {
    let one = |k: &'static str, f: &dyn Fn(u8) -> Value| -> Option<(&'static str, Value, usize)> {
        if b.is_empty() {
            None
        } else {
            Some((k, f(b[0]), 1))
        }
    };
    match tag {
        10 => one(
            "report_when_lateral_dev_exceeds",
            &|x| json!({"lat_dev_treshold_nm": x as f64 / 8.0}),
        ),
        11 => one("report_interval", &|x| {
            let sf = match x >> 6 {
                2 => 8,
                3 => 64,
                s => s as i64,
            };
            json!({"interval_secs": sf * ((x & 0x3F) as i64 + 1)})
        }),
        12 => one("flight_id", &|x| json!({"modulus": x})),
        13 => one("predicted_route", &|x| json!({"modulus": x})),
        14 => one("earth_ref_data", &|x| json!({"modulus": x})),
        15 => one("air_ref_data", &|x| json!({"modulus": x})),
        16 => one("meteo_data", &|x| json!({"modulus": x})),
        17 => one("airframe_id", &|x| json!({"modulus": x})),
        18 => one("report_when_vspd_is", &|x| {
            let v = (x as i8) as i64 * 64;
            json!({"vspd_ftmin_threshold": v.abs(), "higher_than": v >= 0})
        }),
        19 => {
            if b.len() < 4 {
                return None;
            }
            let c = alt(((b[0] as u32) << 8) | b[1] as u32);
            let f = alt(((b[2] as u32) << 8) | b[3] as u32);
            Some((
                "report_when_alt_out_of_range",
                json!({"floor_alt": f, "ceiling_alt": c}),
                4,
            ))
        }
        20 => Some(("report_wpt_changes", json!({}), 0)),
        21 => {
            if b.len() < 2 {
                return None;
            }
            Some((
                "acft_intent_data",
                json!({"modulus": b[0], "proj_time_mins": b[1]}),
                2,
            ))
        }
        _ => None,
    }
}

fn uplink_tag(tag: u8, b: &[u8]) -> Option<(&'static str, Value, usize)> {
    match tag {
        1 => Some(("cancel_all_contracts", json!({}), 0)),
        2 | 6 => {
            if b.is_empty() {
                return None;
            }
            Some((
                if tag == 2 {
                    "cancel_contract"
                } else {
                    "cancel_emergency_mode"
                },
                json!({"contract_num": b[0]}),
                1,
            ))
        }
        7..=9 => {
            if b.is_empty() {
                return None;
            }
            let mut p = 1;
            let mut groups = Vec::new();
            while p < b.len() {
                if !(10..=21).contains(&b[p]) {
                    break;
                }
                let (k, v, n) = request_tag(b[p], &b[p + 1..])?;
                let mut o = Map::new();
                o.insert(k.into(), v);
                groups.push(Value::Object(o));
                p += 1 + n;
            }
            let key = match tag {
                7 => "periodic_contract_req",
                8 => "event_contract_req",
                _ => "emerg_periodic_contract_req",
            };
            let mut v = json!({"contract_num": b[0]});
            if !groups.is_empty() {
                v["groups"] = json!(groups);
            }
            Some((key, v, p))
        }
        _ => None,
    }
}

/// An ADS-C message (`ADS`) or disconnect (`DIS`) body.
pub fn parse(b: &[u8], dir: Dir, dis: bool) -> Value {
    if dis {
        if b.is_empty() {
            return json!({"err": true});
        }
        return json!({"tags": [{"reason": {"reason_code": b[0] >> 4}}], "err": false});
    }
    let mut tags = Vec::new();
    let mut p = 0;
    let mut err = false;
    while p < b.len() {
        let tag = b[p];
        let r = if dir == Dir::Down {
            downlink_tag(tag, &b[p + 1..])
        } else {
            uplink_tag(tag, &b[p + 1..])
        };
        match r {
            Some((k, v, n)) => {
                let mut o = Map::new();
                o.insert(k.into(), v);
                tags.push(Value::Object(o));
                p += 1 + n;
            }
            None => {
                err = true;
                break;
            }
        }
    }
    if tags.is_empty() && !err && b.is_empty() {
        return json!({});
    }
    json!({"tags": tags, "err": err})
}

/// The first valid position of a message's ADS-C report tags, with the
/// report's seconds past the hour (`pdu-layers.md` §9).
pub fn position(adsc: &Value) -> Option<(f64, f64, f64)> {
    for t in adsc["tags"].as_array()? {
        for k in [
            "basic_report",
            "emerg_basic_report",
            "lat_dev_change_event",
            "vspd_chg_event",
            "alt_range_event",
            "wpt_change_event",
        ] {
            if let Some(r) = t.get(k) {
                let (la, lo) = (r["lat"].as_f64()?, r["lon"].as_f64()?);
                if la.abs() <= 90.0 && lo.abs() <= 180.0 {
                    return Some((la, lo, r["ts_sec"].as_f64().unwrap_or(0.0)));
                }
            }
        }
    }
    None
}
