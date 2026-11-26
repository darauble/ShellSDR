//! HFDL PDU layers and the session parser (HFDL seed `pdu-layers.md`,
//! `04-algorithms/03-parser.md`): frame octets -> messages, with the AC ID
//! table, the system table and the reassembly tables. Trees in dumphfdl's
//! JSON names.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::acars::{self, Tables};
use super::stations;

/// The HFDL FCS (CRC-16/X-25) over `b[..n]` against the two octets after it.
pub fn fcs_ok(b: &[u8], n: usize) -> bool {
    if b.len() < n + 2 {
        return false;
    }
    let c = acars::crc16_reflected(&b[..n], 0xFFFF) ^ 0xFFFF;
    b[n] as u16 | (b[n + 1] as u16) << 8 == c
}

/// How much of a frame checks out: its FCS-valid parts (the squitter, or
/// the MPDU header and each LPDU), 0 for none. Picks the better of two
/// demodulations of the same frame.
pub fn frame_quality(b: &[u8]) -> u32 {
    if b.is_empty() {
        return 0;
    }
    if b[0] & 1 == 0 {
        return (b.len() >= 66 && fcs_ok(b, 64)) as u32;
    }
    let Some((hdr_len, groups)) = mpdu_layout(b) else {
        return 0;
    };
    if !fcs_ok(b, hdr_len) {
        return 0;
    }
    let mut p = hdr_len + 2;
    let mut q = 1;
    for len in groups.into_iter().flat_map(|(_, lens)| lens) {
        if p + len > b.len() {
            break;
        }
        q += (len >= 2 && fcs_ok(&b[p..p + len], len - 2)) as u32;
        p += len;
    }
    q
}

/// An MPDU's header length and, per aircraft, its ID and LPDU lengths in
/// order, if the header fits in the frame (its FCS not yet checked).
#[allow(clippy::type_complexity)]
fn mpdu_layout(b: &[u8]) -> Option<(usize, Vec<(u8, Vec<usize>)>)> {
    if b[0] & 2 != 0 {
        let n = ((b[0] >> 2) & 0xF) as usize;
        if b.len() < 6 + n + 2 {
            return None;
        }
        Some((
            6 + n,
            vec![(b[2], (0..n).map(|i| b[6 + i] as usize + 1).collect())],
        ))
    } else {
        let count = (((b[0] & 0x70) >> 4) + 1) as usize;
        let (mut h, mut groups) = (2, Vec::new());
        for _ in 0..count {
            if b.len() < h + 2 {
                return None;
            }
            let m = (b[h + 1] >> 4) as usize;
            if b.len() < h + 2 + m {
                return None;
            }
            groups.push((b[h], (0..m).map(|i| b[h + 2 + i] as usize + 1).collect()));
            h += 2 + m;
        }
        Some((h, groups))
    }
}

/// A 20-bit two's complement coordinate in degrees.
pub fn coord(v: u32) -> f64 {
    let s = ((v << 12) as i32) >> 12;
    s as f64 * 180.0 / 0x7FFFF as f64
}

fn rev8(b: u8) -> u8 {
    b.reverse_bits()
}

fn icao_of(b: &[u8]) -> u32 {
    ((rev8(b[0]) as u32) << 16) | ((rev8(b[1]) as u32) << 8) | rev8(b[2]) as u32
}

fn u16le(b: &[u8]) -> u32 {
    b[0] as u32 | (b[1] as u32) << 8
}

/// One message (`03-parser.md` §3.2) as decoded: the tree and what it
/// carries for the session.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// `spdu` or `lpdu`, and its tree.
    pub key: &'static str,
    pub tree: Value,
    /// The LPDU's own octets (for the day file), or the SPDU's.
    pub octets: Vec<u8>,
    pub downlink: bool,
    pub gs_id: u8,
    pub ac_id: Option<u8>,
    /// The ICAO address the LPDU carries itself.
    pub icao_in_pdu: Option<u32>,
    /// A complete system table this message completed: (version, octets).
    pub systable: Option<(u32, Vec<u8>)>,
}

