//! Offline demod of a baseband (IQ) WAV through the real `VfoChain`.
//!
//! Reads an SDR++-style baseband recording (stereo I/Q, 16-bit PCM or 32-bit
//! float), demodulates it exactly like the live engine, and writes 48 kHz mono
//! 16-bit audio. This gives a bit-identical input for A/B against SDR++
//! playing the same file, removing propagation/timing/hardware differences.
//!
//! Run:
//!   cargo run --release --example iq_wav_demod -- IN.wav OUT.wav \
//!       [offset_hz=0] [mode=usb] [bandwidth_hz=mode default] [volume=1.0] [block=sr/200]
//!
//! `offset_hz` is the tuned (dial) frequency minus the file's center frequency.
//! `block` is the IQ block size per `process` call; it defaults to SDR++'s
//! source block (sample rate / 200), which matters because the AGC's
//! anti-clip look-ahead only scans to the end of the current block.

use std::io::Write;

use num_complex::Complex32;
use shellsdr::dsp::chain::{AUDIO_RATE, VfoChain};
use shellsdr::engine::audio::output_sample;
use shellsdr::vfo::model::{AmAgcMode, Mode};

/// Minimal WAV reader for interleaved stereo IQ: returns (sample_rate, iq).
fn read_iq_wav(path: &str) -> (f64, Vec<Complex32>) {
    let d = std::fs::read(path).expect("read input wav");
    assert!(
        &d[0..4] == b"RIFF" && &d[8..12] == b"WAVE",
        "not a RIFF/WAVE file"
    );
    let (mut fmt_tag, mut ch, mut sr, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut i = 12;
    while i + 8 <= d.len() {
        let id = &d[i..i + 4];
        let sz = u32::from_le_bytes(d[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &d[i + 8..(i + 8 + sz).min(d.len())];
        if id == b"fmt " {
            fmt_tag = u16::from_le_bytes([body[0], body[1]]);
            ch = u16::from_le_bytes([body[2], body[3]]);
            sr = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            assert_eq!(ch, 2, "expected stereo I/Q");
            let iq: Vec<Complex32> = match (fmt_tag, bits) {
                (1, 16) => body
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| {
                        let re = i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0;
                        let im = i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0;
                        Complex32::new(re, im)
                    })
                    .collect(),
                (3, 32) => body
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| {
                        let re = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                        let im = f32::from_le_bytes([c[4], c[5], c[6], c[7]]);
                        Complex32::new(re, im)
                    })
                    .collect(),
                other => panic!("unsupported format/bits {other:?}"),
            };
            return (sr as f64, iq);
        }
        i += 8 + sz + (sz & 1);
    }
    panic!("no data chunk");
}

fn parse_mode(s: &str) -> Mode {
    match s.to_ascii_lowercase().as_str() {
        "nfm" => Mode::Nfm,
        "wfm" => Mode::Wfm,
        "am" => Mode::Am,
        "dsb" => Mode::Dsb,
        "usb" => Mode::Usb,
        "lsb" => Mode::Lsb,
        "cw" => Mode::Cw,
        other => panic!("unknown mode {other}"),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!(
            "usage: iq_wav_demod IN.wav OUT.wav [offset_hz] [mode] [bandwidth_hz] [volume] [block]"
        );
        std::process::exit(2);
    }
    let offset: f64 = a.get(3).map(|s| s.parse().unwrap()).unwrap_or(0.0);
    let mode = a.get(4).map(|s| parse_mode(s)).unwrap_or(Mode::Usb);
    let bw: f64 = a
        .get(5)
        .map(|s| s.parse().unwrap())
        .unwrap_or_else(|| mode.default_bandwidth());
    let volume: f32 = a.get(6).map(|s| s.parse().unwrap()).unwrap_or(1.0);

    let (sr, iq) = read_iq_wav(&a[1]);
    let block: usize = a
        .get(7)
        .map(|s| s.parse().unwrap())
        .unwrap_or((sr / 200.0) as usize);
    eprintln!(
        "{}: {} Hz, {:.1} s; {:?} bw {bw} Hz offset {offset} Hz, block {block}",
        a[1],
        sr,
        iq.len() as f64 / sr,
        mode
    );

    let mut chain = VfoChain::new(sr, offset, mode, bw, AmAgcMode::Audio);
    let mut audio: Vec<f32> = Vec::new();
    for b in iq.chunks(block) {
        audio.extend(chain.process(b));
    }

    let over = audio.iter().filter(|v| v.abs() > 1.0).count();
    let peak = audio.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    eprintln!(
        "audio {:.1} s, peak {peak:.3}, {:.2}% above full scale before volume",
        audio.len() as f64 / AUDIO_RATE,
        100.0 * over as f64 / audio.len().max(1) as f64
    );

    let data_len = (audio.len() * 2) as u32;
    let mut f = std::fs::File::create(&a[2]).expect("create output");
    let mut hdr = Vec::with_capacity(44);
    hdr.extend_from_slice(b"RIFF");
    hdr.extend_from_slice(&(36 + data_len).to_le_bytes());
    hdr.extend_from_slice(b"WAVEfmt ");
    hdr.extend_from_slice(&16u32.to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes());
    hdr.extend_from_slice(&(AUDIO_RATE as u32).to_le_bytes());
    hdr.extend_from_slice(&(AUDIO_RATE as u32 * 2).to_le_bytes());
    hdr.extend_from_slice(&2u16.to_le_bytes());
    hdr.extend_from_slice(&16u16.to_le_bytes());
    hdr.extend_from_slice(b"data");
    hdr.extend_from_slice(&data_len.to_le_bytes());
    f.write_all(&hdr).unwrap();
    let mut pcm = Vec::with_capacity(audio.len() * 2);
    for &s in &audio {
        pcm.extend_from_slice(&((output_sample(s, volume) * 32767.0) as i16).to_le_bytes());
    }
    f.write_all(&pcm).unwrap();
    eprintln!("wrote {}", a[2]);
}
