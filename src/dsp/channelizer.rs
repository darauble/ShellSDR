//! Filtered channelizer: anti-aliasing decimation of complex IQ, modeled on
//! SDR++'s `PowerDecimator` (cascaded decimate-by-2 FIR stages) + a final
//! polyphase `RationalResampler` for the non-power-of-two remainder.
//!
//! The old naive decimator dropped samples with no filter, so for narrow
//! channels the entire captured band aliased into the channel and the AM
//! envelope detector turned that into a DC offset and a constant beat tone.
//! Here each decimate-by-2 stage low-passes before dropping, so nothing
//! aliases; because each stage runs at half the previous rate the cascade is
//! cheap (Raspberry-Pi friendly), and the polyphase remainder runs at the low
//! intermediate rate.
//!
//! Faithful to SDR++'s structure (predecimate by 2^k, then rational resample),
//! using the same Nuttall windowed-sinc low-pass design.

use num_complex::Complex32;

use crate::dsp::fir::low_pass_taps;

/// A decimate-by-2 complex FIR. Low-passes to a quarter-band cutoff (so the
/// kept half-rate output is alias-free) and keeps every 2nd sample. Computes
/// the convolution only at output positions, so cost is proportional to the
/// output rate.
#[derive(Debug, Clone)]
struct Decim2 {
    taps: Vec<f32>,
    hist: Vec<Complex32>,
    /// Index (0 or 1) within the NEXT input block of the next sample to keep.
    /// Carried across blocks so odd-length blocks don't slip the decimation
    /// phase or emit an extra sample (the output must not depend on how the
    /// stream is split into blocks).
    phase: usize,
}

impl Decim2 {
    /// Build a decimate-by-2 stage for an input rate `in_rate`. Cutoff is set
    /// to 0.45 of the output Nyquist (output rate = in_rate/2) so the passband
    /// is preserved and the upper half (which would alias) is rejected.
    fn new(in_rate: f64) -> Self {
        let out_rate = in_rate / 2.0;
        let cutoff = out_rate * 0.45; // keep most of the retained band
        let trans = out_rate * 0.1; // transition width
        let taps = low_pass_taps(cutoff, trans, in_rate);
        let n = taps.len().saturating_sub(1);
        Self {
            taps,
            hist: vec![Complex32::new(0.0, 0.0); n],
            phase: 0,
        }
    }

    fn process(&mut self, input: &[Complex32]) -> Vec<Complex32> {
        let n = self.taps.len();
        let mut ext: Vec<Complex32> = Vec::with_capacity(self.hist.len() + input.len());
        ext.extend_from_slice(&self.hist);
        ext.extend_from_slice(input);
        let mut out = Vec::with_capacity(input.len() / 2 + 1);
        // Keep every 2nd input sample, starting at the carried phase; the
        // window for input index i is ext[i ..= i + n - 1] (ends at input[i]).
        let mut i = self.phase;
        while i < input.len() {
            let mut acc = Complex32::new(0.0, 0.0);
            for (j, &tap) in self.taps.iter().enumerate() {
                acc += ext[i + j] * tap;
            }
            out.push(acc);
            i += 2;
        }
        self.phase = i - input.len();
        if n > 1 {
            let keep = n - 1;
            let s = ext.len() - keep;
            self.hist.copy_from_slice(&ext[s..]);
        }
        out
    }
}

/// Anti-aliasing channelizer: cascade of decimate-by-2 stages to bring a
/// `source_rate` stream down near `target_rate`, leaving a residual integer
/// ratio handled by plain (filtered) decimation is avoided — instead we pick
/// the power-of-two that gets closest without going below the target, then the
/// caller's resampler corrects the exact rate.
#[derive(Debug, Clone)]
pub struct Channelizer {
    stages: Vec<Decim2>,
    /// Effective output rate after the decimate-by-2 cascade.
    out_rate: f64,
}

impl Channelizer {
    /// Build a cascade that decimates `source_rate` by the largest power of two
    /// that keeps the output rate >= `target_rate` (so the following rational
    /// resampler only needs a small ratio). Returns a channelizer plus the
    /// achieved intermediate rate.
    pub fn new(source_rate: f64, target_rate: f64) -> Self {
        let mut stages = Vec::new();
        let mut rate = source_rate;
        // Decimate by 2 while the next halving stays at or above the target.
        while rate / 2.0 >= target_rate {
            stages.push(Decim2::new(rate));
            rate /= 2.0;
        }
        Self {
            stages,
            out_rate: rate,
        }
    }

