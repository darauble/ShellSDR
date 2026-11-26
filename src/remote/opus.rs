//! Opus audio for the network remote (spec 21.2), from the system's
//! libopus loaded at run time (like libfax): no build dependency, and
//! without the library the remote falls back to 16-bit PCM.
//!
//! 48 kHz mono, 20 ms frames (960 samples), `OPUS_APPLICATION_AUDIO` (radio
//! noise and tones are not speech).

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

/// Samples per Opus frame: 20 ms at 48 kHz.
pub const FRAME: usize = 960;
/// The largest packet we produce or accept.
const MAX_PACKET: usize = 4000;

const OPUS_APPLICATION_AUDIO: c_int = 2049;
const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
const OPUS_OK: c_int = 0;

type EncoderCreate = unsafe extern "C" fn(i32, c_int, c_int, *mut c_int) -> *mut c_void;
type EncodeFloat = unsafe extern "C" fn(*mut c_void, *const f32, c_int, *mut u8, i32) -> i32;
type EncoderCtl = unsafe extern "C" fn(*mut c_void, c_int, ...) -> c_int;
type Destroy = unsafe extern "C" fn(*mut c_void);
type DecoderCreate = unsafe extern "C" fn(i32, c_int, *mut c_int) -> *mut c_void;
type DecodeFloat =
    unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut f32, c_int, c_int) -> c_int;
type VersionString = unsafe extern "C" fn() -> *const c_char;

/// The loaded library and its entry points.
struct Lib {
    _lib: libloading::Library,
    encoder_create: EncoderCreate,
    encode_float: EncodeFloat,
    encoder_ctl: EncoderCtl,
    encoder_destroy: Destroy,
    decoder_create: DecoderCreate,
    decode_float: DecodeFloat,
    decoder_destroy: Destroy,
    version: String,
}

