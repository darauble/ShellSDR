//! WAV file reading (seed spec/04-algorithms/01-demodulator.md, "Reading
//! the audio file"). Streams the first channel as 16-bit samples.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// The sample encodings the reader accepts.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Encoding {
    Int(u16),
    Float(u16),
}

/// A WAV stream positioned at its sample data.
pub struct WavReader<R: Read> {
    inner: R,
    pub rate: u32,
    pub channels: u16,
    encoding: Encoding,
    /// Bytes of sample data not yet read.
    left: u64,
    frame: Vec<u8>,
}

const PCM: u16 = 1;
const IEEE_FLOAT: u16 = 3;
const EXTENSIBLE: u16 = 0xFFFE;

impl WavReader<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self, String> {
        let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        WavReader::new(BufReader::new(f))
    }
}

impl<R: Read> WavReader<R> {
    /// Reads the header up to the start of the `data` chunk.
    pub fn new(mut inner: R) -> Result<Self, String> {
        let mut riff = [0u8; 12];
        read_exact(&mut inner, &mut riff)?;
        if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
            return Err("not a WAV file".into());
        }
        let mut fmt: Option<(u16, u16, u32, u16)> = None;
        loop {
            let mut head = [0u8; 8];
            read_exact(&mut inner, &mut head)?;
            let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
            match &head[0..4] {
                b"fmt " => {
                    if !(16..=1024).contains(&size) {
                        return Err("bad fmt chunk".into());
                    }
                    let mut b = vec![0u8; size as usize];
                    read_exact(&mut inner, &mut b)?;
                    skip(&mut inner, size & 1)?;
                    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
                    let mut tag = u16_at(0);
                    if tag == EXTENSIBLE {
                        if size < 26 {
                            return Err("bad extensible fmt chunk".into());
                        }
                        tag = u16_at(24);
                    }
                    let rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                    fmt = Some((tag, u16_at(2), rate, u16_at(14)));
                }
                b"data" => {
                    let (tag, channels, rate, bits) = fmt.ok_or("data before fmt")?;
                    if !(1..=64).contains(&channels) {
                        return Err(format!("unsupported channel count {channels}"));
                    }
                    if !(1000..=1_000_000).contains(&rate) {
                        return Err(format!("unsupported sample rate {rate}"));
                    }
                    let encoding = match (tag, bits) {
                        (PCM, 8 | 16 | 24 | 32) => Encoding::Int(bits),
                        (IEEE_FLOAT, 32 | 64) => Encoding::Float(bits),
                        _ => return Err(format!("unsupported sample format {tag}/{bits} bit")),
                    };
                    let frame = vec![0u8; usize::from(channels) * usize::from(bits / 8)];
                    return Ok(WavReader {
                        inner,
                        rate,
                        channels,
                        encoding,
                        left: size,
                        frame,
                    });
                }
                _ => skip(&mut inner, size + (size & 1))?,
            }
        }
    }

    /// Fills `out` with first-channel samples; returns how many. 0 = end.
    pub fn read(&mut self, out: &mut [i16]) -> Result<usize, String> {
        let fl = self.frame.len() as u64;
        let mut n = 0;
        while n < out.len() && self.left >= fl {
            match self.inner.read_exact(&mut self.frame) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.left = 0;
                    break;
                }
                Err(e) => return Err(e.to_string()),
            }
            self.left -= fl;
            out[n] = convert(self.encoding, &self.frame);
            n += 1;
        }
        Ok(n)
    }

    /// Reads all remaining first-channel samples.
    pub fn read_all(&mut self) -> Result<Vec<i16>, String> {
        let mut all = Vec::new();
        let mut buf = vec![0i16; 8192];
        loop {
            let n = self.read(&mut buf)?;
            if n == 0 {
                return Ok(all);
            }
            all.extend_from_slice(&buf[..n]);
        }
    }
}

/// The first channel's sample of one frame, as a 16-bit integer.
fn convert(enc: Encoding, b: &[u8]) -> i16 {
    match enc {
        Encoding::Int(8) => ((i16::from(b[0])) - 128) * 256,
        Encoding::Int(16) => i16::from_le_bytes([b[0], b[1]]),
        Encoding::Int(24) => i16::from_le_bytes([b[1], b[2]]),
        Encoding::Int(_) => i16::from_le_bytes([b[2], b[3]]),
        Encoding::Float(32) => from_float(f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))),
        Encoding::Float(_) => from_float(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ])),
    }
}

