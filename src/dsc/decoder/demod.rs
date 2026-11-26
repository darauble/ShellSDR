//! The demodulator: audio samples to soft bits (seed
//! spec/04-algorithms/01-demodulator.md).
//!
//! ShellSDR (spec 14.2): the core takes complex samples, so a VFO feeds it
//! its channel IQ directly (mark at +85 Hz, space at -85 Hz around the DSC
//! frequency), with no audio in between. The audio path feeds `(x, x)` into
//! the same core.

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::f64::consts::PI;
use std::sync::Arc;

type C = Complex<f64>;

const N: usize = 512;

/// The lowest IQ rate the demodulator works at (Hz, spec 14.2).
pub const MIN_IQ_RATE: f64 = 8000.0;
const H: usize = N / 2;

/// The raised-cosine low-pass filter, run as FFT overlap-add (step 1).
struct FftFilter {
    response: Arc<Vec<C>>,
    fft: Arc<dyn Fft<f64>>,
    ifft: Arc<dyn Fft<f64>>,
    input: Vec<C>,
    filled: usize,
    overlap: Vec<C>,
    block: Vec<C>,
    scratch: Vec<C>,
}

impl FftFilter {
    fn new(response: Arc<Vec<C>>, fft: Arc<dyn Fft<f64>>, ifft: Arc<dyn Fft<f64>>) -> FftFilter {
        let scratch = vec![
            C::default();
            fft.get_inplace_scratch_len()
                .max(ifft.get_inplace_scratch_len())
        ];
        FftFilter {
            response,
            fft,
            ifft,
            input: vec![C::default(); H],
            filled: 0,
            overlap: vec![C::default(); H],
            block: vec![C::default(); N],
            scratch,
        }
    }

    /// Takes one sample; after every H samples returns H outputs.
    fn run(&mut self, z: C) -> Option<&[C]> {
        self.input[self.filled] = z;
        self.filled += 1;
        if self.filled < H {
            return None;
        }
        self.filled = 0;
        self.block[..H].copy_from_slice(&self.input);
        self.block[H..].fill(C::default());
        self.fft
            .process_with_scratch(&mut self.block, &mut self.scratch);
        for (b, f) in self.block.iter_mut().zip(self.response.iter()) {
            *b *= f;
        }
        self.ifft
            .process_with_scratch(&mut self.block, &mut self.scratch);
        let scale = 1.0 / N as f64;
        for i in 0..H {
            let out = self.block[i] * scale + self.overlap[i];
            self.overlap[i] = self.block[H + i] * scale;
            self.block[i] = out;
        }
        Some(&self.block[..H])
    }
}

/// The frequency response of the filter for this sample rate.
fn response(sample_rate: f64) -> Vec<C> {
    let fc = 1.4 * 100.0 / sample_rate;
    let mut f = vec![C::default(); N];
    for i in 0..H {
        let x = i as f64 / H as f64;
        let mut h = if x <= 0.0 {
            1.0
        } else if x > 2.0 * fc {
            0.0
        } else {
            (PI * x / (4.0 * fc)).cos().powi(2)
        };
        h /= sinc(2.0 * i as f64 * fc);
        let a = i as f64 * PI / 2.0;
        f[i] = C::from_polar(h, -a);
        f[(N - i) % N] = C::from_polar(h, a);
    }
    f
}

fn sinc(t: f64) -> f64 {
    if t == 0.0 {
        1.0
    } else {
        (PI * t).sin() / (PI * t)
    }
}

fn decay(avg: f64, value: f64, w: i64) -> f64 {
    avg + (value - avg) / w as f64
}

pub struct Demodulator {
    rate: f64,
    /// Complex (IQ) input: the input power is |z|^2, not x^2.
    complex: bool,
    bit_samples: f64,
    reverse: bool,
    mark_step: f64,
    space_step: f64,
    mark_phase: f64,
    space_phase: f64,
    mark_filter: FftFilter,
    space_filter: FftFilter,
    mark_env: f64,
    space_env: f64,
    mark_noise: f64,
    space_noise: f64,
    early_acc: i64,
    prompt_acc: i64,
    late_acc: i64,
    next_early: f64,
    next_prompt: f64,
    next_late: f64,
    avg_early: f64,
    avg_prompt: f64,
    avg_late: f64,
    sample_count: u64,
    /// Slow averages of the input power and of the power the two tone
    /// filters pass: their ratio is the channel's tone level.
    input_power: f64,
    tone_power: f64,
}

