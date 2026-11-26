//! The HFDL receiver (HFDL seed `04-algorithms/01-receiver.md`): wideband
//! samples -> frames on every channel of the band. The method follows the
//! seed's (dumphfdl's): channel filter, 3 samples per symbol, gain control,
//! matched filter, symbol timing at 2 outputs per symbol, a Costas loop, a
//! fractionally spaced equaliser trained on the T sequences, and the frame
//! search on A, A, M1. Every channel runs two such demodulators with
//! different settings (`TUNINGS`); a frame both decode comes out once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::phy::{
    self, A_LEN, CARRIER_OFFSET_HZ, M1_LEN, M2_LEN, PREKEY, SYMBOL_RATE, T_LEN, T_PREAMBLE, phy,
};

const SPS: f64 = 3.0;
const CH_RATE: f64 = SYMBOL_RATE * SPS;
/// The symbol timing loop's gain (Gardner error, outputs at 2 per symbol).
const TIMING_GAIN: f32 = -0.01;
/// Each channel is demodulated twice, with these settings, and a frame both
/// decode is kept from the one whose FCS checks pass. Near the noise the
/// settings decide frames either way, so the two together decode more than
/// either: 1058 good frames against 999 on five real recordings (1 h 51 min,
/// 6.6 to 13.3 MHz), SPECIFICATION 26.6.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    /// The gain control's step per sample at 5400 Hz.
    agc: f32,
    /// The equaliser's normalised LMS step.
    eq_mu: f32,
}

const TUNINGS: [Tuning; 2] = [
    Tuning {
        agc: 0.01,
        eq_mu: 0.2,
    },
    Tuning {
        agc: 0.002,
        eq_mu: 0.1,
    },
];

/// How long a frame waits for the other demodulator's copy (s of input).
const MERGE_HOLD_S: f64 = 0.25;
/// The same in the data segments, decision-directed (0: off).
const EQ_MU_DD: f32 = 0.0;
/// The Costas loop's gains (phase, frequency), per symbol.
const COSTAS_ALPHA: f32 = 0.1;
const COSTAS_BETA: f32 = 0.00047;

/// The matched filter (the seed's pulse shape), 3 samples per symbol.
const MF: [f32; 19] = [
    -0.017097465,
    0.011482315,
    0.031383757,
    0.009454399,
    -0.041616442,
    -0.06451565,
    -0.005495793,
    0.13164047,
    0.27596932,
    0.3375902,
    0.27596932,
    0.13164047,
    -0.005495793,
    -0.06451565,
    -0.041616442,
    0.009454399,
    0.031383757,
    0.011482315,
    -0.017097465,
];

/// A frame found on a channel.
static TRACE: AtomicBool = AtomicBool::new(false);

/// Diagnostics for tuning the receiver (the conformance tests' long
/// recordings): every frame search that got past the first A is told on
/// standard error as `trace <start s> <kHz> <stage> <score>`, the stage
/// where it ended (`a2`, `m1`, or `frame`). Off by default.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub freq_khz: u32,
    pub m1: usize,
    pub octets: Vec<u8>,
    /// Seconds from the receiver's first input sample to the frame's start.
    pub t: f64,
    pub level_db: f32,
    pub noise_db: f32,
    pub freq_err_hz: f32,
}

// ── channelizer: FFT overlap-save, one forward FFT for all channels ────────

struct ChanOut {
    k0: isize,
    resid_hz: f64,
    resid_phase: f64,
    ifft: Arc<dyn Fft<f32>>,
}

/// Splits the band into channels at an intermediate rate.
pub struct Channelizer {
    n: usize,
    m: usize,
    hop: usize,
    fwd: Arc<dyn Fft<f32>>,
    h: Vec<Complex32>,
    buf: Vec<Complex32>,
    chans: Vec<ChanOut>,
    block: u64,
    pub out_rate: f64,
}