/// The AC ID table (`pdu-layers.md` §8, F-H06).
#[derive(Debug, Default)]
pub struct AcTable {
    fwd: HashMap<(u32, u8), (u32, f64)>,
    inv: HashMap<u32, (u32, u8)>,
}

const AC_TTL: f64 = 3600.0;

impl AcTable {
    pub fn create(&mut self, freq: u32, id: u8, icao: u32, t: f64) {
        if let Some((old, _)) = self.fwd.remove(&(freq, id)) {
            self.inv.remove(&old);
        }
        if let Some(k) = self.inv.remove(&icao) {
            self.fwd.remove(&k);
        }
        self.fwd.insert((freq, id), (icao, t));
        self.inv.insert(icao, (freq, id));
    }
    pub fn delete(&mut self, freq: u32, icao: u32) {
        if let Some(&(f, id)) = self.inv.get(&icao)
            && f == freq
        {
            self.inv.remove(&icao);
            self.fwd.remove(&(f, id));
        }
    }
    pub fn lookup(&mut self, freq: u32, id: u8, t: f64) -> Option<u32> {
        let &(icao, created) = self.fwd.get(&(freq, id))?;
        if t - created > AC_TTL {
            self.fwd.remove(&(freq, id));
            self.inv.remove(&icao);
            return None;
        }
        Some(icao)
    }
}

/// System table parts being collected (`pdu-layers.md` §7.4).
#[derive(Debug, Default)]
pub struct SystableAsm {
    version: Option<u32>,
    parts: Vec<Option<Vec<u8>>>,
}

/// The session parser's state (`03-parser.md` §3.1).
#[derive(Debug, Default)]
pub struct Parser {
    pub ac: AcTable,
    pub systable: SystableAsm,
    pub tables: Tables,
}

fn gs_obj(id: u8) -> Value {
    let mut v = json!({"type": "Ground station", "id": id});
    if let Some(s) = stations::station(id) {
        v["name"] = json!(s.name);
    }
    v
}

fn freq_list(gs: u8, mask: u32) -> Value {
    Value::Array(
        (0..20)
            .filter(|i| mask >> i & 1 == 1)
            .map(|i| {
                let mut o = json!({"id": i});
                if let Some(f) = stations::frequency_khz(gs, i as usize) {
                    o["freq"] = json!(f);
                }
                o
            })
            .collect(),
    )
}

impl Parser {
    /// Parse one message read back from a day file (`spec/05-data/`): an
    /// SPDU, or an LPDU with its MPDU context, by a parser of its own so the
    /// session's tables are never touched. None: the octets do not parse.
    pub fn stored(
        lpdu: bool,
        b: &[u8],
        downlink: bool,
        gs: u8,
        ac_id: u8,
        freq_khz: u32,
        t: f64,
    ) -> Option<Decoded> {
        let mut p = Parser::default();
        if lpdu {
            p.lpdu(b, downlink, gs, ac_id, freq_khz * 1000, t)
                .filter(|d| d.tree["err"] != json!(true))
        } else {
            p.spdu(b)
        }
    }

    /// Parse one frame's octets on `freq_khz` at time `t` (s): its messages.
    pub fn frame(&mut self, b: &[u8], freq_khz: u32, t: f64) -> Vec<Decoded> {
        if b.is_empty() {
            return Vec::new();
        }
        if b[0] & 1 == 0 {
            return self.spdu(b).into_iter().collect();
        }
        self.mpdu(b, freq_khz, t)
    }

