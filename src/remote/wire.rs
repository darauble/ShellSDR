//! The network remote's wire format (spec 21.2).
//!
//! A TCP stream of frames: `[u32 length][u8 kind][payload]`, little endian,
//! the length counting the kind byte and the payload. Control messages are
//! JSON ([`Msg`]); the streams are binary: audio packets (Opus or 16-bit
//! PCM), spectrum rows (dB quantized to a byte, delta coded along the
//! frequency axis and deflated), VFO view frames and decoded data.

use std::io::{self, Read, Write};

use crate::engine::{Command, EngineStats, VfoViewFrame};
use crate::source::Capabilities;
use crate::vfo::types::{DataItem, DecoderMeter, Settings};

/// The protocol version both ends must speak.
pub const PROTO: u32 = 1;
/// The default TCP port of `shellsdr --serve`.
pub const DEFAULT_PORT: u16 = 7373;
/// The largest frame accepted (a corrupt length must not allocate GBs).
const MAX_FRAME: usize = 32 << 20;

const KIND_CONTROL: u8 = 1;
const KIND_AUDIO: u8 = 2;
const KIND_SPECTRUM: u8 = 3;
const KIND_VIEW: u8 = 4;
const KIND_DATA: u8 = 5;
const KIND_CELLS: u8 = 6;
const KIND_ROWS: u8 = 7;
const KIND_PICTURE: u8 = 8;

/// Overlay streams: the wideband spectrum, the VFO view's channel and its
/// audio.
pub const STREAM_MAIN: u8 = 0;
pub const STREAM_CHANNEL: u8 = 1;
pub const STREAM_AUDIO: u8 = 2;

/// What a client wants from a server (0.4.1): its engine (the Network
/// source, 21.2) or its screen (a window, 22).
pub const WANT_ENGINE: &str = "engine";
pub const WANT_WINDOW: &str = "window";

/// How the server sends the audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Codec {
    /// Opus, 48 kHz mono, 20 ms packets.
    Opus,
    /// Uncompressed 16-bit PCM, 48 kHz mono (no libopus on one end).
    Pcm16,
}

/// An input a server offers: a SoapySDR device or a served file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteInput {
    pub label: String,
    /// What the client sends back to probe or start it.
    pub args: String,
    /// A served file is centered on a fixed frequency (the client locks its
    /// center to it, like the File source).
    pub fixed_center_hz: Option<f64>,
}

/// What a client asks the server to start (the engine configuration, minus
/// what stays on the client: the audio output).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StartReq {
    pub args: String,
    pub sample_rate: f64,
    pub decimation: u32,
    pub center_hz: f64,
    pub vfo_hz: f64,
    pub vfo_type: String,
    pub settings: Settings,
    pub gains: Vec<(String, f64)>,
    pub agc: bool,
    pub antenna: Option<String>,
    pub hw_bandwidth_hz: Option<f64>,
    pub fft_enabled: bool,
    /// The most spectrum bins the client wants (0: as computed); the server
    /// keeps the peak of each group, so a narrow signal is not lost.
    pub spectrum_bins: u32,
    pub codec: Codec,
    /// Opus bit rate, bits per second.
    pub bitrate: u32,
}

