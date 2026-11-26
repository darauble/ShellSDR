//! Windowed-sinc low-pass FIR, ported from SDR++ (behavioral).
//!
//! Piece B2 fix. SDR++'s channelizer anti-alias filter is a Nuttall-windowed
//! sinc low-pass (`core/src/dsp/taps/{low_pass,windowed_sinc,estimate_tap_count}.h`
//! and `window/nuttall.h`). We reproduce its exact math so a decimated VFO
//! channel rejects out-of-band energy instead of aliasing it in (which was
//! silencing the narrow AM/SSB/CW modes).
//!
//! Tap design (SDR++):
//!   N        = floor(3.8 * sampleRate / transitionWidth)
//!   omega    = 2*pi*cutoff/sampleRate
//!   corr     = omega / pi            (= 2*cutoff/sampleRate, DC-gain norm)
//!   t_i      = i - N/2 + 0.5
//!   tap_i    = sinc(t_i*omega) * nuttall(t_i - N/2, N) * corr
//! where the Nuttall window is the 4-term cosine sum with alternating signs.

use num_complex::Complex32;

/// Nuttall 4-term window evaluated as SDR++ does: `cosine(n, N, coefs)` with
/// alternating signs. Note SDR++ passes the window argument as `t - half`.
fn nuttall(n: f64, big_n: f64) -> f64 {
    const C: [f64; 4] = [0.355768, 0.487396, 0.144232, 0.012604];
    let mut win = 0.0;
    let mut sign = 1.0;
    for (i, c) in C.iter().enumerate() {
        win += sign * c * (i as f64 * 2.0 * std::f64::consts::PI * n / big_n).cos();
        sign = -sign;
    }
    win
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 { 1.0 } else { x.sin() / x }
}

/// SDR++ tap-count estimate: `3.8 * sampleRate / transitionWidth`.
pub fn estimate_tap_count(trans_width: f64, sample_rate: f64) -> usize {
    ((3.8 * sample_rate / trans_width) as usize).max(1)
}

/// Design low-pass taps exactly as SDR++'s `taps::lowPass` +
/// `windowedSinc` + Nuttall window.
pub fn low_pass_taps(cutoff: f64, trans_width: f64, sample_rate: f64) -> Vec<f32> {
    let count = estimate_tap_count(trans_width, sample_rate);
    let omega = 2.0 * std::f64::consts::PI * cutoff / sample_rate;
    let corr = omega / std::f64::consts::PI;
    let half = count as f64 / 2.0;
    let mut taps = Vec::with_capacity(count);
    for i in 0..count {
        let t = i as f64 - half + 0.5;
        let w = nuttall(t - half, count as f64);
        taps.push((sinc(t * omega) * w * corr) as f32);
    }
    taps
}

/// A complex FIR filter (real taps) with internal history, so successive
/// `process` calls form a continuous convolution across block boundaries.
#[derive(Debug, Clone)]
pub struct Fir {
    taps: Vec<f32>,
    /// History of the last `taps.len()-1` input samples (most recent last).
    hist: Vec<Complex32>,
}

impl Fir {
    pub fn new(taps: Vec<f32>) -> Self {
        let n = taps.len().saturating_sub(1);
        Self {
            taps,
            hist: vec![Complex32::new(0.0, 0.0); n],
        }
    }

    /// Low-pass FIR with SDR++'s parameters for a channel of `bandwidth` Hz at
    /// `sample_rate`: cutoff = bandwidth/2, transition = cutoff*0.1.
    pub fn low_pass(bandwidth: f64, sample_rate: f64) -> Self {
        let cutoff = bandwidth / 2.0;
        let taps = low_pass_taps(cutoff, cutoff * 0.1, sample_rate);
        Self::new(taps)
    }

    pub fn tap_count(&self) -> usize {
        self.taps.len()
    }

    /// Filter `input` in place-style into a new vector of the same length.
    pub fn process(&mut self, input: &[Complex32]) -> Vec<Complex32> {
        let n = self.taps.len();
        let mut out = Vec::with_capacity(input.len());
        // Work over [history ++ input]; for output sample k, convolve the taps
        // over the window ending at input[k].
        let mut buf: Vec<Complex32> = Vec::with_capacity(self.hist.len() + input.len());
        buf.extend_from_slice(&self.hist);
        buf.extend_from_slice(input);
        let hist_len = self.hist.len();
        for k in 0..input.len() {
            // Window spans buf[k ..= k + n - 1] (length n), newest at the end.
            let mut acc = Complex32::new(0.0, 0.0);
            for (j, &tap) in self.taps.iter().enumerate() {
                acc += buf[k + j] * tap;
            }
            out.push(acc);
            let _ = hist_len;
        }
        // Update history to the last n-1 samples of buf.
        if n > 1 {
            let start = buf.len() - (n - 1);
            self.hist.copy_from_slice(&buf[start..]);
        }
        out
    }
}

