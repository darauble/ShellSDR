//! Per-VFO DSP chain (behavioral).
//!
//! Piece B, seam 2. Assembles the receive chain for one VFO:
//!   wideband IQ -> NCO translate (by -offset) -> integer decimate to the IF
//!   rate -> demodulate -> rational resample IF-rate audio to 48 kHz.
//!
//! `process` consumes a block of wideband IQ and returns however many 48 kHz
//! audio samples are ready (the resampler works in fixed input chunks, so
//! output is produced once enough demodulated audio has accumulated). The
//! chain is a pure function of its input given its configuration, so it is
//! tested end-to-end with synthetic signals without any hardware.

use num_complex::Complex32;

use crate::dsp::agc::Agc;
use crate::dsp::channelizer::Channelizer;
use crate::dsp::deemphasis::{Deemphasis, TAU_50US};
use crate::dsp::demod::{AmDemod, CwDemod, FmDemod, SsbDemod, SsbMode};
use crate::dsp::fir::{Fir, FirReal, low_pass_taps};
use crate::dsp::nco::Nco;
use crate::dsp::resampler::Resampler;
use crate::vfo::model::{AmAgcMode, Mode};

/// Output audio sample rate (Hz).
pub const AUDIO_RATE: f64 = 48_000.0;

/// The demodulator variants, dispatched per block.
enum Demod {
    Fm(FmDemod),
    Am(AmDemod),
    Ssb(SsbDemod),
    Cw(CwDemod),
}

/// One VFO's processing chain, mirroring SDR++'s per-VFO path:
/// `RxVFO` (translate to the channel center -> resample to IF -> channel LPF
/// bandwidth/2) -> mode demodulator (with its own AGC / audio filters) -> AF
/// chain (resample to 48 kHz -> optional de-emphasis).
pub struct VfoChain {
    nco: Nco,
    channelizer: Channelizer,
    if_rate: f64,
    /// Channel low-pass at the IF rate (cutoff = bandwidth/2), centered on the
    /// channel center (for USB/LSB that is carrier +/- bandwidth/2, so this
    /// filter is what selects the sideband, exactly as in SDR++'s RxVFO).
    lpf: Fir,
    demod: Demod,
    /// Post-demod audio AGC (SDR++: AM in Audio mode, DSB/USB/LSB, CW).
    agc: Option<Agc>,
    /// Post-demod low-pass at the IF rate. SDR++: AM (bw/2), NFM (bw/2, the
    /// default-on "Low Pass"), WFM (15 kHz / 4 kHz). None for SSB/DSB/CW.
    audio_lpf: Option<FirReal>,
    /// Polyphase rational resampler IF-rate -> 48 kHz (ported from SDR++).
    resampler: Resampler,
    /// AF-chain de-emphasis at 48 kHz (SDR++ default: 50 us for WFM only).
    deemphasis: Option<Deemphasis>,
    /// Channel center relative to the dial (USB +bw/2, LSB -bw/2, else 0).
    center_offset: f64,
    /// The last block's channel IQ at the IF rate, before the channel
    /// filter (for the zoomed channel view).
    last_chan: Vec<Complex32>,
    /// Channel power after the channel filter, dBFS (last block).
    level_db: f32,
}

/// SDR++'s default CW tone (Hz), `decoder_modules/radio/src/demodulators/cw.h`.
pub const DEFAULT_CW_TONE_HZ: f64 = 800.0;

/// Nominal CW IF rate for a pitch. SDR++ runs CW at 3 kHz and clamps the
/// pitch to 1250 Hz, the most that rate carries: the demod shifts the channel
/// up by the pitch at the IF rate, so `pitch + bw/2` must stay below Nyquist.
/// Up to SDR++'s 1250 Hz the SDR++ rate is kept (identical chain and CPU);
/// above it the rate grows to `2.2 * (pitch + bw/2)` (10% headroom for the
/// resampler's transition band). The channelizer then picks the next
/// power-of-two division at or above it. CPU note: the narrow CW channel FIR
/// costs ~rate^2, so a 3000 Hz pitch from 2 MHz (7812.5 Hz IF instead of
/// 3906.25 Hz) costs ~4x the default CW channel filter.
pub fn cw_if_rate(bandwidth_hz: f64, cw_tone_hz: f64) -> f64 {
    let sdrpp = Mode::Cw.if_sample_rate();
    if cw_tone_hz <= crate::vfo::model::CW_SDRPP_MAX_TONE_HZ {
        return sdrpp;
    }
    sdrpp.max(2.2 * (cw_tone_hz + bandwidth_hz.max(0.0) / 2.0))
}

