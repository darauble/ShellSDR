//! The decoder: demodulator, receiver and parser (seed spec/04-algorithms).

pub mod demod;
pub mod parser;
pub mod receiver;
pub mod tx;

use super::wav as capture_wav;
use crate::dsc::message::Symbol;
use crate::dsc::record::TimeSource;
use crate::dsc::time::{self, TimeZone};
use std::path::Path;

/// A message as a decoder delivers it.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub symbols: Vec<Symbol>,
    /// Reception instant, UTC epoch seconds.
    pub utc: i64,
    /// Local offset at that instant, seconds.
    pub offset: i32,
    pub time_source: TimeSource,
}

/// Decodes a whole WAV file. The start time comes from the file name in
/// `tz`, else from the file's modification time (TIM-02).
pub fn decode_file(
    path: &Path,
    centre: f64,
    reverse: bool,
    tz: &TimeZone,
) -> Result<Vec<Decoded>, String> {
    let mut wav = capture_wav::WavReader::open(path)?;
    let (base, tsrc) = file_base_time(path, tz);
    let mut decoder = Decoder::new(f64::from(wav.rate), centre, reverse);
    let mut found = Vec::new();
    let mut buf = vec![0i16; 16384];
    loop {
        let n = wav.read(&mut buf)?;
        if n == 0 {
            break;
        }
        decoder.feed(&buf[..n], &mut |c| found.push(c));
    }
    decoder.finish(&mut |c| found.push(c));
    let rate = f64::from(wav.rate);
    Ok(found
        .into_iter()
        .map(|c| {
            let utc = base + (c.start_sample as f64 / rate) as i64;
            Decoded {
                symbols: c.symbols,
                utc,
                offset: time::offset_at(tz, utc),
                time_source: tsrc,
            }
        })
        .collect())
}

/// A recording's start: from its file name, else its modification time.
pub fn file_base_time(path: &Path, tz: &TimeZone) -> (i64, TimeSource) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if let Some((utc, _)) = time::base_time_from_name(name, tz) {
        return (utc, TimeSource::Filename);
    }
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    (mtime, TimeSource::Mtime)
}

/// Demodulator and receiver of one channel.
pub struct Decoder {
    demod: demod::Demodulator,
    receiver: receiver::Receiver,
}

impl Decoder {
    pub fn new(sample_rate: f64, centre: f64, reverse: bool) -> Decoder {
        Decoder {
            demod: demod::Demodulator::new(sample_rate, centre, reverse),
            receiver: receiver::Receiver::new(),
        }
    }

    /// For complex IQ centred on the DSC frequency (ShellSDR, spec 14.2), at
    /// `sample_rate` >= [`demod::MIN_IQ_RATE`].
    pub fn new_iq(sample_rate: f64) -> Decoder {
        Decoder {
            demod: demod::Demodulator::new_iq(sample_rate),
            receiver: receiver::Receiver::new(),
        }
    }

    /// Feeds IQ (scaled like 16-bit audio); calls `out` for every call.
    pub fn feed_iq(
        &mut self,
        samples: &[num_complex::Complex<f32>],
        out: &mut dyn FnMut(receiver::Call),
    ) {
        let rx = &mut self.receiver;
        self.demod.process_iq(samples, &mut |soft, at| {
            if let Some(c) = rx.push(soft, at) {
                out(c);
            }
        });
    }

    /// Feeds samples; calls `out` for every call found.
    pub fn feed(&mut self, samples: &[i16], out: &mut dyn FnMut(receiver::Call)) {
        let rx = &mut self.receiver;
        self.demod.process(samples, &mut |soft, at| {
            if let Some(c) = rx.push(soft, at) {
                out(c);
            }
        });
    }

    /// End of input.
    pub fn finish(&mut self, out: &mut dyn FnMut(receiver::Call)) {
        if let Some(c) = self.receiver.flush() {
            out(c);
        }
    }

    /// Input level 0.0 … 1.0.
    pub fn level(&self) -> f64 {
        self.demod.level()
    }

    /// A call is being read (DECODING).
    pub fn is_decoding(&self) -> bool {
        self.receiver.is_reading()
    }
}
