//! Write a synthetic baseband WAV with DSC calls, to try the DSC VFO type
//! without a radio (spec 14). The IQ is centred on `center` Hz at `rate`;
//! the calls are keyed on `dsc` Hz (mark +85 Hz, space -85 Hz) over a
//! little noise.
//!
//! `cargo run --release --example dsc_baseband -- out.wav [rate] [center] [dsc]`
//! (defaults 250000, 8400000, 8414500). Open it with the File source, set
//! the VFO to the DSC frequency and its type to DSC.

use shellsdr::dsc::decoder::tx;
use shellsdr::dsc::enrich::samples;
use shellsdr::record::wav::{WavWriter, format_icmt};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "dsc_baseband.wav".into());
    let rate: f64 = args
        .get(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(250_000.0);
    let center: f64 = args
        .get(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8_400_000.0);
    let dsc: f64 = args
        .get(4)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8_414_500.0);
    let off = dsc - center;
    let mut seed = 7u64;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % 20_000) as f32 / 20_000.0 - 0.5
    };
    let silence = |secs: f64| vec![num_complex::Complex32::new(0.0, 0.0); (rate * secs) as usize];
    let mut iq = silence(2.0);
    for call in [
        &samples::SAFETY[..],
        &samples::DISTRESS[..],
        &samples::RELAY[..],
    ] {
        iq.extend(tx::fsk_iq(&tx::call_bits(call, 200), rate, off, 0.05));
        iq.extend(silence(3.0));
    }
    let f = std::fs::File::create(&out).expect("create");
    let mut w = WavWriter::new(std::io::BufWriter::new(f), 2, rate as u32).expect("wav");
    let mut buf = Vec::with_capacity(iq.len() * 2);
    for z in &iq {
        buf.push(z.re + 0.003 * noise());
        buf.push(z.im + 0.003 * noise());
    }
    w.write_f32(&buf).expect("write");
    let info = vec![
        ("format_version".to_string(), "2".to_string()),
        ("rec_type".to_string(), "baseband".to_string()),
        ("center_freq".to_string(), format!("{center:.0}")),
    ];
    w.finish(&format_icmt(&info)).expect("finish");
    println!(
        "{out}: {:.1} s at {rate} Hz, centre {center} Hz, DSC on {dsc} Hz",
        iq.len() as f64 / rate
    );
}
