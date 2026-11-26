//! ACARS in HFDL (HFDL seed `spec/03-domain/acars.md`): the block, its
//! check, the downlink prefix, sublabel and MFI, reassembly, and handing
//! the text to the applications. Results are trees in libacars' JSON names.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::apps;

const STX: u8 = 0x02;
const ETX: u8 = 0x03;
const ETB: u8 = 0x17;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const DEL: u8 = 0x7F;

/// CRC-16, reflected 0x1021, initial `init`, no final XOR (KERMIT when 0;
/// with 0xFFFF and a final XOR it is the HFDL FCS).
pub fn crc16_reflected(data: &[u8], init: u16) -> u16 {
    let mut crc = init;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// Reassembly status, as libacars names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reasm {
    Unknown,
    Complete,
    InProgress,
    Skipped,
    Duplicate,
    OutOfSequence,
}

impl Reasm {
    pub fn name(self) -> &'static str {
        match self {
            Reasm::Unknown => "unknown",
            Reasm::Complete => "complete",
            Reasm::InProgress => "in progress",
            Reasm::Skipped => "skipped",
            Reasm::Duplicate => "duplicate",
            Reasm::OutOfSequence => "out of sequence",
        }
    }
}

/// One message being joined.
#[derive(Debug, Clone)]
struct Entry {
    first_t: f64,
    timeout: f64,
    prev_seq: i64,
    parts: Vec<(i64, Vec<u8>)>,
    total_len: usize,
    total_cnt: usize,
    collected_len: usize,
}

/// The fragment's description (libacars `la_reasm_fragment_info`).
pub struct Fragment<'a> {
    pub data: &'a [u8],
    pub seq: i64,
    /// The first fragment's number, if the protocol says (None: unknown).
    pub seq_first: Option<i64>,
    /// Wraps to 0 after this number - 1 (None: no wrap).
    pub seq_wrap: Option<i64>,
    pub is_final: bool,
    pub total_len: usize,
    pub total_cnt: usize,
    pub t: f64,
    pub timeout: f64,
    pub out_of_order: bool,
}

/// A reassembly table (libacars `la_reasm_table`), keyed by a string.
#[derive(Debug, Default)]
pub struct Table {
    entries: HashMap<String, Entry>,
}

impl Table {
    /// Add a fragment; returns the status and, when complete, the payload.
    pub fn add(&mut self, key: &str, f: &Fragment) -> (Reasm, Option<Vec<u8>>) {
        loop {
            if !self.entries.contains_key(key) {
                if f.out_of_order {
                    if f.seq_first == Some(f.seq) && f.is_final {
                        return (Reasm::Skipped, None);
                    }
                } else {
                    if let Some(first) = f.seq_first
                        && first != f.seq
                    {
                        return (Reasm::OutOfSequence, None);
                    }
                    if f.is_final {
                        return (Reasm::Skipped, None);
                    }
                }
                self.entries.insert(
                    key.to_string(),
                    Entry {
                        first_t: f.t,
                        timeout: f.timeout,
                        prev_seq: i64::MIN,
                        parts: Vec::new(),
                        total_len: f.total_len,
                        total_cnt: f.total_cnt,
                        collected_len: 0,
                    },
                );
            }
            let e = self.entries.get_mut(key).unwrap();
            if let Some(wrap) = f.seq_wrap
                && f.seq == 0
                && e.prev_seq != i64::MIN
                && wrap == e.prev_seq + 1
            {
                e.prev_seq = -1;
            }
            if f.t > e.first_t + e.timeout {
                self.entries.remove(key);
                continue;
            }
            break;
        }
        let e = self.entries.get_mut(key).unwrap();
        let dup = if f.out_of_order {
            e.parts.iter().any(|(s, _)| *s == f.seq)
        } else {
            e.prev_seq == f.seq
                || (f.seq_wrap.is_none() && e.prev_seq != i64::MIN && f.seq < e.prev_seq)
        };
        if dup {
            return (Reasm::Duplicate, None);
        }
        if !f.out_of_order && !(e.prev_seq == i64::MIN || e.prev_seq + 1 == f.seq) {
            self.entries.remove(key);
            return (Reasm::OutOfSequence, None);
        }
        if f.out_of_order {
            let pos = e
                .parts
                .iter()
                .position(|(s, _)| *s > f.seq)
                .unwrap_or(e.parts.len());
            e.parts.insert(pos, (f.seq, f.data.to_vec()));
            if e.total_len == 0 && f.total_len > 0 {
                e.total_len = f.total_len;
            } else if e.total_cnt == 0 && f.total_cnt > 0 {
                e.total_cnt = f.total_cnt;
            }
        } else {
            e.parts.push((f.seq, f.data.to_vec()));
            e.prev_seq = f.seq;
        }
        e.collected_len += f.data.len();
        let done = if e.total_len > 0 {
            e.collected_len >= e.total_len
        } else if e.total_cnt > 0 {
            e.parts.len() >= e.total_cnt
        } else {
            f.is_final
        };
        if done {
            let e = self.entries.remove(key).unwrap();
            let out: Vec<u8> = e.parts.into_iter().flat_map(|(_, d)| d).collect();
            (Reasm::Complete, Some(out))
        } else {
            (Reasm::InProgress, None)
        }
    }
}