impl Channelizer {
    /// `offsets`: each channel's signal center relative to the band center (Hz).
    pub fn new(fs: f64, offsets: &[f64]) -> Self {
        // FFT size: about 30 ms of input, a power of two
        let n = ((fs * 0.03) as usize).next_power_of_two().max(1024);
        // output rate at least 4.5 kHz wide each side: M bins
        let mut m = 16usize;
        while (m as f64) * fs / (n as f64) < 9000.0 {
            m *= 2;
        }
        let m = m.min(n);
        let taps = n / 4 + 1;
        let hop = n - (taps - 1);
        let mut planner = FftPlanner::new();
        let fwd = planner.plan_fft_forward(n);
        // low-pass of the channel: cut-off 1.35 kHz, Kaiser-windowed sinc
        let fc = 1350.0 / fs;
        let mut h: Vec<Complex32> = (0..n)
            .map(|i| {
                if i >= taps {
                    return Complex32::new(0.0, 0.0);
                }
                let x = i as f64 - (taps - 1) as f64 / 2.0;
                let sinc = if x == 0.0 {
                    2.0 * fc
                } else {
                    (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
                };
                let w = kaiser(i, taps, 8.0);
                Complex32::new((sinc * w) as f32, 0.0)
            })
            .collect();
        fwd.process(&mut h);
        let scale = 1.0 / n as f32;
        for v in h.iter_mut() {
            *v *= scale;
        }
        let chans = offsets
            .iter()
            .map(|&off| {
                let k0 = (off * n as f64 / fs).round() as isize;
                ChanOut {
                    k0,
                    resid_hz: off - k0 as f64 * fs / n as f64,
                    resid_phase: 0.0,
                    ifft: planner.plan_fft_inverse(m),
                }
            })
            .collect();
        Self {
            n,
            m,
            hop,
            fwd,
            h,
            buf: vec![Complex32::new(0.0, 0.0); n - hop],
            chans,
            block: 0,
            out_rate: fs * m as f64 / n as f64,
        }
    }

    /// Feed samples; `out[c]` gets channel c's new samples.
    pub fn process(&mut self, x: &[Complex32], out: &mut [Vec<Complex32>]) {
        self.buf.extend_from_slice(x);
        let (n, m, hop) = (self.n, self.m, self.hop);
        let d = n / m;
        let keep = hop / d;
        let skip = (n - hop) / d;
        let mut spec = vec![Complex32::new(0.0, 0.0); n];
        let mut small = vec![Complex32::new(0.0, 0.0); m];
        while self.buf.len() >= n {
            spec.copy_from_slice(&self.buf[..n]);
            self.fwd.process(&mut spec);
            for (ci, ch) in self.chans.iter_mut().enumerate() {
                for (j, s) in small.iter_mut().enumerate() {
                    // bins k0 - m/2 .. k0 + m/2, filter centred on 0
                    let rel = j as isize - (m / 2) as isize;
                    let k = (ch.k0 + rel).rem_euclid(n as isize) as usize;
                    let hk = rel.rem_euclid(n as isize) as usize;
                    s.re = 0.0;
                    s.im = 0.0;
                    *s = spec[k] * self.h[hk];
                }
                // put bin 0 of the channel at index 0
                small.rotate_left(m / 2);
                ch.ifft.process(&mut small);
                // the shift by k0 bins turns per block by 2 pi k0 hop / n:
                // reduced exactly (an angle of hours of samples has no
                // precision left in a float)
                let turns =
                    (ch.k0 as i128 * self.block as i128 * hop as i128).rem_euclid(n as i128) as f64;
                let ph = -2.0 * std::f64::consts::PI * turns / n as f64;
                let rot = Complex32::from_polar(1.0, ph as f32);
                let w = 2.0 * std::f64::consts::PI * ch.resid_hz / self.out_rate;
                for s in &small[skip..skip + keep] {
                    let r = Complex32::from_polar(1.0, -ch.resid_phase as f32);
                    out[ci].push(s * rot * r);
                    ch.resid_phase = (ch.resid_phase + w) % (2.0 * std::f64::consts::PI);
                }
            }
            self.block += 1;
            self.buf.drain(..hop);
        }
    }
}

fn kaiser(i: usize, n: usize, beta: f64) -> f64 {
    fn i0(x: f64) -> f64 {
        let (mut s, mut t) = (1.0, 1.0);
        for k in 1..30 {
            t *= (x / 2.0 / k as f64).powi(2);
            s += t;
        }
        s
    }
    let r = 2.0 * i as f64 / (n - 1) as f64 - 1.0;
    i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0(beta)
}

// ── one channel's demodulator ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    A1,
    A2,
    M1,
    M2,
    Train,
    Data1,
    Data2,
}

