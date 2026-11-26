//! A synthetic weather fax through the whole FAX path (spec 15): IQ ->
//! `FaxProcessor` (USB 1900 Hz below) -> 16-bit audio -> `FaxService`
//! (libfax.so.1): the picture is found and aligned on its phasing lines, a
//! false START inside the chart is ignored, auto slant finds the clock
//! error, and Save and recording write valid PNGs.
//!
//! Needs `libfax.so.1` (FAX >= 1.3.0) in `$SHELLSDR_LIB_DIR` or
//! `assets/lib/<arch>/`; without it the test says so and passes.

use std::path::PathBuf;
use std::sync::Arc;

use num_complex::Complex32;
use shellsdr::fax::ffi::{Config, FaxLib};
use shellsdr::fax::service::{FaxService, RecNaming, SlantState};
use shellsdr::vfo::types::fax::FaxProcessor;
use shellsdr::vfo::types::{DataItem, VfoProcessor};

fn library() -> Option<Arc<FaxLib>> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("SHELLSDR_LIB_DIR")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("assets/lib/{arch}")));
    let path = dirs
        .iter()
        .map(|d| d.join("libfax.so.1"))
        .find(|p| p.is_file())?;
    Some(Arc::new(FaxLib::open(&path).expect("libfax loads")))
}

/// IQ of a fax transmission at `rate`, the VFO at 0 Hz, with the recorder's
/// clock off by `ppm` (FAX's -s convention).
struct Synth {
    rate: f64,
    ppm: f64,
    n: u64,
    phase: f64,
    iq: Vec<Complex32>,
    columns: Vec<f64>,
}

impl Synth {
    fn new(rate: f64, ppm: f64) -> Self {
        // a fixed pseudo-random chart: 15% of the columns black
        let mut x: u32 = 12345;
        let columns = (0..4000)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                if (x >> 16) % 100 < 15 { 0.0 } else { 1.0 }
            })
            .collect();
        Self {
            rate,
            ppm,
            n: 0,
            phase: 0.0,
            iq: Vec::new(),
            columns,
        }
    }

    fn emit(&mut self, seconds: f64, level: impl Fn(&Self, f64, f64) -> f64) {
        let fs = self.rate * (1.0 + self.ppm * 1e-6);
        let end = self.n + (seconds * fs) as u64;
        while self.n < end {
            let t = self.n as f64 / fs;
            let line = t / 0.5;
            let v = level(self, line - line.floor(), t);
            // the audio tone 1500..2300 Hz is the RF 400 Hz below..above the VFO
            let f = 1500.0 + 800.0 * v - 1900.0;
            self.phase += 2.0 * std::f64::consts::PI * f / fs;
            self.iq
                .push(Complex32::new(self.phase.cos() as f32, self.phase.sin() as f32) * 0.2);
            self.n += 1;
        }
    }

    fn tone(&mut self, seconds: f64, hz: f64) {
        self.emit(
            seconds,
            |_, _, t| if (t * hz).fract() < 0.5 { 0.0 } else { 1.0 },
        );
    }

    fn phasing(&mut self, seconds: f64) {
        self.emit(seconds, |_, x, _| {
            if !(0.025..=0.975).contains(&x) {
                1.0
            } else {
                0.0
            }
        });
    }

    fn chart(&mut self, seconds: f64) {
        self.emit(seconds, |s, x, _| {
            if (x - 0.5).abs() < 0.004 {
                0.0
            } else {
                s.columns[(x * s.columns.len() as f64) as usize]
            }
        });
    }
}

fn png_height(path: &std::path::Path) -> u32 {
    let d = std::fs::read(path).unwrap();
    assert_eq!(&d[..8], b"\x89PNG\r\n\x1a\n", "{}", path.display());
    assert_eq!(&d[12..16], b"IHDR");
    assert_eq!(&d[d.len() - 8..d.len() - 4], b"IEND");
    u32::from_be_bytes(d[20..24].try_into().unwrap())
}

