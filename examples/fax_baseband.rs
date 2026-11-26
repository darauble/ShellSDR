//! Turn a weather fax audio recording (mono 16-bit WAV, USB with the fax
//! carrier at 1900 Hz, e.g. from SDR++ or ShellSDR's native recording) into
//! a baseband WAV, to try the FAX VFO type without a radio (spec 15).
//!
//! `cargo run --release --example fax_baseband -- audio.wav out.wav [fax_hz] [minutes]`
//! (defaults 3855000 and the whole recording). The baseband is 12 kHz IQ
//! centred 2 kHz below the fax. Open it with the File source, set the VFO
//! to `fax_hz` and its type to FAX.

use num_complex::Complex32;
use shellsdr::record::wav::{WavWriter, format_icmt};

/// The samples and the rate of a mono 16-bit PCM WAV.
fn read_mono(path: &str) -> (Vec<i16>, u32) {
    let d = std::fs::read(path).expect("read the audio WAV");
    assert!(&d[..4] == b"RIFF" && &d[8..12] == b"WAVE", "not a WAV");
    let (mut pos, mut rate, mut channels) = (12, 0u32, 0u16);
    while pos + 8 <= d.len() {
        let id = &d[pos..pos + 4];
        let len = u32::from_le_bytes(d[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &d[pos + 8..(pos + 8 + len).min(d.len())];
        if id == b"fmt " {
            channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
            assert!(channels == 1 && bits == 16, "needs mono 16-bit PCM");
        } else if id == b"data" {
            assert!(channels == 1, "fmt chunk first");
            let s = body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect();
            return (s, rate);
        }
        pos += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(input), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: fax_baseband audio.wav out.wav [fax_hz] [minutes]");
        std::process::exit(2);
    };
    let fax: f64 = args
        .get(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(3_855_000.0);
    let minutes: Option<f64> = args.get(4).and_then(|a| a.parse().ok());
    let (audio, fs) = read_mono(input);
    let fs = f64::from(fs);
    let audio = match minutes {
        Some(m) => &audio[..((m * 60.0 * fs) as usize).min(audio.len())],
        None => &audio[..],
    };

    // the fax carrier (1900 Hz) to 0 Hz, low pass +-1300 Hz, down to ~12 kHz
    let decim = ((fs / 12_000.0).round() as usize).max(1);
    let out_rate = fs / decim as f64;
    let taps = 201;
    let fc = 1300.0 / fs;
    let h: Vec<f32> = (0..taps)
        .map(|i| {
            let n = i as f64 - (taps - 1) as f64 / 2.0;
            let sinc = if n == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * n).sin() / (std::f64::consts::PI * n)
            };
            let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (taps - 1) as f64).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (taps - 1) as f64).cos();
            (sinc * w * 2.0) as f32
        })
        .collect();
    let mixed: Vec<Complex32> = audio
        .iter()
        .enumerate()
        .map(|(n, &x)| {
            let ph = -2.0 * std::f64::consts::PI * 1900.0 * n as f64 / fs;
            Complex32::new(ph.cos() as f32, ph.sin() as f32) * (f32::from(x) / 32768.0)
        })
        .collect();
    // the fax 2 kHz above the IQ centre
    let off = 2000.0;
    let center = fax - off;
    let mut iq = Vec::with_capacity(mixed.len() / decim * 2);
    for (k, n) in (taps..mixed.len()).step_by(decim).enumerate() {
        let w = &mixed[n - taps..n];
        let z: Complex32 = w.iter().zip(h.iter().rev()).map(|(a, b)| a * b).sum();
        let ph = 2.0 * std::f64::consts::PI * off * k as f64 / out_rate;
        let z = z * Complex32::new(ph.cos() as f32, ph.sin() as f32) * 0.5;
        iq.push(z.re);
        iq.push(z.im);
    }
    let f = std::fs::File::create(out).expect("create");
    let mut w = WavWriter::new(std::io::BufWriter::new(f), 2, out_rate as u32).expect("wav");
    w.write_f32(&iq).expect("write");
    let info = vec![
        ("format_version".to_string(), "2".to_string()),
        ("rec_type".to_string(), "baseband".to_string()),
        ("center_freq".to_string(), format!("{center:.0}")),
    ];
    w.finish(&format_icmt(&info)).expect("finish");
    println!(
        "{out}: {:.1} s at {out_rate} Hz, centre {center} Hz, the fax on {fax} Hz",
        iq.len() as f64 / 2.0 / out_rate
    );
}
