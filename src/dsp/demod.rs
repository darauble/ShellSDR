//! Demodulators (behavioral).
//!
//! Step 3 seam 3. Each demodulator consumes baseband complex IQ (already
//! frequency-translated and decimated to the mode's IF rate) and produces real
//! audio samples. FM uses a quadrature discriminator; AM an envelope detector
//! with DC removal; SSB a product detector keeping one sideband; CW an SSB
//! product detector offset by the beat-note tone so a carrier becomes an
//! audible tone.

use num_complex::Complex32;

/// FM quadrature discriminator. Output is the instantaneous frequency
/// (phase difference between consecutive samples), scaled so that a deviation
/// of `max_dev_hz` maps to +/-1.0.
#[derive(Debug, Clone)]
pub struct FmDemod {
    last: Complex32,
    gain: f32,
}

impl FmDemod {
    pub fn new(sample_rate_hz: f64, max_dev_hz: f64) -> Self {
        let gain = (sample_rate_hz / (std::f64::consts::TAU * max_dev_hz)) as f32;
        Self {
            last: Complex32::new(1.0, 0.0),
            gain,
        }
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len());
        for &s in input {
            let diff = s * self.last.conj();
            out.push(diff.arg() * self.gain);
            self.last = s;
        }
    }
}

/// AM envelope detector with an SDR++-ported DC blocker and an optional
/// carrier AGC on the complex IF (SDR++'s Carrier AGC mode), which holds a
/// steady level on a strong carrier instead of pumping the audio.
#[derive(Debug, Clone)]
pub struct AmDemod {
    dc: crate::dsp::dc_blocker::DcBlocker,
    carrier_agc: Option<crate::dsp::agc::Agc>,
}

impl AmDemod {
    /// AM demod without carrier AGC, at the given IF sample rate.
    pub fn new(if_rate: f64) -> Self {
        Self {
            dc: crate::dsp::dc_blocker::DcBlocker::am(if_rate),
            carrier_agc: None,
        }
    }

    /// AM demod with a carrier AGC (levels the complex IF before detection).
    pub fn with_carrier_agc(if_rate: f64, attack: f32, decay: f32) -> Self {
        Self {
            dc: crate::dsp::dc_blocker::DcBlocker::am(if_rate),
            carrier_agc: Some(crate::dsp::agc::Agc::audio(attack, decay)),
        }
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len());
        // Optional carrier AGC on a copy of the complex IF.
        let mut iq;
        let src: &[Complex32] = if let Some(agc) = &mut self.carrier_agc {
            iq = input.to_vec();
            agc.process_complex(&mut iq);
            &iq
        } else {
            input
        };
        // Envelope detect.
        for &s in src {
            out.push(s.norm());
        }
        // DC block (removes the carrier term and sub-audio rumble).
        self.dc.process(out);
    }
}

/// Which SSB variant: determines the frequency-translation offset applied
/// before taking the real part (SDR++ `dsp::demod::SSB::getTranslation`:
/// USB = `+bandwidth/2`, LSB = `-bandwidth/2`, DSB = `0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsbMode {
    Usb,
    Lsb,
    Dsb,
}

/// SSB/DSB demod, ported from SDR++ `dsp::demod::SSB`: a `FrequencyXlator`
/// shifts the channel by `+bandwidth/2` (USB), `-bandwidth/2` (LSB), or `0`
/// (DSB) so the desired sideband straddles DC, then the real part is taken.
/// This is NOT a phasing-method (Hilbert) detector; sideband rejection comes
/// entirely from the channel low-pass filter applied upstream (in
/// `VfoChain`) once the channel has been shifted onto that filter's passband.
#[derive(Debug, Clone)]
pub struct SsbDemod {
    nco: crate::dsp::nco::Nco,
}

impl SsbDemod {
    /// `mode` selects the translation; `bandwidth_hz` and `sample_rate_hz` are
    /// the channel bandwidth and the IF sample rate the translation is
    /// computed at (must match the rate `process` is called with).
    pub fn new(mode: SsbMode, bandwidth_hz: f64, sample_rate_hz: f64) -> Self {
        let offset = match mode {
            SsbMode::Usb => bandwidth_hz / 2.0,
            SsbMode::Lsb => -bandwidth_hz / 2.0,
            SsbMode::Dsb => 0.0,
        };
        Self {
            nco: crate::dsp::nco::Nco::new(offset, sample_rate_hz),
        }
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        let mut shifted = input.to_vec();
        self.nco.mix(&mut shifted);
        out.clear();
        out.reserve(shifted.len());
        out.extend(shifted.iter().map(|s| s.re));
    }
}