impl VfoChain {
    /// Build a chain for `mode` at a given source sample rate, VFO offset
    /// (Hz, signed; positive = above center), channel `bandwidth_hz`, and AM
    /// AGC mode (ignored for non-AM modes). CW uses SDR++'s default 800 Hz
    /// tone; use [`VfoChain::new_with_tone`] to set it.
    pub fn new(
        source_rate_hz: f64,
        offset_hz: f64,
        mode: Mode,
        bandwidth_hz: f64,
        am_agc: AmAgcMode,
    ) -> Self {
        Self::new_with_tone(
            source_rate_hz,
            offset_hz,
            mode,
            bandwidth_hz,
            am_agc,
            DEFAULT_CW_TONE_HZ,
        )
    }

    /// As [`VfoChain::new`], with an explicit CW tone (Hz).
    ///
    /// `offset_hz` is the tuned (dial) frequency relative to the source
    /// center, i.e. the suppressed carrier for SSB. Like SDR++ (USB VFO
    /// reference = lower edge, LSB = upper edge) the channel is extracted
    /// centered at `offset + bw/2` for USB and `offset - bw/2` for LSB, so the
    /// symmetric channel filter passes exactly one sideband; the SSB demod then
    /// shifts it back by `+/- bw/2` and takes the real part.
    pub fn new_with_tone(
        source_rate_hz: f64,
        offset_hz: f64,
        mode: Mode,
        bandwidth_hz: f64,
        am_agc: AmAgcMode,
        cw_tone_hz: f64,
    ) -> Self {
        let nominal_if = match mode {
            Mode::Cw => cw_if_rate(bandwidth_hz, cw_tone_hz),
            _ => mode.if_sample_rate(),
        };
        // Anti-aliasing channelizer: cascade of filtered /2 stages down to an
        // intermediate rate >= the nominal IF rate. Its actual output rate is
        // the IF rate used for the rest of the chain.
        let channelizer = Channelizer::new(source_rate_hz, nominal_if);
        let if_rate = channelizer.out_rate();
        // Clamp below the IF rate; the floor keeps narrow CW (200 Hz) sane.
        let bw = bandwidth_hz.clamp(50.0, if_rate * 0.9);
        // Channel center: the passband center (SDR++ VFO reference).
        let channel_center = match mode {
            Mode::Usb => offset_hz + bw / 2.0,
            Mode::Lsb => offset_hz - bw / 2.0,
            _ => offset_hz,
        };
        let center_offset = channel_center - offset_hz;
        let nco = Nco::new(-channel_center, source_rate_hz);
        // SDR++ RxVFO channel filter: lowPass(bw/2, bw/2*0.1, IF).
        let lpf = Fir::low_pass(bw, if_rate);
        let demod = Self::make_demod(mode, if_rate, bw, am_agc, cw_tone_hz);
        let agc = Self::make_agc(mode, if_rate, am_agc);
        let audio_lpf = match mode {
            // AM: lowPass(bw/2, bw/2*0.1) after the AGC (dsp::demod::AM).
            // NFM: the default-on "Low Pass" with the same parameters
            // (dsp::demod::FM::updateFilter).
            Mode::Am | Mode::Nfm => Some(FirReal::low_pass(bw, if_rate)),
            // WFM mono: lowPass(15000, 4000) (dsp::demod::BroadcastFM).
            Mode::Wfm => Some(FirReal::new(low_pass_taps(15_000.0, 4_000.0, if_rate))),
            // SSB/DSB/CW: no post-demod filter in SDR++; audio spans 0..bw.
            Mode::Usb | Mode::Lsb | Mode::Dsb | Mode::Cw => None,
        };
        let resampler = Resampler::new(if_rate, AUDIO_RATE);
        let deemphasis = match mode {
            Mode::Wfm => Some(Deemphasis::new(TAU_50US, AUDIO_RATE)),
            _ => None,
        };
        Self {
            nco,
            channelizer,
            if_rate,
            lpf,
            demod,
            agc,
            audio_lpf,
            resampler,
            deemphasis,
            center_offset,
            last_chan: Vec::new(),
            level_db: -120.0,
        }
    }

