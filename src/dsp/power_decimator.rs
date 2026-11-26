//! Source decimation: a port of SDR++'s `dsp::multirate::PowerDecimator`.
//!
//! SDR++ can decimate the wideband IQ right after capture (Source menu
//! "Decimation": None, 2x .. 64x) so everything downstream (FFT, VFOs, the
//! baseband recorder) runs at `sample_rate / ratio`. It uses precomputed
//! multi-stage plans (`decim/plans.h`) of decimating FIRs with fixed, optimized
//! taps; we carry the same plans and the same taps (`decim_taps`), and the same
//! `DecimatingFIR` streaming rule (history of `taps - 1` samples, a decimation
//! offset carried across blocks so the output does not depend on block sizes).

use num_complex::Complex32;

use crate::dsp::decim_taps::{FIR_2_2, FIR_4_2, FIR_8_4, FIR_16_8, FIR_32_8, FIR_64_8};

/// The decimation ratios offered in the Source module (SDR++ `source.cpp`).
pub const RATIOS: [u32; 7] = [1, 2, 4, 8, 16, 32, 64];

/// Display label for a ratio, as SDR++ shows it ("None", "2x", ...).
pub fn ratio_label(ratio: u32) -> String {
    if ratio <= 1 {
        "None".to_string()
    } else {
        format!("{ratio}x")
    }
}

/// One stage of a plan: (decimation, taps). From SDR++ `decim/plans.h`.
type Stage = (usize, &'static [f32]);

/// The SDR++ plan for `ratio` (a power of two, 2..=64), or `None`.
fn plan(ratio: u32) -> Option<&'static [Stage]> {
    const P2: &[Stage] = &[(2, &FIR_2_2)];
    const P4: &[Stage] = &[(2, &FIR_4_2), (2, &FIR_2_2)];
    const P8: &[Stage] = &[(4, &FIR_8_4), (2, &FIR_2_2)];
    const P16: &[Stage] = &[(8, &FIR_16_8), (2, &FIR_2_2)];
    const P32: &[Stage] = &[(8, &FIR_32_8), (2, &FIR_4_2), (2, &FIR_2_2)];
    const P64: &[Stage] = &[(8, &FIR_64_8), (4, &FIR_8_4), (2, &FIR_2_2)];
    match ratio {
        2 => Some(P2),
        4 => Some(P4),
        8 => Some(P8),
        16 => Some(P16),
        32 => Some(P32),
        64 => Some(P64),
        _ => None,
    }
}

/// A decimating complex FIR with real taps (SDR++ `filter::DecimatingFIR`).
#[derive(Debug, Clone)]
struct DecimatingFir {
    decimation: usize,
    taps: &'static [f32],
    /// `taps - 1` history samples followed by the current block.
    buf: Vec<Complex32>,
    /// Index (within the next block) of the next sample to produce an output
    /// at; carried across blocks.
    offset: usize,
}

impl DecimatingFir {
    fn new(decimation: usize, taps: &'static [f32]) -> Self {
        Self {
            decimation,
            taps,
            buf: vec![Complex32::new(0.0, 0.0); taps.len() - 1],
            offset: 0,
        }
    }

    fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        out.clear();
        let hist = self.taps.len() - 1;
        self.buf.truncate(hist);
        self.buf.extend_from_slice(input);
        let count = input.len();
        while self.offset < count {
            let w = &self.buf[self.offset..self.offset + self.taps.len()];
            let mut acc = Complex32::new(0.0, 0.0);
            for (s, &t) in w.iter().zip(self.taps) {
                acc += s * t;
            }
            out.push(acc);
            self.offset += self.decimation;
        }
        self.offset -= count;
        // Keep the last `taps - 1` samples as history for the next block.
        self.buf.drain(..count);
    }
}

/// Power-of-two decimator for the wideband IQ (SDR++ `PowerDecimator`).
/// Ratio 1 is a pass-through.
#[derive(Debug, Clone)]
pub struct PowerDecimator {
    ratio: u32,
    stages: Vec<DecimatingFir>,
    scratch: Vec<Complex32>,
}

impl PowerDecimator {
    /// Build a decimator for `ratio`. Unsupported ratios fall back to 1.
    pub fn new(ratio: u32) -> Self {
        let (ratio, stages) = match plan(ratio) {
            Some(p) => (
                ratio,
                p.iter().map(|&(d, t)| DecimatingFir::new(d, t)).collect(),
            ),
            None => (1, Vec::new()),
        };
        Self {
            ratio,
            stages,
            scratch: Vec::new(),
        }
    }

    /// The effective ratio (1 = pass-through).
    pub fn ratio(&self) -> u32 {
        self.ratio
    }

