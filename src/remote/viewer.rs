//! A window into a sharing ShellSDR (0.4.1, spec 22.3, 22.6): the shared
//! screen as a cell grid kept up to date, the viewer's input and size sent
//! back, the audio played here with its own volume.
//!
//! Used by the terminal viewer (the Network source's `[Open window]`) and
//! by the egui viewer (`--gui`).

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::cells::Grid;
use super::client::{Jitter, LinkAudio, connect_for, decode_audio, decoder_for};
use super::wire::{Frame, Msg, WANT_WINDOW, read_frame, write_frame};

/// What the viewer asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowReq {
    pub addr: String,
    pub token: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// A graphical viewer draws the overlays.
    pub overlays: bool,
    pub bins: u32,
    /// Opus bit rate; 0 = PCM.
    pub bitrate: u32,
    /// Where the audio plays.
    pub audio_target: crate::engine::audio_target::AudioTarget,
    /// Collect the audio for [`Viewer::take_audio`] instead (tests).
    pub audio_tap: bool,
}

/// What the reader thread keeps.
#[derive(Default)]
struct Shared {
    grid: Grid,
    /// A new frame came since the last [`Viewer::take_frame`].
    dirty: bool,
    /// Why the window closed (None: open).
    closed: Option<String>,
    /// The overlays now (a graphical viewer).
    overlays: Vec<super::wire::Overlay>,
    /// Spectrum rows not taken yet, oldest first.
    rows: std::collections::VecDeque<(u8, Vec<f32>)>,
    /// The latest pixels of each picture slot (0.4.8).
    pictures: Vec<Arc<super::picture::Picture>>,
}

/// Rows kept for a viewer that does not take them.
const MAX_ROWS: usize = 512;

fn push_row(shared: &Mutex<Shared>, stream: u8, row: Vec<f32>) {
    if let Ok(mut s) = shared.lock() {
        if s.rows.len() >= MAX_ROWS {
            s.rows.pop_front();
        }
        s.rows.push_back((stream, row));
    }
}

/// An open window.
pub struct Viewer {
    w: Mutex<TcpStream>,
    shared: Arc<Mutex<Shared>>,
    quitting: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    reader: Option<JoinHandle<()>>,
    _audio: Option<crate::engine::audio::AudioOutput>,
    tap: Option<crossbeam_channel::Receiver<f32>>,
    link: Arc<LinkAudio>,
    /// The server's host and version.
    pub host: String,
    pub version: String,
}