    /// Channel center relative to the dial frequency (Hz).
    pub fn channel_center_offset(&self) -> f64 {
        self.center_offset
    }

    /// The last block's channel IQ (IF rate, before the channel filter).
    pub fn last_channel(&self) -> &[Complex32] {
        &self.last_chan
    }

    /// Channel power after the channel filter (dBFS, last block).
    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    /// The post-demod AGC's gain (dB), if the mode has one.
    pub fn agc_gain_db(&self) -> Option<f32> {
        self.agc.as_ref().map(Agc::gain_db)
    }

    fn make_demod(mode: Mode, if_rate: f64, bw: f64, am_agc: AmAgcMode, cw_tone_hz: f64) -> Demod {
        let sr = if_rate as f32;
        match mode {
            // SDR++ Quadrature deviation = bandwidth/2 for both NFM and WFM.
            Mode::Nfm | Mode::Wfm => Demod::Fm(FmDemod::new(if_rate, bw / 2.0)),
            Mode::Am => {
                if am_agc == AmAgcMode::Carrier {
                    // Carrier AGC uses the AM attack/decay (50/5) on the IF.
                    Demod::Am(AmDemod::with_carrier_agc(if_rate, 50.0 / sr, 5.0 / sr))
                } else {
                    Demod::Am(AmDemod::new(if_rate))
                }
            }
            Mode::Dsb => Demod::Ssb(SsbDemod::new(SsbMode::Dsb, bw, if_rate)),
            Mode::Usb => Demod::Ssb(SsbDemod::new(SsbMode::Usb, bw, if_rate)),
            Mode::Lsb => Demod::Ssb(SsbDemod::new(SsbMode::Lsb, bw, if_rate)),
            Mode::Cw => Demod::Cw(CwDemod::new(if_rate, cw_tone_hz)),
        }
    }

    /// Post-demod AGC per SDR++. SSB/DSB/CW use SDR++'s exact AGC
    /// (setPoint 1.0, maxOut 10); attack/decay are the SDR++ GUI defaults
    /// (SSB/DSB 50/5, CW 100/5) divided by the IF sample rate. AM keeps the
    /// user-confirmed headroom variant (`Agc::audio`, set point 0.3) and runs
    /// it only in Audio mode (Carrier mode levels the IF inside the demod).
    fn make_agc(mode: Mode, if_rate: f64, am_agc: AmAgcMode) -> Option<Agc> {
        let sr = if_rate as f32;
        match mode {
            Mode::Nfm | Mode::Wfm => None,
            Mode::Cw => Some(Agc::sdrpp(100.0 / sr, 5.0 / sr)),
            Mode::Am => match am_agc {
                AmAgcMode::Audio => Some(Agc::audio(50.0 / sr, 5.0 / sr)),
                AmAgcMode::Carrier | AmAgcMode::Off => None,
            },
            Mode::Dsb | Mode::Usb | Mode::Lsb => Some(Agc::sdrpp(50.0 / sr, 5.0 / sr)),
        }
    }

    /// The intermediate (IF) sample rate in Hz.
    pub fn if_rate(&self) -> f64 {
        self.if_rate
    }

