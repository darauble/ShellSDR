//! Print a WAV file's format and ICMT metadata (ShellSDR or SDR++ recordings).
//!
//! `cargo run --example wav_info -- FILE.wav [FILE2.wav ...]`

use shellsdr::record::wav::read_info;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: wav_info FILE.wav [...]");
        std::process::exit(2);
    }
    for f in files {
        match std::fs::File::open(&f).and_then(read_info) {
            Ok(i) => {
                let frame = (i.channels as u64 * i.bits as u64 / 8).max(1);
                let secs = i.data_bytes as f64 / frame as f64 / i.sample_rate.max(1) as f64;
                println!(
                    "{f}: {} ch, {} Hz, {} bit, {} data bytes ({secs:.2} s)",
                    i.channels, i.sample_rate, i.bits, i.data_bytes
                );
                for (k, v) in &i.comment {
                    println!("  {k}={v}");
                }
            }
            Err(e) => println!("{f}: {e}"),
        }
    }
}