#[test]
fn a_synthetic_fax_is_aligned_straightened_saved_and_recorded() {
    let Some(lib) = library() else {
        eprintln!("SKIPPED: libfax.so.1 not found (set SHELLSDR_LIB_DIR)");
        return;
    };
    // a narrow IQ rate keeps the debug build's DSP quick
    let rate = 12_000.0;
    let ppm = -30.0;
    let mut s = Synth::new(rate, ppm);
    s.chart(10.0);
    s.tone(5.0, 300.0); // START
    s.phasing(30.0);
    s.chart(60.0);
    s.tone(5.0, 300.0); // a false START inside the chart
    s.chart(60.0);
    s.tone(5.0, 450.0); // STOP
    s.emit(5.0, |_, _, _| 0.0);

    let dir = std::env::temp_dir().join(format!("shellsdr-fax-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut svc = FaxService::new(
        lib,
        Config {
            lpm: 120,
            width: 1809,
            clock_ppm: 0.0,
        },
        true,
        64 << 20,
    );
    svc.start_recording(RecNaming {
        dir: dir.clone(),
        template: "$y$M$d-$h$m$s-$n-$f-$t".into(),
        vfo_name: "Test".into(),
        freq_hz: 3_855_000.0,
    });

    let mut p = FaxProcessor::new(rate, 0.0);
    for block in s.iq.chunks(1200) {
        p.process(block);
        for item in p.take_data() {
            let DataItem::FaxAudio { samples, rate } = item else {
                panic!("only audio");
            };
            svc.push(&samples, rate, 1_763_000_000.0);
        }
    }
    // let the last slant measurement finish
    for _ in 0..300 {
        svc.tick();
        if !svc.measuring() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let faxes: Vec<_> = svc
        .pictures()
        .into_iter()
        .filter(|p| p.started != 0)
        .collect();
    assert_eq!(
        faxes.len(),
        1,
        "one fax, the false START ignored: {faxes:?}"
    );
    let fax = faxes[0];
    assert_eq!(fax.aligned, 1, "aligned on the phasing lines");
    assert_eq!(fax.open, 0, "the STOP ended it");
    assert!((300..340).contains(&fax.lines), "{} lines", fax.lines);
    match svc.slant_state(&fax) {
        SlantState::Found { ppm: found, .. } => {
            assert!(
                (found - ppm).abs() < 1.0,
                "slant {found} ppm, expected {ppm}"
            )
        }
        other => panic!("slant not found: {other:?}"),
    }
    assert!((fax.ppm - ppm).abs() < 1.0, "drawn with {}", fax.ppm);
    assert!(svc.take_ppm_found().is_some(), "reported for the settings");

    // the marker column is in the middle on the first and the last lines
    for line in [80, fax.lines - 5] {
        let px = svc.render(fax.id, line, 1, 1809, 1).unwrap();
        let dark = (0..1809 - 14)
            .min_by_key(|&i| px[i..i + 14].iter().map(|&v| u32::from(v)).sum::<u32>())
            .unwrap()
            + 7;
        assert!(dark.abs_diff(904) <= 6, "line {line}: marker at {dark}");
    }

    // recording: the fax's file, straightened (rewritten after the slant)
    svc.stop_recording();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert!(!files.is_empty(), "a recording was written");
    let fax_file = files
        .iter()
        .find(|f| png_height(f) as i32 >= fax.lines - 2)
        .unwrap_or_else(|| panic!("no file with the whole fax: {files:?}"));
    assert!(
        fax_file.to_string_lossy().contains("3855000Hz-fax"),
        "{}",
        fax_file.display()
    );

    // save: shown picture to PNG, memory before its end released
    svc.select_by(-10);
    while svc.current().is_some_and(|c| c.id != fax.id) {
        svc.select_by(1);
    }
    let saved = svc.save(&dir, "saved", fax.lines).unwrap();
    assert_eq!(png_height(&saved) as i32, fax.lines - fax.first_line);
    let after = svc.current().unwrap();
    assert_eq!(after.first_line, fax.lines, "freed up to the saved end");
    let _ = std::fs::remove_dir_all(&dir);
}