impl Viewer {
    /// Connect and open the window.
    pub fn open(req: &WindowReq) -> Result<Self, String> {
        let mut c = connect_for(&req.addr, req.token.as_deref(), WANT_WINDOW)?;
        if !c.info.window {
            return Err("this server is headless: choose one of its devices".into());
        }
        let link = Arc::new(LinkAudio::new(&c.info, req.bitrate));
        let codec = link.ask(req.bitrate);
        c.send(Msg::Window {
            cols: req.cols,
            rows: req.rows,
            overlays: req.overlays,
            bins: req.bins,
            codec,
            bitrate: req.bitrate,
        })?;
        let codec = match c.wait(Duration::from_secs(5), |m| {
            matches!(m, Msg::Started { .. } | Msg::Refused { .. })
        })? {
            Msg::Started { codec, bitrate } => {
                link.now(codec, bitrate);
                codec
            }
            Msg::Refused { reason } => return Err(format!("refused: {reason}")),
            _ => unreachable!(),
        };
        let mut decoder = decoder_for(codec)?;
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let (audio_tx, audio_rx) = crossbeam_channel::bounded::<f32>(48_000);
        let (audio, tap) = if req.audio_tap {
            (None, Some(audio_rx))
        } else {
            let out = crate::engine::audio::AudioOutput::new(
                48_000,
                audio_rx,
                Arc::clone(&volume),
                &req.audio_target,
            )
            .map_err(|e| e.to_string())?;
            (Some(out), None)
        };
        let shared = Arc::new(Mutex::new(Shared::default()));
        let quitting = Arc::new(AtomicBool::new(false));
        let mut rd = c.rd.try_clone().map_err(|e| e.to_string())?;
        rd.set_read_timeout(None).map_err(|e| e.to_string())?;
        let reader = {
            let (shared, quitting, link2) = (
                Arc::clone(&shared),
                Arc::clone(&quitting),
                Arc::clone(&link),
            );
            std::thread::Builder::new()
                .name("shellsdr-viewer".into())
                .spawn(move || {
                    let mut jitter = Jitter::default();
                    let reason = loop {
                        match read_frame(&mut rd) {
                            Ok(Frame::Cells(d)) => {
                                if let Ok(mut s) = shared.lock() {
                                    s.grid.apply(&d);
                                    s.dirty = true;
                                }
                            }
                            Ok(Frame::Spectrum(row)) => {
                                push_row(&shared, super::wire::STREAM_MAIN, row);
                            }
                            Ok(Frame::Rows { stream, row }) => push_row(&shared, stream, row),
                            Ok(Frame::Picture(p)) => {
                                if let Ok(mut s) = shared.lock() {
                                    s.pictures.retain(|q| q.slot != p.slot);
                                    s.pictures.push(Arc::new(p));
                                    s.dirty = true;
                                }
                            }
                            Ok(Frame::Control(Msg::Overlays { items })) => {
                                if let Ok(mut s) = shared.lock() {
                                    s.overlays = items;
                                    s.dirty = true;
                                }
                            }
                            Ok(Frame::Audio(p)) => {
                                jitter.feed(decode_audio(&mut decoder, &p), &audio_tx);
                            }
                            Ok(Frame::Control(Msg::AudioIs { codec, bitrate })) => {
                                if let Ok(d) = decoder_for(codec) {
                                    decoder = d;
                                }
                                link2.now(codec, bitrate);
                            }
                            Ok(Frame::Control(Msg::Refused { reason })) => break reason,
                            Ok(_) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                                break "the server closed the window".to_string();
                            }
                            Err(e) => break format!("connection lost: {e}"),
                        }
                    };
                    if let Ok(mut s) = shared.lock()
                        && !quitting.load(Ordering::Relaxed)
                    {
                        s.closed = Some(reason);
                    }
                })
                .map_err(|e| e.to_string())?
        };
        Ok(Self {
            w: Mutex::new(c.w),
            shared,
            quitting,
            volume,
            reader: Some(reader),
            _audio: audio,
            tap,
            link,
            host: c.info.host,
            version: c.info.version,
        })
    }

    /// How the audio travels: "Opus 32 kb/s", or PCM and why (0.4.3).
    pub fn audio(&self) -> String {
        self.link.label()
    }

    /// Change the audio to `bitrate` (0: PCM) while open; an error when the
    /// server is too old for it.
    pub fn set_audio(&self, bitrate: u32) -> Result<(), String> {
        let m = self.link.change(bitrate)?;
        self.send(m);
        Ok(())
    }

    fn send(&self, m: Msg) {
        if let Ok(mut w) = self.w.lock() {
            let _ = write_frame(&mut *w, &Frame::Control(m));
        }
    }

    /// The viewer's size changed.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.send(Msg::Resize { cols, rows });
    }

    /// A key or mouse event for the shared screen.
    pub fn input(&self, event: crossterm::event::Event) {
        self.send(Msg::Input { event });
    }

    /// The screen if a new frame came since the last call.
    pub fn take_frame(&self) -> Option<Grid> {
        let mut s = self.shared.lock().ok()?;
        std::mem::take(&mut s.dirty).then(|| s.grid.clone())
    }

    /// The screen as it is.
    pub fn grid(&self) -> Grid {
        self.shared
            .lock()
            .map(|s| s.grid.clone())
            .unwrap_or_default()
    }

    /// The overlays of the screen now.
    pub fn overlays(&self) -> Vec<super::wire::Overlay> {
        self.shared
            .lock()
            .map(|s| s.overlays.clone())
            .unwrap_or_default()
    }

    /// The latest pixels of each picture slot.
    pub fn pictures(&self) -> Vec<Arc<super::picture::Picture>> {
        self.shared
            .lock()
            .map(|s| s.pictures.clone())
            .unwrap_or_default()
    }

    /// The spectrum rows that came since the last call, oldest first.
    pub fn take_rows(&self) -> Vec<(u8, Vec<f32>)> {
        self.shared
            .lock()
            .map(|mut s| s.rows.drain(..).collect())
            .unwrap_or_default()
    }

    /// Why the window closed, once it did.
    pub fn closed(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.closed.clone())
    }

    /// The audio volume here, 0.0..=1.0.
    pub fn set_volume(&self, v: f32) {
        self.volume
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// The received audio (with an audio tap).
    pub fn take_audio(&self) -> Vec<f32> {
        self.tap
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.quitting.store(true, Ordering::Relaxed);
        if let Ok(w) = self.w.lock() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}
