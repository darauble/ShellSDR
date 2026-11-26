//! Spec 14.2: the conformance recordings (seed `audio-decode`, audio around
//! a centre tone) decoded through the DSC VFO's IQ path. Each WAV is turned
//! into complex IQ centred on its DSC signal (mixed down by the centre
//! tone) and fed to `DscProcessor` in SDR-sized blocks: the same calls
//! must come out as from the audio path.

use num_complex::Complex32;
use serde_json::Value;
use shellsdr::dsc::wav::WavReader;
use shellsdr::vfo::types::dsc::DscProcessor;
use shellsdr::vfo::types::{DataItem, VfoProcessor};
use std::path::Path;

fn root() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/dsc/conformance"
    ))
}

/// The first channel of `file` as IQ around `centre` Hz, and its rate.
fn wav_as_iq(file: &str, centre: f64) -> (Vec<Complex32>, f64) {
    let mut w = WavReader::open(&root().join(file)).expect("wav");
    let rate = f64::from(w.rate);
    let mut audio = Vec::new();
    let mut buf = vec![0i16; 8192];
    loop {
        let n = w.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        audio.extend_from_slice(&buf[..n]);
    }
    let iq = audio
        .iter()
        .enumerate()
        .map(|(n, &x)| {
            let p = -std::f64::consts::TAU * centre * n as f64 / rate;
            let v = f64::from(x) / 32768.0;
            Complex32::new((v * p.cos()) as f32, (v * p.sin()) as f32)
        })
        .collect();
    (iq, rate)
}

fn decode(iq: &[Complex32], rate: f64) -> Vec<Vec<u8>> {
    let mut p = DscProcessor::new(rate, 0.0);
    let mut out = Vec::new();
    let block = ((rate / 200.0) as usize).max(1);
    for b in iq.chunks(block) {
        p.process(b);
        out.extend(p.take_data());
    }
    p.finish();
    out.extend(p.take_data());
    out.into_iter()
        .filter_map(|i| match i {
            DataItem::DscCall { symbols, .. } => Some(symbols),
            _ => None,
        })
        .collect()
}

#[test]
fn every_audio_decode_vector_gives_the_same_calls_through_iq() {
    let suite: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("audio/decode.json")).unwrap())
            .unwrap();
    let mut checked = 0;
    for case in suite["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let input = &case["input"];
        let (iq, rate) = wav_as_iq(
            input["file"].as_str().unwrap(),
            input["center_tone_hz"].as_f64().unwrap(),
        );
        let got = decode(&iq, rate);
        let want: Vec<Vec<u8>> = case["expected"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                m["symbols"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_u64().unwrap() as u8)
                    .collect()
            })
            .collect();
        assert_eq!(got, want, "{id}");
        checked += 1;
    }
    assert_eq!(checked, 15);
}
