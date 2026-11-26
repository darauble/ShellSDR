//! The whole receiver of a Digital VFO (spec 16.3..16.6): channel IQ in;
//! squelched text, pictures and RSID notes out. Pure and synchronous: the
//! service runs it on a worker thread, the tests on a recording.

use num_complex::{Complex32, Complex64};

use super::frontend::{FrontEnd, Sideband};
use super::modes::Mode;
use super::rsid::{self, Detector};
use super::rx::{Event, Picture, Rx};

/// The audio range RSIDs are searched in (Hz).
pub const RSID_RANGE: (f64, f64) = (200.0, 3800.0);
/// The carrier's range (Hz).
pub const CARRIER_RANGE: (f64, f64) = (200.0, 3800.0);
/// The squelch closes this far below the level it opens at.
pub const SQUELCH_HYSTERESIS: f64 = 15.0;
/// The default squelch level: chosen on the Radiogram 475 recording
/// (spec 16.5), where noise never reached 45 and the programme's text,
/// once open, never fell below 38.
pub const DEFAULT_SQUELCH: f64 = 50.0;

/// What the receiver is set to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub mode: Mode,
    /// The audio frequency of the signal's middle (Hz).
    pub carrier: f64,
    pub sideband: Sideband,
    pub afc: bool,
    /// Search RSIDs and switch to the modes they name.
    pub rsid: bool,
    /// The squelch level (0..100; 0 = off).
    pub squelch: f64,
}

/// An RSID heard.
#[derive(Debug, Clone, PartialEq)]
pub enum RsidNote {
    /// A mode this build decodes: the receiver switched to it (fldigi's
    /// "Before RSID" and "New RSID" marks).
    Switch {
        before: (&'static str, f64),
        after: (&'static str, f64),
    },
    /// A mode this build does not decode (`name`), or an unknown code.
    Other {
        name: Option<&'static str>,
        code: u16,
        freq: f64,
    },
}

/// What came out of a block.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Text(String),
    PictureStart {
        width: usize,
        height: usize,
        color: bool,
    },
    PictureDone(Picture),
    Rsid(RsidNote),
}

/// The receiver's state for the view.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Status {
    /// 0..100.
    pub metric: f64,
    pub s2n_db: f64,
    pub carrier: f64,
    /// The squelch lets text through.
    pub open: bool,
    /// `Both`: the AM carrier's offset from the VFO (Hz).
    pub am_offset: Option<f64>,
    /// A picture is being received: (pixels done, of).
    pub picture: Option<(usize, usize)>,
}

/// Bytes to characters: UTF-8, an invalid sequence as Latin-1, CR / LF /
/// CR LF to one line end, other control characters dropped.
#[derive(Debug, Clone, Default)]
pub struct TextAssembler {
    pending: Vec<u8>,
    need: usize,
    after_cr: bool,
}

impl TextAssembler {
    pub fn push(&mut self, b: u8, out: &mut String) {
        if self.need > 0 {
            if b & 0xC0 == 0x80 {
                self.pending.push(b);
                self.need -= 1;
                if self.need == 0 {
                    match std::str::from_utf8(&self.pending) {
                        Ok(s) => out.push_str(s),
                        Err(_) => self.flush_latin1(out),
                    }
                    self.pending.clear();
                }
                return;
            }
            self.flush_latin1(out);
        }
        let need = match b {
            0xC2..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF4 => 3,
            _ => 0,
        };
        if need > 0 {
            self.pending.push(b);
            self.need = need;
            return;
        }
        let after_cr = std::mem::take(&mut self.after_cr);
        match b {
            b'\r' => {
                out.push('\n');
                self.after_cr = true;
            }
            b'\n' if after_cr => {}
            b'\n' | b'\t' => out.push(b as char),
            0x20..=0x7E => out.push(b as char),
            0x7F | 0..=0x1F => {}
            _ => out.push(char::from(b)),
        }
    }

    fn flush_latin1(&mut self, out: &mut String) {
        out.extend(self.pending.drain(..).map(char::from));
        self.need = 0;
    }
}

/// The squelch: opens when the metric reaches the level, closes when it
/// falls `SQUELCH_HYSTERESIS` below it.
#[derive(Debug, Clone, Default)]
struct Gate {
    open: bool,
}

