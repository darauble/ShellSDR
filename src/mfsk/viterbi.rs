//! The rate 1/2 convolutional code of MFSK (NASA K=7, polynomials 0x6d and
//! 0x4f) and its soft-decision Viterbi decoder (fldigi
//! `filters/viterbi.cxx`).
//!
//! Ported from fldigi 4.1.23 (GPL v3 or later; Dave Freese W1HKJ, from gMFSK
//! by Tomi Manninen OH2BNS). The path metrics are 64-bit here, so they never
//! overflow (fldigi's 32-bit ones are never renormalized with a chunk size
//! of 1).

pub const K: u32 = 7;
pub const POLY1: u32 = 0x6d;
pub const POLY2: u32 = 0x4f;
const PATHMEM: usize = 256;

fn parity(w: u32) -> u32 {
    w.count_ones() & 1
}

/// The two code bits of each register state (bit 0: POLY1, bit 1: POLY2).
fn output_table() -> Vec<u8> {
    (0..1u32 << K)
        .map(|i| (parity(POLY1 & i) | (parity(POLY2 & i) << 1)) as u8)
        .collect()
}

/// The encoder (transmit side, for the test signals).
#[derive(Debug, Clone)]
pub struct Encoder {
    output: Vec<u8>,
    shreg: u32,
}

impl Default for Encoder {
    fn default() -> Self {
        Self {
            output: output_table(),
            shreg: 0,
        }
    }
}

impl Encoder {
    /// One data bit in, two code bits out (bit 0 first on the air).
    pub fn encode(&mut self, bit: bool) -> u8 {
        self.shreg = (self.shreg << 1) | u32::from(bit);
        self.output[(self.shreg & ((1 << K) - 1)) as usize]
    }
}

/// The decoder, with a traceback of 45 and one bit out per pair in (the
/// settings of the MFSK receiver).
#[derive(Debug, Clone)]
pub struct Viterbi {
    traceback: usize,
    nstates: usize,
    output: Vec<u8>,
    metrics: Vec<Vec<i64>>,
    history: Vec<Vec<u8>>,
    sequence: Vec<u8>,
    ptr: usize,
}

impl Default for Viterbi {
    fn default() -> Self {
        Self::new(45)
    }
}

/// fldigi's soft metric: a 0..255 sample is 0 when it is 0 (and 255 for 1).
fn mettab(bit: usize, sym: u8) -> i64 {
    if bit == 0 {
        128 - i64::from(sym)
    } else {
        i64::from(sym) - 128
    }
}

impl Viterbi {
    pub fn new(traceback: usize) -> Self {
        let nstates = 1 << (K - 1);
        Self {
            traceback: traceback.min(PATHMEM - 1),
            nstates,
            output: output_table(),
            metrics: vec![vec![0; nstates]; PATHMEM],
            history: vec![vec![0; nstates]; PATHMEM],
            sequence: vec![0; PATHMEM],
            ptr: 0,
        }
    }

    pub fn reset(&mut self) {
        for m in &mut self.metrics {
            m.iter_mut().for_each(|x| *x = 0);
        }
        for h in &mut self.history {
            h.iter_mut().for_each(|x| *x = 0);
        }
        self.ptr = 0;
    }

    /// A pair of soft symbols (0 = surely 0, 255 = surely 1, 128 = unknown)
    /// in; the decoded bit `traceback` pairs ago and its metric out.
    pub fn decode(&mut self, sym: [u8; 2]) -> (bool, i64) {
        let cur = self.ptr;
        let prev = (cur + PATHMEM - 1) % PATHMEM;
        let met = [
            mettab(0, sym[1]) + mettab(0, sym[0]),
            mettab(0, sym[1]) + mettab(1, sym[0]),
            mettab(1, sym[1]) + mettab(0, sym[0]),
            mettab(1, sym[1]) + mettab(1, sym[0]),
        ];
        for n in 0..self.nstates {
            let s0 = n;
            let s1 = n + self.nstates;
            let p0 = s0 >> 1;
            let p1 = s1 >> 1;
            let m0 = self.metrics[prev][p0] + met[self.output[s0] as usize];
            let m1 = self.metrics[prev][p1] + met[self.output[s1] as usize];
            if m0 > m1 {
                self.metrics[cur][n] = m0;
                self.history[cur][n] = p0 as u8;
            } else {
                self.metrics[cur][n] = m1;
                self.history[cur][n] = p1 as u8;
            }
        }
        self.ptr = (self.ptr + 1) % PATHMEM;
        self.traceback()
    }

    fn traceback(&mut self) -> (bool, i64) {
        let mut p = (self.ptr + PATHMEM - 1) % PATHMEM;
        let mut best = i64::MIN;
        let mut best_state = 0;
        for (i, &m) in self.metrics[p].iter().enumerate() {
            if m > best {
                best = m;
                best_state = i;
            }
        }
        self.sequence[p] = best_state as u8;
        for _ in 0..self.traceback {
            let prev = (p + PATHMEM - 1) % PATHMEM;
            self.sequence[prev] = self.history[p][self.sequence[p] as usize];
            p = prev;
        }
        let old = self.metrics[p][self.sequence[p] as usize];
        // the low bit of a state is the bit that entered it
        let bit = self.sequence[p] & 1 == 1;
        let next = (p + 1) % PATHMEM;
        let metric = self.metrics[next][self.sequence[next] as usize] - old;
        (bit, metric)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soft(b: u8) -> u8 {
        if b == 1 { 255 } else { 0 }
    }

    #[test]
    fn decodes_what_was_encoded_with_a_delay_of_the_traceback() {
        let data: Vec<bool> = (0..400u32).map(|i| (i * 7919 % 13) < 6).collect();
        let mut e = Encoder::default();
        let mut v = Viterbi::default();
        let mut out = Vec::new();
        for &b in &data {
            let c = e.encode(b);
            let (bit, metric) = v.decode([soft(c & 1), soft(c >> 1)]);
            out.push(bit);
            if out.len() > 100 {
                assert!(metric >= 254, "a clean pair adds the full metric: {metric}");
            }
        }
        assert_eq!(&out[45..], &data[..data.len() - 45]);
    }

    #[test]
    fn corrects_isolated_errors() {
        let data: Vec<bool> = (0..600u32).map(|i| (i * 31 % 7) < 3).collect();
        let mut e = Encoder::default();
        let mut v = Viterbi::default();
        let mut out = Vec::new();
        for (i, &b) in data.iter().enumerate() {
            let c = e.encode(b);
            let mut s = [soft(c & 1), soft(c >> 1)];
            if i % 17 == 5 {
                s[0] = 255 - s[0];
            }
            out.push(v.decode(s).0);
        }
        assert_eq!(&out[45..], &data[..data.len() - 45]);
    }
}
