//! From the VFO's channel IQ to the decoders' inputs (spec 16.3): the
//! sideband, then complex resampling to the modem's rate and to the RSID
//! detector's 11025 Hz. No audio is made: `Upper` is the IQ itself (the
//! decoders keep its positive frequencies), `Lower` the IQ conjugated, and
//! `Both` the real part of the IQ turned to the AM carrier by a PLL, which
//! adds the two sidebands coherently.

use num_complex::{Complex32, Complex64};
use std::f64::consts::TAU;

use crate::dsp::resampler::Resampler;

/// Which part of the channel carries the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sideband {
    /// Above the VFO frequency (USB; the upper sideband of AM).
    #[default]
    Upper,
    /// Below it, mirrored (LSB; the lower sideband of AM).
    Lower,
    /// Both sidebands of an AM signal, added coherently.
    Both,
}

impl Sideband {
    pub const ALL: [Sideband; 3] = [Sideband::Upper, Sideband::Lower, Sideband::Both];

    pub fn name(self) -> &'static str {
        match self {
            Sideband::Upper => "Upper",
            Sideband::Lower => "Lower",
            Sideband::Both => "Both",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == s)
    }

    /// The sign between the VFO and the carrier in a mark (`+1496`).
    pub fn sign(self) -> char {
        if self == Sideband::Lower { '-' } else { '+' }
    }
}

/// An AM carrier PLL: second order, about 10 Hz natural frequency, with its
/// phase detector behind a 100 Hz low-pass so only the carrier steers it.
#[derive(Debug, Clone)]
pub struct CarrierPll {
    phase: f64,
    freq: f64,
    lp: Complex64,
    alpha: f64,
    kp: f64,
    ki: f64,
    rate: f64,
}

impl CarrierPll {
    pub fn new(rate: f64) -> Self {
        let wn = TAU * 10.0 / rate;
        let zeta = 0.707;
        Self {
            phase: 0.0,
            freq: 0.0,
            lp: Complex64::new(0.0, 0.0),
            alpha: 1.0 - (-TAU * 100.0 / rate).exp(),
            kp: 2.0 * zeta * wn,
            ki: wn * wn,
            rate,
        }
    }

    /// The sample turned so the carrier is real and positive.
    pub fn run(&mut self, x: Complex64) -> Complex64 {
        let y = x * Complex64::from_polar(1.0, -self.phase);
        self.lp += (y - self.lp) * self.alpha;
        let err = self.lp.im.atan2(self.lp.re);
        self.freq += self.ki * err;
        self.phase = (self.phase + self.freq + self.kp * err).rem_euclid(TAU);
        y
    }

    /// The carrier's offset from the VFO (Hz).
    pub fn offset_hz(&self) -> f64 {
        self.freq * self.rate / TAU
    }
}

/// A complex resampler (two real ones).
struct ComplexResampler {
    re: Resampler,
    im: Resampler,
}

impl ComplexResampler {
    fn new(from: f64, to: f64) -> Self {
        Self {
            re: Resampler::new(from, to),
            im: Resampler::new(from, to),
        }
    }

    fn process(&mut self, x: &[Complex32]) -> Vec<Complex64> {
        let re: Vec<f32> = x.iter().map(|z| z.re).collect();
        let im: Vec<f32> = x.iter().map(|z| z.im).collect();
        let re = self.re.process(&re);
        let im = self.im.process(&im);
        re.iter()
            .zip(&im)
            .map(|(&a, &b)| Complex64::new(f64::from(a), f64::from(b)))
            .collect()
    }
}

/// The front end for one sideband and one modem rate.
pub struct FrontEnd {
    sideband: Sideband,
    pll: CarrierPll,
    modem_rate: u32,
    to_modem: Option<ComplexResampler>,
    to_rsid: ComplexResampler,
    work: Vec<Complex32>,
}