/// A real-input FIR filter (real taps) with internal history, for audio-rate
/// low-pass filtering of demodulated audio (SDR++'s output-rate channel FIR).
#[derive(Debug, Clone)]
pub struct FirReal {
    taps: Vec<f32>,
    hist: Vec<f32>,
}

impl FirReal {
    pub fn new(taps: Vec<f32>) -> Self {
        let n = taps.len().saturating_sub(1);
        Self {
            taps,
            hist: vec![0.0; n],
        }
    }

    /// Low-pass FIR (SDR++ params): cutoff = bandwidth/2, transition = 10%.
    pub fn low_pass(bandwidth: f64, sample_rate: f64) -> Self {
        let cutoff = bandwidth / 2.0;
        let taps = low_pass_taps(cutoff, cutoff * 0.1, sample_rate);
        Self::new(taps)
    }

    pub fn tap_count(&self) -> usize {
        self.taps.len()
    }

    /// Filter `buf` in place (continuous across calls via history).
    pub fn process(&mut self, buf: &mut [f32]) {
        let n = self.taps.len();
        if n <= 1 {
            return;
        }
        let mut ext: Vec<f32> = Vec::with_capacity(self.hist.len() + buf.len());
        ext.extend_from_slice(&self.hist);
        ext.extend_from_slice(buf);
        for (k, out) in buf.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (j, &tap) in self.taps.iter().enumerate() {
                acc += ext[k + j] * tap;
            }
            *out = acc;
        }
        let start = ext.len() - (n - 1);
        self.hist.copy_from_slice(&ext[start..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex32;

    fn tone(freq: f64, sr: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = (std::f64::consts::TAU * freq * i as f64 / sr) as f32;
                Complex32::new(p.cos(), p.sin())
            })
            .collect()
    }

    fn mean_mag(x: &[Complex32]) -> f32 {
        if x.is_empty() {
            return 0.0;
        }
        x.iter().map(|c| c.norm()).sum::<f32>() / x.len() as f32
    }

    #[test]
    fn tap_count_matches_sdrpp_formula() {
        // 3.8 * sr / transWidth.
        assert_eq!(estimate_tap_count(5_000.0, 1_000_000.0), 760);
    }

    #[test]
    fn dc_gain_is_approximately_unity() {
        // A DC input (0 Hz tone) should pass at ~unit magnitude.
        let taps = low_pass_taps(5_000.0, 500.0, 50_000.0);
        let sum: f32 = taps.iter().sum();
        assert!((sum - 1.0).abs() < 0.05, "DC gain ~1 (got {sum})");
    }

    #[test]
    fn passes_in_band_rejects_out_of_band() {
        // Channel ~12.5 kHz wide (cutoff 6.25 kHz) at 50 kHz.
        let sr = 50_000.0;
        let mut fir = Fir::low_pass(12_500.0, sr);
        // Warm up history, then measure steady-state.
        let in_band = tone(1_000.0, sr, 8192);
        let out_band = tone(20_000.0, sr, 8192);
        let yi = fir.process(&in_band);
        let mut fir2 = Fir::low_pass(12_500.0, sr);
        let yo = fir2.process(&out_band);
        // Use the second half to avoid the startup transient.
        let in_pass = mean_mag(&yi[4096..]);
        let out_pass = mean_mag(&yo[4096..]);
        assert!(in_pass > 0.8, "in-band passes (got {in_pass})");
        assert!(out_pass < 0.1, "out-of-band rejected (got {out_pass})");
    }

    #[test]
    fn history_makes_block_processing_continuous() {
        // Processing in two halves equals processing the whole at the overlap.
        let sr = 50_000.0;
        let sig = tone(1_000.0, sr, 4096);
        let mut a = Fir::low_pass(12_500.0, sr);
        let whole = a.process(&sig);
        let mut b = Fir::low_pass(12_500.0, sr);
        let mut split = b.process(&sig[..2048]);
        split.extend(b.process(&sig[2048..]));
        // The split result should equal the whole (same filter state path).
        for (x, y) in whole.iter().zip(split.iter()) {
            assert!((x - y).norm() < 1e-4);
        }
    }

    /// Real-tone generator for FirReal tests.
    fn real_tone(freq: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / sr).sin() as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn real_fir_passes_voice_rejects_hf_hiss() {
        // AM-style audio low-pass: bandwidth 10 kHz -> cutoff 5 kHz at 48 kHz.
        // A 1 kHz voice tone passes; an 8 kHz "hiss" tone is attenuated.
        let sr = 48_000.0;
        let mut f1 = FirReal::low_pass(10_000.0, sr);
        let mut voice = real_tone(1_000.0, sr, 16384);
        f1.process(&mut voice);
        let mut f2 = FirReal::low_pass(10_000.0, sr);
        let mut hiss = real_tone(8_000.0, sr, 16384);
        f2.process(&mut hiss);
        let pass = rms(&voice[8192..]);
        let rej = rms(&hiss[8192..]);
        assert!(pass > 0.5, "voice passes ({pass})");
        assert!(rej < 0.15, "HF hiss attenuated ({rej})");
    }
}