/// CW demod, ported from SDR++ `dsp::demod::CW`: translates the complex IQ by
/// `+tone_hz` (so an on-frequency carrier becomes a tone at `tone_hz`), takes
/// the real part, then applies the audio AGC. The translation is on the
/// COMPLEX signal (a true frequency shift), not a post-detection multiply by
/// a real oscillator, which would create extra sum/difference tones.
#[derive(Debug, Clone)]
pub struct CwDemod {
    nco: crate::dsp::nco::Nco,
}

impl CwDemod {
    pub fn new(sample_rate_hz: f64, tone_hz: f64) -> Self {
        Self {
            nco: crate::dsp::nco::Nco::new(tone_hz, sample_rate_hz),
        }
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        let mut shifted = input.to_vec();
        self.nco.mix(&mut shifted);
        out.clear();
        out.reserve(shifted.len());
        out.extend(shifted.iter().map(|s| s.re));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex32;

    /// RMS of a signal, a simple energy measure.
    fn rms(x: &[f32]) -> f32 {
        if x.is_empty() {
            return 0.0;
        }
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Correlate `x` with a reference cosine at `freq_hz`; magnitude indicates
    /// how much energy sits at that frequency. Normalized to ~amplitude/2.
    fn goertzel_like(x: &[f32], freq_hz: f64, sr: f64) -> f32 {
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for (i, &v) in x.iter().enumerate() {
            let p = (std::f64::consts::TAU * freq_hz * i as f64 / sr) as f32;
            re += v * p.cos();
            im += v * p.sin();
        }
        (re * re + im * im).sqrt() / x.len() as f32
    }

    /// Build an FM signal: carrier at baseband, frequency-modulated by a tone
    /// of `audio_hz` with peak deviation `dev_hz`.
    fn fm_signal(audio_hz: f64, dev_hz: f64, sr: f64, n: usize) -> Vec<Complex32> {
        let mut phase = 0.0f64;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let m = (std::f64::consts::TAU * audio_hz * i as f64 / sr).sin();
            let inst = std::f64::consts::TAU * dev_hz * m / sr;
            phase += inst;
            out.push(Complex32::new(phase.cos() as f32, phase.sin() as f32));
        }
        out
    }

    #[test]
    fn fm_recovers_modulating_tone() {
        let sr = 50_000.0;
        let audio = 1_000.0;
        let dev = 5_000.0;
        let sig = fm_signal(audio, dev, sr, 8192);

        let mut fm = FmDemod::new(sr, dev);
        let mut out = Vec::new();
        fm.process(&sig, &mut out);

        // Energy should concentrate at the 1 kHz modulating tone, not at an
        // unrelated frequency.
        let at_tone = goertzel_like(&out, audio, sr);
        let at_other = goertzel_like(&out, 7_000.0, sr);
        assert!(at_tone > 0.05, "recovered tone has energy ({at_tone})");
        assert!(
            at_tone > at_other * 5.0,
            "energy at tone dominates ({at_tone} vs {at_other})"
        );
    }

    #[test]
    fn am_recovers_modulating_tone() {
        let sr = 50_000.0;
        let audio = 1_000.0;
        let n = 8192;
        // AM: carrier amplitude varies as (1 + 0.5*cos(2*pi*audio*t)).
        let sig: Vec<Complex32> = (0..n)
            .map(|i| {
                let a = 1.0 + 0.5 * (std::f64::consts::TAU * audio * i as f64 / sr).cos();
                Complex32::new(a as f32, 0.0)
            })
            .collect();

        let mut am = AmDemod::new(sr);
        let mut out = Vec::new();
        am.process(&sig, &mut out);

        let at_tone = goertzel_like(&out, audio, sr);
        let at_other = goertzel_like(&out, 5_000.0, sr);
        assert!(at_tone > 0.05, "AM recovered tone has energy ({at_tone})");
        assert!(at_tone > at_other * 5.0, "AM energy at tone dominates");
    }

    /// A complex tone at `freq_hz` (can be negative) for `n` samples.
    fn ctone(freq_hz: f64, sr: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = (std::f64::consts::TAU * freq_hz * i as f64 / sr) as f32;
                Complex32::new(p.cos(), p.sin())
            })
            .collect()
    }

    #[test]
    fn usb_shifts_the_channel_up_by_half_bandwidth_then_takes_real() {
        // SDR++ SSB: the channel is centered at carrier + bw/2 for USB, the
        // demod shifts by +bw/2 and takes Re(). With bw = 2800 the carrier sits
        // at -1400 Hz in the channel, so a tone 1000 Hz above the carrier is at
        // -400 Hz and must come out as a 1000 Hz audio tone.
        let sr = 24_000.0;
        let sig = ctone(-400.0, sr, 24_000);
        let mut d = SsbDemod::new(SsbMode::Usb, 2_800.0, sr);
        let mut out = Vec::new();
        d.process(&sig, &mut out);
        let at_tone = goertzel_like(&out, 1_000.0, sr);
        let at_wrong = goertzel_like(&out, 400.0, sr);
        assert!(at_tone > 0.4, "USB audio at carrier+1000 ({at_tone})");
        assert!(at_tone > at_wrong * 10.0, "and not at 400 Hz ({at_wrong})");
    }

    #[test]
    fn lsb_shifts_the_channel_down_by_half_bandwidth_then_takes_real() {
        // Mirror: the channel is centered at carrier - bw/2, the carrier sits
        // at +1400 Hz, a tone 1000 Hz below it is at +400 Hz, shift by -1400
        // gives -1000 Hz, and Re() yields a 1000 Hz audio tone.
        let sr = 24_000.0;
        let sig = ctone(400.0, sr, 24_000);
        let mut d = SsbDemod::new(SsbMode::Lsb, 2_800.0, sr);
        let mut out = Vec::new();
        d.process(&sig, &mut out);
        let at_tone = goertzel_like(&out, 1_000.0, sr);
        let at_wrong = goertzel_like(&out, 400.0, sr);
        assert!(at_tone > 0.4, "LSB audio at carrier-1000 ({at_tone})");
        assert!(at_tone > at_wrong * 10.0, "and not at 400 Hz ({at_wrong})");
    }

    #[test]
    fn dsb_has_no_shift_and_takes_real() {
        // DSB: translation 0, so a tone at 700 Hz is simply Re() = 700 Hz.
        let sr = 24_000.0;
        let sig = ctone(700.0, sr, 24_000);
        let mut d = SsbDemod::new(SsbMode::Dsb, 4_600.0, sr);
        let mut out = Vec::new();
        d.process(&sig, &mut out);
        let at_tone = goertzel_like(&out, 700.0, sr);
        let at_other = goertzel_like(&out, 2_100.0, sr);
        assert!(at_tone > 0.4, "DSB audio at 700 Hz ({at_tone})");
        assert!(at_tone > at_other * 10.0);
    }

    #[test]
    fn cw_turns_carrier_into_beat_tone() {
        // An unmodulated carrier at DC should become a tone at the CW pitch.
        let sr = 3_000.0;
        let tone = 800.0;
        let n = 6000;
        let carrier = vec![Complex32::new(1.0, 0.0); n];

        let mut cw = CwDemod::new(sr, tone);
        let mut out = Vec::new();
        cw.process(&carrier, &mut out);

        let at_tone = goertzel_like(&out, tone, sr);
        let at_other = goertzel_like(&out, 300.0, sr);
        assert!(at_tone > 0.4, "CW produces a beat tone ({at_tone})");
        assert!(
            at_tone > at_other * 10.0,
            "CW energy at the pitch dominates"
        );
    }

    #[test]
    fn cw_shifts_the_complex_signal_so_an_offset_carrier_moves_the_pitch() {
        // SDR++ CW translates the COMPLEX IQ by +tone then takes Re(). A
        // carrier 100 Hz above the tuned frequency therefore sounds at
        // tone + 100 Hz (no extra mixing products like a real-audio multiply
        // by cos() would create).
        let sr = 3_000.0;
        let sig = ctone(100.0, sr, 6000);
        let mut cw = CwDemod::new(sr, 800.0);
        let mut out = Vec::new();
        cw.process(&sig, &mut out);
        let at_900 = goertzel_like(&out, 900.0, sr);
        let at_800 = goertzel_like(&out, 800.0, sr);
        let at_700 = goertzel_like(&out, 700.0, sr);
        assert!(at_900 > 0.4, "pitch moved to tone+100 ({at_900})");
        assert!(at_900 > at_800 * 10.0 && at_900 > at_700 * 10.0);
    }
}
