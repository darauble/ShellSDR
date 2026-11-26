//! The MFSK receiver (fldigi `mfsk/mfsk.cxx`, receive half): analytic
//! signal, mixer, band-pass, sliding FFT, hard and soft decisions, CW
//! interference avoidance, deinterleaver, two Viterbi decoders, varicode,
//! symbol synchronization, AFC, S/N, and the picture receiver.
//!
//! Ported from fldigi 4.1.23 (GPL v3 or later; Dave Freese W1HKJ, from gMFSK
//! by Tomi Manninen OH2BNS; high speed modes by John Douyere VK2ETA). The
//! input is complex: fldigi's Hilbert filter is applied as a complex filter
//! (see `filters::Fir`), so a real signal (`x + 0j`) is decoded as fldigi
//! decodes audio, and IQ keeps only its positive frequencies.

use num_complex::Complex64;
use std::f64::consts::TAU;

use super::filters::{Fir, MovAvg, Sfft, decayavg};
use super::interleave::{Direction, Interleave};
use super::modes::Mode;
use super::varicode;
use super::viterbi::Viterbi;

/// Samples from a picture header to its first pixel (fldigi `tracepair`).
const PIC_DELAY: i64 = 352;
/// The characters kept to find a picture header.
const PICHEADER: usize = 63;
/// A tone that wins this many symbols in a row is taken as a carrier.
const CWI_MAXCOUNT: i32 = 6;

/// What the receiver found.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A decoded byte (NUL is dropped) and the metric when it came.
    Byte { byte: u8, metric: f64 },
    /// A picture header was read; its pixels follow.
    PictureStart {
        width: usize,
        height: usize,
        color: bool,
    },
    /// The picture is complete (`Rx::picture`).
    PictureDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    PictureStart,
    Picture,
}

/// A picture being received: RGB, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub color: bool,
    pub rgb: Vec<u8>,
    /// Pixels written (bytes / 3).
    pub done: usize,
}

pub struct Rx {
    mode: Mode,
    sr: f64,
    tonespacing: f64,
    basefreq: f64,
    bandwidth: f64,
    /// The carrier: the audio frequency of the tones' middle.
    frequency: f64,
    afc: bool,
    /// AFC works only above this metric (fldigi: the squelch level).
    afc_level: f64,
    phaseacc: f64,
    /// One per sideband: one, or two for the diversity of AM's sidebands.
    branches: Vec<Branch>,
    /// The tone magnitudes of the last two symbols, summed over the
    /// branches (fldigi's `pipe`, as magnitudes).
    mags: Vec<Vec<f64>>,
    pipeptr: usize,
    syncfilter: MovAvg,
    dec1: Viterbi,
    dec2: Viterbi,
    rxinlv: Interleave,
    vari: varicode::Decoder,
    state: State,
    counter: i64,
    synccounter: i64,
    symcounter: bool,
    met1: f64,
    met2: f64,
    metric: f64,
    currsymbol: usize,
    prev1symbol: usize,
    prev2symbol: usize,
    s2n: f64,
    afcmetric: f64,
    staticburst: bool,
    freqerr: f64,
    symbolpair: [u8; 2],
    symbolbit: usize,
    cwi: [i32; 32],
    picheader: Vec<u8>,
    rxspp: i64,
    picturesize: i64,
    pic: Option<Picture>,
    pixelnbr: usize,
    col: usize,
    row: usize,
    rgb: usize,
    picf: f64,
    events: Vec<Event>,
}

/// The front of the receiver for one sideband: analytic filter, band-pass
/// and sliding FFT, with the complex bins of the last two symbols.
struct Branch {
    hbfilt: Fir,
    bpfilt: Fir,
    sfft: Sfft,
    pipe: Vec<Vec<Complex64>>,
    /// The last band-passed sample (the picture's frequency).
    z: Complex64,
    prevz: Complex64,
}

impl Branch {
    fn new(mode: Mode, flo: f64, fhi: f64) -> Self {
        Self {
            hbfilt: Fir::hilbert(37),
            bpfilt: Fir::bandpass(127, flo, fhi),
            sfft: Sfft::new(mode.symlen, mode.basetone, mode.basetone + mode.numtones),
            pipe: vec![vec![Complex64::new(0.0, 0.0); mode.numtones]; 2 * mode.symlen],
            z: Complex64::new(0.0, 0.0),
            prevz: Complex64::new(0.0, 0.0),
        }
    }
}