fn load() -> Result<Lib, String> {
    let names = ["libopus.so.0", "libopus.so", "libopus.0.dylib"];
    let mut last = String::new();
    for name in names {
        // SAFETY: loading a system library whose initializers are benign.
        match unsafe { libloading::Library::new(name) } {
            Ok(lib) => {
                // SAFETY: the symbols have the documented libopus types.
                unsafe {
                    let sym = |n: &[u8]| -> Result<*const c_void, String> {
                        lib.get::<*const c_void>(n)
                            .map(|s| *s)
                            .map_err(|e| e.to_string())
                    };
                    let encoder_create: EncoderCreate =
                        std::mem::transmute(sym(b"opus_encoder_create\0")?);
                    let encode_float: EncodeFloat =
                        std::mem::transmute(sym(b"opus_encode_float\0")?);
                    let encoder_ctl: EncoderCtl = std::mem::transmute(sym(b"opus_encoder_ctl\0")?);
                    let encoder_destroy: Destroy =
                        std::mem::transmute(sym(b"opus_encoder_destroy\0")?);
                    let decoder_create: DecoderCreate =
                        std::mem::transmute(sym(b"opus_decoder_create\0")?);
                    let decode_float: DecodeFloat =
                        std::mem::transmute(sym(b"opus_decode_float\0")?);
                    let decoder_destroy: Destroy =
                        std::mem::transmute(sym(b"opus_decoder_destroy\0")?);
                    let version_fn: VersionString =
                        std::mem::transmute(sym(b"opus_get_version_string\0")?);
                    let v = version_fn();
                    let version = if v.is_null() {
                        "libopus".to_string()
                    } else {
                        std::ffi::CStr::from_ptr(v).to_string_lossy().into_owned()
                    };
                    return Ok(Lib {
                        _lib: lib,
                        encoder_create,
                        encode_float,
                        encoder_ctl,
                        encoder_destroy,
                        decoder_create,
                        decode_float,
                        decoder_destroy,
                        version,
                    });
                }
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("libopus not found ({last})"))
}

fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(load).as_ref().map_err(Clone::clone)
}

/// The libopus version string, or why it could not be loaded.
pub fn available() -> Result<String, String> {
    lib().map(|l| l.version.clone())
}

/// An Opus encoder: 48 kHz mono float in, packets out.
pub struct Encoder {
    st: *mut c_void,
    lib: &'static Lib,
    /// Samples waiting for a whole frame.
    pending: Vec<f32>,
}

// SAFETY: the encoder state is used from one thread at a time.
unsafe impl Send for Encoder {}

impl Encoder {
    /// A new encoder at `bitrate` bits per second.
    pub fn new(bitrate: u32) -> Result<Self, String> {
        let lib = lib()?;
        let mut err: c_int = 0;
        // SAFETY: valid arguments; the result is checked.
        let st = unsafe { (lib.encoder_create)(48_000, 1, OPUS_APPLICATION_AUDIO, &mut err) };
        if st.is_null() || err != OPUS_OK {
            return Err(format!("opus_encoder_create failed ({err})"));
        }
        // SAFETY: a live encoder and a documented request.
        unsafe { (lib.encoder_ctl)(st, OPUS_SET_BITRATE_REQUEST, bitrate as i32) };
        Ok(Self {
            st,
            lib,
            pending: Vec::new(),
        })
    }

    /// Feed samples; returns the packets of every completed frame.
    pub fn encode(&mut self, samples: &[f32]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        let mut buf = [0u8; MAX_PACKET];
        let mut at = 0;
        while self.pending.len() - at >= FRAME {
            let frame = &self.pending[at..at + FRAME];
            // SAFETY: `frame` has FRAME samples, `buf` MAX_PACKET bytes.
            let n = unsafe {
                (self.lib.encode_float)(
                    self.st,
                    frame.as_ptr(),
                    FRAME as c_int,
                    buf.as_mut_ptr(),
                    MAX_PACKET as i32,
                )
            };
            if n > 0 {
                out.push(buf[..n as usize].to_vec());
            }
            at += FRAME;
        }
        self.pending.drain(..at);
        out
    }
}

/// The sending end of a connection's audio (0.4.3): Opus at a bit rate,
/// or 16-bit PCM (asked for, or no libopus here); whole 20 ms packets.
pub struct Packer {
    enc: Option<Encoder>,
    bitrate: u32,
    /// PCM samples waiting for a whole packet.
    pcm: Vec<f32>,
}

impl Packer {
    /// Audio as asked: `codec` at `bitrate` (PCM when Opus cannot be had).
    pub fn new(codec: super::wire::Codec, bitrate: u32) -> Self {
        let bitrate = bitrate.clamp(6_000, 256_000);
        let enc = match codec {
            super::wire::Codec::Opus => Encoder::new(bitrate).ok(),
            super::wire::Codec::Pcm16 => None,
        };
        Self {
            bitrate: if enc.is_some() { bitrate } else { 0 },
            enc,
            pcm: Vec::new(),
        }
    }

    /// What it sends.
    pub fn codec(&self) -> super::wire::Codec {
        if self.enc.is_some() {
            super::wire::Codec::Opus
        } else {
            super::wire::Codec::Pcm16
        }
    }

    /// The Opus bit rate (0: PCM).
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Feed samples; returns the packets ready to send.
    pub fn pack(&mut self, samples: &[f32]) -> Vec<Vec<u8>> {
        match &mut self.enc {
            Some(e) => e.encode(samples),
            None => {
                self.pcm.extend_from_slice(samples);
                let whole = self.pcm.len() / FRAME * FRAME;
                let p = self.pcm[..whole]
                    .chunks(FRAME)
                    .map(super::wire::pcm16_encode)
                    .collect();
                self.pcm.drain(..whole);
                p
            }
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_encoder_create, destroyed once.
        unsafe { (self.lib.encoder_destroy)(self.st) }
    }
}

/// An Opus decoder: packets in, 48 kHz mono float out.
pub struct Decoder {
    st: *mut c_void,
    lib: &'static Lib,
}

// SAFETY: the decoder state is used from one thread at a time.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Self, String> {
        let lib = lib()?;
        let mut err: c_int = 0;
        // SAFETY: valid arguments; the result is checked.
        let st = unsafe { (lib.decoder_create)(48_000, 1, &mut err) };
        if st.is_null() || err != OPUS_OK {
            return Err(format!("opus_decoder_create failed ({err})"));
        }
        Ok(Self { st, lib })
    }

    /// Decode one packet (empty on a bad packet).
    pub fn decode(&mut self, packet: &[u8]) -> Vec<f32> {
        // 120 ms is the longest Opus frame.
        let mut pcm = vec![0f32; 5760];
        // SAFETY: `pcm` holds 5760 samples, the packet its length.
        let n = unsafe {
            (self.lib.decode_float)(
                self.st,
                packet.as_ptr(),
                packet.len() as i32,
                pcm.as_mut_ptr(),
                pcm.len() as c_int,
                0,
            )
        };
        pcm.truncate(n.max(0) as usize);
        pcm
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_decoder_create, destroyed once.
        unsafe { (self.lib.decoder_destroy)(self.st) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_survives_encoding() {
        let Ok(v) = available() else {
            eprintln!("libopus not installed: skipped");
            return;
        };
        assert!(v.contains("opus"), "{v}");
        let mut enc = Encoder::new(32_000).unwrap();
        let mut dec = Decoder::new().unwrap();
        let tone: Vec<f32> = (0..48_000)
            .map(|n| 0.5 * (std::f32::consts::TAU * 1_000.0 * n as f32 / 48_000.0).sin())
            .collect();
        let mut out = Vec::new();
        // odd block sizes: frames are cut from what is pending
        for block in tone.chunks(777) {
            for p in enc.encode(block) {
                assert!(p.len() < 400, "32 kb/s: about 80 bytes a frame");
                out.extend(dec.decode(&p));
            }
        }
        assert_eq!(out.len(), 48_000 / FRAME * FRAME);
        // the decoded tone: power at 1 kHz dominates 3 kHz
        let tail = &out[out.len() / 2..];
        let at = |f: f32| {
            let (mut re, mut im) = (0f32, 0f32);
            for (i, x) in tail.iter().enumerate() {
                let p = std::f32::consts::TAU * f * i as f32 / 48_000.0;
                re += x * p.cos();
                im += x * p.sin();
            }
            (re * re + im * im).sqrt() / tail.len() as f32
        };
        assert!(at(1_000.0) > 0.2, "{}", at(1_000.0));
        assert!(at(1_000.0) > 20.0 * at(3_000.0));
    }
}