impl Demodulator {
    /// For audio: `centre` in Hz; `reverse` flips the bits (lower sideband).
    pub fn new(sample_rate: f64, centre: f64, reverse: bool) -> Demodulator {
        Demodulator::build(sample_rate, centre, reverse, false)
    }

    /// For complex IQ centred on the DSC frequency: mark (B, bit 0) at
    /// +85 Hz, space (Y, bit 1) at -85 Hz. The rate must be at least
    /// [`MIN_IQ_RATE`]: below it the filter's sinc correction turns
    /// negative and the tones are lost.
    pub fn new_iq(sample_rate: f64) -> Demodulator {
        Demodulator::build(sample_rate, 0.0, false, true)
    }

    fn build(sample_rate: f64, centre: f64, reverse: bool, complex: bool) -> Demodulator {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(N);
        let ifft = planner.plan_fft_inverse(N);
        let resp = Arc::new(response(sample_rate));
        let bit_samples = sample_rate / 100.0;
        Demodulator {
            rate: sample_rate,
            complex,
            bit_samples,
            reverse,
            mark_step: 2.0 * PI * (centre + 85.0) / sample_rate,
            space_step: 2.0 * PI * (centre - 85.0) / sample_rate,
            mark_phase: 0.0,
            space_phase: 0.0,
            mark_filter: FftFilter::new(resp.clone(), fft.clone(), ifft.clone()),
            space_filter: FftFilter::new(resp, fft, ifft),
            mark_env: 0.0,
            space_env: 0.0,
            mark_noise: 0.0,
            space_noise: 0.0,
            early_acc: 0,
            prompt_acc: 0,
            late_acc: 0,
            next_early: 0.0,
            next_prompt: bit_samples / 5.0,
            next_late: 2.0 * bit_samples / 5.0,
            avg_early: 0.0,
            avg_prompt: 0.0,
            avg_late: 0.0,
            sample_count: 0,
            input_power: 0.0,
            tone_power: 0.0,
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.rate
    }

    /// Tone energy around the centre tone, 0.0 … 1.0: the share of the
    /// input power within the two tone filters (about 0.1 s average). A
    /// clean DSC signal gives about 1; silence 0; noise or other tones
    /// little.
    pub fn level(&self) -> f64 {
        if self.input_power <= 0.0 {
            0.0
        } else {
            (self.tone_power / self.input_power).clamp(0.0, 1.0)
        }
    }

    /// Feeds audio samples; calls `out(soft_bit, sample_number)` for every
    /// bit.
    pub fn process(&mut self, samples: &[i16], out: &mut dyn FnMut(i64, u64)) {
        debug_assert!(!self.complex, "audio into an IQ demodulator");
        for &x in samples {
            let x = f64::from(x);
            self.push(C::new(x, x), x * x, out);
        }
    }

    /// Feeds complex IQ samples, scaled like 16-bit audio (a few thousand
    /// RMS: the discriminator takes the log of integer levels).
    pub fn process_iq(&mut self, samples: &[Complex<f32>], out: &mut dyn FnMut(i64, u64)) {
        debug_assert!(self.complex, "IQ into an audio demodulator");
        for z in samples {
            let z = C::new(f64::from(z.re), f64::from(z.im));
            self.push(z, z.norm_sqr(), out);
        }
    }

    /// Step 1 for one input sample of power `power`.
    fn push(&mut self, z: C, power: f64, out: &mut dyn FnMut(i64, u64)) {
        let level_w = (self.rate / 10.0).max(1.0);
        self.input_power += (power - self.input_power) / level_w;
        let zm = z * C::from_polar(1.0, self.mark_phase);
        let zs = z * C::from_polar(1.0, self.space_phase);
        self.mark_phase = wrap(self.mark_phase - self.mark_step);
        self.space_phase = wrap(self.space_phase - self.space_step);
        let m = self
            .mark_filter
            .run(zm)
            .map(|b| b.iter().map(|c| c.norm()).collect::<Vec<f64>>());
        let s = self
            .space_filter
            .run(zs)
            .map(|b| b.iter().map(|c| c.norm()).collect::<Vec<f64>>());
        if let (Some(m), Some(s)) = (m, s) {
            for (&m, &s) in m.iter().zip(&s) {
                self.tone_power += (m * m + s * s - self.tone_power) / level_w;
                self.step(m, s, out);
            }
        }
    }

    /// Steps 2–4 for one filtered sample.
    fn step(&mut self, m: f64, s: f64, out: &mut dyn FnMut(i64, u64)) {
        self.multicorrelator_step();
        let bs = self.bit_samples;
        let env = |avg: f64, v: f64| {
            if v > avg {
                decay(avg, v, (bs / 4.0) as i64)
            } else {
                decay(avg, v, (bs * 16.0) as i64)
            }
        };
        let noise = |avg: f64, v: f64| {
            if v < avg {
                decay(avg, v, (bs / 4.0) as i64)
            } else {
                decay(avg, v, (bs * 48.0) as i64)
            }
        };
        self.mark_env = env(self.mark_env, m);
        self.mark_noise = noise(self.mark_noise, m);
        self.space_env = env(self.space_env, s);
        self.space_noise = noise(self.space_noise, s);
        let floor = (self.mark_noise + self.space_noise) / 2.0;
        let m = m.min(self.mark_env).max(floor);
        let s = s.min(self.space_env).max(floor);
        let me = self.mark_env - floor;
        let se = self.space_env - floor;
        let logic = (m - floor) * me - (s - floor) * se - 0.5 * (me * me - se * se);
        let mut level = (1.0 + logic.abs()).ln().trunc() as i64;
        if logic < 0.0 {
            level = -level;
        }
        self.early_acc += level;
        self.prompt_acc += level;
        self.late_acc += level;
        self.timing_events(out);
        self.sample_count += 1;
    }

    fn timing_events(&mut self, out: &mut dyn FnMut(i64, u64)) {
        let n = self.sample_count as f64;
        if n >= self.next_early {
            self.avg_early = decay(self.avg_early, self.early_acc.abs() as f64, 64);
            self.next_early += self.bit_samples;
            self.early_acc = 0;
        }
        if n >= self.next_late {
            self.avg_late = decay(self.avg_late, self.late_acc.abs() as f64, 64);
            self.next_late += self.bit_samples;
            self.late_acc = 0;
        }
        if n >= self.next_prompt {
            self.avg_prompt = decay(self.avg_prompt, self.prompt_acc.abs() as f64, 64);
            self.next_prompt += self.bit_samples;
            let soft = if self.reverse {
                self.prompt_acc
            } else {
                -self.prompt_acc
            };
            self.prompt_acc = 0;
            out(soft, self.sample_count);
        }
    }

    fn multicorrelator_step(&mut self) {
        let period = (8.0 * self.bit_samples) as u64;
        if period == 0 || !self.sample_count.is_multiple_of(period) {
            return;
        }
        let bs = self.bit_samples;
        let mut slope = self.avg_late - self.avg_early;
        if self.avg_prompt * 1.05 < self.avg_early && self.avg_prompt * 1.05 < self.avg_late {
            if self.avg_early > self.avg_late {
                slope = ((self.next_early - self.next_prompt) - bs) % bs;
                self.avg_late = self.avg_prompt;
                self.avg_prompt = self.avg_early;
            } else {
                slope = ((self.next_late - self.next_prompt) + bs) % bs;
                self.avg_early = self.avg_prompt;
                self.avg_prompt = self.avg_late;
            }
        } else {
            slope /= 1024.0;
        }
        self.next_early += slope;
        self.next_prompt += slope;
        self.next_late += slope;
    }
}

fn wrap(p: f64) -> f64 {
    if p < -PI {
        p + 2.0 * PI
    } else if p > PI {
        p - 2.0 * PI
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::decoder::tx;

    fn pattern() -> Vec<u8> {
        let mut seed = 99u64;
        (0..400)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed & 1) as u8
            })
            .collect()
    }

    /// The bits the demodulator decides for FSK audio of `bits`.
    fn demodulate(bits: &[u8], rate: f64, centre: f64, tune: f64, reverse: bool) -> Vec<u8> {
        let audio = tx::fsk(bits, rate, centre, 8000.0);
        let mut d = Demodulator::new(rate, tune, reverse);
        let mut out = Vec::new();
        d.process(&audio, &mut |soft, _| out.push(u8::from(soft > 0)));
        out
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn fsk_bits_come_back_in_order() {
        let mut bits = tx::dots(200);
        bits.extend(pattern());
        bits.extend(tx::dots(100));
        for (rate, centre) in [(48000.0, 800.0), (11025.0, 1700.0), (44100.0, 1700.0)] {
            let got = demodulate(&bits, rate, centre, centre, false);
            assert!(contains(&got, &pattern()[..300]), "{rate} Hz, {centre} Hz");
            let per_second = got.len() as f64 / (bits.len() as f64 / 100.0);
            assert!((per_second - 100.0).abs() < 2.0, "{per_second} bits/s");
        }
    }

    #[test]
    fn reverse_flips_every_bit() {
        let mut bits = tx::dots(200);
        bits.extend(pattern());
        bits.extend(tx::dots(100));
        let flipped: Vec<u8> = pattern()[..300].iter().map(|b| 1 - b).collect();
        assert!(contains(
            &demodulate(&bits, 48000.0, 800.0, 800.0, true),
            &flipped
        ));
    }

    #[test]
    fn soft_bits_carry_their_sample_number() {
        let audio = tx::fsk(&tx::dots(300), 48000.0, 800.0, 8000.0);
        let mut d = Demodulator::new(48000.0, 800.0, false);
        let mut at = Vec::new();
        d.process(&audio[..10000], &mut |_, s| at.push(s));
        d.process(&audio[10000..], &mut |_, s| at.push(s));
        assert!(at.len() > 250);
        assert!(
            at.windows(2).all(|w| (470..=490).contains(&(w[1] - w[0]))),
            "{:?}",
            &at[..5]
        );
    }

    /// The tone level after a few seconds of this audio.
    fn level_of(audio: &[i16]) -> f64 {
        let mut d = Demodulator::new(48000.0, 800.0, false);
        d.process(audio, &mut |_, _| {});
        d.level()
    }

    #[test]
    fn the_level_shows_tone_energy_around_the_centre_not_loudness() {
        let mut bits = tx::dots(100);
        bits.extend(pattern());
        assert!(level_of(&tx::fsk(&bits, 48000.0, 800.0, 3000.0)) > 0.6);
        // A loud tone 900 Hz away, and loud white noise, are not activity.
        assert!(level_of(&tx::fsk(&bits, 48000.0, 1700.0, 20000.0)) < 0.2);
        let mut seed = 5u64;
        let noise: Vec<i16> = (0..48000 * 3)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed % 40000) as i32 - 20000) as i16
            })
            .collect();
        assert!(level_of(&noise) < 0.2, "{}", level_of(&noise));
        assert_eq!(level_of(&vec![0; 48000]), 0.0);
    }

    /// The bits the IQ demodulator decides for complex FSK of `bits`.
    fn demodulate_iq(bits: &[u8], rate: f64, offset: f64, amp: f32) -> Vec<u8> {
        let iq = tx::fsk_iq(bits, rate, offset, amp);
        let mut d = Demodulator::new_iq(rate);
        let mut out = Vec::new();
        d.process_iq(&iq, &mut |soft, _| out.push(u8::from(soft > 0)));
        out
    }

    #[test]
    fn iq_fsk_bits_come_back_in_order_at_channel_rates() {
        let mut bits = tx::dots(200);
        bits.extend(pattern());
        bits.extend(tx::dots(100));
        for rate in [8000.0, 11718.75, 15625.0, 48000.0] {
            let got = demodulate_iq(&bits, rate, 0.0, 8000.0);
            assert!(contains(&got, &pattern()[..300]), "{rate} Hz");
            let per_second = got.len() as f64 / (bits.len() as f64 / 100.0);
            assert!((per_second - 100.0).abs() < 2.0, "{per_second} bits/s");
        }
    }

    #[test]
    fn iq_tolerates_a_small_mistuning() {
        let mut bits = tx::dots(200);
        bits.extend(pattern());
        bits.extend(tx::dots(100));
        for off in [-20.0, 20.0] {
            assert!(
                contains(
                    &demodulate_iq(&bits, 15625.0, off, 8000.0),
                    &pattern()[..300]
                ),
                "{off} Hz"
            );
        }
    }

    #[test]
    fn iq_level_is_high_for_fsk_and_low_off_frequency() {
        let mut bits = tx::dots(100);
        bits.extend(pattern());
        let level = |iq: &[Complex<f32>]| {
            let mut d = Demodulator::new_iq(15625.0);
            d.process_iq(iq, &mut |_, _| {});
            d.level()
        };
        assert!(level(&tx::fsk_iq(&bits, 15625.0, 0.0, 3000.0)) > 0.6);
        assert!(level(&tx::fsk_iq(&bits, 15625.0, 900.0, 3000.0)) < 0.2);
    }

    #[test]
    fn silence_gives_bits_but_no_panic() {
        let mut d = Demodulator::new(8000.0, 800.0, false);
        let mut n = 0;
        d.process(&vec![0; 80000], &mut |_, _| n += 1);
        assert!((990..=1000).contains(&n), "{n}");
    }
}
