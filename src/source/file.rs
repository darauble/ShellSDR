//! Baseband (IQ) WAV file source (spec 7.2).
//!
//! Reads the I/Q stereo WAV files that ShellSDR and SDR++ record: 16-bit PCM
//! (SDR++'s default, scaled by 1/32768 like its `volk_16i_s32f_convert_32f`
//! with 32768) or 32-bit IEEE float (SDR++'s "Float32 mode"). The center
//! frequency comes from our ICMT `center_freq`, else from the file name the
//! way SDR++'s file source does it (the first `[0-9]+Hz`, e.g.
//! `20261104T100029_baseband_13264000Hz.wav`).
//!
//! `IqReader` is a plain seekable reader; the real-time pacing lives in
//! `Pacer` (pure, tested) and is applied by the engine's DSP thread.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use num_complex::Complex32;

use crate::record::wav::{WavInfo, read_info};

/// Sample encoding of a playable baseband file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    I16,
    F32,
}

impl SampleFormat {
    /// Bytes per I/Q frame (two channels).
    pub fn frame_bytes(self) -> u64 {
        match self {
            SampleFormat::I16 => 4,
            SampleFormat::F32 => 8,
        }
    }
}

/// What the Source module shows about a loaded file, and what the engine
/// needs to play it.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMeta {
    pub sample_rate: u32,
    /// Number of I/Q frames in the file.
    pub frames: u64,
    /// Center frequency (Hz) from ICMT or the file name; None if unknown.
    pub center_hz: Option<u64>,
    pub format: SampleFormat,
    /// A minimized baseband (spec 13.6) remembers how its VFO was tuned, so
    /// playback puts the VFO back on it.
    pub tuning: Option<FileTuning>,
}

/// The VFO tuning a minimized baseband was recorded with (ICMT
/// `vfo_type`, `vfo_settings`, `vfo_freq`).
#[derive(Debug, Clone, PartialEq)]
pub struct FileTuning {
    pub vfo_type: String,
    pub settings: crate::vfo::types::Settings,
    pub vfo_hz: u64,
}

/// The tuning of a minimized baseband from its ICMT (None for other files).
pub fn tuning_from(info: &WavInfo) -> Option<FileTuning> {
    if info.get("minimized").map(str::trim) != Some("1") {
        return None;
    }
    let settings = match serde_json::from_str(info.get("vfo_settings")?) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => return None,
    };
    Some(FileTuning {
        vfo_type: info.get("vfo_type")?.trim().to_string(),
        settings,
        vfo_hz: info.get("vfo_freq")?.trim().parse().ok()?,
    })
}

impl FileMeta {
    /// Duration in seconds.
    pub fn duration_s(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }
}

/// The center frequency in a file name (HFDL seed `04-support.md` §4.1):
/// the first number directly followed by a unit, `digits[.digits]` then
/// `Hz`, `kHz` or `MHz` in any case (SDR++'s `…_8902500Hz.wav`,
/// `8902.5kHz`, `8.9025MHz`), in Hz.
pub fn center_from_name(name: &str) -> Option<u64> {
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && (b[i - 1].is_ascii_digit() || b[i - 1] == b'.')) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        let rest = name[i..].to_ascii_lowercase();
        let mult = if rest.starts_with("mhz") {
            1e6
        } else if rest.starts_with("khz") {
            1e3
        } else if rest.starts_with("hz") {
            1.0
        } else {
            continue;
        };
        let v: f64 = name[start..i].parse().ok()?;
        return Some((v * mult).round() as u64);
    }
    None
}