    fn spdu(&mut self, b: &[u8]) -> Option<Decoded> {
        if b.len() < 66 || !fcs_ok(b, 64) {
            return None;
        }
        let src = b[1] & 0x7F;
        let st = [
            (
                src,
                b[1] & 0x80 != 0,
                (b[54] >> 4) as u32 | (b[55] as u32) << 4 | (b[56] as u32) << 12,
            ),
            (
                b[57] & 0x7F,
                b[57] & 0x80 != 0,
                b[58] as u32 | (b[59] as u32) << 8 | ((b[60] & 0xF) as u32) << 16,
            ),
            (
                (b[60] >> 4) | (b[61] & 7) << 4,
                b[61] & 8 != 0,
                (b[61] >> 4) as u32 | (b[62] as u32) << 4 | (b[63] as u32) << 12,
            ),
        ];
        let notes = [
            "None",
            "Channel down",
            "Upcoming frequency change",
            "Ground station down",
        ];
        let tree = json!({
            "err": false, "src": gs_obj(src), "spdu_version": (b[0] >> 2) & 3, "rls": b[0] & 2 != 0, "iso": b[0] & 0x20 != 0,
            "change_note": notes[((b[0] & 0xC0) >> 6) as usize],
            "frame_index": b[2] as u32 | ((b[3] & 0xF) as u32) << 8, "frame_offset": b[3] >> 4,
            "min_priority": b[52] & 0xF, "systable_version": b[53] as u32 | ((b[54] & 0xF) as u32) << 8,
            "gs_status": st.iter().map(|(id, sync, f)| json!({"gs": gs_obj(*id), "utc_sync": sync, "freqs": freq_list(*id, *f)})).collect::<Vec<_>>()
        });
        Some(Decoded {
            key: "spdu",
            tree,
            octets: b[..66].to_vec(),
            downlink: false,
            gs_id: src,
            ac_id: None,
            icao_in_pdu: None,
            systable: None,
        })
    }