/// A rectangle of the shared screen a graphical viewer draws itself, at
/// pixel resolution (0.4.1, spec 22.4). `rect` is (x, y, width, height) in
/// cells; the cells under it show the terminal's version.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Overlay {
    /// The spectrum plot: the trace from the spectrum rows, the center and
    /// the active VFO's marker.
    Spectrum {
        rect: [u16; 4],
        center_hz: f64,
        span_hz: f64,
        vfo_hz: f64,
        db_min: f64,
        db_max: f64,
        /// Colours: the trace, the center line, the VFO marker.
        trace: [u8; 3],
        center: [u8; 3],
        marker: [u8; 3],
        /// Whose rows (`STREAM_*`).
        #[serde(default)]
        stream: u8,
        /// The channel filter's width, drawn around the center (0: none).
        #[serde(default)]
        edges_hz: f64,
    },
    /// The waterfall plot: the spectrum rows, newest on top, coloured
    /// through `gradient` (low to high) over `db_min..db_max`.
    Waterfall {
        rect: [u16; 4],
        center_hz: f64,
        span_hz: f64,
        db_min: f64,
        db_max: f64,
        gradient: Vec<[u8; 3]>,
        #[serde(default)]
        stream: u8,
        #[serde(default)]
        edges_hz: f64,
    },
    /// A picture (0.4.8, spec 28): the pixels of `slot` (`picture::SLOT_*`,
    /// sent apart, see [`Frame::Picture`]) over `rect`, stretched to it, or
    /// (`fit`) as large as fits keeping its aspect, at the top and centered,
    /// the rest of the rectangle blank. Only to a viewer of 0.4.8 or newer.
    Picture {
        rect: [u16; 4],
        slot: u8,
        #[serde(default)]
        fit: bool,
    },
}

impl Overlay {
    /// The same overlay `dy` rows further down (a window's own bar above
    /// the shared screen).
    pub fn shifted(mut self, dy: u16) -> Self {
        match &mut self {
            Overlay::Spectrum { rect, .. }
            | Overlay::Waterfall { rect, .. }
            | Overlay::Picture { rect, .. } => {
                rect[1] = rect[1].saturating_add(dy);
            }
        }
        self
    }
}

/// A control message, either way.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "t")]
pub enum Msg {
    /// Client: the first message.
    Hello {
        proto: u32,
        version: String,
        token: Option<String>,
        /// `engine` (default) or `window` (0.4.1).
        #[serde(default)]
        want: Option<String>,
    },
    /// Server: accepted.
    Welcome {
        proto: u32,
        version: String,
        host: String,
        opus: bool,
        /// What the server offers: `engine` (headless, default) or
        /// `window` (a sharing ShellSDR, 0.4.1).
        #[serde(default)]
        kind: Option<String>,
    },
    /// Server: not accepted (wrong token or protocol, busy); it closes.
    Refused {
        reason: String,
    },
    /// Client: what inputs are there?
    ListInputs,
    Inputs {
        inputs: Vec<RemoteInput>,
    },
    /// Client: open an input and report its capabilities.
    Probe {
        args: String,
    },
    Caps {
        args: String,
        caps: Option<Capabilities>,
        error: Option<String>,
    },
    /// Client: start the engine.
    Start {
        req: StartReq,
    },
    /// Server: the engine runs, the audio comes in `codec` (at `bitrate`,
    /// 0.4.3; 0 from older servers or PCM).
    Started {
        codec: Codec,
        #[serde(default)]
        bitrate: u32,
    },
    /// Client: send the audio this way from now on (0.4.3; only to a server
    /// of 0.4.3 or newer, see [`changes_audio`]).
    SetAudio {
        codec: Codec,
        bitrate: u32,
    },
    /// Server: the audio comes this way from the next packet on (0.4.3).
    AudioIs {
        codec: Codec,
        bitrate: u32,
    },
    /// Client: a command for the running engine.
    Cmd {
        cmd: Command,
    },
    /// Server: the engine's counters.
    Stats {
        stats: EngineStats,
    },
    /// Server: the decoder's state.
    Meter {
        meter: Option<DecoderMeter>,
    },
    /// Server: something for the user.
    Notice {
        text: String,
    },
    /// Client: stop the engine (the connection stays).
    Stop,
    /// Server: the engine stopped (on request, or it failed: `reason`).
    Stopped {
        reason: Option<String>,
    },
    /// Viewer: open the window at this size (0.4.1, spec 22.4).
    Window {
        cols: u16,
        rows: u16,
        /// Whether it draws the overlays (a graphical viewer).
        overlays: bool,
        /// Spectrum bins for the overlays.
        bins: u32,
        codec: Codec,
        bitrate: u32,
    },
    /// Viewer: its size changed.
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Viewer: a key or a mouse event, in cell coordinates.
    Input {
        event: crossterm::event::Event,
    },
    /// Server: the overlays of the screen as it is now (sent when they
    /// change; empty while a popup or dialog is open over them).
    Overlays {
        items: Vec<Overlay>,
    },
}