fn graydecode(x: usize) -> usize {
    x ^ (x >> 1)
}

impl Rx {
    /// A receiver for one signal.
    pub fn new(mode: Mode, frequency: f64) -> Self {
        Self::with_branches(mode, frequency, 1)
    }

    /// A diversity receiver for the two sidebands of AM (`process2`): its
    /// decisions add the tone magnitudes of both, so a fade or a carrier in
    /// one sideband does not cancel the other.
    pub fn new_diversity(mode: Mode, frequency: f64) -> Self {
        Self::with_branches(mode, frequency, 2)
    }

    fn with_branches(mode: Mode, frequency: f64, n: usize) -> Self {
        let sr = f64::from(mode.sample_rate);
        let tonespacing = mode.tone_spacing();
        let basefreq = sr * mode.basetone as f64 / mode.symlen as f64;
        let bandwidth = mode.bandwidth();
        let cf = basefreq + bandwidth / 2.0;
        let flo = (cf - bandwidth / 2.0 - 2.0 * tonespacing) / sr;
        let fhi = (cf + bandwidth / 2.0 + 2.0 * tonespacing) / sr;
        let mut rx = Self {
            mode,
            sr,
            tonespacing,
            basefreq,
            bandwidth,
            frequency,
            afc: true,
            afc_level: 0.0,
            phaseacc: 0.0,
            branches: (0..n).map(|_| Branch::new(mode, flo, fhi)).collect(),
            mags: vec![vec![0.0; mode.numtones]; 2 * mode.symlen],
            pipeptr: 0,
            syncfilter: MovAvg::new(8),
            dec1: Viterbi::default(),
            dec2: Viterbi::default(),
            rxinlv: Interleave::new(mode.symbits, mode.depth, Direction::Reverse),
            vari: varicode::Decoder::default(),
            state: State::Data,
            counter: 0,
            synccounter: 0,
            symcounter: false,
            met1: 0.0,
            met2: 0.0,
            metric: 0.0,
            currsymbol: 0,
            prev1symbol: 0,
            prev2symbol: 0,
            s2n: 0.0,
            afcmetric: 0.0,
            staticburst: false,
            freqerr: 0.0,
            symbolpair: [0, 0],
            symbolbit: 0,
            cwi: [0; 32],
            picheader: vec![b' '; PICHEADER],
            rxspp: 8,
            picturesize: 0,
            pic: None,
            pixelnbr: 0,
            col: 0,
            row: 0,
            rgb: 0,
            picf: 0.0,
            events: Vec::new(),
        };
        rx.rx_init();
        rx
    }