struct Demod {
    freq_khz: u32,
    // resampling to 5400 Hz
    step: f64,
    mu: f64,
    hist: [Complex32; 4],
    // gain control
    power: f32,
    level: f32,
    noise: f32,
    noise_clk: u32,
    // matched filter
    /// The matched filter's last 19 inputs, twice over (so the window is
    /// one slice): written at `mf_i` and `mf_i + 19`.
    mf: [Complex32; 38],
    mf_i: usize,
    // symbol timing (2 outputs per symbol, Gardner)
    sym_buf: Vec<Complex32>,
    tau: f64,
    out_idx: u64,
    prev: [Complex32; 2],
    // carrier
    phi: f32,
    dphi: f32,
    // equaliser
    eq: [Complex32; 15],
    eq_x: [Complex32; 15],
    // framing
    state: State,
    wanted: usize,
    retries: u32,
    /// The last 127 bits, the newest in bit 0, and how many there are.
    bits: u128,
    nbits: usize,
    bitmask: u8,
    mode: usize,
    arity: usize,
    segments_left: usize,
    t_seq: usize,
    t_idx: usize,
    data: Vec<Complex32>,
    sig_sum: f32,
    sig_n: f32,
    syms_without_frame: u64,
    n5400: u64,
    frame_n5400: u64,
    ferr: f32,
    /// When the first A was found, and the weakest A match (for the trace).
    a1_n5400: u64,
    a_score: f32,
    tuning: Tuning,
}

impl Demod {
    fn new(freq_khz: u32, in_rate: f64, tuning: Tuning) -> Self {
        let mut eq = [Complex32::new(0.0, 0.0); 15];
        eq[6] = Complex32::new(1.0, 0.0);
        Self {
            freq_khz,
            step: in_rate / CH_RATE,
            mu: 0.0,
            hist: [Complex32::new(0.0, 0.0); 4],
            power: 1e-6,
            level: 0.0,
            noise: 1.0,
            noise_clk: 0,
            mf: [Complex32::new(0.0, 0.0); 38],
            mf_i: 0,
            sym_buf: Vec::new(),
            tau: 0.0,
            out_idx: 0,
            prev: [Complex32::new(0.0, 0.0); 2],
            phi: 0.0,
            dphi: 0.0,
            eq,
            eq_x: [Complex32::new(0.0, 0.0); 15],
            state: State::A1,
            wanted: 1,
            retries: 0,
            bits: 0,
            nbits: 0,
            bitmask: 0,
            mode: 0,
            arity: 1,
            segments_left: 0,
            t_seq: 0,
            t_idx: 0,
            data: Vec::new(),
            sig_sum: 0.0,
            sig_n: 0.0,
            syms_without_frame: 0,
            n5400: 0,
            frame_n5400: 0,
            ferr: 0.0,
            a1_n5400: 0,
            a_score: 0.0,
            tuning,
        }
    }

    fn reset_frame(&mut self) {
        self.state = State::A1;
        self.wanted = 1;
        self.retries = 0;
        self.arity = 1;
        self.bitmask = 0;
        self.data.clear();
        self.eq = [Complex32::new(0.0, 0.0); 15];
        self.eq[6] = Complex32::new(1.0, 0.0);
    }