    /// Process a block of wideband IQ, returning any ready 48 kHz audio.
    pub fn process(&mut self, wideband: &[Complex32]) -> Vec<f32> {
        // RxVFO: translate the channel center to DC, channelize to the IF
        // rate, then the channel low-pass (sideband-selecting for USB/LSB).
        let mut iq = wideband.to_vec();
        self.nco.mix(&mut iq);
        let raw = self.channelizer.process(&iq);
        let chan = self.lpf.process(&raw);
        self.last_chan = raw;
        if !chan.is_empty() {
            let p = chan.iter().map(|c| c.norm_sqr()).sum::<f32>() / chan.len() as f32;
            self.level_db = 10.0 * p.max(1e-12).log10();
        }

        // Demodulate to IF-rate audio.
        let mut audio = Vec::with_capacity(chan.len());
        match &mut self.demod {
            Demod::Fm(d) => d.process(&chan, &mut audio),
            Demod::Am(d) => d.process(&chan, &mut audio),
            Demod::Ssb(d) => d.process(&chan, &mut audio),
            Demod::Cw(d) => d.process(&chan, &mut audio),
        }

        // Demod-internal stages in SDR++ order: AGC, then the audio low-pass.
        if let Some(agc) = &mut self.agc {
            agc.process(&mut audio);
        }
        if let Some(lpf) = &mut self.audio_lpf {
            lpf.process(&mut audio);
        }

        // AF chain: resample IF -> 48 kHz, then de-emphasis (WFM).
        let mut out = self.resampler.process(&audio);
        if let Some(d) = &mut self.deemphasis {
            d.process(&mut out);
        }
        // No clamp here, like SDR++: the AGC may overshoot 1.0, and limiting
        // must happen AFTER the volume (see `engine::audio::output_sample`),
        // otherwise peaks are flattened at full scale and then scaled down,
        // producing a rectangular waveform at the volume level.
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex32;

    /// Correlate `x` with a reference cosine at `freq_hz`; magnitude indicates
    /// energy at that frequency.
    fn energy_at(x: &[f32], freq_hz: f64, sr: f64) -> f32 {
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for (i, &v) in x.iter().enumerate() {
            let p = (std::f64::consts::TAU * freq_hz * i as f64 / sr) as f32;
            re += v * p.cos();
            im += v * p.sin();
        }
        (re * re + im * im).sqrt() / x.len().max(1) as f32
    }

    /// Build a wideband signal: an FM-modulated carrier placed at `offset_hz`
    /// from center, modulated by `audio_hz` with `dev_hz` deviation.
    fn fm_at_offset(
        offset_hz: f64,
        audio_hz: f64,
        dev_hz: f64,
        sr: f64,
        n: usize,
    ) -> Vec<Complex32> {
        let mut mod_phase = 0.0f64;
        let mut car_phase = 0.0f64;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let m = (std::f64::consts::TAU * audio_hz * i as f64 / sr).sin();
            // Instantaneous carrier frequency = offset + dev * m.
            let inst = std::f64::consts::TAU * (offset_hz + dev_hz * m) / sr;
            car_phase += inst;
            let _ = &mut mod_phase;
            out.push(Complex32::new(
                car_phase.cos() as f32,
                car_phase.sin() as f32,
            ));
        }
        out
    }

    #[test]
    fn if_rate_is_a_power_of_two_division_at_or_above_nominal() {
        // The channelizer decimates by the largest 2^k keeping the rate at or
        // above the mode's nominal IF. For NFM (nominal 50 kHz) from 2 MHz:
        // 2e6 / 2^5 = 62500 Hz (>= 50000), not a further /2 to 31250.
        let c = VfoChain::new(2_000_000.0, 0.0, Mode::Nfm, 12_500.0, AmAgcMode::Audio);
        assert!(
            c.if_rate() >= 50_000.0,
            "IF >= nominal (got {})",
            c.if_rate()
        );
        assert!(
            c.if_rate() < 100_000.0,
            "IF within one octave (got {})",
            c.if_rate()
        );
    }

    #[test]
    fn nfm_chain_recovers_offset_tone_at_48k() {
        // Narrow FM at +200 kHz offset, modulated by 1 kHz, 5 kHz deviation.
        let sr = 1_000_000.0;
        let offset = 200_000.0;
        let audio = 1_000.0;
        let n = 1_000_000; // 1 s of IQ so the resampler produces plenty.
        let iq = fm_at_offset(offset, audio, 5_000.0, sr, n);

        let mut chain = VfoChain::new(sr, offset, Mode::Nfm, 12_500.0, AmAgcMode::Audio);
        let out = chain.process(&iq);

        assert!(!out.is_empty(), "produced 48 kHz audio");
        let at_tone = energy_at(&out, audio, AUDIO_RATE);
        let at_other = energy_at(&out, 6_000.0, AUDIO_RATE);
        assert!(
            at_tone > 0.02,
            "recovered 1 kHz tone has energy ({at_tone})"
        );
        assert!(
            at_tone > at_other * 4.0,
            "tone dominates an unrelated frequency ({at_tone} vs {at_other})"
        );
    }

