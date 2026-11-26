//! Numerically-controlled oscillator and frequency translation (behavioral).
//!
//! Step 3 seam 4. Mixing a complex input by a complex exponential shifts it in
//! frequency. Translating a tone at `+f` by `-f` lands it at DC (baseband).

use num_complex::Complex32;

/// A complex NCO that advances phase by a fixed increment each sample.
#[derive(Debug, Clone)]
pub struct Nco {
    phase: f32,
    phase_inc: f32,
}

impl Nco {
    /// Create an NCO that mixes by `freq_hz` at `sample_rate_hz`. Positive
    /// `freq_hz` shifts the spectrum up; use a negative value to bring a
    /// positive-offset channel down to baseband.
    pub fn new(freq_hz: f64, sample_rate_hz: f64) -> Self {
        let inc = (std::f64::consts::TAU * freq_hz / sample_rate_hz) as f32;
        Self {
            phase: 0.0,
            phase_inc: inc,
        }
    }

    /// Set the mixing frequency (e.g. when the VFO offset changes).
    pub fn set_freq(&mut self, freq_hz: f64, sample_rate_hz: f64) {
        self.phase_inc = (std::f64::consts::TAU * freq_hz / sample_rate_hz) as f32;
    }

    /// Mix `input` in place: `out[n] = in[n] * e^{j*phase}`.
    pub fn mix(&mut self, input: &mut [Complex32]) {
        for s in input.iter_mut() {
            let osc = Complex32::new(self.phase.cos(), self.phase.sin());
            *s *= osc;
            self.phase += self.phase_inc;
            // Keep phase bounded to avoid f32 precision loss over long runs.
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            } else if self.phase < -std::f32::consts::TAU {
                self.phase += std::f32::consts::TAU;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex32;
    /// Generate a complex tone of `freq_hz` at `sr` for `n` samples.
    fn tone(freq_hz: f64, sr: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = (std::f64::consts::TAU * freq_hz * i as f64 / sr) as f32;
                Complex32::new(p.cos(), p.sin())
            })
            .collect()
    }

    /// Mean magnitude of the DC component (average of the complex samples).
    fn dc_magnitude(x: &[Complex32]) -> f32 {
        let sum: Complex32 = x.iter().sum();
        (sum / x.len() as f32).norm()
    }

    #[test]
    fn mixing_a_tone_to_baseband_concentrates_energy_at_dc() {
        let sr = 1_000_000.0;
        let f = 100_000.0;
        let mut x = tone(f, sr, 4096);

        // Before mixing, the DC average of a pure off-DC tone is ~0.
        assert!(dc_magnitude(&x) < 0.05, "tone is not at DC initially");

        // Mix by -f to bring it to baseband.
        let mut nco = Nco::new(-f, sr);
        nco.mix(&mut x);

        // Now nearly all energy is at DC: average magnitude ~1.
        assert!(dc_magnitude(&x) > 0.95, "tone translated to DC");
    }
}
