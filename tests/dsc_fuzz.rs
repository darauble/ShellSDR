//! SC-NFR-02: each reader survives random and mutated input without a
//! crash, a hang or unbounded memory. Each runs ≥ 100 000 iterations or
//! 60 s. Run explicitly (release recommended):
//! `cargo test --release --test dsc_fuzz -- --ignored`

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const ITERATIONS: u64 = 100_000;
const TIME: Duration = Duration::from_secs(60);
/// No single input may take longer than this (a hang).
const HANG: Duration = Duration::from_secs(10);

/// xorshift64*: deterministic, no crate needed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Runs `one` until the iteration count and the time are both used up (at
/// least one of them), under a watchdog; any panic fails the test.
fn fuzz(name: &str, seed: u64, mut one: impl FnMut(&mut Rng, u64)) {
    let progress = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let (p, d) = (progress.clone(), done.clone());
    let name_w = name.to_string();
    let watchdog = std::thread::spawn(move || {
        let mut last = (0, Instant::now());
        while !d.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            let now = p.load(Ordering::SeqCst);
            if now != last.0 {
                last = (now, Instant::now());
            } else if last.1.elapsed() > HANG {
                eprintln!("{name_w}: iteration {now} hangs");
                std::process::abort();
            }
        }
    });
    // The watchdog stops with the test, also when an assertion fails.
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let _stop = Stop(done.clone());
    let mut rng = Rng(seed);
    let t = Instant::now();
    let mut i = 0;
    while i < ITERATIONS && t.elapsed() < TIME {
        let r = catch_unwind(AssertUnwindSafe(|| one(&mut rng, i)));
        assert!(r.is_ok(), "{name}: panic at iteration {i} (seed {seed})");
        i += 1;
        progress.store(i, Ordering::SeqCst);
    }
    done.store(true, Ordering::SeqCst);
    let _ = watchdog.join();
    eprintln!(
        "{name}: {i} inputs in {:.1} s, no failure",
        t.elapsed().as_secs_f64()
    );
    assert!(i >= ITERATIONS || t.elapsed() >= TIME);
}

fn conformance(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/conformance")
        .join(p)
}

fn json(p: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(conformance(p)).unwrap()).unwrap()
}

#[test]
#[ignore]
fn sc_nfr_02_parser() {
    let cases: Vec<Vec<u8>> = json("parse/symbols.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            c["input"]["symbols"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect()
        })
        .collect();
    fuzz("parser", 1, |rng, i| {
        let symbols: Vec<u8> = if i % 2 == 0 {
            let n = rng.below(81) as usize;
            (0..n)
                .map(|_| {
                    if rng.below(16) == 0 {
                        255
                    } else {
                        rng.below(128) as u8
                    }
                })
                .collect()
        } else {
            let mut s = cases[rng.below(cases.len() as u64) as usize].clone();
            for _ in 0..1 + rng.below(5) {
                if !s.is_empty() {
                    let at = rng.below(s.len() as u64) as usize;
                    s[at] = if rng.below(8) == 0 {
                        255
                    } else {
                        rng.below(128) as u8
                    };
                }
            }
            s
        };
        let m = shellsdr::dsc::decoder::parser::parse(&symbols);
        assert!(m.length <= symbols.len());
        let r = shellsdr::dsc::record::Record::new(
            symbols,
            0,
            0,
            shellsdr::dsc::record::TimeSource::Wallclock,
        );
        let _ = shellsdr::dsc::presentation::search_terms(&r);
        let _ = shellsdr::dsc::presentation::list_row(&r);
        let _ = shellsdr::dsc::store::ndjson::to_line(&r);
    });
}