/// Validate a parsed WAV as a playable I/Q file and describe it. `name` is
/// the file name, the fallback for the center frequency.
pub fn describe(info: &WavInfo, name: &str) -> Result<FileMeta, String> {
    if info.channels != 2 {
        return Err(format!(
            "not an I/Q file: {} channel(s), need 2",
            info.channels
        ));
    }
    if info.sample_rate == 0 {
        return Err("sample rate is 0".to_string());
    }
    // 0xFFFE (extensible) carries the real type in a sub-format GUID; the
    // bit depth tells the two encodings we support apart.
    let format = match (info.format, info.bits) {
        (1 | 0xFFFE, 16) => SampleFormat::I16,
        (3 | 0xFFFE, 32) => SampleFormat::F32,
        (f, b) => return Err(format!("unsupported sample format {f}/{b}-bit")),
    };
    let center_hz = info
        .get("center_freq")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v >= 0.0)
        .map(|v| v.round() as u64)
        .or_else(|| center_from_name(name));
    Ok(FileMeta {
        sample_rate: info.sample_rate,
        frames: info.data_bytes / format.frame_bytes(),
        center_hz,
        format,
        tuning: tuning_from(info),
    })
}

/// Read the header of the file at `path` and describe it.
pub fn probe(path: &Path) -> Result<FileMeta, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let info = read_info(io::BufReader::new(f)).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    describe(&info, &name)
}

/// A seekable I/Q frame reader over a WAV's data chunk.
pub struct IqReader<R: Read + Seek> {
    r: R,
    data_offset: u64,
    meta: FileMeta,
    pos: u64,
    raw: Vec<u8>,
}

impl IqReader<io::BufReader<std::fs::File>> {
    /// Open the file at `path` for playback.
    pub fn open(path: &Path) -> Result<Self, String> {
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut r = io::BufReader::with_capacity(256 * 1024, f);
        let info = read_info(&mut r).map_err(|e| e.to_string())?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let meta = describe(&info, &name)?;
        IqReader::new(r, &info, meta).map_err(|e| e.to_string())
    }
}

impl<R: Read + Seek> IqReader<R> {
    /// Wrap `r` (positioned anywhere) for the file `info` describes.
    pub fn new(mut r: R, info: &WavInfo, meta: FileMeta) -> io::Result<Self> {
        r.seek(SeekFrom::Start(info.data_offset))?;
        Ok(Self {
            r,
            data_offset: info.data_offset,
            meta,
            pos: 0,
            raw: Vec::new(),
        })
    }

    pub fn meta(&self) -> &FileMeta {
        &self.meta
    }

    /// Current position in frames.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Jump to `frame` (clamped to the end of the data).
    pub fn seek(&mut self, frame: u64) -> io::Result<()> {
        let frame = frame.min(self.meta.frames);
        self.r.seek(SeekFrom::Start(
            self.data_offset + frame * self.meta.format.frame_bytes(),
        ))?;
        self.pos = frame;
        Ok(())
    }

    /// Fill `out` with the next frames; returns how many were read (0 at the
    /// end of the data).
    pub fn read(&mut self, out: &mut [Complex32]) -> io::Result<usize> {
        let fb = self.meta.format.frame_bytes();
        let n = (out.len() as u64).min(self.meta.frames - self.pos) as usize;
        if n == 0 {
            return Ok(0);
        }
        self.raw.resize(n * fb as usize, 0);
        // A file truncated after its header was written: play what exists.
        let mut got = 0;
        while got < self.raw.len() {
            match self.r.read(&mut self.raw[got..])? {
                0 => break,
                k => got += k,
            }
        }
        let n = got / fb as usize;
        match self.meta.format {
            SampleFormat::I16 => {
                for (o, c) in out.iter_mut().zip(self.raw.as_chunks::<4>().0).take(n) {
                    let i = i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0;
                    let q = i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0;
                    *o = Complex32::new(i, q);
                }
            }
            SampleFormat::F32 => {
                for (o, c) in out.iter_mut().zip(self.raw.as_chunks::<8>().0).take(n) {
                    let i = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    let q = f32::from_le_bytes([c[4], c[5], c[6], c[7]]);
                    *o = Complex32::new(i, q);
                }
            }
        }
        if n == 0 {
            // Short file: treat the rest as absent.
            self.meta.frames = self.pos;
        }
        self.pos += n as u64;
        Ok(n)
    }
}

/// Real-time pacing for file playback: how many frames are due after
/// `elapsed_s` seconds at `rate`, given `sent` frames already delivered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pacer {
    rate: f64,
    sent: u64,
}