impl FrontEnd {
    /// For channel IQ at `rate` (at least 8 kHz) and a modem at
    /// `modem_rate` (8000 or 11025).
    pub fn new(rate: f64, sideband: Sideband, modem_rate: u32) -> Self {
        let rsid = crate::mfsk::rsid::SAMPLE_RATE;
        Self {
            sideband,
            pll: CarrierPll::new(rate),
            modem_rate,
            to_modem: (f64::from(modem_rate) != rsid)
                .then(|| ComplexResampler::new(rate, f64::from(modem_rate))),
            to_rsid: ComplexResampler::new(rate, rsid),
            work: Vec::new(),
        }
    }

    pub fn sideband(&self) -> Sideband {
        self.sideband
    }

    pub fn modem_rate(&self) -> u32 {
        self.modem_rate
    }

    /// The AM carrier's offset from the VFO (`Both` only).
    pub fn carrier_offset_hz(&self) -> Option<f64> {
        (self.sideband == Sideband::Both).then(|| self.pll.offset_hz())
    }

    /// Channel IQ in; the modem's and the RSID detector's samples out.
    ///
    /// `Both`: both get the carrier-locked IQ, whose positive frequencies
    /// are the upper sideband and negative ones the lower sideband mirrored:
    /// the modem is a diversity receiver (`Rx::new_diversity`, fed `y` and
    /// `conj(y)`) and the RSID detector adds the two by power
    /// (`Detector::set_fold`). On a real recording a selective fade cancelled
    /// two tones of an RSID in the coherent sum of the sidebands that either
    /// sideband alone had clean.
    pub fn process(&mut self, iq: &[Complex32]) -> (Vec<Complex64>, Vec<Complex64>) {
        self.work.clear();
        match self.sideband {
            Sideband::Upper => self.work.extend_from_slice(iq),
            Sideband::Lower => self.work.extend(iq.iter().map(|z| z.conj())),
            Sideband::Both => {
                for z in iq {
                    let y = self
                        .pll
                        .run(Complex64::new(f64::from(z.re), f64::from(z.im)));
                    self.work.push(Complex32::new(y.re as f32, y.im as f32));
                }
            }
        }
        let rsid = self.to_rsid.process(&self.work);
        let modem = match &mut self.to_modem {
            Some(r) => r.process(&self.work),
            None => rsid.clone(),
        };
        (modem, rsid)
    }

    /// `Both`: the modem's samples hold two sidebands (feed `Rx::process2`
    /// with the sample and its conjugate).
    pub fn diversity(&self) -> bool {
        self.sideband == Sideband::Both
    }

