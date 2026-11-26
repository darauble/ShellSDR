//! Throwaway harness: synthesize an AM signal + noise, run it through the real
//! VfoChain, and dump the 48 kHz audio to a WAV for spectral analysis. Lets us
//! verify the chain's output characteristics (no clip, no DC, no spurious
//! tone, deep HF rejection) objectively before a hardware re-listen.
//!
//! Run: cargo run --release --example am_chain_dump -- /tmp/am_out.wav

use num_complex::Complex32;
use shellsdr::dsp::chain::VfoChain;
use shellsdr::vfo::model::{AmAgcMode, Mode};
use std::io::Write;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/am_out.wav".into());
    let sr = 2_000_000.0_f64;
    let offset = 250_000.0_f64; // VFO 250 kHz off center
    let audio = 1_000.0_f64; // 1 kHz program tone
    let secs = 3;
    let n = (sr as usize) * secs;

    // AM: carrier at `offset`, 60% modulated by a 1 kHz tone, amplitude 0.3,
    // plus wideband complex noise and a strong adjacent carrier 400 kHz away.
    let mut car = 0.0f64;
    let mut adj = 0.0f64;
    let mut seed = 0x1234_5678u64;
    let mut rng = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((seed >> 40) as f32 / 16_777_216.0) - 0.5
    };
    let mut iq = Vec::with_capacity(n);
    for i in 0..n {
        let m = 1.0 + 0.6 * (std::f64::consts::TAU * audio * i as f64 / sr).cos() as f32;
        car += std::f64::consts::TAU * offset / sr;
        adj += std::f64::consts::TAU * (offset + 400_000.0) / sr;
        let sig = Complex32::new(0.3 * m * car.cos() as f32, 0.3 * m * car.sin() as f32);
        let adjacent = Complex32::new(0.4 * adj.cos() as f32, 0.4 * adj.sin() as f32);
        let noise = Complex32::new(rng() * 0.1, rng() * 0.1);
        iq.push(sig + adjacent + noise);
    }

    let mut chain = VfoChain::new(sr, offset, Mode::Am, 10_000.0, AmAgcMode::Audio);
    // Process in 1/60 s blocks like the real engine.
    let block = (sr as usize) / 60;
    let mut audio_out: Vec<f32> = Vec::new();
    let mut p = 0;
    while p < iq.len() {
        let end = (p + block).min(iq.len());
        audio_out.extend(chain.process(&iq[p..end]));
        p = end;
    }

    // Write 48 kHz mono int16 WAV.
    let data_len = (audio_out.len() * 2) as u32;
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    f.write_all(&1u16.to_le_bytes()).unwrap(); // mono
    f.write_all(&48_000u32.to_le_bytes()).unwrap();
    f.write_all(&(48_000u32 * 2).to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    for s in &audio_out {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&v.to_le_bytes()).unwrap();
    }
    eprintln!("wrote {} samples to {}", audio_out.len(), path);
}