    fn mpdu(&mut self, b: &[u8], freq_khz: u32, t: f64) -> Vec<Decoded> {
        let downlink = b[0] & 2 != 0;
        let Some((hdr_len, groups)) = mpdu_layout(b) else {
            return Vec::new();
        };
        let gs = b[1] & 0x7F;
        if !fcs_ok(b, hdr_len) {
            return Vec::new();
        }
        let freq = freq_khz * 1000;
        let mut p = hdr_len + 2;
        let mut out = Vec::new();
        'groups: for (ac_id, lens) in groups {
            for len in lens {
                if p + len > b.len() {
                    break 'groups;
                }
                // an LPDU that fails its FCS or is cut short is counted, not a message
                if let Some(d) = self.lpdu(&b[p..p + len], downlink, gs, ac_id, freq, t)
                    && d.tree["err"] != json!(true)
                {
                    out.push(d);
                }
                p += len;
            }
        }
        // the AC ID table as dumphfdl shows it: after the whole frame
        for d in out.iter_mut() {
            let side = if d.downlink { "src" } else { "dst" };
            if let Some(id) = d.ac_id
                && let Some(icao) = self.ac.lookup(freq, id, t)
            {
                d.tree[side]["ac_info"] = json!({"icao": format!("{icao:06X}")});
            }
        }
        out
    }

    fn lpdu(
        &mut self,
        b: &[u8],
        downlink: bool,
        gs: u8,
        ac_id: u8,
        freq: u32,
        t: f64,
    ) -> Option<Decoded> {
        if b.len() < 3 {
            return None;
        }
        let len = b.len() - 2;
        if !fcs_ok(b, len) {
            return None;
        }
        let ty = b[0];
        let ac = json!({"type": "Aircraft", "id": ac_id});
        let mut tree = json!({"err": false});
        if downlink {
            tree["src"] = ac;
            tree["dst"] = gs_obj(gs);
        } else {
            tree["src"] = gs_obj(gs);
            tree["dst"] = ac;
        }
        let names: HashMap<u8, &str> = [
            (0x0D, "Unnumbered data"),
            (0x1D, "Unnumbered ack'ed data"),
            (0x2F, "Logon denied"),
            (0x3F, "Logoff request"),
            (0x5F, "Logon resume confirm"),
            (0x4F, "Logon resume"),
            (0x8F, "Logon request (normal)"),
            (0x9F, "Logon confirm"),
            (0xBF, "Logon request (DLS)"),
        ]
        .into_iter()
        .collect();
        tree["type"] = json!({"id": ty, "name": names.get(&ty).copied().unwrap_or("unknown")});
        let mut icao_in = None;
        let consumed = match ty {
            0x0D | 0x1D => 1,
            0x2F | 0x3F => {
                if len < 5 {
                    return Some(err_lpdu(b));
                }
                let icao = icao_of(&b[1..4]);
                self.ac.delete(freq, icao);
                icao_in = Some(icao);
                tree["ac_info"] = json!({"icao": format!("{icao:06X}")});
                let o = if ty == 0x2F {
                    [
                        "Aircraft ID not available",
                        "HFDL Ground Station subsystem does not support RLS",
                    ]
                    .get((b[4] as usize).wrapping_sub(1))
                    .copied()
                } else {
                    [
                        "Not within slot boundaries",
                        "Downlink set in uplink slot",
                        "RLS protocol error",
                        "Invalid aircraft ID",
                        "HFDL Ground Station subsystem does not support RLS",
                        "Other",
                    ]
                    .get((b[4] as usize).wrapping_sub(1))
                    .copied()
                };
                tree["reason"] = json!({"code": b[4], "descr": o.unwrap_or("Reserved")});
                5
            }
            0x9F | 0x5F => {
                if len < 8 {
                    return Some(err_lpdu(b));
                }
                let icao = icao_of(&b[1..4]);
                self.ac.create(freq, b[4], icao, t);
                icao_in = Some(icao);
                tree["ac_info"] = json!({"icao": format!("{icao:06X}")});
                tree["assigned_ac_id"] = json!(b[4]);
                8
            }
            0x4F | 0x8F | 0xBF => {
                if len < 4 {
                    return Some(err_lpdu(b));
                }
                let icao = icao_of(&b[1..4]);
                icao_in = Some(icao);
                tree["ac_info"] = json!({"icao": format!("{icao:06X}")});
                4
            }
            _ => {
                tree["unknown_proto"] = json!({"data": &b[..len]});
                len
            }
        };
        let mut systable = None;
        if consumed < len {
            let (h, st) = self.hfnpdu(&b[consumed..len], downlink, t);
            if let Some(h) = h {
                tree["hfnpdu"] = h;
            }
            systable = st;
        }
        let ac_id = Some(ac_id);
        Some(Decoded {
            key: "lpdu",
            tree,
            octets: b.to_vec(),
            downlink,
            gs_id: gs,
            ac_id,
            icao_in_pdu: icao_in,
            systable,
        })
    }

    #[allow(clippy::type_complexity)]
    fn hfnpdu(
        &mut self,
        b: &[u8],
        downlink: bool,
        t: f64,
    ) -> (Option<Value>, Option<(u32, Vec<u8>)>) {
        if b.is_empty() {
            return (None, None);
        }
        if b[0] != 0xFF {
            return (Some(json!({"unknown_proto": {"data": b}})), None);
        }
        if b.len() < 2 {
            return (None, None);
        }
        let ty = b[1];
        let names: HashMap<u8, &str> = [
            (0xD0, "System table (partial)"),
            (0xD1, "Performance data"),
            (0xD2, "System table request"),
            (0xD5, "Frequency data"),
            (0xDE, "Delayed echo"),
            (0xFF, "Enveloped data"),
        ]
        .into_iter()
        .collect();
        let mut v = Map::new();
        v.insert("err".into(), json!(false));
        v.insert(
            "type".into(),
            json!({"id": ty, "name": names.get(&ty).copied().unwrap_or("unknown")}),
        );
        let pos = |p: &[u8]| {
            let la = p[0] as u32 | (p[1] as u32) << 8 | ((p[2] & 0xF) as u32) << 16;
            let lo = (p[2] >> 4) as u32 | (p[3] as u32) << 4 | (p[4] as u32) << 12;
            json!({"lat": coord(la), "lon": coord(lo)})
        };
        let utc = |p: &[u8]| {
            let s = 2 * u16le(p);
            json!({"hour": s / 3600, "min": s % 3600 / 60, "sec": s % 60})
        };
        // a C string in the reference: it ends at the first NUL
        let fid = |p: &[u8]| {
            let f = &p[2..8];
            let end = f.iter().position(|&c| c == 0).unwrap_or(f.len());
            String::from_utf8_lossy(&f[..end]).into_owned()
        };
        let mut systable = None;
        let err = |mut v: Map<String, Value>| {
            v.insert("err".into(), json!(true));
            v.remove("type");
            Value::Object(v)
        };
        match ty {
            0xD0 => {
                if b.len() < 5 {
                    return (Some(err(v)), None);
                }
                let total = (b[2] >> 4) as usize + 1;
                let seq = (b[2] & 0xF) as usize;
                let version = (b[3] >> 4) as u32 | (b[4] as u32) << 4;
                v.insert("version".into(), json!(version));
                v.insert(
                    "systable_partial".into(),
                    json!({"part_num": seq + 1, "parts_cnt": total}),
                );
                if b.len() > 5 {
                    let a = &mut self.systable;
                    if a.version != Some(version) || a.parts.len() != total {
                        a.version = Some(version);
                        a.parts = vec![None; total];
                    }
                    if seq < total {
                        a.parts[seq] = Some(b[5..].to_vec());
                    }
                    if a.parts.iter().all(|p| p.is_some()) {
                        let data: Vec<u8> = a.parts.drain(..).flatten().flatten().collect();
                        a.version = None;
                        let complete = systable_decode(&data, version);
                        v.insert("systable_complete".into(), complete.clone());
                        if complete["err"] == json!(false) {
                            systable = Some((version, data));
                        }
                    }
                }
            }
            0xD1 => {
                if b.len() < 47 {
                    return (Some(err(v)), None);
                }
                let stats = |o: usize| json!({"300bps": b[o + 3], "600bps": b[o + 2], "1200bps": b[o + 1], "1800bps": b[o]});
                let codes = [
                    "First freq. search in this flight leg",
                    "Too many NACKs",
                    "SPDUs no longer received",
                    "HFDL disabled",
                    "GS frequency change",
                    "GS down / channel down",
                    "Poor uplink channel quality",
                    "No change",
                ];
                let gs = b[17] & 0x7F;
                let mut freq = json!({"id": 1u32 << b[18].min(31)});
                if let Some(f) = stations::frequency_khz(gs, b[18] as usize) {
                    freq["freq"] = json!(f);
                }
                for (k, x) in [
                    ("version", json!(b[15])),
                    ("flight_id", json!(fid(b))),
                    ("pos", pos(&b[8..13])),
                    ("time", utc(&b[13..15])),
                    ("flight_leg_num", json!(b[16])),
                    ("gs", gs_obj(gs)),
                    ("frequency", freq),
                    (
                        "freq_search_cnt",
                        json!({"cur_leg": u16le(&b[21..]), "prev_leg": u16le(&b[19..])}),
                    ),
                    (
                        "hfdl_disabled_duration",
                        json!({"this_leg": u16le(&b[25..]), "prev_leg": u16le(&b[23..])}),
                    ),
                    (
                        "pdu_stats",
                        json!({"mpdus_rx_ok_cnt": stats(27), "mpdus_rx_err_cnt": stats(31), "mpdus_tx_cnt": stats(38),
                        "mpdus_delivered_cnt": stats(42), "spdus_rx_ok_cnt": u16le(&b[35..]), "spdus_missed_cnt": b[37]}),
                    ),
                    (
                        "last_freq_change_cause",
                        json!({"code": b[46] & 0xF, "descr": codes.get((b[46] & 0xF) as usize).copied().unwrap_or("unknown")}),
                    ),
                ] {
                    v.insert(k.into(), x);
                }
            }
            0xD2 => {
                if b.len() < 4 {
                    return (Some(err(v)), None);
                }
                v.insert("request_data".into(), json!(u16le(&b[2..])));
            }
            0xD5 => {
                if b.len() < 15 {
                    return (Some(err(v)), None);
                }
                let mut fd = Vec::new();
                let mut p = 15;
                while p + 6 <= b.len() && fd.len() < 6 {
                    let e = &b[p..p + 6];
                    let gs = e[0] & 0x7F;
                    let prop = e[1] as u32 | (e[2] as u32) << 8 | ((e[3] & 0xF) as u32) << 16;
                    let tuned = (e[3] >> 4) as u32 | (e[4] as u32) << 4 | (e[5] as u32) << 12;
                    fd.push(json!({"gs": gs_obj(gs), "listening_on_freqs": freq_list(gs, tuned), "heard_on_freqs": freq_list(gs, prop)}));
                    p += 6;
                }
                v.insert("flight_id".into(), json!(fid(b)));
                v.insert("pos".into(), pos(&b[8..13]));
                v.insert("utc_time".into(), utc(&b[13..15]));
                v.insert("freq_data".into(), json!(fd));
            }
            0xFF => {
                let data = &b[2..];
                if !data.is_empty() && data[0] == 1 {
                    v.insert(
                        "acars".into(),
                        acars::parse(&data[1..], downlink, &mut self.tables, t),
                    );
                } else {
                    v.insert("unknown_proto".into(), json!({"data": data}));
                }
            }
            _ => {}
        }
        (Some(Value::Object(v)), systable)
    }
}

