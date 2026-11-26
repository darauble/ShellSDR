//! fldigi's filters as the MFSK receiver uses them (`filters/filters.cxx`,
//! `include/misc.h`): windowed-sinc FIR band-pass and Hilbert designs, the
//! sliding FFT and the moving average.
//!
//! Ported from fldigi 4.1.23 (GPL v3 or later; Dave Freese W1HKJ). The FIR
//! keeps fldigi's convention: the taps are applied in order to the last
//! `len` samples, oldest first, and the newest sample is not yet in the sum
//! (one sample of delay).

use num_complex::Complex64;
use std::f64::consts::PI;

pub fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-10 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

pub fn cosc(x: f64) -> f64 {
    if x.abs() < 1e-10 {
        0.0
    } else {
        (1.0 - (PI * x).cos()) / (PI * x)
    }
}

pub fn hamming(x: f64) -> f64 {
    0.54 - 0.46 * (2.0 * PI * x).cos()
}

/// fldigi's `decayavg`: a one-pole average with an integer weight.
pub fn decayavg(average: f64, input: f64, weight: i32) -> f64 {
    if weight <= 1 {
        return input;
    }
    (input - average) / f64::from(weight) + average
}

/// A band-pass FIR with 6 dB points `f1`, `f2` (fractions of the sample
/// rate), or its Hilbert (90 degree) counterpart (`bp_FIR`).
pub fn bp_fir(len: usize, hilbert: bool, f1: f64, f2: f64) -> Vec<f64> {
    (0..len)
        .map(|i| {
            let t = i as f64 - (len as f64 - 1.0) / 2.0;
            let h = i as f64 * (1.0 / (len as f64 - 1.0));
            if hilbert {
                -((2.0 * f2 * cosc(2.0 * f2 * t) - 2.0 * f1 * cosc(2.0 * f1 * t)) * hamming(h))
            } else {
                (2.0 * f2 * sinc(2.0 * f2 * t) - 2.0 * f1 * sinc(2.0 * f1 * t)) * hamming(h)
            }
        })
        .collect()
}

/// A FIR with complex taps on complex samples.
///
/// fldigi's Hilbert filter runs the I taps on the real part and the Q taps
/// on a copy of it, which makes the analytic signal of a real input. Here
/// the pair is one complex filter `h_i + j h_q` applied to the complex
/// input: for a real input that is the same analytic signal, and for IQ it
/// keeps the positive frequencies only. With equal real taps (a band-pass)
/// it filters I and Q alike, as fldigi's `C_FIR_filter::run`.
#[derive(Debug, Clone)]
pub struct Fir {
    taps: Vec<Complex64>,
    /// The last `len` samples twice over, so a window is one slice.
    buf: Vec<Complex64>,
    pos: usize,
}

impl Fir {
    pub fn new(taps: Vec<Complex64>) -> Self {
        let n = taps.len().max(1);
        Self {
            taps,
            buf: vec![Complex64::new(0.0, 0.0); 2 * n],
            pos: 0,
        }
    }

    /// fldigi `init_bandpass(len, 1, f1, f2)`.
    pub fn bandpass(len: usize, f1: f64, f2: f64) -> Self {
        Self::new(
            bp_fir(len, false, f1, f2)
                .into_iter()
                .map(|t| Complex64::new(t, 0.0))
                .collect(),
        )
    }

    /// fldigi `init_hilbert(len, 1)`: pass band 0.05..0.45 of the rate.
    pub fn hilbert(len: usize) -> Self {
        let i = bp_fir(len, false, 0.05, 0.45);
        let q = bp_fir(len, true, 0.05, 0.45);
        Self::new(
            i.into_iter()
                .zip(q)
                .map(|(i, q)| Complex64::new(i, q))
                .collect(),
        )
    }

    pub fn run(&mut self, x: Complex64) -> Complex64 {
        let n = self.taps.len();
        let window = &self.buf[self.pos..self.pos + n];
        let out = window
            .iter()
            .zip(&self.taps)
            .fold(Complex64::new(0.0, 0.0), |acc, (s, t)| acc + s * t);
        self.buf[self.pos] = x;
        self.buf[self.pos + n] = x;
        self.pos = (self.pos + 1) % n;
        out
    }
}

/// The sliding FFT (`sfft`): bins `first..last` of a `len`-point DFT,
/// updated with every sample.
#[derive(Debug, Clone)]
pub struct Sfft {
    len: usize,
    first: usize,
    vrot: Vec<Complex64>,
    bins: Vec<Complex64>,
    delay: Vec<Complex64>,
    k2: f64,
    ptr: usize,
}

const K1: f64 = 0.999_999_999_99;

impl Sfft {
    pub fn new(len: usize, first: usize, last: usize) -> Self {
        let tau = 2.0 * PI / len as f64;
        let vrot = (first..last)
            .map(|i| Complex64::from_polar(K1, tau * i as f64))
            .collect();
        Self {
            len,
            first,
            vrot,
            bins: vec![Complex64::new(0.0, 0.0); last - first],
            delay: vec![Complex64::new(0.0, 0.0); len],
            k2: K1.powi(len as i32),
            ptr: 0,
        }
    }

