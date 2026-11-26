//! MIAM (HFDL seed `applications.md` §4), in libacars' JSON names.

use std::io::Read;

use serde_json::{Value, json};

use crate::hfdl::acars::{Fragment, Tables};

const ERR_PDU_TYPE: i64 = 1 << 1;
const ERR_VERSION: i64 = 1 << 2;
const ERR_HDR_TRUNC: i64 = 1 << 3;
const ERR_APP_TYPE: i64 = 1 << 4;
const ERR_BODY_TRUNC: i64 = 1 << 16;
const ERR_INFLATE: i64 = 1 << 17;
const ERR_COMPR: i64 = 1 << 18;
const ERR_CRC: i64 = 1 << 19;
const FILE_TIMEOUT: f64 = 900.0;

/// Raw deflate (RFC 1951): the output, and whether the stream ended well.
pub fn inflate_raw(b: &[u8]) -> (Vec<u8>, bool) {
    let mut d = flate2::read::DeflateDecoder::new(b);
    let mut out = Vec::new();
    let ok = d.read_to_end(&mut out).is_ok();
    (out, ok)
}

pub fn is_printable(b: &[u8]) -> bool {
    !b.is_empty()
        && b.iter()
            .all(|&c| (7..=13).contains(&c) || (32..=126).contains(&c))
}

fn crc32_bzip2(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    !crc
}

/// Base85 groups (`z` = 4 zero octets); leftover characters ignored.
fn base85(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut p = 0;
    while s.len() - p >= 5 || (p < s.len() && s[p] == b'z') {
        if s[p] == b'z' {
            out.extend_from_slice(&[0, 0, 0, 0]);
            p += 1;
            continue;
        }
        let mut v: u32 = 0;
        for i in 0..5 {
            v = v
                .wrapping_mul(85)
                .wrapping_add((s[p + i] as u32).wrapping_sub(0x21));
        }
        out.extend_from_slice(&v.to_be_bytes());
        p += 5;
    }
    out
}

fn message(data: &[u8], err_body: i64) -> Value {
    let mut m = json!({});
    if !data.is_empty() {
        if is_printable(data) {
            m["text"] = json!(String::from_utf8_lossy(data));
        } else {
            m["octet_string"] = json!(data);
        }
    }
    if err_body != 0 {
        m["err"] = json!(err_body);
    }
    m
}

fn app_object(app_type: u8, app_id: &[u8], v2: bool, msg: Value) -> (String, Value) {
    let s = |r: std::ops::Range<usize>| String::from_utf8_lossy(&app_id[r]).into_owned();
    match app_type {
        0..=2 => {
            let mut a = json!({"label": s(0..2)});
            if app_type >= 1 {
                a["sublabel"] = json!(s(2..4));
            }
            if app_type == 2 {
                a["mfi"] = json!(s(4..6));
            }
            a["message"] = msg;
            ("acars".into(), a)
        }
        4..=7 | 0xd if v2 => ("".into(), msg),
        _ => (
            "non_acars".into(),
            json!({"app_id": String::from_utf8_lossy(app_id), "message": msg}),
        ),
    }
}

