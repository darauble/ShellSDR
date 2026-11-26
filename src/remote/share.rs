//! Sharing a running ShellSDR (0.4.1, spec 22.2, 22.4): a listener on a
//! helper thread, one viewer at a time. The UI loop takes the viewer's
//! input and size from [`ShareServer::events`], renders the screen for it
//! and hands the changed cells to [`ShareServer::send_cells`]; the engine's
//! audio comes in through [`ShareServer::audio_sender`].

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};

use super::cells::CellDiff;
use super::opus;
use super::server::{hostname, refuse, same_secret, with_port};
use super::wire::{Codec, Frame, Msg, PROTO, WANT_WINDOW, read_frame, write_frame};

/// What happened since the last call.
#[derive(Debug, Clone, PartialEq)]
pub enum ShareEvent {
    /// A viewer opened the window.
    Connected {
        peer: String,
        cols: u16,
        rows: u16,
        overlays: bool,
        /// Spectrum bins it wants for the overlays.
        bins: u32,
        /// How its audio travels ([`super::wire::audio_label`]).
        audio: String,
        /// It takes pictures (a graphical viewer of 0.4.8 or newer).
        pictures: bool,
    },
    /// The viewer changed how its audio travels (0.4.3).
    Audio(String),
    Resize {
        cols: u16,
        rows: u16,
    },
    Input(crossterm::event::Event),
    /// The viewer left (or was disconnected): why.
    Disconnected {
        peer: String,
        reason: String,
    },
    /// Someone was turned away.
    Refused {
        peer: String,
        reason: String,
    },
}

/// What the writer thread sends.
enum Out {
    Cells(CellDiff),
    Control(Msg),
    Picture(Arc<super::picture::Picture>),
}

/// The connected viewer's ends.
struct Session {
    out: Sender<Out>,
    /// Spectrum rows (encoded), dropped when the viewer is behind.
    rows: Sender<(u8, Vec<u8>)>,
    stream: TcpStream,
}

struct Inner {
    events: Mutex<Vec<ShareEvent>>,
    session: Mutex<Option<Session>>,
    quit: AtomicBool,
    token: Option<String>,
    /// The engine's audio (48 kHz, before the volume) for the viewer.
    audio_rx: Receiver<Vec<f32>>,
}

/// A running share.
pub struct ShareServer {
    inner: Arc<Inner>,
    audio_tx: Sender<Vec<f32>>,
    local: SocketAddr,
    accept: Option<JoinHandle<()>>,
}

impl ShareServer {
    /// Listen on `listen` (the default port when none).
    pub fn start(listen: &str, token: Option<String>) -> Result<Self, String> {
        let addr = with_port(listen);
        let listener = TcpListener::bind(&addr).map_err(|e| format!("{addr}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;
        // about 2 s of audio blocks; more is dropped
        let (audio_tx, audio_rx) = bounded(100);
        let inner = Arc::new(Inner {
            events: Mutex::new(Vec::new()),
            session: Mutex::new(None),
            quit: AtomicBool::new(false),
            token,
            audio_rx,
        });
        let i = Arc::clone(&inner);
        let accept = std::thread::Builder::new()
            .name("shellsdr-share".into())
            .spawn(move || accept_loop(listener, i))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            inner,
            audio_tx,
            local,
            accept: Some(accept),
        })
    }

    /// The address it listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Whether it listens beyond this machine.
    pub fn is_public(&self) -> bool {
        !self.local.ip().is_loopback()
    }

    /// The events since the last call.
    pub fn events(&self) -> Vec<ShareEvent> {
        self.inner
            .events
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default()
    }

    /// Whether a viewer is connected.
    pub fn connected(&self) -> bool {
        self.inner.session.lock().is_ok_and(|s| s.is_some())
    }

    /// Whether the viewer has taken the last cells (render a new frame
    /// only then: a slow link gets fewer frames, never a backlog).
    pub fn ready_for_cells(&self) -> bool {
        self.inner
            .session
            .lock()
            .is_ok_and(|s| s.as_ref().is_some_and(|s| s.out.is_empty()))
    }

    /// Send what changed on the viewer's screen.
    pub fn send_cells(&self, d: CellDiff) {
        if let Ok(s) = self.inner.session.lock()
            && let Some(s) = s.as_ref()
        {
            let _ = s.out.try_send(Out::Cells(d));
        }
    }