    /// Feed channel samples at the channelizer's rate.
    fn process(&mut self, x: &[Complex32], frames: &mut Vec<Frame>) {
        for &s in x {
            self.hist.rotate_left(1);
            self.hist[3] = s;
            while self.mu < 1.0 {
                // cubic interpolation between hist[1] and hist[2]
                let m = self.mu as f32;
                let [y0, y1, y2, y3] = self.hist;
                let a = y3 * 0.5 - y2 * 1.5 + y1 * 1.5 - y0 * 0.5;
                let b = y0 - y1 * 2.5 + y2 * 2.0 - y3 * 0.5;
                let c = (y2 - y0) * 0.5;
                let v = ((a * m + b) * m + c) * m + y1;
                self.sample(v, frames);
                self.mu += self.step;
            }
            self.mu -= 1.0;
        }
    }

    /// One sample at 5400 Hz.
    fn sample(&mut self, x: Complex32, frames: &mut Vec<Frame>) {
        self.n5400 += 1;
        // gain control: normalise to unit power
        self.power += self.tuning.agc * (x.norm_sqr() - self.power);
        let lvl = self.power.sqrt().max(1e-9);
        self.level = lvl;
        let r = x / lvl;
        if self.state == State::A1 {
            self.noise_clk += 1;
            if self.noise_clk & 0xFF == 0xFF {
                self.noise = 0.65 * self.noise + 0.35 * self.noise.min(lvl) + 1e-6;
            }
        }
        // matched filter
        self.mf[self.mf_i] = r;
        self.mf[self.mf_i + 19] = r;
        self.mf_i = if self.mf_i == 18 { 0 } else { self.mf_i + 1 };
        let mut y = Complex32::new(0.0, 0.0);
        for (x, &h) in self.mf[self.mf_i..self.mf_i + 19].iter().zip(&MF) {
            y += x * h;
        }
        self.sym_buf.push(y * 2.2);
        // symbol timing: outputs every 1.5 samples, interpolated at tau
        while self.sym_buf.len() >= 4 && self.tau + 2.0 < self.sym_buf.len() as f64 {
            let i = self.tau.floor() as usize;
            let m = (self.tau - i as f64) as f32;
            let (y0, y1, y2, y3) = (
                self.sym_buf[i],
                self.sym_buf[i + 1],
                self.sym_buf[i + 2],
                self.sym_buf
                    .get(i + 3)
                    .copied()
                    .unwrap_or(self.sym_buf[i + 2]),
            );
            let a = y3 * 0.5 - y2 * 1.5 + y1 * 1.5 - y0 * 0.5;
            let b = y0 - y1 * 2.5 + y2 * 2.0 - y3 * 0.5;
            let c = (y2 - y0) * 0.5;
            let v = ((a * m + b) * m + c) * m + y1;
            self.tau += 1.5;
            self.half_symbol(v, frames);
            if self.tau >= 8.0 {
                self.sym_buf.drain(..8);
                self.tau -= 8.0;
            }
        }
    }

    fn trace(&self, stage: &str, score: f32) {
        if TRACE.load(Ordering::Relaxed) {
            let t = self.a1_n5400 as f64 / CH_RATE - (PREKEY + A_LEN) as f64 / SYMBOL_RATE;
            eprintln!("trace {t:.2} {} {stage} {score:.2}", self.freq_khz);
        }
    }

    fn costas_reset(&mut self) {
        self.phi = 0.0;
        self.dphi = 0.0;
    }

