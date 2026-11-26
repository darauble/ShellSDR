//! FM de-emphasis, ported from SDR++ `dsp::filter::Deemphasis` (behavioral).
//!
//! One-pole low-pass: `out[n] = alpha*in[n] + (1-alpha)*out[n-1]` with
//! `alpha = dt / (tau + dt)`, `dt = 1/samplerate`. SDR++ runs it in the AF
//! chain at the audio rate (48 kHz) with tau = 50 us by default for WFM
//! (`DEEMP_MODE_50US`; NFM defaults to none).

/// SDR++'s default WFM de-emphasis time constant (seconds).
pub const TAU_50US: f64 = 50e-6;

/// One-pole de-emphasis filter for real audio.
#[derive(Debug, Clone)]
pub struct Deemphasis {
    alpha: f32,
    last: f32,
}

impl Deemphasis {
    pub fn new(tau: f64, sample_rate_hz: f64) -> Self {
        let dt = 1.0 / sample_rate_hz;
        Self {
            alpha: (dt / (tau + dt)) as f32,
            last: 0.0,
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let y = self.alpha * *s + (1.0 - self.alpha) * self.last;
            *s = y;
            self.last = y;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_matches_the_sdrpp_formula() {
        // 50 us at 48 kHz: dt = 20.833 us, alpha = dt/(tau+dt) = 0.29412.
        let d = Deemphasis::new(TAU_50US, 48_000.0);
        assert!((d.alpha - 0.294_117_65).abs() < 1e-5, "alpha {}", d.alpha);
    }

    #[test]
    fn passes_dc_unchanged_after_settling() {
        let mut d = Deemphasis::new(TAU_50US, 48_000.0);
        let mut buf = vec![1.0f32; 2000];
        d.process(&mut buf);
        assert!((buf[1999] - 1.0).abs() < 1e-4, "DC gain is 1");
    }
}