    /// Send the overlays (a graphical viewer; with the cells).
    pub fn send_overlays(&self, items: Vec<super::wire::Overlay>) {
        if let Ok(s) = self.inner.session.lock()
            && let Some(s) = s.as_ref()
        {
            let _ = s.out.try_send(Out::Control(Msg::Overlays { items }));
        }
    }

    /// Send a picture's pixels (a graphical viewer of 0.4.8 or newer);
    /// false when the viewer is behind (try again later).
    pub fn send_picture(&self, p: Arc<super::picture::Picture>) -> bool {
        self.inner.session.lock().is_ok_and(|s| {
            s.as_ref()
                .is_some_and(|s| s.out.try_send(Out::Picture(p)).is_ok())
        })
    }

    /// Send a row of an overlay stream at `bins` (a graphical viewer's
    /// waterfalls).
    pub fn send_rows(&self, stream: u8, row: &[f32], bins: u32) {
        if let Ok(s) = self.inner.session.lock()
            && let Some(s) = s.as_ref()
        {
            let _ = s
                .rows
                .try_send(super::wire::rows_payload(stream, row, bins as usize));
        }
    }

    /// Where the engine puts its audio for the viewer.
    pub fn audio_sender(&self) -> Sender<Vec<f32>> {
        self.audio_tx.clone()
    }