/// The reassembly tables of one session (ACARS, MIAM files, OHMA).
#[derive(Debug, Default)]
pub struct Tables {
    pub acars: Table,
    pub miam: Table,
    pub ohma: Table,
}

/// The HFDL bearer's ACARS timeouts (s).
pub const DOWN_TIMEOUT: f64 = 1260.0;
pub const UP_TIMEOUT: f64 = 370.0;

fn is_downlink_block(b: u8) -> bool {
    b.is_ascii_digit()
}

/// libacars `la_acars_extract_sublabel_and_mfi`: (sublabel, mfi, consumed).
fn sublabel_mfi(label: &str, down: bool, txt: &[u8]) -> (Option<String>, Option<String>, usize) {
    if label != "H1" {
        return (None, None, 0);
    }
    let mut p = 0;
    let mut sub = None;
    if !down {
        if txt.len() >= 5 && &txt[..3] == b"- #" {
            sub = Some(String::from_utf8_lossy(&txt[3..5]).into_owned());
            p = 5;
        }
    } else if txt.len() >= 4 && txt[0] == b'#' && txt[3] == b'B' {
        sub = Some(String::from_utf8_lossy(&txt[1..3]).into_owned());
        p = 4;
    }
    if sub.is_none() {
        return (None, None, 0);
    }
    let rest = &txt[p..];
    if rest.len() >= 4 && rest[0] == b'/' && rest[3] == b' ' {
        let mfi = String::from_utf8_lossy(&rest[1..3]).into_owned();
        return (sub, Some(mfi), p + 4);
    }
    (sub, None, p)
}

fn ch(c: u8) -> String {
    (c as char).to_string()
}