impl Gate {
    fn pass(&mut self, metric: f64, level: f64) -> bool {
        if level <= 0.0 || (!self.open && metric >= level) {
            self.open = true;
        } else if self.open && metric < level - SQUELCH_HYSTERESIS {
            self.open = false;
        }
        self.open
    }
}

pub struct Decoder {
    config: Config,
    rate: f64,
    fe: FrontEnd,
    rx: Rx,
    det: Detector,
    gate: Gate,
    text: TextAssembler,
}

fn new_rx(config: &Config) -> Rx {
    let mut rx = if config.sideband == Sideband::Both {
        Rx::new_diversity(config.mode, config.carrier)
    } else {
        Rx::new(config.mode, config.carrier)
    };
    rx.set_afc(config.afc, afc_level(config.squelch));
    rx
}

fn afc_level(squelch: f64) -> f64 {
    (squelch - SQUELCH_HYSTERESIS).max(0.0)
}

impl Decoder {
    /// For channel IQ at `rate` Hz (at least 8 kHz).
    pub fn new(rate: f64, config: Config) -> Self {
        let mut det = Detector::new();
        det.set_range(RSID_RANGE.0, RSID_RANGE.1);
        det.set_fold(config.sideband == Sideband::Both);
        Self {
            fe: FrontEnd::new(rate, config.sideband, config.mode.sample_rate),
            rx: new_rx(&config),
            config,
            rate,
            det,
            gate: Gate::default(),
            text: TextAssembler::default(),
        }
    }