impl Pacer {
    pub fn new(rate: f64) -> Self {
        Self { rate, sent: 0 }
    }

    /// Frames that may be delivered now.
    pub fn due(&self, elapsed_s: f64) -> u64 {
        ((elapsed_s * self.rate).floor() as u64).saturating_sub(self.sent)
    }

    /// Seconds until `frames` more frames are due (0 if already due).
    pub fn wait_for(&self, elapsed_s: f64, frames: u64) -> f64 {
        ((self.sent + frames) as f64 / self.rate - elapsed_s).max(0.0)
    }

    /// Record `frames` as delivered.
    pub fn sent(&mut self, frames: u64) {
        self.sent += frames;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::wav::{WavWriter, format_icmt};
    use std::io::Cursor;

    fn i16_file(frames: &[(f32, f32)], comment: &str) -> Cursor<Vec<u8>> {
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 2, 250_000).unwrap();
        let flat: Vec<f32> = frames.iter().flat_map(|(i, q)| [*i, *q]).collect();
        w.write_f32(&flat).unwrap();
        let mut c = w.finish(comment).unwrap();
        c.set_position(0);
        c
    }

    /// A 32-bit float stereo WAV, as SDR++'s Float32 mode writes it.
    fn f32_file(frames: &[(f32, f32)]) -> Cursor<Vec<u8>> {
        let data: Vec<u8> = frames
            .iter()
            .flat_map(|(i, q)| [i.to_le_bytes(), q.to_le_bytes()].concat())
            .collect();
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&48_000u32.to_le_bytes());
        b.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
        b.extend_from_slice(&8u16.to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        Cursor::new(b)
    }

    fn reader(mut c: Cursor<Vec<u8>>, name: &str) -> IqReader<Cursor<Vec<u8>>> {
        let info = read_info(&mut c).unwrap();
        let meta = describe(&info, name).unwrap();
        IqReader::new(c, &info, meta).unwrap()
    }

    #[test]
    fn center_comes_from_the_sdrpp_file_name() {
        assert_eq!(
            center_from_name("20261104T100029_baseband_13264000Hz.wav"),
            Some(13_264_000)
        );
        assert_eq!(
            center_from_name("baseband_7100000Hz_12-00-00_01-01-2026.wav"),
            Some(7_100_000)
        );
        assert_eq!(center_from_name("recording.wav"), None);
        assert_eq!(center_from_name("12 Hz.wav"), None, "digits must touch Hz");
    }

    #[test]
    fn icmt_center_wins_over_the_name() {
        let mut c = i16_file(
            &[(0.0, 0.0)],
            &format_icmt(&[("center_freq".to_string(), "9000000".to_string())]),
        );
        let info = read_info(&mut c).unwrap();
        let m = describe(&info, "x_13264000Hz.wav").unwrap();
        assert_eq!(m.center_hz, Some(9_000_000));
        assert_eq!(m.sample_rate, 250_000);
        assert_eq!(m.frames, 1);
        assert_eq!(m.format, SampleFormat::I16);
    }

