//! MFSK with pictures and RSID (0.2.5, spec 16), ported from fldigi 4.1.23.
//!
//! fldigi is free software under the GNU GPL v3 or later: Dave Freese W1HKJ,
//! Stelios Bounanos M0GLD, John Douyere VK2ETA and its contributors; its
//! MFSK code comes from gMFSK by Tomi Manninen OH2BNS; the MFSK varicode is
//! by Nino Porcino IZ8BLY and Murray Greenman ZL1BPU; RSID by Patrick
//! Lindecker F6CTE. This port is under the same licence.
//!
//! Receive only (`rx`, `rsid`); `tx` makes test signals. The signals are
//! complex: positive frequencies are the audio.

pub mod decoder;
pub mod filters;
pub mod frontend;
pub mod interleave;
pub mod markdown;
pub mod modes;
pub mod rsid;
pub mod rx;
pub mod tx;
pub mod varicode;
pub mod viterbi;

#[cfg(test)]
mod tests {
    use super::modes::{MODES, Mode, by_name};
    use super::rx::{Event, Rx};
    use super::tx::Tx;
    use num_complex::Complex64;

    const TEXT: &[u8] =
        b"CQ CQ de LY0SDR: The quick brown fox jumps over the lazy dog 0123456789.\n";

    /// A deterministic noise source (uniform, zero mean).
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) - 0.5
        }
    }

    fn transmission(mode: Mode, carrier: f64) -> Vec<Complex64> {
        let mut tx = Tx::new(mode, carrier);
        tx.silence(mode.sample_rate as usize / 2);
        tx.start();
        tx.text(TEXT);
        tx.end();
        tx.silence(mode.sample_rate as usize);
        tx.take()
    }

    fn decode(rx: &mut Rx, signal: &[Complex64]) -> (Vec<u8>, Vec<Event>) {
        let mut bytes = Vec::new();
        let mut events = Vec::new();
        for &s in signal {
            rx.process(s);
            for e in rx.take_events() {
                if let Event::Byte { byte, .. } = e {
                    bytes.push(byte);
                } else {
                    events.push(e);
                }
            }
        }
        (bytes, events)
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn every_mode_decodes_its_own_text_from_audio() {
        for mode in MODES {
            let sig: Vec<Complex64> = transmission(mode, 1500.0)
                .into_iter()
                .map(|z| Complex64::new(z.re, 0.0))
                .collect();
            let mut rx = Rx::new(mode, 1500.0);
            let (bytes, _) = decode(&mut rx, &sig);
            assert!(
                contains(&bytes, TEXT),
                "{}: {:?}",
                mode.name,
                String::from_utf8_lossy(&bytes)
            );
        }
    }

    #[test]
    fn iq_input_rejects_a_signal_in_the_other_sideband() {
        let mode = by_name("MFSK-32").unwrap();
        let wanted = transmission(mode, 1500.0);
        // another MFSK signal mirrored onto the same audio frequencies
        let mut other = Tx::new(mode, 1500.0);
        other.start();
        other.text(&b"zzzz zzzz zzzz zzzz zzzz zzzz zzzz zzzz zzzz zzzz zzzz zzzz".repeat(3));
        let other = other.take();
        let sig: Vec<Complex64> = wanted
            .iter()
            .enumerate()
            .map(|(i, z)| {
                z + other
                    .get(i)
                    .map_or(Complex64::new(0.0, 0.0), |o| o.conj() * 2.0)
            })
            .collect();
        let mut rx = Rx::new(mode, 1500.0);
        let (bytes, _) = decode(&mut rx, &sig);
        assert!(
            contains(&bytes, TEXT),
            "{:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn afc_follows_a_signal_off_the_carrier() {
        let mode = by_name("MFSK-16").unwrap();
        let mut tx = Tx::new(mode, 1504.0);
        tx.start();
        tx.text(&TEXT.repeat(4));
        tx.end();
        let sig = tx.take();
        let mut rx = Rx::new(mode, 1500.0);
        let (bytes, _) = decode(&mut rx, &sig);
        assert!(
            contains(&bytes, TEXT),
            "{:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert!((rx.frequency() - 1504.0).abs() < 1.0, "{}", rx.frequency());
    }

    #[test]
    fn noise_has_a_low_metric_and_a_signal_a_high_one() {
        let mode = by_name("MFSK-32").unwrap();
        let mut n = Noise(7);
        let noise: Vec<Complex64> = (0..8000 * 20)
            .map(|_| Complex64::new(n.next(), n.next()))
            .collect();
        let mut rx = Rx::new(mode, 1500.0);
        let mut worst: f64 = 0.0;
        for (i, &s) in noise.iter().enumerate() {
            rx.process(s);
            if i > 8000 * 5 {
                worst = worst.max(rx.metric());
            }
        }
        let mut rx2 = Rx::new(mode, 1500.0);
        let sig = transmission(mode, 1500.0);
        let mut best: f64 = 0.0;
        for &s in &sig {
            rx2.process(s);
            best = best.max(rx2.metric());
        }
        assert!(best > 90.0, "signal {best}");
        assert!(worst < best - 30.0, "noise {worst}, signal {best}");
    }

    #[test]
    fn a_colour_picture_arrives_pixel_for_pixel() {
        for name in ["MFSK-16", "MFSK-32", "MFSK-64", "MFSK-128"] {
            let mode = by_name(name).unwrap();
            let (w, h) = (24, 10);
            // a smooth picture: the filters span a few pixels
            let rgb: Vec<u8> = (0..w * h)
                .flat_map(|i| {
                    let (x, y) = (i % w, i / w);
                    [
                        (40 + x * 7) as u8,
                        (30 + y * 20) as u8,
                        (200 - x * 3 - y * 4) as u8,
                    ]
                })
                .collect();
            let mut tx = Tx::new(mode, 1500.0);
            tx.silence(4000);
            tx.start();
            tx.text(b"Picture follows");
            tx.picture(w, h, true, &rgb);
            tx.text(b"After the picture.\n");
            tx.end();
            tx.silence(8000);
            let mut rx = Rx::new(mode, 1500.0);
            let (bytes, events) = decode(&mut rx, &tx.take());
            assert_eq!(
                events,
                [
                    Event::PictureStart {
                        width: w,
                        height: h,
                        color: true
                    },
                    Event::PictureDone
                ],
                "{name}"
            );
            assert!(contains(&bytes, b"Sending Pic:24x10C;"), "{name}");
            assert!(
                contains(&bytes, b"After the picture."),
                "{name}: {:?}",
                String::from_utf8_lossy(&bytes)
            );
            let got = &rx.picture().unwrap().rgb;
            // fldigi never writes the last pixel's last colour; the first
            // pixels of each line follow a jump from the line before
            let bad = (0..rgb.len() - 1)
                .filter(|&i| (6..w - 3).contains(&((i / 3) % w)))
                .filter(|&i| (i32::from(got[i]) - i32::from(rgb[i])).abs() > 8)
                .count();
            assert!(bad == 0, "{name}: {bad} pixels off");
        }
    }

    #[test]
    fn rsid_codes_of_both_tables_are_found_with_their_frequency() {
        use super::rsid::{Detector, SAMPLE_RATE};
        for (table, code, carrier) in [(1u8, 147u16, 1500.0), (2, 620, 1000.0), (1, 1, 2200.0)] {
            let mode = by_name("MFSK-11").unwrap(); // 11025 Hz
            assert_eq!(f64::from(mode.sample_rate), SAMPLE_RATE);
            let mut tx = Tx::new(mode, carrier);
            tx.silence(5000);
            tx.rsid(table, code);
            tx.silence(11025);
            let mut d = Detector::new();
            d.set_range(200.0, 3800.0);
            d.process(&tx.take());
            let found = d.take_found();
            assert_eq!(found.len(), 1, "{table}/{code}: {found:?}");
            assert_eq!((found[0].table, found[0].code), (table, code));
            assert!((found[0].freq_hz - carrier).abs() < 6.0, "{found:?}");
        }
    }
}