    /// Decimate one block into `out` (cleared first).
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        out.clear();
        if self.stages.is_empty() {
            out.extend_from_slice(input);
            return;
        }
        // Ping-pong between `out` and `scratch` so the final stage lands in
        // `out` without per-block allocation.
        let n = self.stages.len();
        let mut tmp = std::mem::take(&mut self.scratch);
        for (i, st) in self.stages.iter_mut().enumerate() {
            // Stage i writes into `out` when (n - 1 - i) is even, else `tmp`.
            let to_out = (n - 1 - i).is_multiple_of(2);
            if i == 0 {
                if to_out {
                    st.process(input, out);
                } else {
                    st.process(input, &mut tmp);
                }
            } else if to_out {
                st.process(&tmp, out);
            } else {
                st.process(out, &mut tmp);
            }
        }
        self.scratch = tmp;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, sr: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = std::f64::consts::TAU * freq * i as f64 / sr;
                Complex32::new(p.cos() as f32, p.sin() as f32)
            })
            .collect()
    }

    fn rms(x: &[Complex32]) -> f32 {
        (x.iter().map(|c| c.norm_sqr()).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn run(d: &mut PowerDecimator, x: &[Complex32]) -> Vec<Complex32> {
        let mut out = Vec::new();
        d.process(x, &mut out);
        out
    }

    #[test]
    fn ratios_and_labels_match_sdrpp_source_menu() {
        assert_eq!(RATIOS, [1, 2, 4, 8, 16, 32, 64]);
        assert_eq!(ratio_label(1), "None");
        assert_eq!(ratio_label(8), "8x");
        assert_eq!(ratio_label(64), "64x");
    }

    #[test]
    fn plans_multiply_to_the_ratio_like_sdrpp_plans_h() {
        // From SDR++ decim/plans.h: plan_32 = {8, 2, 2}, plan_64 = {8, 4, 2}.
        let stages = |r| plan(r).unwrap().iter().map(|s| s.0).collect::<Vec<_>>();
        assert_eq!(stages(2), vec![2]);
        assert_eq!(stages(4), vec![2, 2]);
        assert_eq!(stages(8), vec![4, 2]);
        assert_eq!(stages(16), vec![8, 2]);
        assert_eq!(stages(32), vec![8, 2, 2]);
        assert_eq!(stages(64), vec![8, 4, 2]);
        assert_eq!(FIR_2_2.len(), 69, "fir_2_2_len in SDR++");
        assert_eq!(FIR_64_8.len(), 36, "fir_64_8_len in SDR++");
    }

    #[test]
    fn ratio_one_is_a_passthrough_and_bad_ratios_fall_back() {
        let x = tone(1_000.0, 48_000.0, 100);
        let mut d = PowerDecimator::new(1);
        assert_eq!(run(&mut d, &x), x);
        assert_eq!(PowerDecimator::new(3).ratio(), 1);
        assert_eq!(PowerDecimator::new(128).ratio(), 1, "UI offers up to 64x");
    }

    #[test]
    fn output_count_is_exactly_len_over_ratio() {
        for &r in &RATIOS[1..] {
            let mut d = PowerDecimator::new(r);
            let n = 64 * 1000;
            assert_eq!(
                run(&mut d, &vec![Complex32::new(0.0, 0.0); n]).len(),
                n / r as usize
            );
        }
    }

    #[test]
    fn in_band_passes_and_would_alias_is_rejected() {
        // 2 MHz / 8 = 250 kHz output. A tone at +50 kHz is in band (~unity
        // gain); a tone at +700 kHz would alias to -50 kHz and must be deep
        // down (> 60 dB).
        let sr = 2_000_000.0;
        for &(r, f_in, f_alias) in &[(8u32, 50_000.0, 700_000.0), (64, 5_000.0, 500_000.0)] {
            let mut a = PowerDecimator::new(r);
            let pass = run(&mut a, &tone(f_in, sr, 400_000));
            let mut b = PowerDecimator::new(r);
            let alias = run(&mut b, &tone(f_alias, sr, 400_000));
            let skip = 200; // filter settle
            let p = rms(&pass[skip..]);
            let s = rms(&alias[skip..]);
            assert!((p - 1.0).abs() < 0.05, "{r}x passband gain {p}");
            assert!(20.0 * (s / p).log10() < -60.0, "{r}x alias {s}");
        }
    }

    #[test]
    fn output_is_independent_of_block_boundaries() {
        let sr = 2_000_000.0;
        let x = tone(12_345.0, sr, 64 * 997);
        for &r in &[2u32, 8, 32, 64] {
            let mut whole = PowerDecimator::new(r);
            let want = run(&mut whole, &x);
            let mut blocked = PowerDecimator::new(r);
            let mut got = Vec::new();
            let sizes = [1usize, 7, 1000, 13, 4096, 3];
            let (mut p, mut i) = (0, 0);
            while p < x.len() {
                let n = sizes[i % sizes.len()].min(x.len() - p);
                got.extend(run(&mut blocked, &x[p..p + n]));
                p += n;
                i += 1;
            }
            assert_eq!(got.len(), want.len(), "{r}x count");
            let err = got
                .iter()
                .zip(&want)
                .map(|(a, b)| (a - b).norm())
                .fold(0.0f32, f32::max);
            assert!(err < 1e-5, "{r}x max err {err}");
        }
    }
}