    pub fn config(&self) -> Config {
        Config {
            carrier: self.rx.frequency(),
            ..self.config
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Change the settings: a new mode or sideband starts the modem again
    /// (a picture being received is lost), the rest apply at once.
    pub fn set_config(&mut self, c: Config) {
        let c = Config {
            carrier: c.carrier.clamp(CARRIER_RANGE.0, CARRIER_RANGE.1),
            ..c
        };
        let old = self.config();
        if c.sideband != old.sideband || c.mode.sample_rate != old.mode.sample_rate {
            self.fe = FrontEnd::new(self.rate, c.sideband, c.mode.sample_rate);
            self.det.set_fold(c.sideband == Sideband::Both);
        }
        self.config = c;
        if c.sideband != old.sideband || c.mode != old.mode {
            self.rx = new_rx(&c);
        } else {
            if c.carrier != old.carrier {
                self.rx.set_frequency(c.carrier);
            }
            self.rx.set_afc(c.afc, afc_level(c.squelch));
        }
        if !c.rsid {
            self.det.reset();
        }
    }

    pub fn status(&self) -> Status {
        Status {
            metric: self.rx.metric(),
            s2n_db: self.rx.s2n_db(),
            carrier: self.rx.frequency(),
            open: self.gate.open,
            am_offset: self.fe.carrier_offset_hz(),
            picture: self
                .rx
                .in_picture()
                .then(|| self.rx.picture().map(|p| (p.done, p.width * p.height)))
                .flatten(),
        }
    }

    /// The picture being received (or the last one).
    pub fn picture(&self) -> Option<&Picture> {
        self.rx.picture()
    }

    /// The RSID detector's last spectrum (0..5512 Hz) and its count.
    pub fn spectrum(&self) -> (&[f32], u64) {
        self.det.spectrum()
    }

    fn apply_rsid(&mut self, f: rsid::Found, out: &mut Vec<Output>) {
        if f.table == 1 && f.code == rsid::EOT {
            return;
        }
        let Some(mode) = super::modes::by_rsid(f.table, f.code) else {
            out.push(Output::Rsid(RsidNote::Other {
                name: rsid::name(f.table, f.code),
                code: f.code,
                freq: f.freq_hz,
            }));
            return;
        };
        let before = self.config();
        let carrier = f.freq_hz.clamp(CARRIER_RANGE.0, CARRIER_RANGE.1);
        out.push(Output::Rsid(RsidNote::Switch {
            before: (before.mode.name, before.carrier),
            after: (mode.name, carrier),
        }));
        self.set_config(Config {
            mode,
            carrier,
            ..before
        });
    }

    /// A block of channel IQ in.
    pub fn process(&mut self, iq: &[Complex32]) -> Vec<Output> {
        let mut out = Vec::new();
        let (modem, rsid) = self.fe.process(iq);
        if self.config.rsid {
            self.det.process(&rsid);
            for f in self.det.take_found() {
                self.apply_rsid(f, &mut out);
            }
        }
        let diversity = self.fe.diversity();
        let mut text = String::new();
        for s in modem {
            if diversity {
                self.rx.process2(s, s.conj());
            } else {
                self.rx.process(s);
            }
            for e in self.rx.take_events() {
                match e {
                    Event::Byte { byte, metric } => {
                        if self.gate.pass(metric, self.config.squelch) {
                            self.text.push(byte, &mut text);
                        }
                    }
                    Event::PictureStart {
                        width,
                        height,
                        color,
                    } => {
                        if !text.is_empty() {
                            out.push(Output::Text(std::mem::take(&mut text)));
                        }
                        out.push(Output::PictureStart {
                            width,
                            height,
                            color,
                        });
                    }
                    Event::PictureDone => {
                        if let Some(p) = self.rx.picture() {
                            out.push(Output::PictureDone(p.clone()));
                        }
                    }
                }
            }
        }
        if !text.is_empty() {
            out.push(Output::Text(text));
        }
        out
    }
}

/// A real sample as the modem's input (for tests that start from audio).
pub fn real(x: f64) -> Complex64 {
    Complex64::new(x, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mfsk::modes::by_name;
    use crate::mfsk::tx::Tx;

    fn text_of(bytes: &[u8]) -> String {
        let mut a = TextAssembler::default();
        let mut s = String::new();
        for &b in bytes {
            a.push(b, &mut s);
        }
        s
    }

    #[test]
    fn bytes_become_text() {
        assert_eq!(text_of(b"a\r\nb\rc\nd\x02\x04e\x08"), "a\nb\nc\nde");
        assert_eq!(text_of("go – from ą".as_bytes()), "go – from ą");
        // an invalid sequence is read as Latin-1
        assert_eq!(text_of(&[b'x', 0xE9, b'y', 0xC3]), "xéy");
        assert_eq!(text_of(&[0xC3, 0xA9]), "é");
    }

    #[test]
    fn the_squelch_has_hysteresis_and_zero_is_off() {
        let mut g = Gate::default();
        assert!(!g.pass(45.0, 50.0));
        assert!(g.pass(50.0, 50.0));
        assert!(g.pass(36.0, 50.0));
        assert!(!g.pass(34.0, 50.0));
        assert!(!g.pass(49.0, 50.0));
        assert!(g.pass(0.0, 0.0));
    }

    /// An SSB (upper) channel at 15625 Hz carrying `audio` made at 8 kHz.
    fn channel(audio: &[Complex64]) -> Vec<Complex32> {
        use crate::dsp::resampler::Resampler;
        let re: Vec<f32> = audio.iter().map(|z| z.re as f32).collect();
        let im: Vec<f32> = audio.iter().map(|z| z.im as f32).collect();
        let re = Resampler::new(8000.0, 15625.0).process(&re);
        let im = Resampler::new(8000.0, 15625.0).process(&im);
        re.iter()
            .zip(&im)
            .map(|(&a, &b)| Complex32::new(a, b))
            .collect()
    }

    fn config(mode: &str) -> Config {
        Config {
            mode: by_name(mode).unwrap(),
            carrier: 1500.0,
            sideband: Sideband::Upper,
            afc: true,
            rsid: true,
            squelch: DEFAULT_SQUELCH,
        }
    }

    fn run(d: &mut Decoder, iq: &[Complex32]) -> Vec<Output> {
        let mut out = Vec::new();
        for b in iq.chunks(1600) {
            out.extend(d.process(b));
        }
        // join the text pieces
        let mut joined: Vec<Output> = Vec::new();
        for o in out {
            match (joined.last_mut(), o) {
                (Some(Output::Text(a)), Output::Text(b)) => a.push_str(&b),
                (_, o) => joined.push(o),
            }
        }
        joined
    }

    #[test]
    fn an_rsid_switches_mode_and_carrier_and_the_text_follows() {
        // MFSK-16 at 1500, then an RSID for MFSK-64 at 1000 Hz and its text
        let m16 = by_name("MFSK-16").unwrap();
        let m64 = by_name("MFSK-64").unwrap();
        let mut a = Tx::new(m16, 1500.0);
        a.silence(4000);
        a.start();
        a.text(b"first part\n");
        a.end();
        let mut b = Tx::new(m64, 1000.0);
        b.rsid(2, 620);
        b.start();
        b.text(b"second part in MFSK-64\n");
        b.end();
        b.silence(8000);
        let mut audio = a.take();
        audio.extend(b.take());
        let mut d = Decoder::new(15625.0, config("MFSK-16"));
        let out = run(&mut d, &channel(&audio));
        let switch = out
            .iter()
            .find_map(|o| match o {
                Output::Rsid(RsidNote::Switch { before, after }) => Some((*before, *after)),
                _ => None,
            })
            .expect("an RSID switch");
        assert_eq!(switch.0.0, "MFSK-16");
        assert!((switch.0.1 - 1500.0).abs() < 1.0, "{switch:?}");
        assert_eq!(switch.1.0, "MFSK-64");
        assert!((switch.1.1 - 1000.0).abs() < 6.0, "{switch:?}");
        let text: String = out
            .iter()
            .filter_map(|o| match o {
                Output::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains("first part\n"), "{text:?}");
        assert!(text.contains("second part in MFSK-64\n"), "{text:?}");
        assert_eq!(d.config().mode.name, "MFSK-64");
    }

    #[test]
    fn an_rsid_of_another_mode_is_only_noted() {
        let m32 = by_name("MFSK-32").unwrap();
        let mut a = Tx::new(m32, 1200.0);
        a.silence(4000);
        a.rsid(1, 1); // BPSK31
        a.silence(8000);
        let mut d = Decoder::new(15625.0, config("MFSK-32"));
        let out = run(&mut d, &channel(&a.take()));
        assert!(
            out.iter().any(|o| matches!(
                o,
                Output::Rsid(RsidNote::Other {
                    name: Some("BPSK31"),
                    code: 1,
                    ..
                })
            )),
            "{out:?}"
        );
        assert_eq!(d.config().mode.name, "MFSK-32");
    }

    #[test]
    fn noise_prints_nothing_with_the_squelch_and_garbage_without() {
        let n = 15625 * 30;
        let mut seed = 99u64;
        let mut r = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5) as f32
        };
        let noise: Vec<Complex32> = (0..n).map(|_| Complex32::new(r(), r())).collect();
        let mut d = Decoder::new(15625.0, config("MFSK-32"));
        assert!(run(&mut d, &noise).is_empty());
        let mut open = Decoder::new(
            15625.0,
            Config {
                squelch: 0.0,
                ..config("MFSK-32")
            },
        );
        assert!(!run(&mut open, &noise).is_empty());
    }

    #[test]
    fn a_picture_comes_out_between_its_header_and_the_next_text() {
        let m = by_name("MFSK-32").unwrap();
        let mut a = Tx::new(m, 1500.0);
        a.silence(4000);
        a.start();
        a.text(b"Look:");
        let rgb: Vec<u8> = (0..16 * 8).flat_map(|i| [i as u8, 100, 200]).collect();
        a.picture(16, 8, true, &rgb);
        a.text(b"done\n");
        a.end();
        a.silence(8000);
        let mut d = Decoder::new(15625.0, config("MFSK-32"));
        let out = run(&mut d, &channel(&a.take()));
        let kinds: Vec<&str> = out
            .iter()
            .map(|o| match o {
                Output::Text(_) => "text",
                Output::PictureStart { .. } => "start",
                Output::PictureDone(_) => "done",
                Output::Rsid(_) => "rsid",
            })
            .collect();
        assert_eq!(kinds, ["text", "start", "done", "text"], "{out:?}");
        let Output::Text(t) = &out[0] else {
            unreachable!()
        };
        assert!(t.ends_with("Look:\nSending Pic:16x8C;"), "{t:?}");
        let Output::PictureDone(p) = &out[2] else {
            unreachable!()
        };
        assert_eq!((p.width, p.height, p.color), (16, 8, true));
    }
}