fn from_float(v: f64) -> i16 {
    if v.is_nan() {
        return 0;
    }
    (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

fn read_exact<R: Read>(r: &mut R, b: &mut [u8]) -> Result<(), String> {
    r.read_exact(b)
        .map_err(|_| "truncated WAV header".to_string())
}

fn skip<R: Read>(r: &mut R, n: u64) -> Result<(), String> {
    let got = std::io::copy(&mut r.take(n), &mut std::io::sink()).map_err(|e| e.to_string())?;
    if got < n {
        Err("truncated WAV header".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file: `fmt` (tag, channels, rate, bits), extra chunks, data.
    fn wav(
        tag: u16,
        channels: u16,
        rate: u32,
        bits: u16,
        extra: &[(&[u8; 4], Vec<u8>)],
        data: &[u8],
    ) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits / 8)).to_le_bytes());
        fmt.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"fmt ", fmt)];
        chunks.extend(extra.iter().cloned());
        chunks.push((b"data", data.to_vec()));
        let mut body = b"WAVE".to_vec();
        for (id, c) in chunks {
            body.extend_from_slice(id);
            body.extend_from_slice(&(c.len() as u32).to_le_bytes());
            body.extend_from_slice(&c);
            if c.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut f = b"RIFF".to_vec();
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend(body);
        f
    }

    fn samples(bytes: Vec<u8>) -> Result<Vec<i16>, String> {
        WavReader::new(&bytes[..])?.read_all()
    }

    #[test]
    fn pcm16_mono_as_is() {
        let data: Vec<u8> = [1i16, -2, 32767]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(
            samples(wav(1, 1, 8000, 16, &[], &data)),
            Ok(vec![1, -2, 32767])
        );
    }

    #[test]
    fn eight_bit_is_unsigned_times_256() {
        assert_eq!(
            samples(wav(1, 1, 8000, 8, &[], &[0, 128, 255])),
            Ok(vec![-32768, 0, 127 * 256])
        );
    }

    #[test]
    fn first_channel_of_stereo_24_bit_takes_the_top_16_bits() {
        // frames: (L=0x123456, R=x), (L=-1, R=x)
        let data = [0x56, 0x34, 0x12, 9, 9, 9, 0xFF, 0xFF, 0xFF, 9, 9, 9];
        assert_eq!(
            samples(wav(1, 2, 8000, 24, &[], &data)),
            Ok(vec![0x1234, -1])
        );
    }

    #[test]
    fn unknown_odd_chunk_is_skipped_with_its_pad_byte() {
        let data: Vec<u8> = [7i16].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(
            samples(wav(1, 1, 8000, 16, &[(b"LIST", vec![1, 2, 3])], &data)),
            Ok(vec![7])
        );
    }

    #[test]
    fn float_is_rounded_clamped_and_nan_is_zero() {
        let data: Vec<u8> = [0.5f32, -1.0, 2.0, f32::NAN]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(
            samples(wav(3, 1, 8000, 32, &[], &data)),
            Ok(vec![16384, -32768, 32767, 0])
        );
        let data: Vec<u8> = [0.25f64].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(samples(wav(3, 1, 8000, 64, &[], &data)), Ok(vec![8192]));
    }

    #[test]
    fn extensible_takes_the_sub_format() {
        let mut f = wav(EXTENSIBLE, 1, 8000, 32, &[], &1.0f32.to_le_bytes());
        // Grow fmt to 40 bytes: cbSize 22, valid bits, mask, GUID (tag first).
        let pos = f.windows(4).position(|w| w == b"fmt ").unwrap();
        let mut ext = vec![22, 0, 32, 0, 4, 0, 0, 0];
        ext.extend_from_slice(&IEEE_FLOAT.to_le_bytes());
        ext.extend_from_slice(&[0; 14]);
        f[pos + 4] = 40;
        let at = pos + 8 + 16;
        f.splice(at..at, ext);
        let riff = (f.len() - 8) as u32;
        f[4..8].copy_from_slice(&riff.to_le_bytes());
        assert_eq!(samples(f), Ok(vec![32767]));
    }

    #[test]
    fn bad_headers_are_rejected() {
        assert!(samples(b"RIFX0000WAVE".to_vec()).is_err());
        assert!(samples(wav(1, 0, 8000, 16, &[], &[])).is_err());
        assert!(samples(wav(1, 65, 8000, 16, &[], &[])).is_err());
        assert!(samples(wav(1, 1, 999, 16, &[], &[])).is_err());
        assert!(samples(wav(1, 1, 1_000_001, 16, &[], &[])).is_err());
        assert!(samples(wav(1, 1, 8000, 12, &[], &[])).is_err());
        assert!(samples(wav(2, 1, 8000, 16, &[], &[])).is_err());
        assert!(samples(b"RIFF\0\0\0\0WAVEfmt ".to_vec()).is_err());
    }

    #[test]
    fn a_data_chunk_longer_than_the_file_ends_at_the_file() {
        let mut f = wav(1, 1, 8000, 16, &[], &[5, 0, 6, 0]);
        let pos = f.windows(4).position(|w| w == b"data").unwrap();
        f[pos + 4..pos + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        f.push(1); // half a sample
        assert_eq!(samples(f), Ok(vec![5, 6]));
    }
}
