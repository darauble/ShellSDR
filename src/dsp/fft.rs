//! Power-spectrum FFT analyzer (behavioral), modeled on SDR++'s IQ front-end.
//!
//! Step 4. Produces a DC-centered dB magnitude spectrum from wideband IQ at a
//! fixed frame rate that is decoupled from the sample rate (the "reshaper"
//! throttle). The numeric recipe mirrors SDR++:
//!   - Reshaper: `interval = round(sr/fps)`, `keep = min(interval, N)`,
//!     `skip = interval - keep`. Collect `keep` samples per frame (at ~fps),
//!     zero-pad to the FFT size `N`.
//!   - Window the kept samples with a Nuttall window, multiplied by an
//!     alternating +/-1 sequence, which shifts the FFT output by N/2 so the
//!     result is DC-centered (time-domain fftshift, no post-swap).
//!   - Forward complex FFT of size `N` (rustfft).
//!   - `dB[k] = 10*log10((re^2 + im^2) / N^2) = 20*log10(|X[k]|/N)`.
//!   - Optional one-pole IIR time averaging on the dB values:
//!     `out = a*new + (1-a)*prev`.
//!
//! The analyzer is a pure function of the IQ it is fed given its config, so it
//! is tested with synthetic tones (peak lands in the expected bin, DC at the
//! middle) without any hardware or threads.

use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// A finished spectrum frame: `len` dB values, DC-centered (bin `len/2` = DC,
/// bin 0 = most-negative frequency, bin `len-1` = most-positive).
pub type SpectrumFrame = Vec<f32>;

/// Fixed-rate power-spectrum analyzer.
pub struct SpectrumAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    size: usize,
    /// Nuttall window already multiplied by the alternating +/-1 fftshift sign,
    /// length `keep`.
    window: Vec<f32>,
    /// Non-zero (kept) sample count per frame; the rest of the FFT input is
    /// zero-padding.
    keep: usize,
    /// Samples to discard after each kept frame (the stride gap).
    skip: usize,
    /// Accumulator of kept samples for the current frame.
    acc: Vec<Complex32>,
    /// Remaining samples to skip before accumulating the next frame.
    skip_left: usize,
    /// One-pole IIR averaging coefficient in (0, 1]; 1.0 = no averaging.
    alpha: f32,
    /// Previous averaged frame (dB), for the IIR; empty until the first frame.
    prev: Vec<f32>,
    /// Scratch FFT buffer.
    scratch: Vec<Complex32>,
}

impl SpectrumAnalyzer {
    /// Create an analyzer of FFT `size` for `sample_rate_hz`, producing frames
    /// at ~`fps`. `avg_alpha` is the IIR coefficient in (0, 1]; use 1.0 for no
    /// time averaging.
    pub fn new(size: usize, sample_rate_hz: f64, fps: f64, avg_alpha: f32) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(size);
        let (keep, skip) = reshape_params(sample_rate_hz, size, fps);
        let window = nuttall_shift_window(keep);
        Self {
            fft,
            size,
            window,
            keep,
            skip,
            acc: Vec::with_capacity(keep),
            skip_left: 0,
            alpha: avg_alpha.clamp(1e-4, 1.0),
            prev: Vec::new(),
            scratch: vec![Complex32::new(0.0, 0.0); size],
        }
    }

    /// The FFT size (number of output bins).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Feed a block of wideband IQ. Returns any completed frames (usually zero
    /// or one per call, since the reshaper throttles to ~fps). Each frame is a
    /// DC-centered dB spectrum of length `size`.
    pub fn feed(&mut self, iq: &[Complex32]) -> Vec<SpectrumFrame> {
        let mut frames = Vec::new();
        let mut i = 0;
        while i < iq.len() {
            // Skip the stride gap between frames first.
            if self.skip_left > 0 {
                let take = self.skip_left.min(iq.len() - i);
                self.skip_left -= take;
                i += take;
                continue;
            }
            // Accumulate kept samples.
            let need = self.keep - self.acc.len();
            let take = need.min(iq.len() - i);
            self.acc.extend_from_slice(&iq[i..i + take]);
            i += take;
            if self.acc.len() == self.keep {
                frames.push(self.compute_frame());
                self.acc.clear();
                self.skip_left = self.skip;
            }
        }
        frames
    }

    /// Window, FFT, and dB-convert the accumulated frame, applying the IIR.
    fn compute_frame(&mut self) -> SpectrumFrame {
        // Window the kept samples into the (zero-padded) FFT input.
        for s in self.scratch.iter_mut() {
            *s = Complex32::new(0.0, 0.0);
        }
        for (dst, (src, w)) in self
            .scratch
            .iter_mut()
            .zip(self.acc.iter().zip(self.window.iter()))
        {
            *dst = *src * *w;
        }
        self.fft.process(&mut self.scratch);

        // dB = 10*log10(|X|^2 / N^2). Output already DC-centered thanks to the
        // alternating-sign window.
        let n2 = (self.size as f32) * (self.size as f32);
        let mut out: Vec<f32> = self
            .scratch
            .iter()
            .map(|c| {
                let p = (c.re * c.re + c.im * c.im) / n2;
                10.0 * (p.max(1e-20)).log10()
            })
            .collect();

        // One-pole IIR time averaging on the dB values.
        if self.alpha < 1.0 && self.prev.len() == out.len() {
            for (o, p) in out.iter_mut().zip(self.prev.iter()) {
                *o = self.alpha * *o + (1.0 - self.alpha) * *p;
            }
        }
        self.prev = out.clone();
        out
    }
}