/// One frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Control(Msg),
    /// An audio packet in the session's codec.
    Audio(Vec<u8>),
    Spectrum(Vec<f32>),
    View(VfoViewFrame),
    Data {
        pos: u64,
        items: Vec<DataItem>,
    },
    /// What changed on the shared screen (0.4.1).
    Cells(super::cells::CellDiff),
    /// A spectrum row of an overlay stream (`STREAM_*`).
    Rows {
        stream: u8,
        row: Vec<f32>,
    },
    /// A picture's pixels (0.4.8, an [`Overlay::Picture`]'s).
    Picture(super::picture::Picture),
}

/// Write one frame.
pub fn write_frame(w: &mut impl Write, f: &Frame) -> io::Result<()> {
    let (kind, payload) = match f {
        Frame::Control(m) => (
            KIND_CONTROL,
            serde_json::to_vec(m).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        ),
        Frame::Audio(p) => (KIND_AUDIO, p.clone()),
        Frame::Spectrum(s) => (KIND_SPECTRUM, encode_spectrum(s, 0)),
        Frame::View(v) => (KIND_VIEW, encode_view(v)),
        Frame::Data { pos, items } => (KIND_DATA, encode_data(*pos, items)),
        Frame::Cells(d) => (KIND_CELLS, d.encode()),
        Frame::Rows { stream, row } => rows_payload(*stream, row, 0),
        Frame::Picture(p) => (KIND_PICTURE, p.encode()),
    };
    write_raw(w, kind, &payload)
}

/// Write a frame from its parts (a spectrum already encoded with a bin
/// limit).
pub fn write_raw(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes());
    buf.push(kind);
    buf.extend_from_slice(payload);
    w.write_all(&buf)
}

/// A spectrum frame reduced to at most `bins`, ready for [`write_raw`].
pub fn spectrum_payload(s: &[f32], bins: usize) -> (u8, Vec<u8>) {
    (KIND_SPECTRUM, encode_spectrum(s, bins))
}

/// A row of an overlay stream, reduced to at most `bins`, for
/// [`write_raw`].
pub fn rows_payload(stream: u8, s: &[f32], bins: usize) -> (u8, Vec<u8>) {
    let mut p = vec![stream];
    p.extend(encode_spectrum(s, bins));
    (KIND_ROWS, p)
}

/// A picture's frame, for [`write_raw`] (no copy of its pixels).
pub fn picture_payload(p: &super::picture::Picture) -> (u8, Vec<u8>) {
    (KIND_PICTURE, p.encode())
}

