//! ACARS applications (HFDL seed `spec/03-domain/applications.md`): which
//! label tries which, ARINC 622 and media advisory here; ADS-C, CPDLC,
//! MIAM and OHMA in their modules. Trees in libacars' JSON names.

pub mod adsc;
pub mod cpdlc;
pub mod miam;
pub mod ohma;

use serde_json::{Value, json};

use super::acars::Tables;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
}

/// Decode the text of a message with `label` (`acars.md` §4): the first
/// application that recognises it, as (key, tree).
pub fn decode(
    reg: &str,
    label: &str,
    txt: &str,
    dir: Dir,
    tables: Option<&mut Tables>,
    t: f64,
) -> Option<(String, Value)> {
    let l = label.as_bytes();
    let tries: &[&str] = match (l.first(), l.get(1)) {
        (Some(b'A'), Some(b'6' | b'A')) | (Some(b'B'), Some(b'6' | b'A')) => &["arinc"],
        (Some(b'H'), Some(b'1')) => &["arinc", "miam", "ohma"],
        (Some(b'M'), Some(b'A')) => &["miam"],
        (Some(b'S'), Some(b'A')) => &["media"],
        _ => &[],
    };
    let mut tables = tables;
    // the sublabel and MFI of H1 are removed before ARINC 622 (libacars
    // passes the text after them); MIAM and OHMA get the same text
    for app in tries {
        let r = match *app {
            "arinc" => arinc622(txt, dir).map(|v| ("arinc622".to_string(), v)),
            "miam" => {
                miam::parse(reg, txt, tables.as_deref_mut(), t).map(|v| ("miam".to_string(), v))
            }
            "ohma" => {
                ohma::parse(reg, txt, tables.as_deref_mut(), t).map(|v| ("ohma".to_string(), v))
            }
            "media" => Some(("media-adv".to_string(), media_adv(txt))),
            _ => None,
        };
        if r.is_some() {
            return r;
        }
    }
    None
}

const IMIS: [(&str, &str); 6] = [
    (".AT1", "fans1a_cpdlc_msg"),
    (".CR1", "fans1a_cpdlc_connect_request"),
    (".CC1", "fans1a_cpdlc_connect_confirm"),
    (".DR1", "fans1a_cpdlc_disconnect_request"),
    (".ADS", "adsc_msg"),
    (".DIS", "adsc_disconnect_request"),
];

/// CRC-16 polynomial 0x1021, not reflected, initial `init` (CCITT-FALSE).
pub fn crc16_ccitt(data: &[u8], init: u16) -> u16 {
    let mut crc = init;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Hex digits to octets, stopping at the first non-hex character (the
/// octet it falls in is dropped); an odd last digit is ignored (libacars
/// `la_slurp_hexstring`).
pub fn slurp_hex(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in s.as_chunks::<2>().0 {
        match (
            (pair[0] as char).to_digit(16),
            (pair[1] as char).to_digit(16),
        ) {
            (Some(a), Some(b)) => out.push((a * 16 + b) as u8),
            _ => break,
        }
    }
    out
}

/// ARINC 622 (`applications.md` §1): None when the text is not one.
pub fn arinc622(txt: &str, dir: Dir) -> Option<Value> {
    let t = txt.strip_prefix('/').unwrap_or(txt);
    let (pos, imi, key) = IMIS
        .iter()
        .find_map(|(s, k)| t.find(s).map(|p| (p, &s[1..], *k)))?;
    if !(pos == 7 || pos == 4)
        || !t[..pos]
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    let gs_addr = &t[..pos];
    let payload = &t.as_bytes()[pos + 1..];
    if payload.len() < 3 + 7 + 4 {
        return None;
    }
    let air = String::from_utf8_lossy(&payload[3..10]).into_owned();
    let bin = slurp_hex(&payload[10..]);
    let mut crcbuf = payload[..10].to_vec();
    crcbuf.extend_from_slice(&bin);
    let crc_ok = crc16_ccitt(&crcbuf, 0xFFFF) == 0x1D0F;
    let body: &[u8] = if bin.len() >= 2 {
        &bin[..bin.len() - 2]
    } else {
        &[]
    };
    let mut v = json!({"msg_type": key, "crc_ok": crc_ok, "gs_addr": gs_addr, "air_addr": air});
    let short = bin.len() < 2;
    match imi {
        "AT1" | "CR1" | "CC1" | "DR1" => {
            v["cpdlc"] = cpdlc::parse(body, dir);
        }
        "ADS" | "DIS" => {
            v["adsc"] = if short {
                json!({})
            } else {
                adsc::parse(body, dir, imi == "DIS")
            };
        }
        _ => {}
    }
    Some(v)
}

fn link_descr(c: u8) -> Option<&'static str> {
    Some(match c {
        b'V' => "VHF ACARS",
        b'S' => "Default SATCOM",
        b'H' => "HF",
        b'G' => "Global Star Satcom",
        b'C' => "ICO Satcom",
        b'2' => "VDL2",
        b'X' => "Inmarsat Aero H/H+/I/L",
        b'I' => "Iridium Satcom",
        _ => return None,
    })
}

/// Media advisory (`applications.md` §5).
pub fn media_adv(txt: &str) -> Value {
    let b = txt.as_bytes();
    let err = json!({"err": true});
    if b.len() < 10 || b[0] != b'0' || !(b[1] == b'E' || b[1] == b'L') || link_descr(b[2]).is_none()
    {
        return err;
    }
    if !b[3..9].iter().all(|c| c.is_ascii_digit()) {
        return err;
    }
    let n = |i: usize| ((b[i] - b'0') * 10 + (b[i + 1] - b'0')) as i64;
    let (h, mi, s) = (n(3), n(5), n(7));
    if h > 23 || mi > 59 || s > 59 {
        return err;
    }
    let mut links = Vec::new();
    let mut i = 9;
    while i < b.len() && b[i] != b'/' {
        match link_descr(b[i]) {
            Some(d) => links.push(json!({"code": (b[i] as char).to_string(), "descr": d})),
            None => return err,
        }
        i += 1;
    }
    let mut v = json!({
        "err": false, "version": 0,
        "current_link": {"code": (b[2] as char).to_string(), "descr": link_descr(b[2]), "established": b[1] == b'E',
                         "time": {"hour": h, "min": mi, "sec": s}},
        "links_avail": links
    });
    if i < b.len() && i + 1 < b.len() {
        v["text"] = json!(&txt[i + 1..]);
    }
    v
}
