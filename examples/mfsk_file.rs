//! Decode MFSK from a baseband recording through the Digital type's IQ
//! path, printing the text with the decoder's metric (for tuning the
//! squelch) and writing the pictures as PPM.
//!
//! `cargo run --release --example mfsk_file -- <baseband.wav> [Upper|Lower|Both] [vfo offset Hz] [out dir]`
//!
//! `MFSK_DUMP=1` prints every character with its time and metric to stderr;
//! `COHERENT=1` decodes `Both` as one coherent sum instead of diversity.

use num_complex::Complex32;
use shellsdr::dsp::channelizer::Channelizer;
use shellsdr::dsp::nco::Nco;
use shellsdr::mfsk::frontend::{FrontEnd, Sideband};
use shellsdr::mfsk::modes::{self, by_name};
use shellsdr::mfsk::rsid::{self, Detector};
use shellsdr::mfsk::rx::{Event, Rx};
use shellsdr::source::file::IqReader;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::Path::new(&args[1]);
    let sideband = args
        .get(2)
        .and_then(|s| Sideband::from_name(s))
        .unwrap_or(Sideband::Upper);
    let offset: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let out = std::path::PathBuf::from(args.get(4).cloned().unwrap_or_else(|| ".".into()));
    let mut r = IqReader::open(path).expect("open");
    let rate = f64::from(r.meta().sample_rate);
    let mut nco = Nco::new(-offset, rate);
    let mut chan = Channelizer::new(rate, 8000.0);
    let mut mode = by_name(modes::DEFAULT).unwrap();
    let mut fe = FrontEnd::new(chan.out_rate(), sideband, mode.sample_rate);
    let coherent = std::env::var_os("COHERENT").is_some();
    let make = |mode, f, fe: &FrontEnd| {
        if fe.diversity() && !coherent {
            Rx::new_diversity(mode, f)
        } else {
            Rx::new(mode, f)
        }
    };
    let mut rx = make(mode, 1500.0, &fe);
    let mut det = Detector::new();
    det.set_range(200.0, 3800.0);
    det.set_fold(fe.rsid_fold());
    let mut buf = vec![Complex32::new(0.0, 0.0); 4096];
    let mut t = 0.0f64;
    let mut line = String::new();
    let mut line_min: f64 = 100.0;
    let mut line_t = 0.0;
    let mut pics = 0;
    let mut hist = [0u64; 11];
    let dump = std::env::var_os("MFSK_DUMP").is_some();
    loop {
        let n = r.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        let mut x = buf[..n].to_vec();
        nco.mix(&mut x);
        let y = chan.process(&x);
        let (m, rs) = fe.process(&y);
        det.process(&rs);
        for f in det.take_found() {
            let name = rsid::name(f.table, f.code).unwrap_or("?");
            println!(
                "\n[{:7.1}s] RSID {name} ({}/{}) at {:.1} Hz",
                t, f.table, f.code, f.freq_hz
            );
            if let Some(m2) = modes::by_rsid(f.table, f.code) {
                mode = m2;
                if fe.modem_rate() != mode.sample_rate {
                    fe = FrontEnd::new(chan.out_rate(), sideband, mode.sample_rate);
                }
                rx = make(mode, f.freq_hz, &fe);
            }
        }
        for s in m {
            if fe.diversity() && !coherent {
                rx.process2(s, s.conj());
            } else if fe.diversity() {
                rx.process(num_complex::Complex64::new(s.re, 0.0));
            } else {
                rx.process(s);
            }
            t += 1.0 / f64::from(mode.sample_rate);
            for e in rx.take_events() {
                match e {
                    Event::Byte { byte, metric } => {
                        hist[(metric / 10.0) as usize] += 1;
                        if dump {
                            eprintln!("{t:.2} {metric:.1} {byte}");
                        }
                        if line.is_empty() {
                            line_t = t;
                        }
                        line_min = line_min.min(metric);
                        if byte == b'\n' || byte == b'\r' {
                            if !line.is_empty() {
                                println!("[{line_t:7.1}s m{line_min:3.0}] {line}");
                            }
                            line.clear();
                            line_min = 100.0;
                        } else {
                            line.push(if byte.is_ascii_graphic() || byte == b' ' {
                                byte as char
                            } else {
                                '·'
                            });
                        }
                    }
                    Event::PictureStart {
                        width,
                        height,
                        color,
                    } => {
                        println!("\n[{t:7.1}s] picture {width}x{height} {color}");
                    }
                    Event::PictureDone => {
                        let p = rx.picture().unwrap();
                        pics += 1;
                        let f = out.join(format!("pic{pics:02}.ppm"));
                        let mut data = format!("P6\n{} {}\n255\n", p.width, p.height).into_bytes();
                        data.extend_from_slice(&p.rgb);
                        std::fs::write(&f, data).unwrap();
                        println!("[{t:7.1}s] picture done -> {}", f.display());
                    }
                }
            }
        }
    }
    println!("\nmetric histogram of characters (0-9, 10-19, ...): {hist:?}");
}
