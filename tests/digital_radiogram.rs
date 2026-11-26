//! A real reception through the Digital type's IQ path (spec 16): Shortwave
//! Radiogram 475 from WRMI on 15770 kHz (AM), a 30 minute minimized
//! baseband at 15625 Hz, with fldigi's text and pictures of the same
//! programme beside it. Both sidebands; MFSK-32, RSID to MFSK-64, 11
//! pictures, RSID back to MFSK-32.
//!
//! `cargo test --release --test digital_radiogram -- --ignored`
//! (the folder: `$RADIOGRAM_DIR`, default the user's archive).

use num_complex::Complex32;
use shellsdr::dsp::channelizer::Channelizer;
use shellsdr::mfsk::decoder::{Config, DEFAULT_SQUELCH, Decoder, Output, RsidNote};
use shellsdr::mfsk::frontend::Sideband;
use shellsdr::mfsk::modes::by_name;
use shellsdr::source::file::IqReader;
use std::path::PathBuf;

fn dir() -> PathBuf {
    std::env::var_os("RADIOGRAM_DIR").map_or_else(
        || PathBuf::from("/media/Multimedia/SWLING/Radiograms/Swradiogram 475"),
        PathBuf::from,
    )
}

fn read_png(p: &std::path::Path) -> (usize, usize, Vec<u8>) {
    let d = png::Decoder::new(std::fs::File::open(p).unwrap());
    let mut r = d.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    buf.truncate(info.buffer_size());
    (info.width as usize, info.height as usize, buf)
}

#[test]
#[ignore = "needs the Radiogram 475 recording"]
fn radiogram_475_decodes_like_fldigi() {
    let wav = dir().join("20261118-133000_baseband_15770000Hz-WRMI-Radiogram.wav");
    let mut r = IqReader::open(&wav).expect("the recording");
    let rate = f64::from(r.meta().sample_rate);
    let mut chan = Channelizer::new(rate, 8000.0);
    let mut d = Decoder::new(
        chan.out_rate(),
        Config {
            mode: by_name("MFSK-32").unwrap(),
            carrier: 1500.0,
            sideband: Sideband::Both,
            afc: true,
            rsid: true,
            squelch: DEFAULT_SQUELCH,
        },
    );
    let mut buf = vec![Complex32::new(0.0, 0.0); 4096];
    let mut t = 0.0;
    let mut text = String::new();
    let mut first_text_at = None;
    let mut switches = Vec::new();
    let mut pictures = Vec::new();
    loop {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        t += n as f64 / rate;
        for o in d.process(&chan.process(&buf[..n])) {
            match o {
                Output::Text(s) => {
                    first_text_at.get_or_insert(t);
                    text.push_str(&s);
                }
                Output::Rsid(RsidNote::Switch { after, .. }) => switches.push(after.0),
                Output::PictureDone(p) => pictures.push(p),
                _ => {}
            }
        }
    }
    assert_eq!(switches, ["MFSK-32", "MFSK-64", "MFSK-32"]);
    // the hiss before the programme (RSID at 1:42) prints nothing
    assert!(first_text_at.unwrap() > 100.0, "{first_text_at:?}");
    for line in [
        "Welcome to program 475 of Shortwave Radiogram.",
        " 2:52  MFSK32: Appeals Court Rules VOA director illegally fired",
        "https://www.yahoo.com/news/politics/articles/appeals-court-rules-kari-lake-223520643.html",
        "This is Shortwave Radiogram in MFSK64",
        "The Cybertruck was plagued from the get-go – from promises of its",
        "Our painting of the week is \"Lamp On, Harbour Settling\" (2025) by",
        "This is Shortwave Radiogram in MFSK32 ...",
        "I'm Kim Elliott. Please join us for the next Shortwave",
    ] {
        assert!(text.contains(line), "missing {line:?}");
    }
    // fldigi's pictures, in order
    let mut theirs: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    theirs.sort();
    assert_eq!(pictures.len(), theirs.len());
    for (ours, path) in pictures.iter().zip(&theirs) {
        let (w, h, rgb) = read_png(path);
        assert_eq!((ours.width, ours.height), (w, h), "{}", path.display());
        let diff = ours
            .rgb
            .iter()
            .zip(&rgb)
            .map(|(a, b)| u64::from((i32::from(*a) - i32::from(*b)).unsigned_abs()))
            .sum::<u64>()
            / rgb.len() as u64;
        let signed = ours
            .rgb
            .iter()
            .zip(&rgb)
            .map(|(a, b)| i64::from(*a) - i64::from(*b))
            .sum::<i64>()
            / rgb.len() as i64;
        let median = {
            let mut v: Vec<i32> = ours
                .rgb
                .iter()
                .zip(&rgb)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .collect();
            v.sort_unstable();
            v[v.len() / 2]
        };
        eprintln!(
            "{}: {w}x{h}, mean difference {diff}, signed {signed}, median {median}",
            path.display()
        );
        // the same picture: no brightness bias (a carrier error would shift
        // every pixel), differences only where noise streaks fall (ours are
        // fewer: the two sidebands' diversity)
        assert!(
            signed.abs() <= 5 && median <= 16,
            "{}: {signed} {median}",
            path.display()
        );
    }
}