/// Read one frame (blocking).
pub fn read_frame(r: &mut impl Read) -> io::Result<Frame> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(bad(format!("frame length {len}")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let payload = &buf[1..];
    Ok(match buf[0] {
        KIND_CONTROL => Frame::Control(serde_json::from_slice(payload).map_err(bad)?),
        KIND_AUDIO => Frame::Audio(payload.to_vec()),
        KIND_SPECTRUM => Frame::Spectrum(decode_spectrum(payload)?),
        KIND_VIEW => Frame::View(decode_view(payload)?),
        KIND_DATA => {
            let (pos, items) = decode_data(payload)?;
            Frame::Data { pos, items }
        }
        KIND_CELLS => Frame::Cells(super::cells::CellDiff::decode(payload)?),
        KIND_ROWS if !payload.is_empty() => Frame::Rows {
            stream: payload[0],
            row: decode_spectrum(&payload[1..])?,
        },
        KIND_PICTURE => Frame::Picture(super::picture::Picture::decode(payload)?),
        k => return Err(bad(format!("frame kind {k}"))),
    })
}

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Audio packet payloads: Opus packets as they are, PCM as i16 LE.
/// Whether a server of `version` changes the audio of a running
/// connection ([`Msg::SetAudio`], 0.4.3).
pub fn changes_audio(version: &str) -> bool {
    crate::version::parse(version).is_some_and(|v| v >= (0, 4, 3))
}

/// The codec to ask for at `bitrate` (0: PCM), and why it is PCM when Opus
/// was wanted but cannot be had.
pub fn wanted_codec(bitrate: u32, server_opus: bool) -> (Codec, Option<&'static str>) {
    if bitrate == 0 {
        (Codec::Pcm16, None)
    } else if super::opus::available().is_err() {
        (Codec::Pcm16, Some("no libopus here"))
    } else if !server_opus {
        (Codec::Pcm16, Some("no libopus on the server"))
    } else {
        (Codec::Opus, None)
    }
}

/// The audio as it travels, for the user: "Opus 32 kb/s", "PCM 768 kb/s",
/// with why it is PCM when Opus was asked for.
pub fn audio_label(codec: Codec, bitrate: u32, why: Option<&str>) -> String {
    match (codec, why) {
        (Codec::Opus, _) if bitrate > 0 => format!("Opus {} kb/s", bitrate / 1000),
        (Codec::Opus, _) => "Opus".to_string(),
        (Codec::Pcm16, None) => "PCM 768 kb/s".to_string(),
        (Codec::Pcm16, Some(w)) => format!("PCM 768 kb/s ({w})"),
    }
}

pub fn pcm16_encode(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|x| ((x.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect()
}

pub fn pcm16_decode(p: &[u8]) -> Vec<f32> {
    p.as_chunks::<2>()
        .0
        .iter()
        .map(|b| f32::from(i16::from_le_bytes(*b)) / 32767.0)
        .collect()
}

// ── Spectrum rows ───────────────────────────────────────────────────────

/// `[u32 n][f32 base][f32 step][deflate(delta(q))]`, q = (dB - base) / step
/// in a byte. The step is 0.5 dB unless the row spans more than 127 dB.
fn encode_spectrum(s: &[f32], bins: usize) -> Vec<u8> {
    let s = reduce_peak(s, bins);
    let finite = || s.iter().copied().filter(|x| x.is_finite());
    let lo = finite().fold(f32::INFINITY, f32::min);
    let hi = finite().fold(f32::NEG_INFINITY, f32::max);
    let (base, step) = if lo.is_finite() {
        (lo.floor(), ((hi - lo.floor()) / 255.0).max(0.5))
    } else {
        (-200.0, 0.5)
    };
    let mut prev = 0u8;
    let delta: Vec<u8> = s
        .iter()
        .map(|&x| {
            let x = if x.is_finite() { x } else { base };
            let q = ((x - base) / step).round().clamp(0.0, 255.0) as u8;
            let d = q.wrapping_sub(prev);
            prev = q;
            d
        })
        .collect();
    let mut out = Vec::with_capacity(12 + delta.len() / 2);
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(&base.to_le_bytes());
    out.extend_from_slice(&step.to_le_bytes());
    let mut z = flate2::write::DeflateEncoder::new(out, flate2::Compression::fast());
    let _ = z.write_all(&delta);
    z.finish().unwrap_or_default()
}

fn decode_spectrum(p: &[u8]) -> io::Result<Vec<f32>> {
    let mut c = Cursor(p);
    let n = c.u32()? as usize;
    let base = c.f32()?;
    let step = c.f32()?;
    if n > MAX_FRAME {
        return Err(bad("spectrum length"));
    }
    let mut delta = Vec::with_capacity(n);
    flate2::read::DeflateDecoder::new(c.0)
        .take(n as u64)
        .read_to_end(&mut delta)?;
    if delta.len() != n {
        return Err(bad("short spectrum"));
    }
    let mut q = 0u8;
    Ok(delta
        .into_iter()
        .map(|d| {
            q = q.wrapping_add(d);
            base + f32::from(q) * step
        })
        .collect())
}

/// At most `bins` values (0: all), each the peak of its group.
fn reduce_peak(s: &[f32], bins: usize) -> std::borrow::Cow<'_, [f32]> {
    if bins == 0 || s.len() <= bins {
        return std::borrow::Cow::Borrowed(s);
    }
    let n = s.len();
    std::borrow::Cow::Owned(
        (0..bins)
            .map(|i| {
                s[i * n / bins..((i + 1) * n / bins).max(i * n / bins + 1)]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max)
            })
            .collect(),
    )
}

// ── VFO view frames ─────────────────────────────────────────────────────

fn encode_view(v: &VfoViewFrame) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&v.zoom_rate.to_le_bytes());
    out.extend_from_slice(&v.center_offset.to_le_bytes());
    out.extend_from_slice(&v.level_db.to_le_bytes());
    out.push(u8::from(v.agc_gain_db.is_some()));
    out.extend_from_slice(&v.agc_gain_db.unwrap_or(0.0).to_le_bytes());
    for s in [&v.zoom_db, &v.audio_db] {
        let e = encode_spectrum(s, 0);
        out.extend_from_slice(&(e.len() as u32).to_le_bytes());
        out.extend_from_slice(&e);
    }
    out
}