    #[test]
    fn mono_files_are_rejected() {
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 1, 48_000).unwrap();
        w.write_f32(&[0.0; 4]).unwrap();
        let mut c = w.finish("").unwrap();
        c.set_position(0);
        let info = read_info(&mut c).unwrap();
        assert!(describe(&info, "audio.wav").is_err());
    }

    #[test]
    fn int16_frames_scale_like_sdrpp() {
        // Written as round(x * 32767), read back / 32768 (SDR++ both ways).
        let mut r = reader(i16_file(&[(0.5, -0.5), (1.0, -1.0)], ""), "a.wav");
        let mut out = [Complex32::new(9.0, 9.0); 4];
        assert_eq!(r.read(&mut out).unwrap(), 2);
        assert_eq!(
            out[0],
            Complex32::new(16384.0 / 32768.0, -16384.0 / 32768.0)
        );
        assert_eq!(
            out[1],
            Complex32::new(32767.0 / 32768.0, -32767.0 / 32768.0)
        );
        assert_eq!(r.read(&mut out).unwrap(), 0, "end of data");
        assert_eq!(r.position(), 2);
    }

    #[test]
    fn the_trailing_icmt_chunk_is_not_played() {
        let mut r = reader(
            i16_file(&[(0.1, 0.2); 3], "center_freq=1000\nrec_type=baseband"),
            "a.wav",
        );
        let mut out = vec![Complex32::default(); 100];
        assert_eq!(r.read(&mut out).unwrap(), 3);
        assert_eq!(r.read(&mut out).unwrap(), 0);
    }

    #[test]
    fn float32_files_play_unscaled() {
        let mut r = reader(f32_file(&[(0.25, -0.75), (2.0, 0.0)]), "f.wav");
        assert_eq!(r.meta().format, SampleFormat::F32);
        assert_eq!(r.meta().sample_rate, 48_000);
        let mut out = [Complex32::default(); 2];
        assert_eq!(r.read(&mut out).unwrap(), 2);
        assert_eq!(out, [Complex32::new(0.25, -0.75), Complex32::new(2.0, 0.0)]);
    }

    #[test]
    fn seek_jumps_to_a_frame_and_clamps_at_the_end() {
        let frames: Vec<(f32, f32)> = (0..10).map(|k| (k as f32 / 100.0, 0.0)).collect();
        let mut r = reader(i16_file(&frames, ""), "a.wav");
        r.seek(7).unwrap();
        let mut out = [Complex32::default(); 1];
        r.read(&mut out).unwrap();
        assert_eq!(out[0].re, (0.07f32 * 32767.0).round() / 32768.0);
        assert_eq!(r.position(), 8);
        r.seek(1_000).unwrap();
        assert_eq!(r.position(), 10);
        assert_eq!(r.read(&mut out).unwrap(), 0);
        r.seek(0).unwrap();
        assert_eq!(r.read(&mut out).unwrap(), 1, "rewind plays again");
    }

    #[test]
    fn reading_in_chunks_matches_reading_whole() {
        let frames: Vec<(f32, f32)> = (0..37)
            .map(|k| ((k as f32 * 0.37).sin() * 0.9, (k as f32 * 0.11).cos() * 0.9))
            .collect();
        let mut whole = reader(i16_file(&frames, ""), "a.wav");
        let mut all = vec![Complex32::default(); 37];
        assert_eq!(whole.read(&mut all).unwrap(), 37);
        let mut chunked = reader(i16_file(&frames, ""), "a.wav");
        let mut got = Vec::new();
        for size in [1usize, 5, 2, 13, 100] {
            let mut b = vec![Complex32::default(); size];
            let n = chunked.read(&mut b).unwrap();
            got.extend_from_slice(&b[..n]);
        }
        assert_eq!(got, all);
    }

    #[test]
    fn pacer_releases_frames_at_the_sample_rate() {
        let mut p = Pacer::new(250_000.0);
        assert_eq!(p.due(0.0), 0);
        assert_eq!(p.due(0.005), 1250, "one SDR++ block (rate/200) per 5 ms");
        p.sent(1250);
        assert_eq!(p.due(0.005), 0);
        assert_eq!(p.due(0.010), 1250);
        assert!((p.wait_for(0.006, 1250) - 0.004).abs() < 1e-9);
        assert_eq!(p.wait_for(0.020, 1250), 0.0);
    }

    #[test]
    fn the_shipped_test_recording_describes_correctly() {
        // The SDR++ recording kept in the repo for A/B tests (skipped when
        // the wav/ folder is not present).
        let p = Path::new("wav/20261104T100029_baseband_13264000Hz.wav");
        if !p.exists() {
            return;
        }
        let m = probe(p).unwrap();
        assert_eq!(m.sample_rate, 250_000);
        assert_eq!(m.center_hz, Some(13_264_000));
        assert_eq!(m.format, SampleFormat::I16);
        assert_eq!(m.frames, 40_196_520 / 4);
    }
}
