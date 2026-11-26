//! Offline SSB chain harness: synthesizes a USB "speech-band" signal (tones
//! 300..2700 Hz above the suppressed carrier) plus an equally strong
//! interferer in the opposite (lower) sideband and wideband noise, runs it
//! through the real `VfoChain` in USB mode, prints per-tone levels and the
//! image rejection, and writes a 48 kHz WAV for listening / PSD analysis.
//!
//! Run: `cargo run --release --example ssb_chain_dump [out.wav]`

use num_complex::Complex32;
use shellsdr::dsp::chain::{AUDIO_RATE, VfoChain};
use shellsdr::vfo::model::{AmAgcMode, Mode};

/// Energy of `x` at `freq_hz` (single-bin correlation, normalized).
fn energy_at(x: &[f32], freq_hz: f64, sr: f64) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &v) in x.iter().enumerate() {
        let p = std::f64::consts::TAU * freq_hz * i as f64 / sr;
        re += v as f64 * p.cos();
        im += v as f64 * p.sin();
    }
    (re * re + im * im).sqrt() / x.len().max(1) as f64
}

fn main() {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/ssb_chain_dump.wav".to_string());

    let sr = 2_000_000.0; // RSPduo-like capture rate
    let carrier = 225_000.0; // dial frequency offset from source center
    let bw = 2_800.0;
    let secs = 3.0;
    let n = (sr * secs) as usize;

    // USB "speech" tones (wanted) and an LSB interferer (must be rejected).
    let wanted = [300.0, 700.0, 1_200.0, 1_800.0, 2_400.0, 2_700.0];
    let image = 1_100.0; // carrier - 1100 Hz, lower sideband

    let mut seed = 0x1234_5678u64;
    let mut iq = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / sr;
        let mut s = Complex32::new(0.0, 0.0);
        for &f in &wanted {
            let p = std::f64::consts::TAU * (carrier + f) * t;
            s += Complex32::new(0.05 * p.cos() as f32, 0.05 * p.sin() as f32);
        }
        let p = std::f64::consts::TAU * (carrier - image) * t;
        s += Complex32::new(0.05 * p.cos() as f32, 0.05 * p.sin() as f32);
        // Wideband noise.
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let nr = ((seed >> 40) as f32 / 16_777_216.0 - 0.5) * 0.02;
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let ni = ((seed >> 40) as f32 / 16_777_216.0 - 0.5) * 0.02;
        iq.push(s + Complex32::new(nr, ni));
    }

    let mut chain = VfoChain::new(sr, carrier, Mode::Usb, bw, AmAgcMode::Audio);
    let mut audio = Vec::new();
    for block in iq.chunks(65_536) {
        audio.extend(chain.process(block));
    }
    // Skip the first 0.5 s (AGC + filter settling).
    let settle = (AUDIO_RATE * 0.5) as usize;
    let a = &audio[settle.min(audio.len())..];

    println!("USB chain, bw {bw} Hz, carrier offset {carrier} Hz");
    let mut levels = Vec::new();
    for &f in &wanted {
        let e = energy_at(a, f, AUDIO_RATE);
        levels.push(e);
        println!("  wanted {f:6.0} Hz: {e:.4}");
    }
    let mean_wanted = levels.iter().sum::<f64>() / levels.len() as f64;
    let img = energy_at(a, image, AUDIO_RATE);
    println!("  image  {image:6.0} Hz (LSB interferer, would alias here): {img:.5}");
    println!(
        "  image rejection: {:.1} dB",
        20.0 * (mean_wanted / img.max(1e-12)).log10()
    );
    let flat_db = 20.0
        * (levels.iter().cloned().fold(f64::MIN, f64::max)
            / levels.iter().cloned().fold(f64::MAX, f64::min))
        .log10();
    println!("  passband ripple 300..2700 Hz: {flat_db:.1} dB");
    let rms = (a.iter().map(|v| (v * v) as f64).sum::<f64>() / a.len() as f64).sqrt();
    let over = a.iter().filter(|v| v.abs() > 1.0).count() as f64 / a.len() as f64 * 100.0;
    let peak = a.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    println!("  rms {rms:.3}, peak {peak:.3}, above full scale at volume 1.0: {over:.2}%");
    println!(
        "  volume <= {:.2} avoids any clipping (limiting happens after volume)",
        1.0 / peak.max(1e-6)
    );

    // Write 16-bit mono WAV.
    let mut wav = Vec::new();
    let data_len = (audio.len() * 2) as u32;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&(AUDIO_RATE as u32).to_le_bytes());
    wav.extend_from_slice(&((AUDIO_RATE as u32) * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &s in &audio {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        wav.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&out_path, wav).expect("write wav");
    println!("wrote {out_path}");
}