fn decode_view(p: &[u8]) -> io::Result<VfoViewFrame> {
    let mut c = Cursor(p);
    let zoom_rate = c.f64()?;
    let center_offset = c.f64()?;
    let level_db = c.f32()?;
    let has_agc = c.u8()? != 0;
    let agc = c.f32()?;
    let mut spectra = [Vec::new(), Vec::new()];
    for s in &mut spectra {
        let n = c.u32()? as usize;
        *s = decode_spectrum(c.take(n)?)?;
    }
    let [zoom_db, audio_db] = spectra;
    Ok(VfoViewFrame {
        zoom_db,
        zoom_rate,
        center_offset,
        audio_db,
        level_db,
        agc_gain_db: has_agc.then_some(agc),
    })
}

// ── Decoded data ────────────────────────────────────────────────────────

fn encode_data(pos: u64, items: &[DataItem]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&pos.to_le_bytes());
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for it in items {
        match it {
            DataItem::DscCall { symbols, age_s } => {
                out.push(0);
                out.extend_from_slice(&age_s.to_le_bytes());
                out.extend_from_slice(&(symbols.len() as u32).to_le_bytes());
                out.extend_from_slice(symbols);
            }
            DataItem::FaxAudio { samples, rate } => {
                out.push(1);
                out.extend_from_slice(&rate.to_le_bytes());
                out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
                for s in samples {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
            DataItem::HfdlFrame {
                freq_khz,
                m1,
                octets,
                age_s,
                level_db,
                noise_db,
                ferr_hz,
            } => {
                out.push(3);
                out.extend_from_slice(&age_s.to_le_bytes());
                out.extend_from_slice(&(octets.len() as u32).to_le_bytes());
                out.extend_from_slice(octets);
                out.extend_from_slice(&freq_khz.to_le_bytes());
                out.push(*m1);
                out.extend_from_slice(&level_db.to_le_bytes());
                out.extend_from_slice(&noise_db.to_le_bytes());
                out.extend_from_slice(&ferr_hz.to_le_bytes());
            }
            DataItem::HfdlStatus { channels } => {
                out.push(4);
                out.extend_from_slice(&0f64.to_le_bytes());
                out.extend_from_slice(&(channels.len() as u32).to_le_bytes());
                for ch in channels {
                    out.extend_from_slice(&ch.freq_khz.to_le_bytes());
                    out.push(ch.decoding as u8);
                    out.extend_from_slice(&ch.level_db.to_le_bytes());
                    out.extend_from_slice(&ch.noise_db.to_le_bytes());
                }
            }
            DataItem::DigitalIq { samples, rate } => {
                out.push(2);
                out.extend_from_slice(&rate.to_le_bytes());
                out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
                for s in samples {
                    out.extend_from_slice(&s.re.to_le_bytes());
                    out.extend_from_slice(&s.im.to_le_bytes());
                }
            }
        }
    }
    out
}

fn decode_data(p: &[u8]) -> io::Result<(u64, Vec<DataItem>)> {
    let mut c = Cursor(p);
    let pos = c.u64()?;
    let count = c.u32()?;
    let mut items = Vec::new();
    for _ in 0..count {
        let tag = c.u8()?;
        let x = c.f64()?;
        let n = c.u32()? as usize;
        items.push(match tag {
            0 => DataItem::DscCall {
                symbols: c.take(n)?.to_vec(),
                age_s: x,
            },
            1 => DataItem::FaxAudio {
                samples: c
                    .take(n.checked_mul(2).ok_or_else(|| bad("size"))?)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b))
                    .collect(),
                rate: x,
            },
            2 => DataItem::DigitalIq {
                samples: c
                    .take(n.checked_mul(8).ok_or_else(|| bad("size"))?)?
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|b| {
                        num_complex::Complex32::new(
                            f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                            f32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                        )
                    })
                    .collect(),
                rate: x,
            },
            3 => {
                let octets = c.take(n)?.to_vec();
                let freq_khz = c.u32()?;
                let m1 = c.u8()?;
                DataItem::HfdlFrame {
                    freq_khz,
                    m1,
                    octets,
                    age_s: x,
                    level_db: c.f32()?,
                    noise_db: c.f32()?,
                    ferr_hz: c.f32()?,
                }
            }
            4 => {
                let mut channels = Vec::new();
                for _ in 0..n.min(4096) {
                    let freq_khz = c.u32()?;
                    let decoding = c.u8()? != 0;
                    channels.push(crate::hfdl::rx::ChannelStatus {
                        freq_khz,
                        decoding,
                        level_db: c.f32()?,
                        noise_db: c.f32()?,
                    });
                }
                DataItem::HfdlStatus { channels }
            }
            t => return Err(bad(format!("data item {t}"))),
        });
    }
    Ok((pos, items))
}

