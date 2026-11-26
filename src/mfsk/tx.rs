//! An MFSK and RSID transmitter for test signals (fldigi `mfsk.cxx` and
//! `rsid.cxx`, transmit halves): what fldigi sends for a text, a picture
//! and an RSID, as a complex signal whose positive frequencies are the audio
//! (its real part is the audio itself).
//!
//! Ported from fldigi 4.1.23 (GPL v3 or later; Dave Freese W1HKJ, from gMFSK
//! by Tomi Manninen OH2BNS).

use num_complex::Complex64;
use std::f64::consts::TAU;

use super::interleave::{Direction, Interleave};
use super::modes::Mode;
use super::rsid;
use super::varicode;
use super::viterbi::Encoder;

/// Samples sent before a picture's first pixel.
const PIC_PROLOGUE: usize = 352;
/// Samples per pixel.
const TX_SPP: usize = 8;

fn grayencode(data: usize) -> usize {
    let mut bits = data;
    for s in 1..8 {
        bits ^= data >> s;
    }
    bits
}

pub struct Tx {
    mode: Mode,
    /// The carrier (Hz): the middle of the tones.
    carrier: f64,
    sr: f64,
    enc: Encoder,
    inlv: Interleave,
    bitshreg: u32,
    bitstate: usize,
    phase: f64,
    out: Vec<Complex64>,
}

impl Tx {
    pub fn new(mode: Mode, carrier: f64) -> Self {
        Self {
            mode,
            carrier,
            sr: f64::from(mode.sample_rate),
            enc: Encoder::default(),
            inlv: Interleave::new(mode.symbits, mode.depth, Direction::Forward),
            bitshreg: 0,
            bitstate: 0,
            phase: 0.0,
            out: Vec::new(),
        }
    }

    /// The signal so far.
    pub fn take(&mut self) -> Vec<Complex64> {
        std::mem::take(&mut self.out)
    }

    fn tone(&mut self, f: f64, n: usize) {
        let inc = TAU * f / self.sr;
        for _ in 0..n {
            self.out.push(Complex64::from_polar(1.0, self.phase));
            self.phase = (self.phase + inc) % TAU;
        }
    }

    /// Silence of `n` samples.
    pub fn silence(&mut self, n: usize) {
        self.out
            .extend(std::iter::repeat_n(Complex64::new(0.0, 0.0), n));
    }

    fn sendsymbol(&mut self, sym: u32) {
        let n = self.mode.numtones;
        let tone = grayencode(sym as usize & (n - 1));
        let f = self.carrier - self.mode.bandwidth() / 2.0 + tone as f64 * self.mode.tone_spacing();
        self.tone(f, self.mode.symlen);
    }

    fn sendbit(&mut self, bit: bool) {
        let data = self.enc.encode(bit);
        for i in 0..2 {
            self.bitshreg = (self.bitshreg << 1) | u32::from((data >> i) & 1);
            self.bitstate += 1;
            if self.bitstate == self.mode.symbits {
                let s = self.inlv.bits(self.bitshreg);
                self.sendsymbol(s);
                self.bitstate = 0;
                self.bitshreg = 0;
            }
        }
    }

    fn sendchar(&mut self, c: u8) {
        for b in varicode::encode(c) {
            self.sendbit(b == 1);
        }
    }

    fn flushtx(&mut self, nbits: usize) {
        self.sendbit(true);
        for _ in 0..nbits {
            self.sendbit(false);
        }
        self.bitstate = 0;
    }

    /// The start of a transmission: the lead-in and `CR STX CR`.
    pub fn start(&mut self) {
        // clearbits: the interleaver filled with coded zeros, not sent
        let data = self.enc.encode(false);
        for _ in 0..self.mode.preamble {
            for i in 0..2 {
                self.bitshreg = (self.bitshreg << 1) | u32::from((data >> i) & 1);
                self.bitstate += 1;
                if self.bitstate == self.mode.symbits {
                    self.inlv.bits(self.bitshreg);
                    self.bitstate = 0;
                    self.bitshreg = 0;
                }
            }
        }
        if !self.mode.name.ends_with('L') {
            for _ in 0..self.mode.preamble / 3 {
                self.sendbit(false);
            }
        }
        for c in [b'\r', 2, b'\r'] {
            self.sendchar(c);
        }
    }

    pub fn text(&mut self, s: &[u8]) {
        for &c in s {
            self.sendchar(c);
        }
    }

    /// Idle between characters (fldigi `sendidle`).
    pub fn idle(&mut self) {
        self.sendchar(0);
        self.sendbit(true);
        for _ in 0..32 {
            self.sendbit(false);
        }
    }

    /// A picture: its header, then its pixels (RGB, row by row; a grey
    /// picture sends the first byte of each pixel).
    pub fn picture(&mut self, width: usize, height: usize, color: bool, rgb: &[u8]) {
        let header = format!(
            "\nSending Pic:{width}x{height}{};",
            if color { "C" } else { "" }
        );
        self.text(header.as_bytes());
        self.flushtx(self.mode.preamble);
        let bw = self.mode.bandwidth();
        self.tone(self.carrier - bw / 2.0, PIC_PROLOGUE);
        let mut bytes = Vec::new();
        for row in 0..height {
            if color {
                for c in 0..3 {
                    for col in 0..width {
                        bytes.push(rgb[(row * width + col) * 3 + c]);
                    }
                }
            } else {
                for col in 0..width {
                    bytes.push(rgb[(row * width + col) * 3]);
                }
            }
        }
        for b in bytes {
            let f = self.carrier + bw * (f64::from(b) - 128.0) / 256.0;
            self.tone(f, TX_SPP);
        }
        self.flushtx(self.mode.preamble);
    }

    /// The end of a transmission: `CR EOT CR` and the flush.
    pub fn end(&mut self) {
        for c in [b'\r', 4, b'\r'] {
            self.sendchar(c);
        }
        self.flushtx(self.mode.preamble);
    }

    /// An RSID for `code` of `table` (1 or 2) centred on the carrier, with
    /// its silences (a second-table code is sent after ESCAPE).
    pub fn rsid(&mut self, table: u8, code: u16) {
        let len = (rsid::SYMLEN_S * self.sr).floor() as usize;
        self.silence(5 * len);
        if table == 2 {
            self.rsid_tones(rsid::ESCAPE);
            self.silence(10 * len);
        }
        self.rsid_tones(code);
        self.silence(5 * len);
    }

    fn rsid_tones(&mut self, code: u16) {
        let len = (rsid::SYMLEN_S * self.sr).floor() as usize;
        let spacing = rsid::SAMPLE_RATE / 1024.0;
        let fr = self.carrier - spacing * 7.0;
        for t in rsid::encode(code) {
            self.tone(fr + f64::from(t) * spacing, len);
        }
    }
}