    /// One output at 2 per symbol.
    fn half_symbol(&mut self, v: Complex32, frames: &mut Vec<Frame>) {
        // carrier: step per output (as the reference)
        self.phi += self.dphi;
        if self.phi > std::f32::consts::PI {
            self.phi -= std::f32::consts::TAU;
        } else if self.phi < -std::f32::consts::PI {
            self.phi += std::f32::consts::TAU;
        }
        let r = v * Complex32::from_polar(1.0, -self.phi);
        if self.dphi.abs() > 0.25 && self.state == State::A1 {
            self.costas_reset();
        }
        self.eq_x.rotate_right(1);
        self.eq_x[0] = r;
        self.out_idx += 1;
        // Gardner timing error at the on-time output
        let on_time = self.out_idx & 1 == 0;
        if !on_time {
            self.prev[0] = r; // the mid-symbol sample
            return;
        }
        let mid = self.prev[0];
        let e = ((self.prev[1] - r) * mid.conj()).re;
        self.prev[1] = r;
        self.tau = (self.tau + (TIMING_GAIN * e.clamp(-1.0, 1.0)).clamp(-0.2, 0.2) as f64).max(0.0);
        // equaliser output
        let mut s = Complex32::new(0.0, 0.0);
        for (w, x) in self.eq.iter().zip(&self.eq_x) {
            s += w * x;
        }
        if self.state == State::Train {
            let p = phy();
            let tb = p.t[self.t_idx.min(T_LEN - 1)] ^ (self.bitmask & 1);
            let d = Complex32::new(if tb == 0 { 1.0 } else { -1.0 }, 0.0);
            let err = d - s;
            let energy: f32 = self.eq_x.iter().map(|x| x.norm_sqr()).sum::<f32>() + 1e-3;
            for (w, x) in self.eq.iter_mut().zip(&self.eq_x) {
                *w += err * x.conj() * (self.tuning.eq_mu / energy);
            }
            self.t_idx += 1;
        }
        // decision and phase error
        let arity = self.arity;
        let m = 1usize << arity;
        let ang = s.arg();
        let k = ((ang / (std::f32::consts::TAU / m as f32)).round() as i32).rem_euclid(m as i32)
            as usize;
        let ref_ = Complex32::from_polar(1.0, k as f32 * std::f32::consts::TAU / m as f32);
        // decision-directed equaliser updates in the data segments
        if EQ_MU_DD > 0.0 && matches!(self.state, State::Data1 | State::Data2) {
            let err = ref_ - s;
            let energy: f32 = self.eq_x.iter().map(|x| x.norm_sqr()).sum::<f32>() + 1e-3;
            for (w, x) in self.eq.iter_mut().zip(&self.eq_x) {
                *w += err * x.conj() * (EQ_MU_DD / energy);
            }
        }
        let perr = (s * ref_.conj()).im.clamp(-1.0, 1.0);
        self.phi += COSTAS_ALPHA * perr;
        self.dphi = (self.dphi + COSTAS_BETA * perr).clamp(-0.5, 0.5);
        self.symbol(s, k as u32, frames);
    }

