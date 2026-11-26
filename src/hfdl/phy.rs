//! The HFDL physical layer's constants and the frame decoder (HFDL seed:
//! `spec/03-domain/physical-layer.md`, `spec/04-algorithms/02-frame.md`):
//! a frame's data symbols -> its octets.

use std::sync::OnceLock;

/// Symbols per second.
pub const SYMBOL_RATE: f64 = 1800.0;
/// The signal's center above the assigned frequency.
pub const CARRIER_OFFSET_HZ: f64 = 1440.0;
pub const PREKEY: usize = 448;
pub const A_LEN: usize = 127;
pub const M1_LEN: usize = 127;
pub const M2_LEN: usize = 15;
pub const T_LEN: usize = 15;
pub const T_PREAMBLE: usize = 9;
pub const DATA_SEG: usize = 30;

/// One of the eight M1 modes.
#[derive(Debug, Clone)]
pub struct Mode {
    pub index: usize,
    pub m1: Vec<u8>,
    pub slots: u8,
    pub bits_per_symbol: usize,
    pub segments: usize,
    pub repetition: usize,
    pub columns: usize,
    pub shift: usize,
    pub user_bits: usize,
    pub user_octets: usize,
    pub bit_rate: u32,
}

/// The constants of `physical.json`.
#[derive(Debug)]
pub struct Phy {
    pub a: Vec<u8>,
    pub t: Vec<u8>,
    pub scrambler: Vec<u8>,
    pub modes: Vec<Mode>,
    /// Per mode: the deinterleaver's pop order as push indices.
    order: Vec<Vec<u32>>,
}

fn bits(s: &str) -> Vec<u8> {
    s.bytes().map(|c| c - b'0').collect()
}

/// The constants, loaded once.
pub fn phy() -> &'static Phy {
    static P: OnceLock<Phy> = OnceLock::new();
    P.get_or_init(|| {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("data/physical.json")).expect("physical.json");
        let s = |k: &str| v[k].as_str().unwrap().to_string();
        let modes: Vec<Mode> = v["modes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                let u = |k: &str| m[k].as_u64().unwrap() as usize;
                Mode {
                    index: u("m1_index"),
                    m1: bits(m["m1"].as_str().unwrap()),
                    slots: u("slots") as u8,
                    bits_per_symbol: u("bits_per_symbol"),
                    segments: u("data_segments"),
                    repetition: u("repetition"),
                    columns: u("interleaver_columns"),
                    shift: u("interleaver_column_shift"),
                    user_bits: u("user_bits"),
                    user_octets: u("user_octets"),
                    bit_rate: u("bit_rate_bps") as u32,
                }
            })
            .collect();
        let order = modes
            .iter()
            .map(|m| deinterleave_order(m.columns, m.shift))
            .collect();
        Phy {
            a: bits(&s("A")),
            t: bits(&s("T")),
            scrambler: bits(&s("scrambler")),
            modes,
            order,
        }
    })
}

/// For each bit in code order, the index of the received bit it is
/// (physical-layer.md §6: push and pop over a 40-row table).
fn deinterleave_order(columns: usize, shift: usize) -> Vec<u32> {
    const ROWS: usize = 40;
    let n = ROWS * columns;
    let mut table = vec![0u32; n];
    let (mut r, mut c) = (0usize, 0isize);
    for i in 0..n {
        table[r * columns + c as usize] = i as u32;
        r += 1;
        if r == ROWS {
            r = 0;
            c += 1;
        }
        c -= shift as isize;
        if c < 0 {
            c += columns as isize;
        }
    }
    let mut out = Vec::with_capacity(n);
    let (mut r, mut c) = (0usize, 0usize);
    for _ in 0..n {
        out.push(table[r * columns + c]);
        r = (r + 9) % ROWS;
        if r == 0 {
            c += 1;
        }
    }
    out
}

pub fn gray_encode(k: u32) -> u32 {
    k ^ (k >> 1)
}

/// Soft bits of one data symbol, most significant first, as probabilities
/// of a 1 scaled 0..=255, from its complex value (phase resolved,
/// scrambler already undone): the distance to the nearest point with the
/// bit 0 against the nearest with the bit 1.
pub fn soft_bits(re: f32, im: f32, bps: usize, out: &mut Vec<u8>) {
    let m = 1usize << bps;
    let mut d = [0f32; 8];
    for (k, dk) in d.iter_mut().enumerate().take(m) {
        let a = std::f32::consts::TAU * k as f32 / m as f32;
        let (dr, di) = (re - a.cos(), im - a.sin());
        *dk = dr * dr + di * di;
    }
    for j in (0..bps).rev() {
        let (mut d0, mut d1) = (f32::MAX, f32::MAX);
        for (k, &dk) in d.iter().enumerate().take(m) {
            if gray_encode(k as u32) >> j & 1 == 1 {
                d1 = d1.min(dk);
            } else {
                d0 = d0.min(dk);
            }
        }
        // log-likelihood-like: positive favours 1
        let llr = (d0 - d1) * 0.25;
        out.push((127.5 + llr.clamp(-1.0, 1.0) * 127.5) as u8);
    }
}