    fn rx_init(&mut self) {
        self.state = State::Data;
        self.synccounter = 0;
        self.symcounter = false;
        self.met1 = 0.0;
        self.met2 = 0.0;
        self.counter = 0;
        self.rxspp = 8;
        for b in &mut self.branches {
            for v in &mut b.pipe {
                v.iter_mut().for_each(|z| *z = Complex64::new(0.0, 0.0));
            }
        }
        for v in &mut self.mags {
            v.iter_mut().for_each(|m| *m = 0.0);
        }
        self.freqerr = 0.0;
        self.syncfilter.reset();
        self.s2n = 0.0;
        self.picheader.iter_mut().for_each(|c| *c = b' ');
        self.staticburst = false;
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The carrier (Hz of the input).
    pub fn frequency(&self) -> f64 {
        self.frequency
    }

    pub fn set_frequency(&mut self, f: f64) {
        self.frequency = f;
        self.freqerr = 0.0;
    }

    pub fn set_afc(&mut self, on: bool, level: f64) {
        self.afc = on;
        self.afc_level = level;
    }

    /// The decoder's quality, 0..100 (fldigi's metric display).
    pub fn metric(&self) -> f64 {
        self.metric
    }

    /// Signal to noise, dB (fldigi's status line).
    pub fn s2n_db(&self) -> f64 {
        20.0 * self.s2n.max(1e-6).log10()
    }

    /// The picture being received or the last one.
    pub fn picture(&self) -> Option<&Picture> {
        self.pic.as_ref()
    }

    /// A picture is being received.
    pub fn in_picture(&self) -> bool {
        self.state != State::Data
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    fn check_picture_header(&mut self, c: u8) -> Option<(usize, usize, bool, i64)> {
        if (b' '..=b'z').contains(&c) {
            self.picheader.remove(0);
            self.picheader.push(c);
        }
        let s = &self.picheader;
        let start = s.windows(4).position(|w| w == b"Pic:")? + 4;
        let mut p = start;
        let num = |p: &mut usize| {
            let mut v = 0usize;
            while *p < s.len() && s[*p].is_ascii_digit() {
                v = v * 10 + (s[*p] - b'0') as usize;
                *p += 1;
            }
            v
        };
        if p >= s.len() {
            return None;
        }
        let w = num(&mut p);
        if s.get(p) != Some(&b'x') {
            return None;
        }
        p += 1;
        let h = num(&mut p);
        let mut color = false;
        if s.get(p) == Some(&b'C') {
            color = true;
            p += 1;
        }
        let ok = |w: usize, h: usize| w > 0 && h > 0 && w <= 4095 && h <= 4095;
        if s.get(p) == Some(&b';') {
            return ok(w, h).then_some((w, h, color, 8));
        }
        if s.get(p) != Some(&b'p') {
            return None;
        }
        p += 1;
        let spp = match s.get(p)? {
            b'4' => 4,
            b'2' => 2,
            _ => 8,
        };
        p += 1;
        (s.get(p) == Some(&b';') && ok(w, h)).then_some((w, h, color, spp))
    }

    fn recvchar(&mut self, c: u8) {
        if c == 0 {
            return;
        }
        self.events.push(Event::Byte {
            byte: c,
            metric: self.metric,
        });
        if let Some((w, h, color, spp)) = self.check_picture_header(c) {
            let symlen = self.mode.symlen as i64;
            let symbits = self.mode.symbits;
            self.counter = match self.mode.name {
                "MFSK-16" | "MFSK-32" if self.symbolbit == symbits => PIC_DELAY + symlen,
                "MFSK-64" => {
                    4956 + if self.symbolbit.is_multiple_of(2) {
                        symlen
                    } else {
                        0
                    }
                }
                "MFSK-128" => {
                    1824 + if self.symbolbit.is_multiple_of(2) {
                        symlen
                    } else {
                        0
                    }
                }
                "MFSK-31" => 5216 + if self.symbolbit == symbits { symlen } else { 0 },
                _ => PIC_DELAY,
            };
            self.rxspp = spp;
            self.state = State::PictureStart;
            self.picturesize = spp * (w * h * if color { 3 } else { 1 }) as i64;
            self.pic = Some(Picture {
                width: w,
                height: h,
                color,
                rgb: vec![0; w * h * 3],
                done: 0,
            });
            self.pixelnbr = 0;
            self.col = 0;
            self.row = 0;
            self.rgb = 0;
            self.picheader.iter_mut().for_each(|c| *c = b' ');
        } else {
            self.counter = 0;
        }
    }

    fn decodesymbol(&mut self, symbol: u8) {
        self.symbolpair = [self.symbolpair[1], symbol];
        self.symcounter = !self.symcounter;
        let (bit, metric) = if self.mode.symbits % 2 == 1 {
            // odd bits per symbol: two decoders, one per pair alignment
            if self.symcounter {
                let (c, met) = self.dec1.decode(self.symbolpair);
                self.met1 = decayavg(self.met1, met as f64, 50);
                if self.met1 < self.met2 {
                    return;
                }
                (c, self.met1)
            } else {
                let (c, met) = self.dec2.decode(self.symbolpair);
                self.met2 = decayavg(self.met2, met as f64, 50);
                if self.met2 < self.met1 {
                    return;
                }
                (c, self.met2)
            }
        } else {
            if self.symcounter {
                return;
            }
            let (c, met) = self.dec2.decode(self.symbolpair);
            self.met2 = decayavg(self.met2, met as f64, 50);
            (c, self.met2)
        };
        self.metric = ((metric - 60.0) * 0.5).clamp(0.0, 100.0);
        // a code that is no character (noise) is dropped, as fldigi's -1
        if let Some(Some(c)) = self.vari.bit(bit) {
            self.recvchar(c);
        }
    }

    fn softdecode(&mut self, bins: &[f64]) {
        let n = self.mode.numtones;
        let symbits = self.mode.symbits;
        let mut b = [0.0f64; 5];
        let mut sum: f64 = (0..n)
            .filter(|&i| self.cwi[i] < CWI_MAXCOUNT)
            .map(|i| bins[i])
            .sum();
        let avg = sum / n as f64;
        if sum < 1e-10 {
            sum = 1e-10;
        }
        let cwisymbol = self.currsymbol;
        for k in 1..n {
            if k == cwisymbol {
                self.cwi[k] += 1;
            } else {
                self.cwi[k] -= 1;
            }
            self.cwi[k] = self.cwi[k].clamp(0, CWI_MAXCOUNT + 1);
            if self.cwi[k] > CWI_MAXCOUNT {
                self.cwi[k] = CWI_MAXCOUNT + 1;
            }
        }
        for (i, bin) in bins.iter().enumerate().take(n) {
            let j = graydecode(i);
            let binmag = if self.cwi[i] > CWI_MAXCOUNT {
                avg
            } else if cwisymbol == i {
                2.0 * bin
            } else {
                *bin
            };
            for (k, bk) in b.iter_mut().enumerate().take(symbits) {
                if j & (1 << (symbits - k - 1)) != 0 {
                    *bk += binmag;
                } else {
                    *bk -= binmag;
                }
            }
        }
        let mut symbols = [0u8; 5];
        for i in 0..symbits {
            symbols[i] = if self.staticburst {
                128
            } else {
                (128.0 + b[i] / sum * 256.0).clamp(0.0, 255.0) as u8
            };
        }
        self.rxinlv.symbols(&mut symbols[..symbits]);
        for (i, &s) in symbols.iter().enumerate().take(symbits) {
            self.symbolbit = i + 1;
            self.decodesymbol(s);
            if self.counter != 0 {
                return;
            }
        }
    }

    /// The mixer's turn for this sample (fldigi `mixer`).
    fn mixer(&mut self) -> Complex64 {
        let f =
            self.frequency - (self.tonespacing * self.mode.basetone as f64 + self.bandwidth / 2.0);
        let out = Complex64::from_polar(1.0, self.phaseacc);
        self.phaseacc = (self.phaseacc - TAU * f / self.sr).rem_euclid(TAU);
        out
    }

    fn harddecode(&mut self, bins: &[f64]) -> usize {
        let n = self.mode.numtones;
        let avg = (bins.iter().take(n).sum::<f64>() / n as f64).max(1e-20);
        let mut max = 0.0;
        let mut symbol = 0;
        let mut burst = 0;
        for (i, &x) in bins.iter().enumerate().take(n) {
            if x > max {
                max = x;
                symbol = i;
            }
            if x > 2.0 * avg {
                burst += 1;
            }
        }
        self.staticburst = burst == n;
        self.afcmetric = if self.staticburst {
            0.0
        } else {
            0.95 * self.afcmetric + 0.05 * (2.0 * max / avg)
        };
        symbol
    }

    fn synchronize(&mut self) {
        if self.currsymbol == self.prev1symbol || self.prev1symbol == self.prev2symbol {
            return;
        }
        let len = 2 * self.mode.symlen;
        let mut j = self.pipeptr;
        let mut syn = -1.0;
        let mut max = 0.0;
        for i in 0..len {
            let v = self.mags[j][self.prev1symbol];
            if v > max {
                max = v;
                syn = i as f64;
            }
            j = (j + 1) % len;
        }
        let syn = self.syncfilter.run(syn);
        self.synccounter +=
            ((syn - self.mode.symlen as f64) / self.mode.numtones as f64 + 0.5).floor() as i64;
    }

    fn afc(&mut self) {
        if self.staticburst
            || !self.afc
            || self.metric < self.afc_level
            || self.afcmetric < 3.0
            || self.currsymbol != self.prev1symbol
        {
            return;
        }
        let len = 2 * self.mode.symlen;
        let (k, now, before) = (
            self.currsymbol,
            self.pipeptr,
            (self.pipeptr + len - 1) % len,
        );
        let z: Complex64 = self
            .branches
            .iter()
            .map(|b| b.pipe[before][k].conj() * b.pipe[now][k])
            .sum();
        let f = z.arg() * self.sr / TAU;
        let f1 = self.tonespacing * (self.mode.basetone + self.currsymbol) as f64;
        if (f1 - f).abs() < self.tonespacing / 4.0 {
            // fldigi subtracts the average error itself each symbol, an
            // integrator that overshoots and rings; a first-order loop with
            // the same smoothing settles on the signal
            self.freqerr = decayavg(self.freqerr, f1 - f, 32);
            self.frequency -= (f1 - f) / 32.0;
        }
    }

    fn eval_s2n(&mut self) {
        let sig = self.mags[self.pipeptr][self.currsymbol];
        let noise = (self.mode.numtones as f64 - 1.0) * self.mags[self.pipeptr][self.prev2symbol];
        if noise > 0.0 {
            self.s2n = decayavg(self.s2n, sig / noise, 64);
        }
    }

    fn recvpic(&mut self) {
        let turn: Complex64 = self.branches.iter().map(|b| b.prevz.conj() * b.z).sum();
        self.picf += turn.arg() * self.sr / TAU;
        for b in &mut self.branches {
            b.prevz = b.z;
        }
        if self.counter % self.rxspp != 0 {
            return;
        }
        let picf = 256.0 * (self.picf / self.rxspp as f64 - self.basefreq) / self.bandwidth;
        let byte = picf.clamp(0.0, 255.0) as u8;
        self.picf = 0.0;
        let Some(p) = self.pic.as_mut() else { return };
        if p.color {
            let n = self.rgb + self.row + 3 * self.col;
            if let Some(px) = p.rgb.get_mut(n) {
                *px = byte;
            }
            self.col += 1;
            if self.col == p.width {
                self.col = 0;
                self.rgb += 1;
                if self.rgb == 3 {
                    self.rgb = 0;
                    self.row += 3 * p.width;
                }
            }
            p.done =
                (self.row / 3 + if self.rgb == 2 { self.col } else { 0 }).min(p.width * p.height);
        } else {
            for _ in 0..3 {
                if let Some(px) = p.rgb.get_mut(self.pixelnbr) {
                    *px = byte;
                }
                self.pixelnbr += 1;
            }
            p.done = (self.pixelnbr / 3).min(p.width * p.height);
        }
    }

    /// One sample in (at the mode's rate).
    pub fn process(&mut self, x: Complex64) {
        self.process_branches(&[x]);
    }

    /// One sample of each sideband in (`new_diversity`): the upper, and
    /// the lower conjugated (both read as the same audio).
    pub fn process2(&mut self, upper: Complex64, lower: Complex64) {
        self.process_branches(&[upper, lower]);
    }

    fn process_branches(&mut self, xs: &[Complex64]) {
        let turn = self.mixer();
        let ptr = self.pipeptr;
        let mags = &mut self.mags[ptr];
        mags.iter_mut().for_each(|m| *m = 0.0);
        for (b, &x) in self.branches.iter_mut().zip(xs) {
            let z = b.hbfilt.run(x) * turn;
            b.z = b.bpfilt.run(z);
            let bins = b.sfft.run(b.z);
            b.pipe[ptr].copy_from_slice(bins);
            for (m, bin) in mags.iter_mut().zip(bins) {
                *m += bin.norm();
            }
        }

        if self.state == State::PictureStart {
            self.counter -= 1;
            if self.counter == 0 {
                self.counter = self.picturesize;
                self.state = State::Picture;
                let p = self.pic.as_ref().expect("set with the header");
                self.events.push(Event::PictureStart {
                    width: p.width,
                    height: p.height,
                    color: p.color,
                });
            }
        }
        if self.state == State::Picture {
            self.counter -= 1;
            if self.counter == 0 {
                if let Some(p) = self.pic.as_mut() {
                    p.done = p.width * p.height;
                }
                self.events.push(Event::PictureDone);
                self.rx_init();
            } else {
                self.recvpic();
            }
            return;
        }

        self.synccounter -= 1;
        if self.synccounter <= 0 {
            self.synccounter = self.mode.symlen as i64;
            let bins = self.mags[self.pipeptr].clone();
            self.currsymbol = self.harddecode(&bins);
            self.softdecode(&bins);
            self.synchronize();
            self.afc();
            self.eval_s2n();
            self.prev2symbol = self.prev1symbol;
            self.prev1symbol = self.currsymbol;
        }
        self.pipeptr = (self.pipeptr + 1) % (2 * self.mode.symlen);
    }
}