    /// Whether the RSID detector should add the mirrored sideband.
    pub fn rsid_fold(&self) -> bool {
        self.sideband == Sideband::Both
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mfsk::modes::by_name;
    use crate::mfsk::rx::{Event, Rx};
    use crate::mfsk::tx::Tx;

    const TEXT: &[u8] = b"Shortwave Radiogram in MFSK32 via the IQ path.\n";

    /// The MFSK audio at 8 kHz, upsampled to `rate` (real).
    fn audio(rate: f64) -> Vec<f32> {
        let mode = by_name("MFSK-32").unwrap();
        let mut tx = Tx::new(mode, 1500.0);
        tx.silence(4000);
        tx.start();
        tx.text(TEXT);
        tx.end();
        tx.silence(8000);
        let a: Vec<f32> = tx.take().iter().map(|z| z.re as f32).collect();
        Resampler::new(8000.0, rate).process(&a)
    }

    /// An analytic version of a real signal (positive frequencies), by FFT.
    fn analytic(x: &[f32]) -> Vec<Complex64> {
        use rustfft::FftPlanner;
        let n = x.len();
        let mut buf: Vec<Complex64> = x
            .iter()
            .map(|&v| Complex64::new(f64::from(v), 0.0))
            .collect();
        let mut p = FftPlanner::new();
        p.plan_fft_forward(n).process(&mut buf);
        for (i, b) in buf.iter_mut().enumerate() {
            if i == 0 || i > n / 2 {
                *b = Complex64::new(0.0, 0.0);
            }
        }
        p.plan_fft_inverse(n).process(&mut buf);
        buf.iter().map(|z| z * (2.0 / n as f64)).collect()
    }

    fn decode(fe: &mut FrontEnd, iq: &[Complex32]) -> Vec<u8> {
        let mode = by_name("MFSK-32").unwrap();
        let mut rx = if fe.diversity() {
            Rx::new_diversity(mode, 1500.0)
        } else {
            Rx::new(mode, 1500.0)
        };
        let mut out = Vec::new();
        for block in iq.chunks(1000) {
            let (m, _) = fe.process(block);
            for s in m {
                if fe.diversity() {
                    rx.process2(s, s.conj());
                } else {
                    rx.process(s);
                }
            }
            for e in rx.take_events() {
                if let Event::Byte { byte, .. } = e {
                    out.push(byte);
                }
            }
        }
        out
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// AM: carrier + the audio on both sidebands, the carrier `df` Hz off
    /// the VFO; `jam_upper`/`jam_lower` put a strong other signal over one
    /// sideband.
    fn am(rate: f64, df: f64, jam_upper: bool, jam_lower: bool) -> Vec<Complex32> {
        let a = audio(rate);
        let jam = analytic(&audio(rate).iter().rev().copied().collect::<Vec<_>>());
        (0..a.len())
            .map(|i| {
                let t = i as f64 / rate;
                let mut z = Complex64::from_polar(1.0 + 0.5 * f64::from(a[i]), TAU * df * t + 0.3);
                if jam_upper {
                    z += jam[i] * 3.0 * Complex64::from_polar(1.0, TAU * df * t);
                }
                if jam_lower {
                    z += jam[i].conj() * 3.0 * Complex64::from_polar(1.0, TAU * df * t);
                }
                Complex32::new(z.re as f32, z.im as f32)
            })
            .collect()
    }

    #[test]
    fn upper_and_lower_decode_an_ssb_signal_and_its_mirror() {
        let rate = 15_625.0;
        let usb: Vec<Complex32> = analytic(&audio(rate))
            .iter()
            .map(|z| Complex32::new(z.re as f32, z.im as f32))
            .collect();
        let lsb: Vec<Complex32> = usb.iter().map(|z| z.conj()).collect();
        assert!(contains(
            &decode(&mut FrontEnd::new(rate, Sideband::Upper, 8000), &usb),
            TEXT
        ));
        assert!(contains(
            &decode(&mut FrontEnd::new(rate, Sideband::Lower, 8000), &lsb),
            TEXT
        ));
        // the wrong sideband hears nothing
        assert!(!contains(
            &decode(&mut FrontEnd::new(rate, Sideband::Upper, 8000), &lsb),
            TEXT
        ));
    }

    #[test]
    fn am_decodes_on_either_sideband_when_the_other_is_jammed() {
        let rate = 15_625.0;
        let up_jammed = am(rate, 0.0, true, false);
        let low_jammed = am(rate, 0.0, false, true);
        assert!(contains(
            &decode(&mut FrontEnd::new(rate, Sideband::Lower, 8000), &up_jammed),
            TEXT
        ));
        assert!(contains(
            &decode(&mut FrontEnd::new(rate, Sideband::Upper, 8000), &low_jammed),
            TEXT
        ));
        assert!(!contains(
            &decode(&mut FrontEnd::new(rate, Sideband::Upper, 8000), &up_jammed),
            TEXT
        ));
    }

    #[test]
    fn both_locks_to_a_carrier_off_the_vfo_and_decodes() {
        let rate = 15_625.0;
        let sig = am(rate, 12.0, false, false);
        let mut fe = FrontEnd::new(rate, Sideband::Both, 8000);
        assert!(contains(&decode(&mut fe, &sig), TEXT));
        let off = fe.carrier_offset_hz().unwrap();
        assert!((off - 12.0).abs() < 0.5, "{off}");
    }

    #[test]
    fn the_pll_locks_within_a_second_at_30_hz() {
        let rate = 15_625.0;
        let mut pll = CarrierPll::new(rate);
        let mut err = 0.0;
        for i in 0..rate as usize * 2 {
            let x = Complex64::from_polar(1.0, TAU * 30.0 * i as f64 / rate + 1.0);
            let y = pll.run(x);
            if i > rate as usize {
                err = f64::max(err, y.arg().abs());
            }
        }
        assert!(err < 0.05, "{err}");
    }
}