    #[test]
    fn output_is_finite_and_bounded() {
        let sr = 1_000_000.0;
        let iq = fm_at_offset(100_000.0, 1_000.0, 5_000.0, sr, 200_000);
        let mut chain = VfoChain::new(sr, 100_000.0, Mode::Nfm, 12_500.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    /// Build a small-amplitude AM signal at `offset_hz`: carrier amplitude
    /// `base*(1 + depth*cos(2*pi*audio*t))`, like a real weak SDR signal.
    fn am_at_offset(
        offset_hz: f64,
        audio_hz: f64,
        base: f32,
        depth: f32,
        sr: f64,
        n: usize,
    ) -> Vec<Complex32> {
        let mut car_phase = 0.0f64;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let a = base
                * (1.0 + depth * (std::f64::consts::TAU * audio_hz * i as f64 / sr).cos() as f32);
            car_phase += std::f64::consts::TAU * offset_hz / sr;
            out.push(Complex32::new(
                a * car_phase.cos() as f32,
                a * car_phase.sin() as f32,
            ));
        }
        out
    }

    #[test]
    fn am_chain_is_audible_on_small_amplitude_signal() {
        // A weak AM signal (carrier amplitude 0.02) must become audible audio
        // thanks to the AGC — this is the bug that silenced AM on real IQ.
        let sr = 1_000_000.0;
        let offset = 150_000.0;
        let audio = 1_000.0;
        let iq = am_at_offset(offset, audio, 0.02, 0.5, sr, 1_000_000);

        let mut chain = VfoChain::new(sr, offset, Mode::Am, 10_000.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        assert!(!out.is_empty());

        // Output RMS should be well above the tiny input, i.e. not silent.
        let rms = (out.iter().map(|v| v * v).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms > 0.05, "AM audio is audible after AGC (rms {rms})");
        // And the modulating tone dominates.
        let at_tone = energy_at(&out, audio, AUDIO_RATE);
        let at_other = energy_at(&out, 7_000.0, AUDIO_RATE);
        assert!(
            at_tone > at_other * 3.0,
            "AM tone dominates ({at_tone} vs {at_other})"
        );
    }

    #[test]
    fn channel_lpf_rejects_a_strong_adjacent_signal() {
        // Weak AM in-channel at +150 kHz plus a STRONG off-channel tone at
        // +400 kHz. The channel low-pass must keep the AM audible and not let
        // the strong neighbor swamp it.
        let sr = 1_000_000.0;
        let offset = 150_000.0;
        let audio = 1_000.0;
        let mut iq = am_at_offset(offset, audio, 0.02, 0.5, sr, 1_000_000);
        // Add a strong adjacent carrier 250 kHz away from the VFO.
        let mut ph = 0.0f64;
        for s in iq.iter_mut() {
            ph += std::f64::consts::TAU * 400_000.0 / sr;
            *s += Complex32::new(0.5 * ph.cos() as f32, 0.5 * ph.sin() as f32);
        }

        let mut chain = VfoChain::new(sr, offset, Mode::Am, 10_000.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        let at_tone = energy_at(&out, audio, AUDIO_RATE);
        let at_other = energy_at(&out, 9_000.0, AUDIO_RATE);
        assert!(
            at_tone > at_other * 3.0,
            "AM tone survives a strong adjacent signal ({at_tone} vs {at_other})"
        );
    }

    #[test]
    fn am_carrier_agc_holds_level_through_a_fade() {
        // A strong AM carrier whose amplitude slowly fades by 10x across the
        // block. Carrier AGC should hold the recovered-tone level much steadier
        // than Audio AGC, which pumps with the carrier.
        let sr = 1_000_000.0;
        let offset = 120_000.0;
        let audio = 1_000.0;
        let n = 1_000_000;
        // Build AM with a linearly fading carrier envelope.
        let mut car_phase = 0.0f64;
        let mut iq = Vec::with_capacity(n);
        for i in 0..n {
            let fade = 1.0 - 0.9 * (i as f32 / n as f32); // 1.0 -> 0.1
            let a = 0.3
                * fade
                * (1.0 + 0.5 * (std::f64::consts::TAU * audio * i as f64 / sr).cos() as f32);
            car_phase += std::f64::consts::TAU * offset / sr;
            iq.push(Complex32::new(
                a * car_phase.cos() as f32,
                a * car_phase.sin() as f32,
            ));
        }

        let tone_level = |mode: AmAgcMode| {
            let mut ch = VfoChain::new(sr, offset, Mode::Am, 10_000.0, mode);
            let out = ch.process(&iq);
            // Compare tone energy in the first vs last quarter of the audio.
            let q = out.len() / 4;
            let early = energy_at(&out[..q], audio, AUDIO_RATE);
            let late = energy_at(&out[out.len() - q..], audio, AUDIO_RATE);
            (early, late)
        };

        let (c_early, c_late) = tone_level(AmAgcMode::Carrier);
        // Carrier AGC: the late (faded) level should remain within a modest
        // ratio of the early level, i.e. it holds up despite the 10x fade.
        assert!(c_early > 0.0 && c_late > 0.0);
        let carrier_ratio = (c_early / c_late).max(c_late / c_early);
        assert!(
            carrier_ratio < 3.0,
            "carrier AGC holds level through the fade (ratio {carrier_ratio})"
        );
    }

    /// Sum of complex tones `(freq_hz, amplitude)` for `n` samples at `sr`.
    fn tones(parts: &[(f64, f32)], sr: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let mut s = Complex32::new(0.0, 0.0);
                for &(f, a) in parts {
                    let p = (std::f64::consts::TAU * f * i as f64 / sr) as f32;
                    s += Complex32::new(a * p.cos(), a * p.sin());
                }
                s
            })
            .collect()
    }

    #[test]
    fn usb_chain_output_is_not_hard_clipped_before_volume() {
        // SDR++ does not clamp the demod output; the AGC may overshoot 1.0 and
        // the volume is applied before any limiting (in the audio sink). A
        // loud multitone must therefore NOT pile up on exactly +/-1.0.
        let sr = 1_000_000.0;
        let carrier = 100_000.0;
        let parts: Vec<(f64, f32)> = [300.0, 700.0, 1_200.0, 1_800.0, 2_400.0]
            .iter()
            .map(|f| (carrier + f, 0.2))
            .collect();
        let iq = tones(&parts, sr, 1_000_000);
        let mut chain = VfoChain::new(sr, carrier, Mode::Usb, 2_800.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        let tail = &out[out.len() / 2..];
        let at_rail = tail.iter().filter(|v| v.abs() == 1.0).count();
        assert!(
            (at_rail as f64) < tail.len() as f64 * 0.001,
            "{at_rail} of {} samples flattened at exactly full scale",
            tail.len()
        );
    }

    #[test]
    fn chain_output_is_independent_of_block_size() {
        // The live engine and SDR++ deliver IQ in blocks (SDR++: sr/200 =
        // 1250 at 250 kHz) that are not multiples of the total decimation.
        // NFM has no AGC (whose look-ahead is block-scoped by design), so its
        // output must be identical whether processed whole or in blocks, with
        // exactly len * 48000 / sr samples overall.
        let sr = 250_000.0;
        let iq = fm_at_offset(20_000.0, 1_000.0, 3_000.0, sr, 250_000);
        let mut whole = VfoChain::new(sr, 20_000.0, Mode::Nfm, 12_500.0, AmAgcMode::Audio);
        let a = whole.process(&iq);
        let mut blocked = VfoChain::new(sr, 20_000.0, Mode::Nfm, 12_500.0, AmAgcMode::Audio);
        let mut b = Vec::new();
        for blk in iq.chunks(1250) {
            b.extend(blocked.process(blk));
        }
        let expected = 250_000.0 * AUDIO_RATE / sr;
        assert!(
            (b.len() as f64 - expected).abs() <= 2.0,
            "blocked output {} samples, expected ~{expected}",
            b.len()
        );
        let n = a.len().min(b.len());
        let max_err = a[..n]
            .iter()
            .zip(&b[..n])
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-4, "blocked == whole (max err {max_err})");
    }

    #[test]
    fn usb_chain_keeps_the_upper_sideband_and_rejects_the_lower() {
        // Suppressed carrier at +100 kHz. Upper-sideband tone 1000 Hz above it,
        // lower-sideband tone 1500 Hz below it. USB must output the 1000 Hz
        // tone and reject the 1500 Hz image (SDR++: channel centered at
        // carrier + bw/2, shift back, Re()).
        let sr = 1_000_000.0;
        let carrier = 100_000.0;
        let iq = tones(
            &[(carrier + 1_000.0, 0.3), (carrier - 1_500.0, 0.3)],
            sr,
            1_000_000,
        );
        let mut chain = VfoChain::new(sr, carrier, Mode::Usb, 2_800.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        assert!(!out.is_empty());
        let wanted = energy_at(&out, 1_000.0, AUDIO_RATE);
        let image = energy_at(&out, 1_500.0, AUDIO_RATE);
        assert!(
            wanted > image * 10.0,
            "USB keeps carrier+1000 and rejects carrier-1500 ({wanted} vs {image})"
        );
    }

    #[test]
    fn lsb_chain_keeps_the_lower_sideband_and_rejects_the_upper() {
        let sr = 1_000_000.0;
        let carrier = 100_000.0;
        let iq = tones(
            &[(carrier - 1_000.0, 0.3), (carrier + 1_500.0, 0.3)],
            sr,
            1_000_000,
        );
        let mut chain = VfoChain::new(sr, carrier, Mode::Lsb, 2_800.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        let wanted = energy_at(&out, 1_000.0, AUDIO_RATE);
        let image = energy_at(&out, 1_500.0, AUDIO_RATE);
        assert!(
            wanted > image * 10.0,
            "LSB keeps carrier-1000 and rejects carrier+1500 ({wanted} vs {image})"
        );
    }

    #[test]
    fn usb_audio_extends_to_the_full_bandwidth_not_half() {
        // SDR++ has no audio low-pass on SSB: audio runs 0..bandwidth. A tone
        // 2500 Hz above the carrier (inside a 2800 Hz channel) must survive at
        // a level comparable to a 1000 Hz tone, not be cut at bw/2 = 1400 Hz.
        let sr = 1_000_000.0;
        let carrier = 100_000.0;
        let iq = tones(
            &[(carrier + 1_000.0, 0.3), (carrier + 2_500.0, 0.3)],
            sr,
            1_000_000,
        );
        let mut chain = VfoChain::new(sr, carrier, Mode::Usb, 2_800.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        let low = energy_at(&out, 1_000.0, AUDIO_RATE);
        let high = energy_at(&out, 2_500.0, AUDIO_RATE);
        assert!(
            high > low * 0.5,
            "2500 Hz audio is passed ({high} vs 1000 Hz {low})"
        );
    }

    #[test]
    fn cw_chain_puts_an_on_frequency_carrier_at_the_configured_pitch() {
        // Unmodulated carrier exactly on the VFO frequency becomes a tone at
        // the configured CW pitch (SDR++: shift complex IQ by +tone, Re()).
        let sr = 1_000_000.0;
        let carrier = 200_000.0;
        let iq = tones(&[(carrier, 0.3)], sr, 1_000_000);
        let mut chain =
            VfoChain::new_with_tone(sr, carrier, Mode::Cw, 200.0, AmAgcMode::Audio, 600.0);
        let out = chain.process(&iq);
        let at_pitch = energy_at(&out, 600.0, AUDIO_RATE);
        let elsewhere = energy_at(&out, 1_000.0, AUDIO_RATE);
        assert!(at_pitch > 0.05, "CW pitch present ({at_pitch})");
        assert!(at_pitch > elsewhere * 10.0, "and dominant ({elsewhere})");
    }

    #[test]
    fn cw_chain_reproduces_a_3000_hz_pitch_without_aliasing() {
        // From a 2 MHz source the nominal 3 kHz CW IF is 3906.25 Hz (Nyquist
        // 1953 Hz), so a 3000 Hz pitch shifted at that rate would alias to
        // 3906.25 - 3000 = 906.25 Hz. The chain must raise the IF rate for
        // high pitches. Narrowest (100 Hz) and widest (500 Hz) CW filters.
        let sr = 2_000_000.0;
        let carrier = 200_000.0;
        let iq = tones(&[(carrier, 0.3)], sr, 2_000_000);
        for bw in [100.0, 500.0] {
            let mut chain =
                VfoChain::new_with_tone(sr, carrier, Mode::Cw, bw, AmAgcMode::Audio, 3_000.0);
            let out = chain.process(&iq);
            let tail = &out[out.len() / 2..];
            let at_pitch = energy_at(tail, 3_000.0, AUDIO_RATE);
            let alias = energy_at(tail, 906.25, AUDIO_RATE);
            assert!(
                at_pitch > 0.05,
                "bw {bw}: 3000 Hz pitch present ({at_pitch})"
            );
            assert!(at_pitch > alias * 10.0, "bw {bw}: no alias ({alias})");
        }
    }

    #[test]
    fn cw_if_rate_is_unchanged_for_sdrpp_range_pitches() {
        // Pitches inside SDR++'s 250..1250 Hz keep the SDR++ 3 kHz IF
        // (3906.25 Hz from 2 MHz), so the default chain and its CPU cost do
        // not change.
        for tone in [250.0, 800.0, 1_250.0] {
            let c =
                VfoChain::new_with_tone(2_000_000.0, 0.0, Mode::Cw, 500.0, AmAgcMode::Audio, tone);
            assert_eq!(c.if_rate(), 3_906.25, "tone {tone}");
        }
    }

    #[test]
    fn nfm_deviation_tracks_the_channel_bandwidth() {
        // SDR++ sets the FM deviation to bandwidth/2. With a 12.5 kHz channel
        // the discriminator is scaled to 6.25 kHz, so a 5 kHz-deviation signal
        // recovers at 5000 / 6250 = 0.8 of full scale (not 1.0 as with the old
        // hardcoded 5 kHz).
        let sr = 1_000_000.0;
        let offset = 200_000.0;
        let iq = fm_at_offset(offset, 1_000.0, 5_000.0, sr, 1_000_000);
        let mut chain = VfoChain::new(sr, offset, Mode::Nfm, 12_500.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        let tail = &out[out.len() / 2..];
        let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            (peak - 0.8).abs() < 0.08,
            "peak {peak} should be ~0.8 (= 5000 / 6250)"
        );
    }

    #[test]
    fn wfm_applies_50us_deemphasis() {
        // SDR++ WFM defaults to 50 us de-emphasis (one-pole, alpha =
        // dt/(tau+dt) at 48 kHz). Equal-deviation tones at 1 kHz and 10 kHz
        // must come out with a ratio near the analog response
        // |H| = 1/sqrt(1+(2*pi*f*tau)^2): 0.303 / 0.953 = 0.318.
        let sr = 1_000_000.0;
        let offset = 100_000.0;
        let level = |audio_hz: f64| {
            let iq = fm_at_offset(offset, audio_hz, 40_000.0, sr, 1_000_000);
            let mut chain = VfoChain::new(sr, offset, Mode::Wfm, 150_000.0, AmAgcMode::Audio);
            let out = chain.process(&iq);
            energy_at(&out[out.len() / 4..], audio_hz, AUDIO_RATE)
        };
        let ratio = level(10_000.0) / level(1_000.0);
        // Independent reference: the analog 50 us RC response.
        let h = |f: f64| 1.0 / (1.0 + (std::f64::consts::TAU * f * 50e-6).powi(2)).sqrt();
        let expected = (h(10_000.0) / h(1_000.0)) as f32;
        assert!(
            (ratio - expected).abs() < 0.07,
            "10 kHz / 1 kHz ratio {ratio} should be ~{expected} with 50 us de-emphasis"
        );
    }

    #[test]
    fn am_output_is_clean_bounded_and_hf_rejected() {
        // A strong AM signal plus wideband noise. The output must: never clip
        // (|s| <= 1), sit at a sane level (not slammed to full scale), and
        // reject HF well above the audio band (SDR++-grade clarity).
        let sr = 1_000_000.0;
        let offset = 100_000.0;
        let audio = 1_000.0;
        let n = 1_000_000;
        // AM carrier (amplitude 0.3) + pseudo-random wideband noise.
        let mut car = 0.0f64;
        let mut seed = 12345u64;
        let mut iq = Vec::with_capacity(n);
        for i in 0..n {
            let a =
                0.3 * (1.0 + 0.6 * (std::f64::consts::TAU * audio * i as f64 / sr).cos() as f32);
            car += std::f64::consts::TAU * offset / sr;
            // cheap LCG noise in [-0.1, 0.1]
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let nr = ((seed >> 40) as f32 / 16_777_216.0 - 0.5) * 0.2;
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let ni = ((seed >> 40) as f32 / 16_777_216.0 - 0.5) * 0.2;
            iq.push(Complex32::new(
                a * car.cos() as f32 + nr,
                a * car.sin() as f32 + ni,
            ));
        }

        let mut chain = VfoChain::new(sr, offset, Mode::Am, 10_000.0, AmAgcMode::Audio);
        let out = chain.process(&iq);
        assert!(!out.is_empty());

        // Never exceeds full scale.
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= 1.0, "no clipping (peak {peak})");

        // HF rejection: energy well above the 5 kHz audio edge (e.g. 12 kHz)
        // must be far below the in-band tone. (Audio rate 48 kHz.)
        let at_tone = energy_at(&out[2000..], audio, AUDIO_RATE);
        let at_hf = energy_at(&out[2000..], 12_000.0, AUDIO_RATE);
        assert!(at_tone > 0.0);
        assert!(
            at_hf < at_tone * 0.1,
            "HF deeply rejected vs in-band ({at_hf} vs {at_tone})"
        );
    }
}