/// A bounds-checked reader over a payload.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(bad("short payload"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or_default(),
        ))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or_default(),
        ))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or_default(),
        ))
    }
    fn f64(&mut self) -> io::Result<f64> {
        Ok(f64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or_default(),
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_audio_is_described_and_changed_only_by_new_servers() {
        assert_eq!(audio_label(Codec::Opus, 32_000, None), "Opus 32 kb/s");
        assert_eq!(audio_label(Codec::Pcm16, 0, None), "PCM 768 kb/s");
        assert_eq!(
            audio_label(Codec::Pcm16, 0, Some("no libopus on the server")),
            "PCM 768 kb/s (no libopus on the server)"
        );
        assert!(changes_audio("0.4.3") && changes_audio("0.5.0"));
        assert!(!changes_audio("0.4.2") && !changes_audio("junk"));
        assert_eq!(wanted_codec(0, true), (Codec::Pcm16, None));
        if super::super::opus::available().is_ok() {
            assert_eq!(wanted_codec(32_000, true), (Codec::Opus, None));
            assert_eq!(
                wanted_codec(32_000, false),
                (Codec::Pcm16, Some("no libopus on the server"))
            );
        }
        // an older server's Started has no bit rate
        let mut v = serde_json::to_value(Msg::Started {
            codec: Codec::Opus,
            bitrate: 9,
        })
        .unwrap();
        v.as_object_mut().unwrap().remove("bitrate").unwrap();
        let m: Msg = serde_json::from_value(v).unwrap();
        assert_eq!(
            m,
            Msg::Started {
                codec: Codec::Opus,
                bitrate: 0
            }
        );
    }

    use super::*;

    fn round_trip(f: &Frame) -> Frame {
        let mut buf = Vec::new();
        write_frame(&mut buf, f).unwrap();
        let mut r = &buf[..];
        let got = read_frame(&mut r).unwrap();
        assert!(r.is_empty(), "the whole frame was read");
        got
    }

    #[test]
    fn control_messages_round_trip() {
        for m in [
            Msg::Hello {
                proto: PROTO,
                version: "0.4.0".into(),
                token: Some("x".into()),
                want: Some(WANT_WINDOW.into()),
            },
            Msg::Input {
                event: crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('q'),
                    crossterm::event::KeyModifiers::CONTROL,
                )),
            },
            Msg::Cmd {
                cmd: Command::SetGain("IFGR".into(), 40.0),
            },
            Msg::Stats {
                stats: EngineStats {
                    samples: 9,
                    load: 0.25,
                    ..EngineStats::default()
                },
            },
            Msg::Stopped { reason: None },
        ] {
            assert_eq!(round_trip(&Frame::Control(m.clone())), Frame::Control(m));
        }
    }

    #[test]
    fn a_spectrum_row_comes_back_within_half_a_step_and_small() {
        // noise around -110 dB with a carrier at -40 dB
        let row: Vec<f32> = (0..4096)
            .map(|i| {
                if i == 2000 {
                    -40.0
                } else {
                    -110.0 + ((i * 7919) % 13) as f32 * 0.4
                }
            })
            .collect();
        let enc = encode_spectrum(&row, 0);
        assert!(enc.len() < row.len() / 2, "{} bytes", enc.len());
        let Frame::Spectrum(got) = round_trip(&Frame::Spectrum(row.clone())) else {
            panic!()
        };
        assert_eq!(got.len(), row.len());
        for (a, b) in row.iter().zip(&got) {
            assert!((a - b).abs() <= 0.26, "{a} vs {b}");
        }
    }

    #[test]
    fn fewer_bins_keep_the_peaks() {
        let mut row = vec![-120.0f32; 4096];
        row[1001] = -30.0;
        row[4095] = f32::NEG_INFINITY;
        let got = decode_spectrum(&encode_spectrum(&row, 512)).unwrap();
        assert_eq!(got.len(), 512);
        let peak = got.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (peak + 30.0).abs() < 0.5,
            "a one-bin carrier survives: {peak}"
        );
        assert_eq!(
            got.iter().position(|x| *x > -100.0),
            Some(1001 * 512 / 4096)
        );
    }

    #[test]
    fn view_and_data_round_trip() {
        let v = VfoViewFrame {
            zoom_db: vec![-80.0, -70.0, -60.0],
            zoom_rate: 37_500.0,
            center_offset: 1_400.0,
            audio_db: Vec::new(),
            level_db: -50.5,
            agc_gain_db: Some(12.0),
        };
        assert_eq!(round_trip(&Frame::View(v.clone())), Frame::View(v));
        let items = vec![
            DataItem::DscCall {
                symbols: vec![1, 2, 125],
                age_s: 1.5,
            },
            DataItem::FaxAudio {
                samples: vec![-3, 0, 32767],
                rate: 11_025.0,
            },
            DataItem::DigitalIq {
                samples: vec![num_complex::Complex32::new(0.5, -0.25)],
                rate: 8_000.0,
            },
            DataItem::HfdlFrame {
                freq_khz: 8977,
                m1: 5,
                octets: vec![0x26, 0x01, 0xFF],
                age_s: 2.25,
                level_db: -20.5,
                noise_db: -40.0,
                ferr_hz: 1.5,
            },
            DataItem::HfdlStatus {
                channels: vec![crate::hfdl::rx::ChannelStatus {
                    freq_khz: 8942,
                    decoding: true,
                    level_db: -30.0,
                    noise_db: -45.5,
                }],
            },
        ];
        let f = Frame::Data { pos: 77, items };
        assert_eq!(round_trip(&f), f);
    }

    #[test]
    fn pcm_round_trips_and_a_bad_frame_is_an_error() {
        let x = [0.0f32, 0.5, -1.0];
        let back = pcm16_decode(&pcm16_encode(&x));
        assert!(x.iter().zip(&back).all(|(a, b)| (a - b).abs() < 1e-4));
        let mut r: &[u8] = &[0, 0, 0, 0];
        assert!(read_frame(&mut r).is_err(), "length 0");
        let mut r: &[u8] = &[2, 0, 0, 0, 9, 0];
        assert!(read_frame(&mut r).is_err(), "unknown kind");
    }
}