    /// Disconnect the viewer (the sharing side's `[Disconnect]`).
    pub fn disconnect(&self) {
        if let Ok(s) = self.inner.session.lock()
            && let Some(s) = s.as_ref()
        {
            let _ = s.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for ShareServer {
    fn drop(&mut self) {
        self.inner.quit.store(true, Ordering::Relaxed);
        self.disconnect();
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

fn push(inner: &Inner, ev: ShareEvent) {
    if let Ok(mut e) = inner.events.lock() {
        e.push(ev);
    }
}

fn accept_loop(listener: TcpListener, inner: Arc<Inner>) {
    let mut sessions: Vec<JoinHandle<()>> = Vec::new();
    while !inner.quit.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let _ = stream.set_nonblocking(false);
                let i = Arc::clone(&inner);
                sessions.push(std::thread::spawn(move || {
                    session(stream, peer.ip().to_string(), &i);
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
        sessions.retain(|h| !h.is_finished());
    }
    for h in sessions {
        let _ = h.join();
    }
}

/// One connection: the handshake, then (a viewer) its window.
fn session(stream: TcpStream, peer: String, inner: &Inner) {
    let refused = |reason: &str| {
        push(
            inner,
            ShareEvent::Refused {
                peer: peer.clone(),
                reason: reason.to_string(),
            },
        )
    };
    let _ = stream.set_nodelay(true);
    let Ok(mut rd) = stream.try_clone() else {
        return;
    };
    let mut w = stream;
    let _ = w.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = rd.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(Frame::Control(Msg::Hello {
        proto,
        token,
        want,
        version,
    })) = read_frame(&mut rd)
    else {
        return;
    };
    if proto != PROTO {
        refuse(w, &format!("protocol {proto} is not {PROTO}"));
        return refused("another protocol");
    }
    if let Some(want) = &inner.token
        && !token.as_deref().is_some_and(|t| same_secret(t, want))
    {
        refuse(w, "wrong token");
        return refused("wrong token");
    }
    let welcome = Msg::Welcome {
        proto: PROTO,
        version: crate::version::VERSION.to_string(),
        host: hostname(),
        opus: opus::available().is_ok(),
        kind: Some(WANT_WINDOW.to_string()),
    };
    if want.as_deref() != Some(WANT_WINDOW) {
        // a Network source asking: it learns this is a window, and leaves
        let _ = write_frame(&mut w, &Frame::Control(welcome));
        return;
    }
    let (out_tx, out_rx) = bounded::<Out>(4);
    let (rows_tx, rows_rx) = bounded::<(u8, Vec<u8>)>(8);
    {
        let Ok(mut s) = inner.session.lock() else {
            return;
        };
        if s.is_some() {
            refuse(w, "busy: another viewer is connected");
            return refused("busy");
        }
        let Ok(stream) = w.try_clone() else { return };
        *s = Some(Session {
            out: out_tx,
            rows: rows_tx,
            stream,
        });
    }
    let pictures = super::picture::takes_pictures(&version);
    let reason = window(
        &mut w,
        &mut rd,
        &peer,
        inner,
        welcome,
        (out_rx, rows_rx),
        pictures,
    );
    if let Ok(mut s) = inner.session.lock() {
        *s = None;
    }
    push(inner, ShareEvent::Disconnected { peer, reason });
}

/// The window session: returns why it ended.
fn window(
    w: &mut TcpStream,
    rd: &mut TcpStream,
    peer: &str,
    inner: &Inner,
    welcome: Msg,
    (out_rx, rows_rx): (Receiver<Out>, Receiver<(u8, Vec<u8>)>),
    pictures: bool,
) -> String {
    if write_frame(w, &Frame::Control(welcome)).is_err() {
        return "the viewer left".into();
    }
    let Ok(Frame::Control(Msg::Window {
        cols,
        rows,
        overlays,
        bins,
        codec,
        bitrate,
    })) = read_frame(rd)
    else {
        return "no window request".into();
    };
    let _ = rd.set_read_timeout(None);
    let mut packer = opus::Packer::new(codec, bitrate);
    let (codec, bitrate) = (packer.codec(), packer.bitrate());
    if write_frame(w, &Frame::Control(Msg::Started { codec, bitrate })).is_err() {
        return "the viewer left".into();
    }
    push(
        inner,
        ShareEvent::Connected {
            peer: peer.to_string(),
            cols,
            rows,
            overlays,
            bins,
            audio: super::wire::audio_label(codec, bitrate, None),
            pictures: overlays && pictures,
        },
    );
    // what the viewer sends comes in on a reader thread
    let Ok(mut r2) = rd.try_clone() else {
        return "socket".into();
    };
    let gone = Arc::new(AtomicBool::new(false));
    let g = Arc::clone(&gone);
    let events: Arc<Mutex<Vec<ShareEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let ev2 = Arc::clone(&events);
    let (set_tx, set_rx) = crossbeam_channel::unbounded::<(Codec, u32)>();
    let reader = std::thread::spawn(move || {
        while let Ok(f) = read_frame(&mut r2) {
            let ev = match f {
                Frame::Control(Msg::SetAudio { codec, bitrate }) => {
                    let _ = set_tx.send((codec, bitrate));
                    continue;
                }
                Frame::Control(Msg::Input { event }) => ShareEvent::Input(event),
                Frame::Control(Msg::Resize { cols, rows }) => ShareEvent::Resize { cols, rows },
                _ => continue,
            };
            if let Ok(mut e) = ev2.lock() {
                e.push(ev);
            }
        }
        g.store(true, Ordering::Relaxed);
    });
    // stale audio from before the viewer came
    while inner.audio_rx.try_recv().is_ok() {}
    let reason = loop {
        if inner.quit.load(Ordering::Relaxed) {
            break "sharing stopped".to_string();
        }
        if gone.load(Ordering::Relaxed) {
            break "the viewer left".to_string();
        }
        if let Ok(mut e) = events.lock() {
            for ev in e.drain(..) {
                push(inner, ev);
            }
        }
        let mut sent = false;
        while let Ok(o) = out_rx.try_recv() {
            let r = match o {
                Out::Cells(d) => write_frame(w, &Frame::Cells(d)),
                Out::Control(m) => write_frame(w, &Frame::Control(m)),
                Out::Picture(p) => {
                    let (kind, payload) = super::wire::picture_payload(&p);
                    super::wire::write_raw(w, kind, &payload)
                }
            };
            if r.is_err() {
                break;
            }
            sent = true;
        }
        while let Ok((kind, payload)) = rows_rx.try_recv() {
            let _ = super::wire::write_raw(w, kind, &payload);
            sent = true;
        }
        // the viewer changed how its audio travels: from the next packet on
        while let Ok((codec, bitrate)) = set_rx.try_recv() {
            packer = opus::Packer::new(codec, bitrate);
            let (codec, bitrate) = (packer.codec(), packer.bitrate());
            let _ = write_frame(w, &Frame::Control(Msg::AudioIs { codec, bitrate }));
            push(
                inner,
                ShareEvent::Audio(super::wire::audio_label(codec, bitrate, None)),
            );
            sent = true;
        }
        while let Ok(a) = inner.audio_rx.try_recv() {
            for p in packer.pack(&a) {
                let _ = write_frame(w, &Frame::Audio(p));
            }
            sent = true;
        }
        if !sent {
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let _ = w.shutdown(std::net::Shutdown::Both);
    let _ = reader.join();
    reason
}
