//! Audio AGC, ported from SDR++ `dsp::loop::AGC` (behavioral).
//!
//! Piece B2 fix. SDR++ applies this AGC on the AM/DSB/USB/LSB/CW audio paths
//! (not FM/WFM). It is a peak-tracking automatic gain control: an asymmetric
//! one-pole envelope follower with separate attack/decay coefficients, target
//! gain = setPoint/envelope clamped to maxGain, plus a look-ahead anti-clip
//! pass that snaps the envelope to the block peak when the output would exceed
//! maxOutputAmp. Reproduced from `core/src/dsp/loop/agc.h`.
//!
//! SDR++ fixed construction params for the audio AGC: setPoint = 1.0,
//! maxGain = 1e7, maxOutputAmp = 10.0, initGain = INFINITY. Attack/decay are
//! per-mode GUI values divided by the IF sample rate.

/// Peak-tracking AGC for real audio samples.
#[derive(Debug, Clone)]
pub struct Agc {
    set_point: f32,
    attack: f32,
    inv_attack: f32,
    decay: f32,
    inv_decay: f32,
    max_gain: f32,
    max_output_amp: f32,
    /// Running envelope estimate.
    amp: f32,
}

impl Agc {
    /// Construct with SDR++'s semantics. `init_gain` of `f32::INFINITY` makes
    /// the initial envelope 0 (so the first samples get full gain up to
    /// `max_gain`), matching SDR++.
    pub fn new(
        set_point: f32,
        attack: f32,
        decay: f32,
        max_gain: f32,
        max_output_amp: f32,
        init_gain: f32,
    ) -> Self {
        Self {
            set_point,
            attack,
            inv_attack: 1.0 - attack,
            decay,
            inv_decay: 1.0 - decay,
            max_gain,
            max_output_amp,
            amp: set_point / init_gain,
        }
    }

    /// Audio AGC with headroom. Uses SDR++'s algorithm but a lower set point
    /// (0.3) so the normalized output peaks around ~0.35 rather than ~1.0,
    /// leaving headroom so varying modulation depth never clips the output.
    /// `max_output_amp` is also tightened to the set point's neighborhood.
    pub fn audio(attack: f32, decay: f32) -> Self {
        Self::new(0.3, attack, decay, 1e7, 0.9, f32::INFINITY)
    }

    /// SDR++'s exact AGC construction, as used by `dsp::demod::SSB` and
    /// `dsp::demod::CW`: `AGC(setPoint 1.0, attack, decay, maxGain 10e6,
    /// maxOutputAmp 10.0, initGain INFINITY)`.
    pub fn sdrpp(attack: f32, decay: f32) -> Self {
        Self::new(1.0, attack, decay, 10e6, 10.0, f32::INFINITY)
    }

    /// The gain the AGC currently applies, dB (`setPoint / envelope`,
    /// limited to the maximum gain).
    pub fn gain_db(&self) -> f32 {
        let g = if self.amp > 0.0 {
            (self.set_point / self.amp).min(self.max_gain)
        } else {
            self.max_gain
        };
        20.0 * g.log10()
    }

    /// Process a block of real samples in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        let count = buf.len();
        for i in 0..count {
            let in_amp = buf[i].abs();
            let mut gain;
            if in_amp != 0.0 {
                self.amp = if in_amp > self.amp {
                    self.amp * self.inv_attack + in_amp * self.attack
                } else {
                    self.amp * self.inv_decay + in_amp * self.decay
                };
                gain = (self.set_point / self.amp).min(self.max_gain);
            } else {
                gain = 1.0;
            }

            // Look-ahead anti-clip: if this sample would clip, snap the
            // envelope to the remaining block's peak and recompute gain.
            if in_amp * gain > self.max_output_amp {
                let mut max_amp = 0.0f32;
                for &v in buf.iter().skip(i) {
                    let a = v.abs();
                    if a > max_amp {
                        max_amp = a;
                    }
                }
                self.amp = max_amp;
                gain = (self.set_point / self.amp).min(self.max_gain);
            }

            buf[i] *= gain;
        }
    }

    /// Process complex samples (carrier AGC): track the magnitude envelope and
    /// scale each complex sample by the gain. Used on the AM IF before the
    /// envelope detector to hold a constant carrier level. The anti-clip
    /// look-ahead uses the remaining block's peak magnitude.
    pub fn process_complex(&mut self, buf: &mut [num_complex::Complex32]) {
        let count = buf.len();
        for i in 0..count {
            let in_amp = buf[i].norm();
            let mut gain;
            if in_amp != 0.0 {
                self.amp = if in_amp > self.amp {
                    self.amp * self.inv_attack + in_amp * self.attack
                } else {
                    self.amp * self.inv_decay + in_amp * self.decay
                };
                gain = (self.set_point / self.amp).min(self.max_gain);
            } else {
                gain = 1.0;
            }
            if in_amp * gain > self.max_output_amp {
                let mut max_amp = 0.0f32;
                for v in buf.iter().skip(i) {
                    let a = v.norm();
                    if a > max_amp {
                        max_amp = a;
                    }
                }
                self.amp = max_amp;
                gain = (self.set_point / self.amp).min(self.max_gain);
            }
            buf[i] *= gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        if x.is_empty() {
            return 0.0;
        }
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn small_signal_is_amplified_toward_setpoint() {
        // A quiet 0.01-amplitude tone should be brought up toward ~1.0.
        let sr = 24_000.0;
        let attack = 50.0 / sr as f32;
        let decay = 5.0 / sr as f32;
        let mut agc = Agc::audio(attack, decay);
        let mut buf: Vec<f32> = (0..24_000)
            .map(|i| 0.01 * (std::f64::consts::TAU * 1_000.0 * i as f64 / sr).sin() as f32)
            .collect();
        agc.process(&mut buf);
        // After settling, the RMS should be far above the input's ~0.007 and
        // on the order of the setpoint (1.0), not silent.
        let settled = rms(&buf[12_000..]);
        assert!(settled > 0.2, "quiet signal amplified (rms {settled})");
    }

    #[test]
    fn loud_signal_is_attenuated_toward_setpoint() {
        let sr = 24_000.0;
        let mut agc = Agc::audio(50.0 / sr as f32, 5.0 / sr as f32);
        let mut buf: Vec<f32> = (0..24_000)
            .map(|i| 5.0 * (std::f64::consts::TAU * 1_000.0 * i as f64 / sr).sin() as f32)
            .collect();
        agc.process(&mut buf);
        let settled = rms(&buf[12_000..]);
        // Brought down near the setpoint region, not left at ~3.5 rms.
        assert!(settled < 2.0, "loud signal attenuated (rms {settled})");
    }

    #[test]
    fn output_never_wildly_exceeds_clip_ceiling() {
        let sr = 24_000.0;
        let mut agc = Agc::audio(50.0 / sr as f32, 5.0 / sr as f32);
        let mut buf: Vec<f32> = (0..8_000)
            .map(|i| 0.5 * (std::f64::consts::TAU * 800.0 * i as f64 / sr).sin() as f32)
            .collect();
        agc.process(&mut buf);
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= 10.5, "peak within the clip ceiling (got {peak})");
    }
}