    pub fn reset(&mut self) {
        self.bins
            .iter_mut()
            .for_each(|b| *b = Complex64::new(0.0, 0.0));
        self.delay
            .iter_mut()
            .for_each(|b| *b = Complex64::new(0.0, 0.0));
    }

    /// One sample in; the bins out.
    pub fn run(&mut self, input: Complex64) -> &[Complex64] {
        let de = &mut self.delay[self.ptr];
        let z = input - *de * self.k2;
        *de = input;
        self.ptr = (self.ptr + 1) % self.len;
        for (b, r) in self.bins.iter_mut().zip(&self.vrot) {
            *b = (*b + z) * r;
        }
        &self.bins
    }

    /// The first bin's index.
    pub fn first(&self) -> usize {
        self.first
    }
}

/// fldigi's `Cmovavg`: a moving average whose first value fills it.
#[derive(Debug, Clone)]
pub struct MovAvg {
    buf: Vec<f64>,
    sum: f64,
    ptr: usize,
    empty: bool,
}

impl MovAvg {
    pub fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            sum: 0.0,
            ptr: 0,
            empty: true,
        }
    }

    pub fn reset(&mut self) {
        self.empty = true;
    }

    pub fn run(&mut self, a: f64) -> f64 {
        if self.empty {
            self.empty = false;
            self.buf.iter_mut().for_each(|x| *x = a);
            self.sum = a * self.buf.len() as f64;
            self.ptr = 0;
            return a;
        }
        self.sum += a - self.buf[self.ptr];
        self.buf[self.ptr] = a;
        self.ptr = (self.ptr + 1) % self.buf.len();
        self.sum / self.buf.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f64, sr: f64, n: usize) -> Vec<Complex64> {
        (0..n)
            .map(|i| Complex64::from_polar(1.0, 2.0 * PI * f * i as f64 / sr))
            .collect()
    }

    fn gain(fir: &mut Fir, x: &[Complex64]) -> f64 {
        let out: Vec<Complex64> = x.iter().map(|&s| fir.run(s)).collect();
        let tail = &out[out.len() / 2..];
        tail.iter().map(|z| z.norm()).sum::<f64>() / tail.len() as f64
    }

    #[test]
    fn the_hilbert_filter_keeps_positive_frequencies_of_iq() {
        let sr = 8000.0;
        let up = gain(&mut Fir::hilbert(37), &tone(1500.0, sr, 4000));
        let down = gain(&mut Fir::hilbert(37), &tone(-1500.0, sr, 4000));
        assert!((up - 2.0).abs() < 0.1, "{up}");
        assert!(down < 0.05, "{down}");
    }

    #[test]
    fn the_hilbert_filter_makes_a_real_tone_analytic() {
        let sr = 8000.0;
        let x: Vec<Complex64> = tone(1000.0, sr, 4000)
            .into_iter()
            .map(|z| Complex64::new(z.re, 0.0))
            .collect();
        let mut f = Fir::hilbert(37);
        let out: Vec<Complex64> = x.iter().map(|&s| f.run(s)).collect();
        // constant magnitude and a phase turning forward
        for w in out[200..].windows(2) {
            assert!((w[1].norm() - 1.0).abs() < 0.05);
            let d = (w[1] * w[0].conj()).arg();
            assert!((d - 2.0 * PI * 1000.0 / sr).abs() < 0.02, "{d}");
        }
    }

    #[test]
    fn the_bandpass_passes_its_band_only() {
        let sr = 8000.0;
        let pass = gain(&mut Fir::bandpass(127, 0.1, 0.2), &tone(1200.0, sr, 4000));
        let stop = gain(&mut Fir::bandpass(127, 0.1, 0.2), &tone(3000.0, sr, 4000));
        assert!((pass - 1.0).abs() < 0.05, "{pass}");
        assert!(stop < 0.01, "{stop}");
    }

    #[test]
    fn the_sliding_fft_finds_the_tone_bin() {
        let mut s = Sfft::new(256, 30, 50);
        let mut last = Vec::new();
        for z in tone(40.0 * 8000.0 / 256.0, 8000.0, 600) {
            last = s.run(z).to_vec();
        }
        let best = (0..last.len())
            .max_by(|&a, &b| last[a].norm().total_cmp(&last[b].norm()))
            .unwrap();
        assert_eq!(best + s.first(), 40);
        assert!((last[best].norm() - 256.0).abs() < 1.0);
    }

    #[test]
    fn the_moving_average_starts_full() {
        let mut m = MovAvg::new(4);
        assert_eq!(m.run(8.0), 8.0);
        assert_eq!(m.run(0.0), 6.0);
        assert_eq!(decayavg(0.0, 10.0, 10), 1.0);
    }
}