    /// The intermediate rate after the cascade (>= target).
    pub fn out_rate(&self) -> f64 {
        self.out_rate
    }

    /// Decimate a block of IQ through the cascade.
    pub fn process(&mut self, input: &[Complex32]) -> Vec<Complex32> {
        let mut buf = input.to_vec();
        for st in &mut self.stages {
            buf = st.process(&buf);
        }
        buf
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
    fn picks_power_of_two_above_target() {
        // 2 MHz -> target 15 kHz: 2e6 / 2^k >= 15000 -> k=7 (15625 Hz).
        let ch = Channelizer::new(2_000_000.0, 15_000.0);
        assert!(
            (ch.out_rate() - 15_625.0).abs() < 1.0,
            "out {}",
            ch.out_rate()
        );
    }

    #[test]
    fn passes_in_band_rejects_what_would_alias() {
        // Source 1 MHz down to ~16 kHz. An in-band low tone (2 kHz) passes; a
        // high tone near the source band (300 kHz) that naive decimation would
        // alias into the channel is deeply rejected.
        let sr = 1_024_000.0;
        let mut ch_in = Channelizer::new(sr, 16_000.0);
        let out_in = ch_in.process(&tone(2_000.0, sr, 1_024_000));
        let mut ch_al = Channelizer::new(sr, 16_000.0);
        let out_al = ch_al.process(&tone(300_000.0, sr, 1_024_000));
        let pass = mean_mag(&out_in[2000..]);
        let alias = mean_mag(&out_al[2000..]);
        assert!(pass > 0.8, "in-band passes ({pass})");
        assert!(alias < 0.05, "aliasing tone rejected ({alias})");
    }

    #[test]
    fn no_stages_when_already_low() {
        let ch = Channelizer::new(20_000.0, 15_000.0);
        assert!(ch.stages.is_empty());
        assert_eq!(ch.out_rate(), 20_000.0);
    }

    #[test]
    fn output_is_independent_of_block_boundaries() {
        // Real sources deliver blocks whose length is not a multiple of the
        // total decimation (e.g. SDR++'s 1250-sample blocks at 250 kHz, /8).
        // Splitting the stream must not change the result: same sample count
        // (exactly len/8) and the same samples as one big block.
        let sr = 250_000.0;
        let x = tone(1_000.0, sr, 125_000);
        let mut whole = Channelizer::new(sr, 24_000.0);
        let ref_out = whole.process(&x);
        let mut blocked = Channelizer::new(sr, 24_000.0);
        let mut got = Vec::new();
        for b in x.chunks(1250) {
            got.extend(blocked.process(b));
        }
        assert_eq!(ref_out.len(), 125_000 / 8, "whole-block count is len/8");
        assert_eq!(
            got.len(),
            ref_out.len(),
            "blocked count matches (no extra samples)"
        );
        let max_err = got
            .iter()
            .zip(&ref_out)
            .map(|(a, b)| (a - b).norm())
            .fold(0.0f32, f32::max);
        assert!(
            max_err < 1e-5,
            "blocked output identical (max err {max_err})"
        );
    }

    #[test]
    fn odd_and_uneven_blocks_keep_the_decimation_phase() {
        let sr = 64_000.0;
        let x = tone(500.0, sr, 10_007);
        let mut whole = Channelizer::new(sr, 8_000.0);
        let ref_out = whole.process(&x);
        let mut blocked = Channelizer::new(sr, 8_000.0);
        let mut got = Vec::new();
        let sizes = [1, 3, 7, 2, 999, 13, 1001];
        let mut p = 0;
        let mut i = 0;
        while p < x.len() {
            let n = sizes[i % sizes.len()].min(x.len() - p);
            got.extend(blocked.process(&x[p..p + n]));
            p += n;
            i += 1;
        }
        assert_eq!(got.len(), ref_out.len());
        let max_err = got
            .iter()
            .zip(&ref_out)
            .map(|(a, b)| (a - b).norm())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-5, "max err {max_err}");
    }
}
