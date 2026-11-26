//! Stream health check on real hardware: read the device for N seconds while
//! doing the engine's DSP work, and report driver overflows, read errors, the
//! sample count against wall time, block sizes and the DSP load. Lost IQ
//! blocks are audible clicks; this tells them apart from RF impulse noise.
//!
//! `cargo run --release --example stream_check -- "ARGS" [rate] [freq] [secs] [no-dsp|OUT.wav]`
//!
//! With `OUT.wav` the AM (10 kHz) audio of the VFO at `freq` is written too.

use num_complex::Complex32;
use shellsdr::dsp::chain::VfoChain;
use shellsdr::dsp::fft::SpectrumAnalyzer;
use shellsdr::engine::{FFT_AVG_ALPHA, FFT_FPS, FFT_SIZE};
use shellsdr::source::soapy::SoapySource;
use shellsdr::vfo::model::{AmAgcMode, Mode};

fn main() {
    soapysdr::configure_logging();
    let a: Vec<String> = std::env::args().skip(1).collect();
    let args = a
        .first()
        .cloned()
        .unwrap_or_else(|| "driver=sdrplay".into());
    let sr: f64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(2_000_000.0);
    let freq: f64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(225_000.0);
    let secs: f64 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let dsp = a.get(4).map(String::as_str) != Some("no-dsp");
    let out = a.get(4).filter(|s| s.ends_with(".wav")).cloned();
    let mut audio: Vec<f32> = Vec::new();
    let mut src = SoapySource::open(&args).expect("open");
    src.set_sample_rate(sr).expect("rate");
    src.set_frequency(freq).expect("freq");
    let mut stream = src.start_stream().expect("stream");
    let mtu = stream.mtu().unwrap_or(65536).max(1024);
    let mut buf = vec![Complex32::new(0.0, 0.0); mtu];
    let mut fft = SpectrumAnalyzer::new(FFT_SIZE, sr, FFT_FPS, FFT_AVG_ALPHA);
    let mut chain =
        VfoChain::new_with_tone(sr, 0.0, Mode::Am, 10_000.0, AmAgcMode::default(), 800.0);
    let (mut samples, mut ovf, mut tmo, mut other, mut reads) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut min_n, mut max_n) = (usize::MAX, 0usize);
    let mut busy = 0.0f64;
    let mut max_gap = 0.0f64;
    let t0 = std::time::Instant::now();
    let mut last = t0;
    while t0.elapsed().as_secs_f64() < secs {
        match stream.read(&mut [&mut buf], 200_000) {
            Ok(n) => {
                let now = std::time::Instant::now();
                max_gap = max_gap.max((now - last).as_secs_f64());
                last = now;
                reads += 1;
                samples += n as u64;
                min_n = min_n.min(n);
                max_n = max_n.max(n);
                if dsp {
                    let _ = fft.feed(&buf[..n]);
                    let au = chain.process(&buf[..n]);
                    if out.is_some() {
                        audio.extend(au);
                    }
                    busy += now.elapsed().as_secs_f64();
                }
            }
            Err(e) if e.code == soapysdr::ErrorCode::Overflow => ovf += 1,
            Err(e) if e.code == soapysdr::ErrorCode::Timeout => tmo += 1,
            Err(e) => {
                other += 1;
                eprintln!("read error: {e}");
            }
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    let _ = stream.deactivate(None);
    println!("args={args} rate={sr} freq={freq} dsp={dsp} mtu={mtu}");
    println!(
        "wall {wall:.2} s, samples {samples} = {:.3} s of IQ ({:.2}% of wall), measured rate {:.0} Hz",
        samples as f64 / sr,
        100.0 * samples as f64 / sr / wall,
        samples as f64 / wall
    );
    println!(
        "reads {reads}, block {min_n}..{max_n}, max gap between reads {:.1} ms",
        max_gap * 1e3
    );
    if let Some(p) = &out {
        let f = std::fs::File::create(p).expect("create");
        let mut w = shellsdr::record::wav::WavWriter::new(std::io::BufWriter::new(f), 1, 48_000)
            .expect("wav");
        w.write_f32(&audio).expect("write");
        w.finish("source=stream_check").expect("finish");
        println!("wrote {p} ({} samples)", audio.len());
    }
    println!(
        "overflows {ovf}, timeouts {tmo}, other errors {other}, DSP load {:.1}%",
        100.0 * busy / wall
    );
}