fn body_of(compression: u8, body: Option<&[u8]>, err: &mut i64) -> Vec<u8> {
    match body {
        Some(b) if !b.is_empty() => {
            if compression == 1 {
                let (out, ok) = inflate_raw(b);
                if !ok {
                    *err |= ERR_INFLATE;
                }
                out
            } else if compression == 0 {
                b.to_vec()
            } else {
                *err |= ERR_COMPR;
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// The core PDU (`applications.md` §4.2); None: not MIAM.
fn core(txt: &[u8]) -> Option<Value> {
    if txt.len() < 3 {
        return None;
    }
    let (bpad, hpad) = (txt[0], txt[1]);
    if !(matches!(bpad, b'0'..=b'3' | b'-' | b'.')) || !(b'0'..=b'3').contains(&hpad) {
        return None;
    }
    let hpad = (hpad - b'0') as usize;
    let rest = &txt[2..];
    let delim = rest.iter().position(|&c| c == b'|')?;
    if delim == 0 {
        return None;
    }
    let mut header = base85(&rest[..delim]);
    if header.len() < hpad {
        return None;
    }
    let body_txt = &rest[delim + 1..];
    let body: Option<Vec<u8>> = if body_txt.is_empty() {
        None
    } else if bpad.is_ascii_digit() {
        let mut b = base85(body_txt);
        let pad = (bpad - b'0') as usize;
        if b.len() >= pad {
            b.truncate(b.len() - pad);
        }
        Some(b)
    } else if bpad == b'-' {
        Some(body_txt.to_vec())
    } else {
        None
    };
    header.truncate(header.len() - hpad);
    let h = &header;
    let version = h[0] & 0x0F;
    let pdu_type = h[0] >> 4;
    if version != 1 && version != 2 {
        return Some(json!({"err": ERR_VERSION}));
    }
    if pdu_type > 3 {
        return Some(json!({"err": ERR_PDU_TYPE}));
    }
    let mut v = json!({"version": version, "pdu_type": pdu_type});
    let (key, val) = match (version, pdu_type) {
        (_, 2) | (_, 3) => {
            let k = if pdu_type == 2 {
                "aloha"
            } else {
                "aloha_reply"
            };
            if h.len() < 13 {
                (k, json!({"err": ERR_HDR_TRUNC}))
            } else {
                let len = ((h[1] as u32) << 16) | ((h[2] as u32) << 8) | h[3] as u32;
                let comp: Vec<Value> = (0..8)
                    .filter(|i| h[11] >> i & 1 == 1)
                    .map(|i| {
                        if i == 0 {
                            json!("deflate")
                        } else {
                            json!(1 << i)
                        }
                    })
                    .collect();
                let names = ["ACARS", "IP Middleware", "TCP/IP", "Satcom Data 3", "UDP"];
                let nets: Vec<Value> = (0..8)
                    .filter(|i| h[12] >> i & 1 == 1)
                    .map(|i| {
                        if i < 5 {
                            json!(names[i])
                        } else {
                            json!(1 << i)
                        }
                    })
                    .collect();
                (
                    k,
                    json!({"pdu_len": len, "aircraft_id": String::from_utf8_lossy(&h[4..11]),
                           if pdu_type == 2 { "comp_supported" } else { "comp_selected" }: comp, "networks_supported": nets}),
                )
            }
        }
        (1, 0) => ("data", v1_data(h, body.as_deref())),
        (2, 0) => ("data", v2_data(h, body.as_deref())),
        (1, 1) => {
            if h.len() < 20 {
                ("ack", json!({"err": ERR_HDR_TRUNC}))
            } else {
                (
                    "ack",
                    json!({"pdu_len": ((h[1] as u32) << 16) | ((h[2] as u32) << 8) | h[3] as u32,
                               "aircraft_id": String::from_utf8_lossy(&h[4..11]),
                               "msg_ack_num": (h[11] >> 1) & 0x7F, "ack_xfer_result": (h[12] >> 4) & 0x0F}),
                )
            }
        }
        _ => {
            if h.len() < 8 {
                ("ack", json!({"err": ERR_HDR_TRUNC}))
            } else {
                (
                    "ack",
                    json!({"msg_ack_num": (h[1] >> 1) & 0x7F, "ack_xfer_result": ((h[1] << 3) | (h[2] >> 5)) & 0x0F}),
                )
            }
        }
    };
    v[key] = val;
    Some(v)
}

fn v1_data(h: &[u8], body: Option<&[u8]>) -> Value {
    if h.len() < 20 {
        return json!({"err": ERR_HDR_TRUNC});
    }
    let len = ((h[1] as u32) << 16) | ((h[2] as u32) << 8) | h[3] as u32;
    let mut err_body = 0;
    if len as usize > h.len() + body.map_or(0, |b| b.len()) {
        err_body |= ERR_BODY_TRUNC;
    }
    let msg_num = (h[11] >> 1) & 0x7F;
    let ack = h[11] & 1 == 1;
    let compression = ((h[12] << 2) | ((h[13] >> 6) & 3)) & 7;
    let encoding = (h[13] >> 4) & 3;
    let app_type = h[13] & 0x0F;
    let id_len = match app_type {
        0 => 2,
        1 => 4,
        2 | 3 => 6,
        _ => return json!({"err": ERR_APP_TYPE}),
    };
    let p = 14;
    if h.len() < p + id_len + 4 {
        return json!({"err": ERR_HDR_TRUNC});
    }
    let app_id = &h[p..p + id_len];
    let crc = u32::from_be_bytes([
        h[p + id_len],
        h[p + id_len + 1],
        h[p + id_len + 2],
        h[p + id_len + 3],
    ]);
    let data = body_of(compression, body, &mut err_body);
    if body.is_some_and(|b| !b.is_empty()) && crc32_bzip2(&data) != crc {
        err_body |= ERR_CRC;
    }
    let (k, a) = app_object(app_type, app_id, false, message(&data, err_body));
    let mut v = json!({"pdu_len": len, "aircraft_id": String::from_utf8_lossy(&h[4..11]), "msg_num": msg_num,
                       "ack_required": ack, "compression": compression, "encoding": encoding, "app_type": app_type});
    v[k] = a;
    v
}

fn v2_data(h: &[u8], body: Option<&[u8]>) -> Value {
    if h.len() < 7 {
        return json!({"err": ERR_HDR_TRUNC});
    }
    let msg_num = (h[1] >> 1) & 0x7F;
    let ack = h[1] & 1 == 1;
    let compression = ((h[2] << 2) | ((h[3] >> 6) & 3)) & 7;
    let encoding = (h[3] >> 4) & 3;
    let app_type = h[3] & 0x0F;
    let id_len = match app_type {
        0 => 2,
        1 => 4,
        2 | 3 => 6,
        t if t & 8 != 0 && t != 0xd => ((t & 7) + 1) as usize,
        _ => return json!({"err": ERR_APP_TYPE}),
    };
    let p = 4;
    if h.len() < p + id_len + 2 {
        return json!({"err": ERR_HDR_TRUNC});
    }
    let app_id = &h[p..p + id_len];
    let crc = u16::from_be_bytes([h[p + id_len], h[p + id_len + 1]]);
    let mut err_body = 0;
    let data = body_of(compression, body, &mut err_body);
    if body.is_some_and(|b| !b.is_empty()) && super::crc16_ccitt(&data, 0xFFFF) != crc {
        err_body |= ERR_CRC;
    }
    let (k, a) = app_object(app_type, app_id, true, message(&data, err_body));
    let mut v = json!({"msg_num": msg_num, "ack_required": ack, "compression": compression, "encoding": encoding, "app_type": app_type});
    if k.is_empty() {
        v["message"] = a;
    } else {
        v[k] = a;
    }
    v
}

fn digits(s: &[u8], n: usize) -> Option<u32> {
    if s.len() < n || !s[..n].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(s[..n].iter().fold(0, |a, &c| a * 10 + (c - b'0') as u32))
}

fn chomped_len(s: &[u8]) -> usize {
    let mut n = s.len();
    while n > 0 && (s[n - 1] == b'\r' || s[n - 1] == b'\n') {
        n -= 1;
    }
    n
}

/// A MIAM convergence frame (`applications.md` §4.1); None: not MIAM.
pub fn parse(reg: &str, txt: &str, tables: Option<&mut Tables>, t: f64) -> Option<Value> {
    let b = txt.as_bytes();
    let (&fid, rest) = b.split_first()?;
    let body = match fid {
        b'T' => json!({"single_transfer": {"miam_core": core(rest)?}})
            .as_object()?
            .clone(),
        b'F' => {
            if chomped_len(rest) != 21 {
                return None;
            }
            let file_id = digits(rest, 3)?;
            let size = digits(&rest[3..], 6)?;
            let d = &rest[9..];
            if !d[..12].iter().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let n = |i: usize| ((d[i] - b'0') * 10 + (d[i + 1] - b'0')) as u32;
            let (yy, mo, dd, hh, mi, ss) = (n(0), n(2), n(4), n(6), n(8), n(10));
            if mo > 12 || dd > 31 || hh > 23 || mi > 59 || ss > 59 {
                return None;
            }
            if let Some(tb) = tables {
                let key = format!("{reg}\u{1}{file_id}");
                tb.miam.add(
                    &key,
                    &Fragment {
                        data: &[],
                        seq: 0,
                        seq_first: Some(0),
                        seq_wrap: None,
                        is_final: false,
                        total_len: size as usize,
                        total_cnt: 0,
                        t,
                        timeout: FILE_TIMEOUT,
                        out_of_order: false,
                    },
                );
            }
            json!({"file_transfer_request": {"file_id": file_id, "file_size": size,
                "complete_until_datetime": {"date": {"year": 2000 + yy, "month": mo, "day": dd},
                                            "time": {"hour": hh, "minute": mi, "second": ss}}}})
            .as_object()?
            .clone()
        }
        b'K' => {
            if chomped_len(rest) != 10 {
                return None;
            }
            let file_id = digits(rest, 3)?;
            let seg = (rest[3] as char)
                .to_digit(16)
                .filter(|_| rest[3].is_ascii_digit() || (b'A'..=b'F').contains(&rest[3]))?;
            json!({"file_transfer_accept": {"file_id": file_id, "segment_size": seg,
                "on_ground_seg_temp_secs": digits(&rest[4..], 3)?, "in_flight_seg_temp_secs": digits(&rest[7..], 3)?}})
                .as_object()?
                .clone()
        }
        b'S' => {
            let file_id = digits(rest, 3)?;
            let seg = digits(&rest[3..], 3)?;
            let data = &rest[6..];
            let mut v = json!({"file_id": file_id, "segment_id": seg});
            let mut payload: Option<Vec<u8>> = Some(data.to_vec());
            if let Some(tb) = tables {
                let key = format!("{reg}\u{1}{file_id}");
                let (st, joined) = tb.miam.add(
                    &key,
                    &Fragment {
                        data,
                        seq: seg as i64,
                        seq_first: Some(0),
                        seq_wrap: None,
                        is_final: false,
                        total_len: 0,
                        total_cnt: 0,
                        t,
                        timeout: FILE_TIMEOUT,
                        out_of_order: false,
                    },
                );
                payload = match st {
                    crate::hfdl::acars::Reasm::Complete => joined,
                    crate::hfdl::acars::Reasm::InProgress
                    | crate::hfdl::acars::Reasm::Duplicate => None,
                    _ => Some(data.to_vec()),
                };
            }
            if let Some(p) = payload
                && let Some(c) = core(&p)
            {
                v["miam_core"] = c;
            }
            json!({"file_segment": v}).as_object()?.clone()
        }
        b'A' => {
            if chomped_len(rest) != 4 || !rest[3].is_ascii_digit() {
                return None;
            }
            json!({"file_transfer_abort": {"file_id": digits(rest, 3)?, "reason": rest[3] - b'0'}})
                .as_object()?
                .clone()
        }
        b'Y' | b'X' => {
            let need = if fid == b'Y' { 3 } else { 9 };
            if chomped_len(rest) != need {
                return None;
            }
            let file_id = digits(rest, 3).or_else(|| (&rest[..3] == b"FFF").then_some(0xFFF))?;
            let mut v = json!({"all_files": file_id == 0xFFF});
            if file_id != 0xFFF {
                v["file_id"] = json!(file_id);
            }
            if fid == b'X' {
                v["on_ground_seg_temp_secs"] = json!(digits(&rest[3..], 3)?);
                v["in_flight_seg_temp_secs"] = json!(digits(&rest[6..], 3)?);
                json!({"file_xon_ind": v}).as_object()?.clone()
            } else {
                json!({"file_xoff_ind": v}).as_object()?.clone()
            }
        }
        _ => return None,
    };
    Some(Value::Object(body))
}