    fn symbol(&mut self, s: Complex32, k: u32, frames: &mut Vec<Frame>) {
        let p = phy();
        self.syms_without_frame += 1;
        if self.state == State::A1 && self.syms_without_frame > 13 * 4219 {
            self.syms_without_frame = 0;
            self.costas_reset();
        }
        match self.state {
            State::A1 | State::A2 | State::M1 => {
                let bit = (if self.arity == 1 {
                    k as u8
                } else {
                    (s.re < 0.0) as u8
                }) ^ (self.bitmask & 1);
                self.bits = (self.bits << 1 | bit as u128) & SEQ_MASK;
                self.nbits = (self.nbits + 1).min(M1_LEN);
            }
            State::Train | State::Data1 | State::Data2 => {
                if matches!(self.state, State::Data1 | State::Data2) {
                    self.data.push(s);
                }
            }
            State::M2 => {}
        }
        if self.state != State::A1 {
            self.sig_sum += self.level;
            self.sig_n += 1.0;
        }
        if self.wanted > 1 {
            self.wanted -= 1;
            return;
        }
        let (bits, nbits) = (self.bits, self.nbits);
        let corr = |seq: u128| -> f32 {
            if nbits < A_LEN {
                return 0.0;
            }
            let differ = (seq ^ bits).count_ones();
            1.0 - 2.0 * differ as f32 / A_LEN as f32
        };
        let seqs = seq_regs();
        match self.state {
            State::A1 => {
                let c = corr(seqs.a);
                if c.abs() > 0.36 {
                    self.a1_n5400 = self.n5400;
                    self.a_score = c.abs();
                    self.bitmask = if c > 0.0 { 0 } else { 1 };
                    self.sig_sum = self.level;
                    self.sig_n = 1.0;
                    self.wanted = A_LEN;
                    self.retries = 0;
                    self.state = State::A2;
                }
            }
            State::A2 => {
                // bits are stored with the bitmask applied: A now correlates positively
                let c = corr(seqs.a);
                if c.abs() > 0.30 {
                    self.a_score = self.a_score.min(c.abs());
                    self.frame_n5400 = self.n5400;
                    self.ferr = self.dphi * 2.0 * SYMBOL_RATE as f32 / std::f32::consts::TAU;
                    self.wanted = M1_LEN;
                    self.retries = 0;
                    self.state = State::M1;
                } else {
                    self.retries += 1;
                    self.a_score = self.a_score.min(c.abs());
                    if self.retries >= 3 {
                        self.trace("a2", self.a_score);
                        self.reset_frame();
                    }
                }
            }
            State::M1 => {
                let (mut best, mut bi) = (0.0f32, None);
                for (i, &m1) in seqs.m1.iter().enumerate() {
                    let c = corr(m1).abs();
                    if c > best {
                        best = c;
                        bi = Some(i);
                    }
                }
                match bi {
                    Some(i) if best > 0.30 => {
                        self.mode = i;
                        self.segments_left = p.modes[i].segments;
                        self.wanted = M2_LEN;
                        self.state = State::M2;
                    }
                    _ => {
                        self.trace("m1", best);
                        self.reset_frame()
                    }
                }
            }
            State::M2 => {
                self.t_seq = T_PREAMBLE;
                self.t_idx = 0;
                self.wanted = T_LEN;
                self.state = State::Train;
            }
            State::Train => {
                if self.t_seq > 1 {
                    self.t_seq -= 1;
                    self.t_idx = 0;
                    self.wanted = T_LEN;
                } else if self.segments_left > 0 {
                    self.wanted = 15;
                    self.state = State::Data1;
                    self.arity = p.modes[self.mode].bits_per_symbol;
                } else {
                    self.finish(frames);
                }
            }
            State::Data1 => {
                self.wanted = 15;
                self.state = State::Data2;
            }
            State::Data2 => {
                self.segments_left -= 1;
                self.arity = 1;
                self.state = State::Train;
                self.t_seq = 1;
                self.t_idx = 0;
                self.wanted = T_LEN;
            }
        }
    }

    fn finish(&mut self, frames: &mut Vec<Frame>) {
        let p = phy();
        let mode = &p.modes[self.mode];
        let bps = mode.bits_per_symbol;
        let mut soft = Vec::with_capacity(self.data.len() * bps);
        for (i, &s) in self.data.iter().enumerate() {
            let mut s = s;
            if p.scrambler[i % p.scrambler.len()] == 1 {
                s = -s;
            }
            if self.bitmask & 1 == 1 {
                s = -s;
            }
            phy::soft_bits(s.re, s.im, bps, &mut soft);
        }
        let octets = phy::decode_soft(mode, &soft);
        self.trace("frame", self.a_score);
        // the frame began (448 + 254) symbols before the end of the second A
        let t = self.frame_n5400 as f64 / CH_RATE - (PREKEY + 2 * A_LEN) as f64 / SYMBOL_RATE;
        let lvl = if self.sig_n > 0.0 {
            self.sig_sum / self.sig_n
        } else {
            self.level
        };
        frames.push(Frame {
            freq_khz: self.freq_khz,
            m1: self.mode,
            octets,
            t,
            level_db: 20.0 * lvl.max(1e-9).log10(),
            noise_db: 20.0 * self.noise.max(1e-9).log10(),
            freq_err_hz: self.ferr,
        });
        self.syms_without_frame = 0;
        self.reset_frame();
    }
}

/// A frame waiting for its copy from the channel's other demodulator.
struct Held {
    frame: Frame,
    quality: u32,
    /// Input sample count when it goes out.
    due: u64,
}

const SEQ_MASK: u128 = (1 << A_LEN) - 1;

/// A and the modes' M1 as 127-bit registers like `Demod::bits` (the first
/// bit in bit 126), for correlating by XOR and popcount.
struct SeqRegs {
    a: u128,
    m1: Vec<u128>,
}