fn err_lpdu(_b: &[u8]) -> Decoded {
    Decoded {
        key: "lpdu",
        tree: json!({"err": true}),
        octets: _b.to_vec(),
        downlink: false,
        gs_id: 0,
        ac_id: None,
        icao_in_pdu: None,
        systable: None,
    }
}

/// A complete system table (`pdu-layers.md` §7.4) as dumphfdl's tree.
pub fn systable_decode(data: &[u8], version: u32) -> Value {
    let mut p = 0;
    let mut stations = Vec::new();
    while data.len() - p >= 8 {
        let b = &data[p..];
        let la = b[1] as u32 | (b[2] as u32) << 8 | ((b[3] & 0xF) as u32) << 16;
        let lo = (b[3] >> 4) as u32 | (b[4] as u32) << 4 | (b[5] as u32) << 12;
        let n = ((b[6] >> 3) & 0x1F) as usize;
        if n > 20 || b.len() < 7 + n * 4 {
            return json!({"err": true});
        }
        let freqs: Vec<Value> = (0..n)
            .map(|i| {
                let f = &b[7 + i * 4..];
                let hz = 100 * (f[0] & 0xF) as u32
                    + 1000 * (f[0] >> 4) as u32
                    + 10_000 * (f[1] & 0xF) as u32
                    + 100_000 * (f[1] >> 4) as u32
                    + 1_000_000 * (f[2] & 0xF) as u32
                    + 10_000_000 * (f[2] >> 4) as u32;
                json!({"id": i, "freq": hz as f64 / 1000.0, "master_frame_slot": f[3] & 0xF})
            })
            .collect();
        stations.push(json!({"gs": {"type": "Ground station", "id": b[0] & 0x7F}, "utc_sync": b[0] & 0x80 != 0,
            "gs_location": {"lat": coord(la), "lon": coord(lo)}, "spdu_version": b[6] & 7, "freqs": freqs}));
        p += 7 + n * 4;
    }
    json!({"err": false, "version": version, "stations": stations})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_fcs(mut b: Vec<u8>) -> Vec<u8> {
        let c = acars::crc16_reflected(&b, 0xFFFF) ^ 0xFFFF;
        b.extend([c as u8, (c >> 8) as u8]);
        b
    }

    #[test]
    fn frame_quality_counts_the_parts_that_check() {
        // a squitter: 64 octets and the FCS
        let sq = with_fcs(vec![0x04; 64]);
        assert_eq!(frame_quality(&sq), 1);
        let mut bad = sq.clone();
        bad[10] ^= 1;
        assert_eq!(frame_quality(&bad), 0);
        // a downlink MPDU with two LPDUs of 5 octets (3 + FCS)
        let lpdu = with_fcs(vec![1, 2, 3]);
        let mut f = with_fcs(vec![0x03 | (2 << 2), 7, 23, 0, 0, 0, 4, 4]);
        f.extend(&lpdu);
        f.extend(&lpdu);
        f.extend([0; 6]);
        assert_eq!(frame_quality(&f), 3);
        let n = f.len();
        f[n - 8] ^= 1; // the second LPDU
        assert_eq!(frame_quality(&f), 2);
        f[3] ^= 1; // the header
        assert_eq!(frame_quality(&f), 0);
        // too short for its header: no panic
        assert_eq!(frame_quality(&[0x03]), 0);
        assert!(Parser::default().frame(&[0x03], 8977, 0.0).is_empty());
    }
}
