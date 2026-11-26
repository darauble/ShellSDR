//! DC blocker, ported from SDR++ `dsp::correction::DCBlocker` (behavioral).
//!
//! `offset += (in - offset) * rate; out = in - offset`, where `rate` is given
//! as `cutoff_rate / sample_rate`. SDR++'s AM demod constructs it with
//! `rate = 100.0 / IFSampleRate` (`decoder_modules/radio/src/demodulators/am.h`
//! passes `100.0 / getIFSampleRate()`), which is what removes the carrier DC
//! term and sub-audio rumble on the detected envelope.

/// Single-pole DC blocker for real samples.
#[derive(Debug, Clone)]
pub struct DcBlocker {
    rate: f32,
    offset: f32,
}

impl DcBlocker {
    /// Create with the normalized `rate` (= cutoff_rate / sample_rate), as
    /// SDR++'s `DCBlocker::init(in, rate)` expects.
    pub fn new(rate: f32) -> Self {
        Self { rate, offset: 0.0 }
    }

    /// SDR++'s AM DC-block rate: `100.0 / sample_rate`.
    pub fn am(sample_rate: f64) -> Self {
        Self::new((100.0 / sample_rate) as f32)
    }

    /// Filter `buf` in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            self.offset += (*s - self.offset) * self.rate;
            *s -= self.offset;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_a_constant_dc_offset() {
        let mut dc = DcBlocker::am(50_000.0);
        // Constant 1.0 plus a small 1 kHz tone. After settling, the mean
        // should be ~0 (DC removed) while the tone survives.
        let sr = 50_000.0;
        let mut buf: Vec<f32> = (0..50_000)
            .map(|i| 1.0 + 0.1 * (std::f64::consts::TAU * 1_000.0 * i as f64 / sr).sin() as f32)
            .collect();
        dc.process(&mut buf);
        let tail = &buf[40_000..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(mean.abs() < 0.02, "DC removed (residual mean {mean})");
        // The tone energy survives (peak-to-peak well above the residual DC).
        let (min, max) = tail
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        assert!(max - min > 0.1, "tone preserved (pp {})", max - min);
    }

    #[test]
    fn am_rate_matches_sdrpp() {
        // rate = 100 / sr.
        let dc = DcBlocker::am(15_000.0);
        assert!((dc.rate - (100.0 / 15_000.0)).abs() < 1e-9);
    }
}