fn seq_regs() -> &'static SeqRegs {
    static R: std::sync::OnceLock<SeqRegs> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        let reg = |seq: &[u8]| seq.iter().fold(0u128, |r, &b| r << 1 | b as u128);
        let p = phy();
        SeqRegs {
            a: reg(&p.a),
            m1: p.modes.iter().map(|m| reg(&m.m1)).collect(),
        }
    })
}

/// The receiver of a band: every listed channel.
pub struct Receiver {
    chan: Channelizer,
    /// `TUNINGS.len()` per channel, channel by channel.
    demods: Vec<Demod>,
    bufs: Vec<Vec<Complex32>>,
    /// Seconds of the channelizer's delay, removed from frame times.
    delay_s: f64,
    fs: f64,
    fed: u64,
    held: Vec<Held>,
}

impl Receiver {
    /// `fs`: input rate; `center_hz`: the input's center; `channels_khz`:
    /// assigned frequencies.
    pub fn new(fs: f64, center_hz: f64, channels_khz: &[u32]) -> Self {
        let offs: Vec<f64> = channels_khz
            .iter()
            .map(|&k| k as f64 * 1000.0 + CARRIER_OFFSET_HZ - center_hz)
            .collect();
        let chan = Channelizer::new(fs, &offs);
        let demods = channels_khz
            .iter()
            .flat_map(|&k| TUNINGS.map(|t| (k, t)))
            .map(|(k, t)| Demod::new(k, chan.out_rate, t))
            .collect();
        let delay_s = (chan.n / 8) as f64 / fs + 9.0 / CH_RATE;
        Self {
            bufs: vec![Vec::new(); channels_khz.len()],
            chan,
            demods,
            delay_s,
            fs,
            fed: 0,
            held: Vec::new(),
        }
    }

    /// Feed samples; returns the frames completed, time-ordered by end.
    pub fn process(&mut self, x: &[Complex32]) -> Vec<Frame> {
        for b in self.bufs.iter_mut() {
            b.clear();
        }
        self.chan.process(x, &mut self.bufs);
        self.fed += x.len() as u64;
        let mut frames = Vec::new();
        for (ds, b) in self.demods.chunks_mut(TUNINGS.len()).zip(&self.bufs) {
            for d in ds {
                d.process(b, &mut frames);
            }
        }
        for mut f in frames {
            f.t -= self.delay_s;
            self.hold(f);
        }
        self.release()
    }

    /// Hold a frame for its copy from the channel's other demodulator; of
    /// the two, the one with more FCS-valid parts is kept (the first on a
    /// tie).
    fn hold(&mut self, f: Frame) {
        let quality = super::pdu::frame_quality(&f.octets);
        if let Some(h) = self
            .held
            .iter_mut()
            .find(|h| h.frame.freq_khz == f.freq_khz && (h.frame.t - f.t).abs() < 0.5)
        {
            if quality > h.quality {
                h.frame = f;
                h.quality = quality;
            }
        } else {
            self.held.push(Held {
                frame: f,
                quality,
                due: self.fed + (MERGE_HOLD_S * self.fs) as u64,
            });
        }
    }

    /// The held frames whose time is up, in time order.
    fn release(&mut self) -> Vec<Frame> {
        let fed = self.fed;
        let (out, keep): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|h| h.due <= fed);
        self.held = keep;
        let mut out: Vec<Frame> = out.into_iter().map(|h| h.frame).collect();
        out.sort_by(|a, b| a.t.total_cmp(&b.t));
        out
    }

    /// The channels decoded.
    pub fn channels(&self) -> Vec<u32> {
        self.demods
            .iter()
            .step_by(TUNINGS.len())
            .map(|d| d.freq_khz)
            .collect()
    }

    /// Each channel's state now.
    pub fn status(&self) -> Vec<ChannelStatus> {
        self.demods
            .chunks(TUNINGS.len())
            .map(|ds| {
                let d = &ds[0];
                ChannelStatus {
                    freq_khz: d.freq_khz,
                    decoding: ds.iter().any(|d| !matches!(d.state, State::A1 | State::A2)),
                    level_db: 20.0 * d.level.max(1e-9).log10(),
                    noise_db: 20.0 * d.noise.max(1e-9).log10(),
                }
            })
            .collect()
    }
}