#[test]
#[ignore]
fn sc_nfr_02_day_file_reader() {
    let lines: Vec<String> = json("ndjson/read.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["input"]["line"].as_str().unwrap().to_string())
        .collect();
    fuzz("day-file reader", 2, |rng, i| {
        let bytes = if i % 2 == 0 {
            let n = rng.below(4097) as usize;
            rng.bytes(n)
        } else {
            let mut b = lines[rng.below(lines.len() as u64) as usize]
                .as_bytes()
                .to_vec();
            for _ in 0..1 + rng.below(8) {
                if !b.is_empty() {
                    let at = rng.below(b.len() as u64) as usize;
                    b[at] = rng.next() as u8;
                }
            }
            b
        };
        if let Ok(text) = std::str::from_utf8(&bytes)
            && let Some(r) = shellsdr::dsc::store::ndjson::from_line(text)
        {
            let _ = shellsdr::dsc::store::ndjson::to_line(&r);
        }
    });
}

#[test]
#[ignore]
fn sc_nfr_02_wav_reader() {
    let files: Vec<Vec<u8>> = std::fs::read_dir(conformance("audio"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
        .map(|e| std::fs::read(e.path()).unwrap())
        .collect();
    fuzz("wav reader", 3, |rng, i| {
        let mut f = files[rng.below(files.len() as u64) as usize].clone();
        match i % 4 {
            // Random header bytes changed.
            0 => {
                for _ in 0..1 + rng.below(6) {
                    let at = rng.below(64.min(f.len() as u64)) as usize;
                    f[at] = rng.next() as u8;
                }
            }
            // Truncated at a random point.
            1 => f.truncate(rng.below(f.len() as u64 + 1) as usize),
            // A chunk size set to 0, odd, or past the end of the file.
            _ => {
                let chunks: Vec<usize> = (12..f.len().saturating_sub(8))
                    .filter(|&p| matches!(&f[p..p + 4], b"fmt " | b"data" | b"LIST"))
                    .collect();
                if let Some(&p) = chunks.get(rng.below(chunks.len().max(1) as u64) as usize) {
                    let size: u32 = match rng.below(3) {
                        0 => 0,
                        1 => (rng.next() as u32) | 1,
                        _ => u32::MAX - rng.below(16) as u32,
                    };
                    f[p + 4..p + 8].copy_from_slice(&size.to_le_bytes());
                }
            }
        }
        // Read only a bounded prefix of the samples: the reader must not
        // allocate from header values.
        if let Ok(mut w) = shellsdr::dsc::wav::WavReader::new(&f[..]) {
            let mut buf = vec![0i16; 4096];
            for _ in 0..8 {
                if w.read(&mut buf).map(|n| n == 0).unwrap_or(true) {
                    break;
                }
            }
        }
    });
}

#[test]
#[ignore]
fn sc_nfr_02_settings_reader() {
    let keys = [
        "ndjson_dir",
        "save_dir",
        "default_tone",
        "has_location",
        "user_lat",
        "user_lon",
        "online_lookup",
        "lookup_endpoint",
        "lookup_apikey",
        "alert_bell",
        "channels",
    ];
    fuzz("settings reader", 4, |rng, _| {
        fn value(rng: &mut Rng, depth: u32) -> serde_json::Value {
            use serde_json::Value;
            match rng.below(if depth > 2 { 6 } else { 8 }) {
                0 => Value::Null,
                1 => Value::Bool(rng.below(2) == 0),
                2 => Value::from(rng.next() as i64),
                3 => Value::from(f64::from_bits(rng.next()).clamp(-1e300, 1e300)),
                4 => Value::from(rng.below(3) as i64),
                5 => {
                    let n = rng.below(12) as usize;
                    Value::from(String::from_utf8_lossy(&rng.bytes(n)).into_owned())
                }
                6 => Value::Array((0..rng.below(4)).map(|_| value(rng, depth + 1)).collect()),
                _ => {
                    let mut o = serde_json::Map::new();
                    for k in ["channel_label", "device", "tone", "x"] {
                        if rng.below(2) == 0 {
                            o.insert(k.into(), value(rng, depth + 1));
                        }
                    }
                    Value::Object(o)
                }
            }
        }
        let mut o = serde_json::Map::new();
        for k in keys {
            if rng.below(3) != 0 {
                o.insert(k.into(), value(rng, 0));
            }
        }
        let text = serde_json::Value::Object(o).to_string();
        let s = shellsdr::dsc::settings::from_json(&text);
        // What was read saves and reads back exactly.
        assert_eq!(
            shellsdr::dsc::settings::from_json(&shellsdr::dsc::settings::to_json(&s)),
            s,
            "{text}"
        );
        let cut = rng.below(text.len() as u64) as usize;
        let _ =
            shellsdr::dsc::settings::from_json(&String::from_utf8_lossy(&text.as_bytes()[..cut]));
    });
}