/// Parse one block (the octets after SOH) with reassembly at time `t` (s).
/// Returns the `acars` tree (with any application inside).
pub fn parse(buf: &[u8], down: bool, tables: &mut Tables, t: f64) -> Value {
    let mut m = Map::new();
    let err = || json!({"err": true});
    if buf.len() < 16 || *buf.last().unwrap() != DEL {
        return err();
    }
    let len = buf.len() - 1;
    let crc_ok = crc16_reflected(&buf[..len], 0) == 0;
    let body: Vec<u8> = buf[..len - 2].iter().map(|b| b & 0x7F).collect();
    let final_block = match body.last() {
        Some(&ETX) => true,
        Some(&ETB) => false,
        _ => return err(),
    };
    let b = &body[..body.len() - 1];
    let mode = b[0];
    let reg = String::from_utf8_lossy(&b[1..8]).into_owned();
    let ack = match b[8] {
        NAK => b'!',
        ACK => b'^',
        x => x,
    };
    let mut label = String::new();
    label.push(b[9] as char);
    label.push(if b[10] == DEL { 'd' } else { b[10] as char });
    let block_id = if b[11] == 0 { b' ' } else { b[11] };
    let mut rest: Vec<u8> = b[12..].to_vec();
    let downlink_blk = is_downlink_block(block_id);
    let dir_down = down;
    let mut msg_num = String::new();
    let mut msg_seq = 0u8;
    let mut flight = String::new();
    let mut sub = None;
    let mut mfi = None;
    let reasm;
    let text: Vec<u8>;
    if rest.is_empty() {
        if !downlink_blk {
            reasm = Reasm::Skipped;
            text = Vec::new();
        } else {
            return err();
        }
    } else {
        if rest[0] != STX {
            return err();
        }
        rest.remove(0);
        for c in rest.iter_mut() {
            if *c == 0 {
                *c = b'.';
            }
        }
        let mut p = 0;
        if downlink_blk {
            if rest.len() < 10 {
                return err();
            }
            msg_num = String::from_utf8_lossy(&rest[..3]).into_owned();
            msg_seq = rest[3];
            flight = String::from_utf8_lossy(&rest[4..10]).into_owned();
            p = 10;
        }
        let (s, f, used) = sublabel_mfi(&label, dir_down, &rest[p..]);
        sub = s;
        mfi = f;
        p += used;
        let data = &rest[p..];
        let key = format!("{reg}\u{1}{label}\u{1}{msg_num}");
        let (status, joined) = tables.acars.add(
            &key,
            &Fragment {
                data,
                seq: if downlink_blk {
                    msg_seq as i64 - b'A' as i64
                } else {
                    block_id as i64 - b'A' as i64
                },
                seq_first: if downlink_blk { Some(0) } else { None },
                seq_wrap: if downlink_blk {
                    None
                } else {
                    Some((b'X' - b'A') as i64)
                },
                is_final: final_block,
                total_len: 0,
                total_cnt: 0,
                t,
                timeout: if downlink_blk {
                    DOWN_TIMEOUT
                } else {
                    UP_TIMEOUT
                },
                out_of_order: false,
            },
        );
        reasm = status;
        text = joined.unwrap_or_else(|| data.to_vec());
    }
    let txt = String::from_utf8_lossy(&text).into_owned();
    m.insert("err".into(), json!(false));
    m.insert("crc_ok".into(), json!(crc_ok));
    m.insert("more".into(), json!(!final_block));
    m.insert("reassembly".into(), json!(reasm.name()));
    m.insert("reg".into(), json!(reg));
    m.insert("mode".into(), json!(ch(mode)));
    m.insert("label".into(), json!(label));
    m.insert("blk_id".into(), json!(ch(block_id)));
    m.insert("ack".into(), json!(ch(ack)));
    if downlink_blk {
        m.insert("flight".into(), json!(flight));
        m.insert("msg_num".into(), json!(msg_num));
        m.insert("msg_num_seq".into(), json!(ch(msg_seq)));
    }
    if let Some(s) = sub {
        m.insert("sublabel".into(), json!(s));
    }
    if let Some(f) = mfi {
        m.insert("mfi".into(), json!(f));
    }
    m.insert("msg_text".into(), json!(txt));
    // applications: only for a complete text (decode_fragments off)
    if !txt.is_empty() && !matches!(reasm, Reasm::InProgress | Reasm::Duplicate) {
        let direction = if dir_down {
            apps::Dir::Down
        } else {
            apps::Dir::Up
        };
        if let Some((k, v)) = apps::decode(&reg, &label, &txt, direction, Some(tables), t) {
            m.insert(k, v);
        }
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crcs_have_their_check_values() {
        assert_eq!(crc16_reflected(b"123456789", 0), 0x2189, "KERMIT");
        assert_eq!(
            crc16_reflected(b"123456789", 0xFFFF) ^ 0xFFFF,
            0x906E,
            "X.25"
        );
    }
}