/// Reshaper keep/skip for `sample_rate`, FFT `size`, and `fps`:
/// `interval = round(sr/fps)`, `keep = min(interval, size)`,
/// `skip = interval - keep`.
pub fn reshape_params(sample_rate_hz: f64, size: usize, fps: f64) -> (usize, usize) {
    let interval = (sample_rate_hz / fps).round() as usize;
    let keep = interval.min(size).max(1);
    let skip = interval.saturating_sub(keep);
    (keep, skip)
}

/// A Nuttall window of length `n` multiplied by the alternating +/-1 sequence
/// that performs the N/2 fftshift in the time domain (so the FFT output is
/// DC-centered). SDR++ Nuttall coefficients.
fn nuttall_shift_window(n: usize) -> Vec<f32> {
    const A: [f64; 4] = [0.355768, 0.487396, 0.144232, 0.012604];
    (0..n)
        .map(|i| {
            let x = std::f64::consts::TAU * i as f64 / n as f64;
            let w = A[0] - A[1] * x.cos() + A[2] * (2.0 * x).cos() - A[3] * (3.0 * x).cos();
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            (w * sign) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complex tone at `freq_hz` for `n` samples at `sr`.
    fn tone(freq_hz: f64, sr: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = (std::f64::consts::TAU * freq_hz * i as f64 / sr) as f32;
                Complex32::new(p.cos(), p.sin())
            })
            .collect()
    }

    #[test]
    fn reshape_params_match_sdrpp_formula() {
        // 2 MHz, 8192-pt FFT, 20 fps: interval = 100000, keep = 8192 (capped),
        // skip = 91808.
        let (keep, skip) = reshape_params(2_000_000.0, 8192, 20.0);
        assert_eq!(keep, 8192);
        assert_eq!(skip, 100_000 - 8192);
        // Low SR where interval < size: keep = interval, skip = 0.
        let (keep, skip) = reshape_params(48_000.0, 8192, 20.0);
        assert_eq!(keep, 2400);
        assert_eq!(skip, 0);
    }

    #[test]
    fn dc_tone_peaks_at_center_bin() {
        // A DC tone (0 Hz) must peak at bin size/2 (DC is centered).
        let size = 1024;
        let sr = 1_000_000.0;
        // fps chosen so keep == size (interval >= size): sr/fps >= size.
        let mut an = SpectrumAnalyzer::new(size, sr, 100.0, 1.0);
        let iq = vec![Complex32::new(1.0, 0.0); size * 4];
        let frames = an.feed(&iq);
        assert!(!frames.is_empty(), "produced at least one frame");
        let f = &frames[0];
        let peak = f
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak, size / 2, "DC peaks at the center bin");
    }

    #[test]
    fn positive_offset_tone_peaks_right_of_center() {
        // A tone at +sr/4 should peak a quarter of the span above center, i.e.
        // around bin size/2 + size/4.
        let size = 1024;
        let sr = 1_000_000.0;
        let f0 = sr / 4.0;
        let mut an = SpectrumAnalyzer::new(size, sr, 100.0, 1.0);
        let iq = tone(f0, sr, size * 4);
        let frames = an.feed(&iq);
        let f = frames.last().unwrap();
        let peak = f
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        let expected = size / 2 + size / 4;
        assert!(
            (peak as i32 - expected as i32).abs() <= 2,
            "peak near +sr/4 bin (got {peak}, expected ~{expected})"
        );
    }

    #[test]
    fn negative_offset_tone_peaks_left_of_center() {
        let size = 1024;
        let sr = 1_000_000.0;
        let f0 = -sr / 4.0;
        let mut an = SpectrumAnalyzer::new(size, sr, 100.0, 1.0);
        let iq = tone(f0, sr, size * 4);
        let frames = an.feed(&iq);
        let f = frames.last().unwrap();
        let peak = f
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        let expected = size / 2 - size / 4;
        assert!(
            (peak as i32 - expected as i32).abs() <= 2,
            "peak near -sr/4 bin (got {peak}, expected ~{expected})"
        );
    }

    #[test]
    fn throttle_limits_frames_per_block() {
        // With a big skip (2 MHz, 20 fps, 1024-pt), one 2M-sample second should
        // yield ~20 frames, not thousands.
        let size = 1024;
        let sr = 2_000_000.0;
        let mut an = SpectrumAnalyzer::new(size, sr, 20.0, 1.0);
        let iq = tone(250_000.0, sr, 2_000_000);
        let frames = an.feed(&iq);
        assert!(
            (18..=22).contains(&frames.len()),
            "~20 frames per second (got {})",
            frames.len()
        );
    }

    #[test]
    fn frames_are_finite_and_sized() {
        let size = 512;
        let mut an = SpectrumAnalyzer::new(size, 1_000_000.0, 100.0, 0.5);
        let iq = tone(100_000.0, 1_000_000.0, size * 8);
        for f in an.feed(&iq) {
            assert_eq!(f.len(), size);
            assert!(f.iter().all(|v| v.is_finite()));
        }
    }
}