/// A channel's live state (HFDL seed `06-behaviour.md` §2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelStatus {
    pub freq_khz: u32,
    /// A frame is being read (preamble found).
    pub decoding: bool,
    pub level_db: f32,
    pub noise_db: f32,
}

/// The channels of the ground-station table whose signal (assigned + 0 to
/// + 3 kHz) lies inside 90 % of the band `center_hz` ± `fs`/2.
pub fn channels_in_band(center_hz: f64, fs: f64) -> Vec<u32> {
    let half = 0.45 * fs;
    let mut v: Vec<u32> = super::stations::all_frequencies_khz()
        .into_iter()
        .filter(|&k| {
            let lo = k as f64 * 1000.0;
            lo >= center_hz - half && lo + 3000.0 <= center_hz + half
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone on a channel comes out as a steady tone however long the
    /// channelizer has run (its per-block rotation once lost all precision
    /// after a few minutes at 250 kHz).
    #[test]
    fn a_frame_from_both_demodulators_comes_out_once_the_better() {
        let mut r = Receiver::new(48_000.0, 8_942_000.0, &[8942, 8948]);
        let frame = |freq_khz, t, octets: Vec<u8>| Frame {
            freq_khz,
            m1: 0,
            octets,
            t,
            level_db: 0.0,
            noise_db: 0.0,
            freq_err_hz: 0.0,
        };
        let mut good = vec![0x04; 64];
        let c = super::super::acars::crc16_reflected(&good, 0xFFFF) ^ 0xFFFF;
        good.extend([c as u8, (c >> 8) as u8]);
        let mut bad = good.clone();
        bad[5] ^= 1;
        r.hold(frame(8942, 10.0, bad.clone()));
        r.hold(frame(8942, 10.01, good.clone()));
        r.hold(frame(8948, 10.0, bad.clone()));
        r.hold(frame(8942, 12.0, bad));
        assert!(r.release().is_empty(), "held for the other copy");
        r.fed += 48_000;
        let out = r.release();
        assert_eq!(out.len(), 3);
        assert!(out.windows(2).all(|w| w[0].t <= w[1].t), "time order");
        let first = out.iter().find(|f| f.freq_khz == 8942).unwrap();
        assert_eq!(first.octets, good, "the copy that checks");
        assert!(out.iter().any(|f| f.freq_khz == 8948));
        assert_eq!(out[2].t, 12.0);
        assert_eq!(r.channels(), [8942, 8948]);
        assert_eq!(r.status().len(), 2);
    }

    #[test]
    fn a_channel_stays_coherent_after_hours() {
        let fs = 250_000.0;
        let off = 75_940.0;
        let mut c = Channelizer::new(fs, &[off]);
        for start_block in [0u64, 40_000_000] {
            c.block = start_block;
            c.buf.clear();
            c.buf.resize(c.n - c.hop, Complex32::new(0.0, 0.0));
            // the tone 100 Hz above the channel center, phase continuous
            // with the block count
            let t0 = start_block as f64 * c.hop as f64;
            let x: Vec<Complex32> = (0..c.n * 8)
                .map(|i| {
                    let t = (t0 + i as f64) / fs;
                    let a = (2.0 * std::f64::consts::PI * (off + 100.0) * t)
                        .rem_euclid(2.0 * std::f64::consts::PI);
                    Complex32::from_polar(1.0, a as f32)
                })
                .collect();
            let mut out = vec![Vec::new()];
            c.process(&x, &mut out);
            let y = &out[0][out[0].len() / 2..];
            // the step between samples is 2 pi 100 / out_rate everywhere
            let want = 2.0 * std::f64::consts::PI * 100.0 / c.out_rate;
            for w in y.windows(2) {
                let d = (w[1] * w[0].conj()).arg() as f64;
                assert!(
                    (d - want).abs() < 0.05,
                    "block {start_block}: step {d}, want {want}"
                );
            }
        }
    }
}
