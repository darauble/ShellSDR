//! Measure the DSP thread's per-block work (decimation + FFT + VFO chain) on
//! synthetic IQ, as a fraction of real time. Above ~100% the live engine
//! cannot keep up and the driver drops IQ blocks (audible clicks).
//!
//! `cargo run --release --example dsp_load -- [sample_rate] [decimation] [mode] [bandwidth]`

use num_complex::Complex32;
use shellsdr::dsp::chain::VfoChain;
use shellsdr::dsp::fft::SpectrumAnalyzer;
use shellsdr::dsp::power_decimator::PowerDecimator;
use shellsdr::engine::{FFT_AVG_ALPHA, FFT_FPS, FFT_SIZE};
use shellsdr::vfo::model::{AmAgcMode, Mode};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let sr: f64 = a
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000.0);
    let dec: u32 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mode = match a.get(2).map(String::as_str).unwrap_or("AM") {
        "NFM" => Mode::Nfm,
        "WFM" => Mode::Wfm,
        "USB" => Mode::Usb,
        "LSB" => Mode::Lsb,
        "CW" => Mode::Cw,
        "DSB" => Mode::Dsb,
        _ => Mode::Am,
    };
    let bw: f64 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(10_000.0);
    let block = 65_536usize; // a typical SoapySDR MTU-sized read
    let secs = 10.0;
    let mut d = PowerDecimator::new(dec);
    let eff = sr / d.ratio() as f64;
    let mut fft = SpectrumAnalyzer::new(FFT_SIZE, eff, FFT_FPS, FFT_AVG_ALPHA);
    let mut chain = VfoChain::new_with_tone(eff, 10_000.0, mode, bw, AmAgcMode::default(), 800.0);
    let mut ph = 0.0f32;
    let iq: Vec<Complex32> = (0..block)
        .map(|_| {
            ph += 0.0123;
            Complex32::new(ph.cos() * 0.05 + 0.001, ph.sin() * 0.05)
        })
        .collect();
    let mut out = Vec::new();
    let n_blocks = (sr * secs / block as f64) as usize;
    let (mut t_dec, mut t_fft, mut t_chain) = (0.0, 0.0, 0.0);
    for _ in 0..n_blocks {
        let t = std::time::Instant::now();
        d.process(&iq, &mut out);
        let t1 = std::time::Instant::now();
        let _ = fft.feed(&out);
        let t2 = std::time::Instant::now();
        let _ = chain.process(&out);
        let t3 = std::time::Instant::now();
        t_dec += (t1 - t).as_secs_f64();
        t_fft += (t2 - t1).as_secs_f64();
        t_chain += (t3 - t2).as_secs_f64();
    }
    let p = |t: f64| 100.0 * t / secs;
    println!(
        "{sr} Hz /{dec} {mode:?} bw {bw}: decim {:.1}%  fft {:.1}%  chain {:.1}%  total {:.1}% of real time",
        p(t_dec),
        p(t_fft),
        p(t_chain),
        p(t_dec + t_fft + t_chain)
    );
}
