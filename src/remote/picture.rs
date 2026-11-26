//! Pictures a graphical front paints at pixel resolution (0.4.8, spec 28):
//! the FAX picture, the MFSK picture and the MFSK waterfall. The terminal
//! draws them with half blocks; the window (`--gui`), and a window into a
//! sharing ShellSDR, paints the pixels over those cells, as it does the
//! spectrum and the waterfall.
//!
//! A view that shows a picture records an [`Overlay::Picture`] (where, and
//! which slot) and the [`Picture`] itself (the pixels of that slot now).
//! Over the network the overlay travels with the others and the pixels in
//! a frame of their own, deflated, when they change.
//!
//! [`Overlay::Picture`]: super::wire::Overlay::Picture

use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};

/// The FAX view's picture.
pub const SLOT_FAX: u8 = 0;
/// The MFSK (digital) view's picture.
pub const SLOT_MFSK: u8 = 1;
/// The MFSK (digital) view's waterfall.
pub const SLOT_DIGITAL_WATERFALL: u8 = 2;

/// The most pixels a picture is sent or painted with (a picture larger is
/// scaled down first).
pub const MAX_PIXELS: usize = 2_000_000;

/// One picture's pixels.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    pub slot: u8,
    /// What the pixels show: a new key, new pixels.
    pub key: u64,
    pub width: u32,
    pub height: u32,
    /// RGB (3 bytes a pixel), else grey (1 byte).
    pub rgb: bool,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Picture(slot {}, key {:x}, {}x{}{})",
            self.slot,
            self.key,
            self.width,
            self.height,
            if self.rgb { " rgb" } else { " grey" }
        )
    }
}

/// A key for what `v` identifies.
pub fn key_of(v: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// A key made from the pixels themselves.
pub fn pixels_key(slot: u8, width: usize, height: usize, pixels: &[u8]) -> u64 {
    key_of((slot, width, height, pixels))
}

impl Picture {
    /// A grey picture; `key` names what it shows.
    pub fn grey(slot: u8, key: u64, width: usize, height: usize, pixels: Vec<u8>) -> Self {
        debug_assert_eq!(pixels.len(), width * height);
        Self {
            slot,
            key,
            width: width as u32,
            height: height as u32,
            rgb: false,
            pixels,
        }
    }

    /// An RGB picture; `key` names what it shows ([`pixels_key`]).
    pub fn rgb(slot: u8, key: u64, width: usize, height: usize, pixels: Vec<u8>) -> Self {
        debug_assert_eq!(pixels.len(), width * height * 3);
        Self {
            slot,
            key,
            width: width as u32,
            height: height as u32,
            rgb: true,
            pixels,
        }
    }

    /// The pixels as RGBA (for a texture).
    pub fn rgba(&self) -> Vec<u8> {
        if self.rgb {
            self.pixels
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect()
        } else {
            self.pixels.iter().flat_map(|&g| [g, g, g, 255]).collect()
        }
    }

    /// Whether the pixels are as many as the size says.
    pub fn valid(&self) -> bool {
        let n = self.width as usize * self.height as usize;
        n > 0 && self.pixels.len() == n * if self.rgb { 3 } else { 1 }
    }

    /// `[u8 slot][u64 key][u32 w][u32 h][u8 rgb][deflate(pixels)]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(18 + self.pixels.len() / 4);
        out.push(self.slot);
        out.extend_from_slice(&self.key.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(u8::from(self.rgb));
        let mut z = flate2::write::DeflateEncoder::new(out, flate2::Compression::fast());
        let _ = z.write_all(&self.pixels);
        z.finish().unwrap_or_default()
    }

    pub fn decode(p: &[u8]) -> io::Result<Self> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        if p.len() < 18 {
            return Err(bad("short picture"));
        }
        let u32_at = |i: usize| u32::from_le_bytes(p[i..i + 4].try_into().expect("4 bytes"));
        let (width, height) = (u32_at(9), u32_at(13));
        let rgb = p[17] != 0;
        let n = width as usize * height as usize * if rgb { 3 } else { 1 };
        if n == 0 || n > MAX_PIXELS * 3 {
            return Err(bad("picture size"));
        }
        let mut pixels = Vec::with_capacity(n);
        flate2::read::DeflateDecoder::new(&p[18..])
            .take(n as u64)
            .read_to_end(&mut pixels)?;
        let pic = Self {
            slot: p[0],
            key: u64::from_le_bytes(p[1..9].try_into().expect("8 bytes")),
            width,
            height,
            rgb,
            pixels,
        };
        if !pic.valid() {
            return Err(bad("picture pixels"));
        }
        Ok(pic)
    }
}

/// A size of at most [`MAX_PIXELS`] (and at most `max_w` wide) with the
/// aspect of `w` x `h`.
pub fn capped(w: usize, h: usize, max_w: usize) -> (usize, usize) {
    if w == 0 || h == 0 {
        return (0, 0);
    }
    let mut s = (max_w as f64 / w as f64).min(1.0);
    let n = w as f64 * h as f64 * s * s;
    if n > MAX_PIXELS as f64 {
        s *= (MAX_PIXELS as f64 / n).sqrt();
    }
    (
        ((w as f64 * s).round() as usize).max(1),
        ((h as f64 * s).round() as usize).max(1),
    )
}

/// Whether a ShellSDR of `version` takes pictures (0.4.8): an older one
/// would close the connection on their frames.
pub fn takes_pictures(version: &str) -> bool {
    crate::version::parse(version).is_some_and(|v| v >= (0, 4, 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_travels_and_comes_back_the_same() {
        let px: Vec<u8> = (0..300 * 200).map(|i| (i % 251) as u8).collect();
        let p = Picture::grey(SLOT_FAX, 42, 300, 200, px);
        let back = Picture::decode(&p.encode()).unwrap();
        assert_eq!(back, p);
        let c = Picture::rgb(SLOT_MFSK, 7, 2, 1, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(Picture::decode(&c.encode()).unwrap(), c);
        assert_eq!(c.rgba(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(Picture::decode(&[0; 10]).is_err());
        // a size the pixels do not fill
        let mut e = p.encode();
        e[9] = 0xff;
        assert!(Picture::decode(&e).is_err());
    }

    #[test]
    fn sizes_are_capped_keeping_the_aspect() {
        assert_eq!(capped(800, 600, 1809), (800, 600));
        assert_eq!(capped(2000, 1000, 1000), (1000, 500));
        let (w, h) = capped(4000, 4000, 10_000);
        assert!(w * h <= MAX_PIXELS && w == h);
        assert!(takes_pictures("0.4.8") && takes_pictures("0.5.0") && !takes_pictures("0.4.7"));
    }
}
