//! OHMA (HFDL seed `applications.md` §6), in libacars' JSON names.

use serde_json::{Value, json};

use super::miam::{inflate_raw, is_printable};
use crate::hfdl::acars::{Fragment, Reasm, Tables};

const TIMEOUT: f64 = 1200.0;

fn b64_idx(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => (c - b'A') as u32,
        b'a'..=b'z' => (c - b'a' + 26) as u32,
        b'0'..=b'9' => (c - b'0' + 52) as u32,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// libacars `la_base64_decode`: whole 4-character blocks only.
pub fn base64(s: &[u8]) -> Option<Vec<u8>> {
    let n = s.len() & !3;
    if n == 0 {
        return None;
    }
    let mut len = n * 3 / 4;
    if s[n - 1] == b'=' {
        len -= 1;
    }
    if s[n - 2] == b'=' {
        len -= 1;
    }
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while i < n {
        let a = b64_idx(s[i])?;
        let b = b64_idx(s[i + 1])?;
        out.push(((a << 2) | (b >> 4)) as u8);
        if out.len() >= len {
            break;
        }
        let c = b64_idx(s[i + 2])?;
        out.push((((b & 0xF) << 4) | (c >> 2)) as u8);
        if out.len() >= len {
            break;
        }
        let d = b64_idx(s[i + 3])?;
        out.push((((c & 3) << 6) | d) as u8);
        if out.len() >= len {
            break;
        }
        i += 4;
    }
    Some(out)
}

/// None: not OHMA.
pub fn parse(reg: &str, txt: &str, tables: Option<&mut Tables>, t: f64) -> Option<Value> {
    let all = txt.as_bytes();
    let mut start = 0usize;
    let payload = loop {
        let p = &all[start..];
        let mut skip = 0;
        if p.len() >= 13 && p[0] == b'/' && p[8] == b'.' {
            skip = 9;
        } else if p.len() >= 8 && p[0] == b'/' && p[3] == b'.' {
            skip = 4;
        }
        let q = &p[skip..];
        if !(q.starts_with(b"OHMA") || q.starts_with(b"RYKO")) {
            return None;
        }
        let prefix_len = skip + 4;
        let body = &q[4..];
        // the prefix repeated later (a sender bug): parse from there
        let prefix = &all[..start + prefix_len];
        if let Some(pos) = body.windows(prefix.len()).position(|w| w == prefix) {
            start = start + prefix_len + pos;
            continue;
        }
        break body;
    };
    let mut n = payload.len();
    while n > 0 && (payload[n - 1] == b'\r' || payload[n - 1] == b'\n') {
        n -= 1;
    }
    let decoded = base64(&payload[..n])?;
    if decoded.len() < 3 {
        return Some(json!({"err": 1}));
    }
    if decoded[0] & 0x0F != 8 {
        return Some(json!({"err": 2}));
    }
    let (inflated, ok) = inflate_raw(&decoded[2..]);
    if !ok {
        return Some(json!({"err": 3}));
    }
    // libacars: 4 = not JSON, 5 = an unexpected structure
    let Ok(root) = serde_json::from_slice::<Value>(&inflated) else {
        return Some(json!({"err": 4}));
    };
    let bad = || Some(json!({"err": 5}));
    if !root.is_object() {
        return bad();
    }
    let (Some(version), Some(message)) = (
        root.get("version").and_then(|v| v.as_str()),
        root.get("message").and_then(|v| v.as_str()),
    ) else {
        return bad();
    };
    if root.get("convo_id").is_some_and(|v| !v.is_string()) {
        return bad();
    }
    for k in ["msg_seq", "msg_total"] {
        if root.get(k).is_some_and(|v| !v.is_i64()) {
            return bad();
        }
    }
    let convo = root.get("convo_id").and_then(|v| v.as_str());
    let seq = root.get("msg_seq").and_then(|v| v.as_i64()).unwrap_or(0);
    let total = root.get("msg_total").and_then(|v| v.as_i64()).unwrap_or(0);
    let mut v = json!({"err": 0, "version": version});
    if let Some(c) = convo {
        v["msg_id"] = json!(c);
    }
    let mut status = Reasm::Skipped;
    let mut text = message.to_string();
    if seq > 0 {
        let Some(c) = convo else { return bad() };
        if seq == 1 && total == 0 {
            return bad();
        }
        v["msg_seq"] = json!(seq);
        status = Reasm::Unknown;
        if let Some(tb) = tables {
            let key = format!("{reg}\u{1}{c}");
            let (st, joined) = tb.ohma.add(
                &key,
                &Fragment {
                    data: message.as_bytes(),
                    seq,
                    seq_first: Some(1),
                    seq_wrap: None,
                    is_final: false,
                    total_len: 0,
                    total_cnt: total.max(0) as usize,
                    t,
                    timeout: TIMEOUT,
                    out_of_order: true,
                },
            );
            status = st;
            if let Some(j) = joined {
                text = String::from_utf8_lossy(&j).into_owned();
            }
        }
    }
    if total > 0 {
        v["msg_total"] = json!(total);
    }
    v["reasm_status"] = json!(status.name());
    for k in ["sym_key", "iv", "signature"] {
        if let Some(s) = root.get(k).and_then(|x| x.as_str())
            && let Some(b) = base64(s.as_bytes())
        {
            v[k] = json!(b);
        }
    }
    if is_printable(text.as_bytes()) {
        v["text"] = json!(text);
    } else {
        v["octet_string"] = json!(text.as_bytes());
    }
    Some(v)
}