fn parity(x: u32) -> u8 {
    (x.count_ones() & 1) as u8
}

/// Maximum-likelihood decoding of the K=7, rate 1/2 code (polynomials
/// 0x6D, 0x4F, register `(reg << 1 | bit) & 0x7F`), soft symbols 0..=255
/// (255 = surely 1), start state 0, best end state (F-H16).
pub fn viterbi(soft: &[u8]) -> Vec<u8> {
    static OUT: OnceLock<[[u8; 2]; 128]> = OnceLock::new();
    let outs = OUT.get_or_init(|| {
        let mut o = [[0u8; 2]; 128];
        for (reg, e) in o.iter_mut().enumerate() {
            *e = [parity(reg as u32 & 0x6D), parity(reg as u32 & 0x4F)];
        }
        o
    });
    let n = soft.len() / 2;
    let mut metric = [u32::MAX; 64];
    metric[0] = 0;
    let mut back = vec![[0u8; 64]; n];
    for i in 0..n {
        let (a, b) = (soft[2 * i] as u32, soft[2 * i + 1] as u32);
        let cost = |o: [u8; 2]| {
            (if o[0] == 1 { 255 - a } else { a }) + (if o[1] == 1 { 255 - b } else { b })
        };
        let mut new = [u32::MAX; 64];
        for (s, &m) in metric.iter().enumerate() {
            if m == u32::MAX {
                continue;
            }
            for x in 0..2usize {
                let reg = (s << 1) | x;
                let c = m + cost(outs[reg & 0x7F]);
                let ns = reg & 0x3F;
                if c < new[ns] {
                    new[ns] = c;
                    back[i][ns] = ((s << 1) | x) as u8;
                }
            }
        }
        metric = new;
    }
    let mut s = if metric[0] != u32::MAX {
        0
    } else {
        (0..64).min_by_key(|&s| metric[s]).unwrap_or(0)
    };
    let mut out = vec![0u8; n];
    for i in (0..n).rev() {
        let prev = back[i][s];
        out[i] = prev & 1;
        s = (prev >> 1) as usize;
    }
    out
}

/// A frame's octets from its soft bits as received (symbol order, scrambler
/// and phase already undone): deinterleave, average repeated bits, decode,
/// pack least significant bit first.
pub fn decode_soft(mode: &Mode, soft: &[u8]) -> Vec<u8> {
    let order = &phy().order[mode.index];
    let mut coded: Vec<u8> = order
        .iter()
        .map(|&i| soft.get(i as usize).copied().unwrap_or(128))
        .collect();
    if mode.repetition == 2 {
        coded = coded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| ((p[0] as u16 + p[1] as u16) / 2) as u8)
            .collect();
    }
    let bits = viterbi(&coded);
    let mut octets = vec![0u8; mode.user_octets];
    for (i, b) in bits.iter().enumerate().take(mode.user_bits) {
        octets[i / 8] |= b << (i % 8);
    }
    octets
}

/// A frame's octets from its data symbols as phase indices k (symbol =
/// e^(j 2 pi k / M)), scrambled as received: the hard-decision path of the
/// conformance suite `frame/decode.json`.
pub fn decode_indices(mode: &Mode, symbols: &[u32]) -> Vec<u8> {
    let p = phy();
    let bps = mode.bits_per_symbol;
    let m = 1u32 << bps;
    let mut soft = Vec::with_capacity(symbols.len() * bps);
    for (i, &k) in symbols.iter().enumerate() {
        let k = if p.scrambler[i % p.scrambler.len()] == 1 {
            (k + m / 2) % m
        } else {
            k
        };
        let g = gray_encode(k);
        for j in (0..bps).rev() {
            soft.push(if g >> j & 1 == 1 { 255 } else { 0 });
        }
    }
    decode_soft(mode, &soft)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constants_load_and_hold_together() {
        let p = phy();
        assert_eq!(p.a.len(), A_LEN);
        assert_eq!(p.scrambler.len(), 120);
        assert_eq!(p.modes.len(), 8);
        for m in &p.modes {
            assert_eq!(m.columns * 40, m.segments * DATA_SEG * m.bits_per_symbol);
            let mut seen = p.order[m.index].clone();
            seen.sort_unstable();
            assert!(
                seen.iter().enumerate().all(|(i, &x)| i as u32 == x),
                "a permutation"
            );
        }
    }

    #[test]
    fn soft_bits_follow_the_gray_mapping() {
        let mut out = Vec::new();
        // 8-PSK k=3 (135 degrees) is 010
        let a = 3.0 * std::f32::consts::TAU / 8.0;
        soft_bits(a.cos(), a.sin(), 3, &mut out);
        assert_eq!(
            out.iter().map(|&b| (b > 127) as u8).collect::<Vec<_>>(),
            [0, 1, 0]
        );
    }
}
