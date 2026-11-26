//! WAV (RIFF) writer and info reader for recordings.
//!
//! The writer produces 16-bit PCM (SDR++'s default recorder sample type,
//! `volk_32f_s32f_convert_16i` with scale 32767 and saturation): mono audio or
//! interleaved I/Q as a stereo pair for baseband. The RIFF and `data` sizes are
//! placeholders until `finish`, which patches them. Metadata goes into a
//! `LIST`/`INFO`/`ICMT` chunk as `key=value` lines (spec 8.4). It is written at
//! `finish`, AFTER the `data` chunk, so it can carry the real end time; RIFF
//! readers find chunks by id, so the order is legal.
//!
//! The reader parses the format, the data chunk position/size and the ICMT
//! comment (baseband playback reads `center_freq`, see `source::file`).

use std::io::{self, Read, Seek, SeekFrom, Write};

/// Bits per sample of every file we write.
pub const BITS: u16 = 16;

/// Largest `data` payload a classic RIFF file can describe, minus room for the
/// trailing LIST chunk and header. The recorder starts a new part before this.
pub const MAX_DATA_BYTES: u64 = 0xFFFF_FFFF - 64 * 1024;

/// Encode metadata pairs as the ICMT text: one `key=value` per line.
pub fn format_icmt(info: &[(String, String)]) -> String {
    info.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Decode ICMT text into `key=value` pairs (lines without `=` are skipped).
pub fn parse_icmt(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// A streaming 16-bit PCM WAV writer.
pub struct WavWriter<W: Write + Seek> {
    w: W,
    channels: u16,
    data_bytes: u64,
    scratch: Vec<u8>,
}

impl<W: Write + Seek> WavWriter<W> {
    /// Write the header (sizes patched at `finish`).
    pub fn new(mut w: W, channels: u16, sample_rate: u32) -> io::Result<Self> {
        let block_align = channels * (BITS / 8);
        w.write_all(b"RIFF")?;
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(b"WAVE")?;
        w.write_all(b"fmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&1u16.to_le_bytes())?; // PCM
        w.write_all(&channels.to_le_bytes())?;
        w.write_all(&sample_rate.to_le_bytes())?;
        w.write_all(&(sample_rate * block_align as u32).to_le_bytes())?;
        w.write_all(&block_align.to_le_bytes())?;
        w.write_all(&BITS.to_le_bytes())?;
        w.write_all(b"data")?;
        w.write_all(&0u32.to_le_bytes())?;
        Ok(Self {
            w,
            channels,
            data_bytes: 0,
            scratch: Vec::new(),
        })
    }

    /// Channel count.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Bytes of sample data written so far.
    pub fn data_bytes(&self) -> u64 {
        self.data_bytes
    }

    /// Write interleaved float samples (full scale = +/-1.0, saturating).
    pub fn write_f32(&mut self, samples: &[f32]) -> io::Result<()> {
        self.scratch.clear();
        self.scratch.reserve(samples.len() * 2);
        for &s in samples {
            let v = (s * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
            self.scratch.extend_from_slice(&v.to_le_bytes());
        }
        self.w.write_all(&self.scratch)?;
        self.data_bytes += self.scratch.len() as u64;
        Ok(())
    }

    /// Append the ICMT comment, patch the chunk sizes and return the inner
    /// writer (flushed).
    pub fn finish(mut self, comment: &str) -> io::Result<W> {
        if self.data_bytes % 2 == 1 {
            self.w.write_all(&[0])?; // RIFF chunks are word aligned
        }
        // ICMT payload: text + NUL, padded to even length.
        let mut text = comment.as_bytes().to_vec();
        text.push(0);
        let icmt_len = text.len() as u32;
        if text.len() % 2 == 1 {
            text.push(0);
        }
        let list_len = 4 + 8 + text.len() as u32;
        self.w.write_all(b"LIST")?;
        self.w.write_all(&list_len.to_le_bytes())?;
        self.w.write_all(b"INFO")?;
        self.w.write_all(b"ICMT")?;
        self.w.write_all(&icmt_len.to_le_bytes())?;
        self.w.write_all(&text)?;

        let end = self.w.stream_position()?;
        let riff_len = (end - 8).min(u32::MAX as u64) as u32;
        let data_len = self.data_bytes.min(u32::MAX as u64) as u32;
        self.w.seek(SeekFrom::Start(4))?;
        self.w.write_all(&riff_len.to_le_bytes())?;
        self.w.seek(SeekFrom::Start(40))?;
        self.w.write_all(&data_len.to_le_bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        self.w.flush()?;
        Ok(self.w)
    }
}

/// What a WAV file holds, as far as playback needs to know.
#[derive(Debug, Clone, PartialEq)]
pub struct WavInfo {
    /// WAVE format tag: 1 = integer PCM, 3 = IEEE float (SDR++ Float32
    /// recordings), 0xFFFE = extensible.
    pub format: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits: u16,
    /// Byte offset and length of the sample data.
    pub data_offset: u64,
    pub data_bytes: u64,
    /// ICMT `key=value` pairs (empty when absent).
    pub comment: Vec<(String, String)>,
}

impl WavInfo {
    /// Value of an ICMT key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.comment
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn read_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn read_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Parse the chunks of a WAV file.
pub fn read_info<R: Read + Seek>(mut r: R) -> io::Result<WavInfo> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let mut hdr = [0u8; 12];
    r.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Err(bad("not a RIFF/WAVE file"));
    }
    let file_len = r.seek(SeekFrom::End(0))?;
    let mut pos = 12u64;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<(u64, u64)> = None;
    let mut comment = Vec::new();
    while pos + 8 <= file_len {
        r.seek(SeekFrom::Start(pos))?;
        let mut ch = [0u8; 8];
        r.read_exact(&mut ch)?;
        let id = [ch[0], ch[1], ch[2], ch[3]];
        let mut len = read_u32(&ch[4..]) as u64;
        let body = pos + 8;
        match &id {
            b"fmt " => {
                let mut f = [0u8; 16];
                r.read_exact(&mut f)?;
                fmt = Some((
                    read_u16(&f[0..]),
                    read_u16(&f[2..]),
                    read_u32(&f[4..]),
                    read_u16(&f[14..]),
                ));
            }
            b"data" => {
                // A writer that died before `finish` leaves 0: use the rest.
                if len == 0 || body + len > file_len {
                    len = file_len - body;
                }
                data = Some((body, len));
            }
            b"LIST" => {
                let mut b = vec![0u8; len.min(file_len - body) as usize];
                r.read_exact(&mut b)?;
                if b.starts_with(b"INFO") {
                    let mut p = 4;
                    while p + 8 <= b.len() {
                        let sub = &b[p..p + 4];
                        let sl = read_u32(&b[p + 4..]) as usize;
                        let end = (p + 8 + sl).min(b.len());
                        if sub == b"ICMT" {
                            let t = String::from_utf8_lossy(&b[p + 8..end]);
                            comment = parse_icmt(t.trim_end_matches('\0'));
                        }
                        p = end + (sl % 2);
                    }
                }
            }
            _ => {}
        }
        pos = body + len + (len % 2);
    }
    let (format, channels, sample_rate, bits) = fmt.ok_or_else(|| bad("no fmt chunk"))?;
    let (data_offset, data_bytes) = data.ok_or_else(|| bad("no data chunk"))?;
    Ok(WavInfo {
        format,
        channels,
        sample_rate,
        bits,
        data_offset,
        data_bytes,
        comment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn header_is_canonical_16_bit_pcm() {
        let w = WavWriter::new(Cursor::new(Vec::new()), 2, 250_000).unwrap();
        let bytes = w.finish("").unwrap().into_inner();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(read_u16(&bytes[20..]), 1, "PCM");
        assert_eq!(read_u16(&bytes[22..]), 2, "channels");
        assert_eq!(read_u32(&bytes[24..]), 250_000, "sample rate");
        assert_eq!(read_u32(&bytes[28..]), 1_000_000, "byte rate = 250k * 4");
        assert_eq!(read_u16(&bytes[32..]), 4, "block align");
        assert_eq!(read_u16(&bytes[34..]), 16, "bits");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(
            read_u32(&bytes[4..]) as usize,
            bytes.len() - 8,
            "RIFF size patched"
        );
    }

    #[test]
    fn samples_scale_like_sdrpp_int16_and_saturate() {
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 1, 48_000).unwrap();
        w.write_f32(&[0.0, 1.0, -1.0, 0.5, 2.0, -3.0]).unwrap();
        assert_eq!(w.data_bytes(), 12);
        let bytes = w.finish("").unwrap().into_inner();
        assert_eq!(read_u32(&bytes[40..]), 12, "data size patched");
        let s: Vec<i16> = bytes[44..56]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(s, vec![0, 32767, -32767, 16384, 32767, -32768]);
    }

    #[test]
    fn icmt_round_trips_through_write_and_read() {
        let info = kv(&[
            ("center_freq", "13264000"),
            ("rec_type", "baseband"),
            ("vfo_name", "Master"),
        ]);
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 2, 250_000).unwrap();
        w.write_f32(&[0.1, -0.1, 0.2, -0.2, 0.3]).unwrap();
        let mut cur = w.finish(&format_icmt(&info)).unwrap();
        cur.set_position(0);
        let got = read_info(cur).unwrap();
        assert_eq!(got.format, 1, "PCM");
        assert_eq!(got.channels, 2);
        assert_eq!(got.sample_rate, 250_000);
        assert_eq!(got.bits, 16);
        assert_eq!(got.data_offset, 44);
        assert_eq!(got.data_bytes, 10);
        assert_eq!(got.comment, info);
        assert_eq!(got.get("center_freq"), Some("13264000"));
        assert_eq!(got.get("missing"), None);
    }

    #[test]
    fn icmt_text_is_key_value_lines() {
        let info = kv(&[("a", "1"), ("b", "x y")]);
        assert_eq!(format_icmt(&info), "a=1\nb=x y");
        assert_eq!(parse_icmt("a=1\nnoise\nb = x y\n"), info);
    }

    #[test]
    fn reader_recovers_data_of_an_unfinished_file() {
        // A crash leaves the placeholder sizes (0); the samples are still
        // usable up to the end of the file.
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 1, 48_000).unwrap();
        w.write_f32(&[0.5; 100]).unwrap();
        let mut cur = w.w; // skip finish
        cur.set_position(0);
        let got = read_info(cur).unwrap();
        assert_eq!(got.data_bytes, 200);
        assert!(got.comment.is_empty());
    }

    #[test]
    fn reader_rejects_non_wav() {
        assert!(read_info(Cursor::new(b"hello world, not a wave".to_vec())).is_err());
    }
}
