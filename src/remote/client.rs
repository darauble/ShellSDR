//! The client side of the network remote (spec 21.2): queries for the
//! Network source's view, and [`RemoteEngine`], which the run loop drives
//! like the local engine.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use super::opus;
use super::wire::{Codec, Frame, Msg, PROTO, RemoteInput, StartReq, read_frame, write_frame};
use crate::engine::{Command, EngineConfig, EngineHandle, Event, RecTaps};
use crate::record::recorder::RecSink;
use crate::source::Capabilities;
use crate::vfo::types::DataItem;

/// How long a connection or a request may take.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Starting a device can take a while (an RSP opens slowly).
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// Spectrum bins asked for: a terminal is at most a few hundred columns
/// (braille: two dots each); the peak of each group is kept.
pub const SPECTRUM_BINS: u32 = 1024;
/// Audio held before playing (and again after the output ran dry), to ride
/// over network jitter.
const PREBUFFER: usize = 4_800;
/// More audio than this waiting is dropped (latency stays low).
const MAX_QUEUED: usize = 24_000;

/// What the server said about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerInfo {
    pub version: String,
    pub host: String,
    pub opus: bool,
    /// A sharing ShellSDR (a window, 0.4.1), not a headless engine server.
    pub window: bool,
}

/// A connection after the handshake.
pub struct Conn {
    pub(super) w: TcpStream,
    pub(super) rd: TcpStream,
    pub info: ServerInfo,
}

/// Connect to `addr` (`host:port`; the default port when none) and shake
/// hands, for the engine.
pub fn connect(addr: &str, token: Option<&str>) -> Result<Conn, String> {
    connect_for(addr, token, super::wire::WANT_ENGINE)
}

/// Connect and shake hands wanting `want` (engine or window).
pub fn connect_for(addr: &str, token: Option<&str>, want: &str) -> Result<Conn, String> {
    let addr = super::server::with_port(addr);
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| format!("{addr}: {e}"))?
        .collect();
    let mut last = format!("{addr}: no address");
    let mut stream = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, TIMEOUT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    let w = stream.ok_or(last)?;
    let _ = w.set_nodelay(true);
    w.set_write_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    let rd = w.try_clone().map_err(|e| e.to_string())?;
    let mut c = Conn {
        w,
        rd,
        info: ServerInfo {
            version: String::new(),
            host: String::new(),
            opus: false,
            window: false,
        },
    };
    c.send(Msg::Hello {
        proto: PROTO,
        version: crate::version::VERSION.to_string(),
        token: token.filter(|t| !t.is_empty()).map(str::to_string),
        want: Some(want.to_string()),
    })?;
    match c.wait(TIMEOUT, |m| {
        matches!(m, Msg::Welcome { .. } | Msg::Refused { .. })
    })? {
        Msg::Welcome {
            version,
            host,
            opus,
            kind,
            ..
        } => {
            c.info = ServerInfo {
                version,
                host,
                opus,
                window: kind.as_deref() == Some(super::wire::WANT_WINDOW),
            };
            Ok(c)
        }
        Msg::Refused { reason } => Err(format!("refused: {reason}")),
        _ => unreachable!(),
    }
}

impl Conn {
    pub(super) fn send(&mut self, m: Msg) -> Result<(), String> {
        write_frame(&mut self.w, &Frame::Control(m)).map_err(|e| format!("send: {e}"))
    }

    /// Read until a control message `want` accepts (other frames are
    /// skipped).
    pub(super) fn wait(
        &mut self,
        timeout: Duration,
        want: impl Fn(&Msg) -> bool,
    ) -> Result<Msg, String> {
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the server did not answer".into());
            }
            self.rd
                .set_read_timeout(Some(left))
                .map_err(|e| e.to_string())?;
            match read_frame(&mut self.rd) {
                Ok(Frame::Control(m)) if want(&m) => return Ok(m),
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err("the server did not answer".into());
                }
                Err(e) => return Err(format!("connection: {e}")),
            }
        }
    }

    /// The inputs the server offers.
    pub fn list_inputs(&mut self) -> Result<Vec<RemoteInput>, String> {
        self.send(Msg::ListInputs)?;
        // enumerating SoapySDR modules can be slow
        match self.wait(START_TIMEOUT, |m| matches!(m, Msg::Inputs { .. }))? {
            Msg::Inputs { inputs } => Ok(inputs),
            _ => unreachable!(),
        }
    }

    /// An input's capabilities.
    pub fn probe(&mut self, args: &str) -> Result<Capabilities, String> {
        self.send(Msg::Probe {
            args: args.to_string(),
        })?;
        let a = args.to_string();
        match self.wait(
            START_TIMEOUT,
            move |m| matches!(m, Msg::Caps { args, .. } if *args == a),
        )? {
            Msg::Caps { caps, error, .. } => {
                caps.ok_or_else(|| error.unwrap_or_else(|| "no capabilities".into()))
            }
            _ => unreachable!(),
        }
    }
}

/// What the Network view learns from one query.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub info: ServerInfo,
    pub inputs: Vec<RemoteInput>,
    /// The capabilities of the input asked about.
    pub caps: Option<Result<Capabilities, String>>,
}

/// Connect, list the inputs and probe `args` (or the first input).
pub fn query(addr: &str, token: Option<&str>, args: Option<&str>) -> Result<Query, String> {
    let mut c = connect(addr, token)?;
    if c.info.window {
        // a sharing ShellSDR: it offers its screen, not inputs
        return Ok(Query {
            info: c.info.clone(),
            inputs: Vec::new(),
            caps: None,
        });
    }
    let inputs = c.list_inputs()?;
    let pick = args
        .filter(|a| inputs.iter().any(|i| i.args == *a))
        .map(str::to_string)
        .or_else(|| inputs.first().map(|i| i.args.clone()));
    let caps = pick.map(|a| c.probe(&a));
    Ok(Query {
        info: c.info.clone(),
        inputs,
        caps,
    })
}

/// Connect and probe one input.
pub fn probe(addr: &str, token: Option<&str>, args: &str) -> Result<Capabilities, String> {
    connect(addr, token)?.probe(args)
}

/// Where the Network source connects and what it asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub addr: String,
    pub token: Option<String>,
    /// The server's input (its `RemoteInput::args`).
    pub args: String,
    /// Opus bit rate; 0 = 16-bit PCM.
    pub bitrate: u32,
}

/// Events from the reader thread, latest-only kinds collapsed.
#[derive(Default)]
struct Inbox {
    events: Vec<Event>,
}

impl Inbox {
    fn push(&mut self, ev: Event) {
        let same = |e: &Event| {
            std::mem::discriminant(e) == std::mem::discriminant(&ev)
                && matches!(e, Event::Spectrum(_) | Event::View(_) | Event::Stats(_))
        };
        if let Some(i) = self.events.iter().position(same) {
            self.events[i] = ev;
        } else {
            self.events.push(ev);
        }
    }
}

/// A server's engine, driven over the network.
pub struct RemoteEngine {
    w: Mutex<TcpStream>,
    inbox: Arc<Mutex<Inbox>>,
    quitting: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    volume: Arc<AtomicU32>,
    native: Arc<Mutex<Option<RecSink>>>,
    /// A copy of the audio for a viewer of this screen (0.4.1).
    tee: Arc<Mutex<Option<Sender<Vec<f32>>>>>,
    /// The audio output (None with an audio tap).
    _audio: Option<crate::engine::audio::AudioOutput>,
    /// The audio tap's receiving end (`EngineConfig::audio_tap`).
    tap: Option<crossbeam_channel::Receiver<f32>>,
    /// How the audio travels (0.4.3).
    link: Arc<LinkAudio>,
}

impl RemoteEngine {
    /// Connect and start the server's engine with `cfg`.
    pub fn start(target: &Target, cfg: &EngineConfig) -> Result<Self, String> {
        let mut c = connect(&target.addr, target.token.as_deref())?;
        let link = Arc::new(LinkAudio::new(&c.info, target.bitrate));
        let codec = link.ask(target.bitrate);
        c.send(Msg::Start {
            req: StartReq {
                args: target.args.clone(),
                sample_rate: cfg.sample_rate,
                decimation: cfg.decimation,
                center_hz: cfg.center_hz,
                vfo_hz: cfg.vfo_hz,
                vfo_type: cfg.vfo_type.clone(),
                settings: cfg.settings.clone(),
                gains: cfg.gains.clone(),
                agc: cfg.agc,
                antenna: cfg.antenna.clone(),
                hw_bandwidth_hz: cfg.hw_bandwidth_hz,
                fft_enabled: cfg.fft_enabled,
                spectrum_bins: SPECTRUM_BINS,
                codec,
                bitrate: target.bitrate,
            },
        })?;
        let codec = match c.wait(START_TIMEOUT, |m| {
            matches!(m, Msg::Started { .. } | Msg::Stopped { .. })
        })? {
            Msg::Started { codec, bitrate } => {
                link.now(codec, bitrate);
                codec
            }
            Msg::Stopped { reason } => {
                return Err(reason.unwrap_or_else(|| "the server did not start".into()));
            }
            _ => unreachable!(),
        };
        let mut decoder = decoder_for(codec)?;
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let (audio_tx, audio_rx) = crossbeam_channel::bounded::<f32>(48_000);
        let (audio, tap) = if cfg.audio_tap {
            (None, Some(audio_rx))
        } else {
            let out = crate::engine::audio::AudioOutput::new(
                48_000,
                audio_rx,
                Arc::clone(&volume),
                &cfg.audio_target,
            )
            .map_err(|e| e.to_string())?;
            (Some(out), None)
        };
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let quitting = Arc::new(AtomicBool::new(false));
        let native: Arc<Mutex<Option<RecSink>>> = Arc::new(Mutex::new(None));
        let tee: Arc<Mutex<Option<Sender<Vec<f32>>>>> = Arc::new(Mutex::new(None));
        let Conn { w, mut rd, .. } = c;
        rd.set_read_timeout(None).map_err(|e| e.to_string())?;
        let reader = {
            let (inbox, quitting, native, tee, link) = (
                Arc::clone(&inbox),
                Arc::clone(&quitting),
                Arc::clone(&native),
                Arc::clone(&tee),
                Arc::clone(&link),
            );
            std::thread::Builder::new()
                .name("shellsdr-remote".into())
                .spawn(move || {
                    let mut jitter = Jitter::default();
                    let push = |ev| {
                        if let Ok(mut i) = inbox.lock() {
                            i.push(ev);
                        }
                    };
                    loop {
                        let f = match read_frame(&mut rd) {
                            Ok(f) => f,
                            Err(e) => {
                                if !quitting.load(Ordering::Relaxed) {
                                    push(Event::Failed(
                                        if e.kind() == std::io::ErrorKind::UnexpectedEof {
                                            "the server closed the connection".to_string()
                                        } else {
                                            format!("connection lost: {e}")
                                        },
                                    ));
                                }
                                break;
                            }
                        };
                        match f {
                            Frame::Audio(p) => {
                                let samples = decode_audio(&mut decoder, &p);
                                if let Ok(n) = native.lock()
                                    && let Some(sink) = n.as_ref()
                                {
                                    sink.push(samples.clone());
                                }
                                if let Ok(t) = tee.lock()
                                    && let Some(tx) = t.as_ref()
                                {
                                    let _ = tx.try_send(samples.clone());
                                }
                                jitter.feed(samples, &audio_tx);
                            }
                            Frame::Spectrum(s) => push(Event::Spectrum(s)),
                            Frame::View(v) => push(Event::View(v)),
                            Frame::Data { pos, items } => push(Event::Data { pos, items }),
                            Frame::Control(Msg::Stats { stats }) => push(Event::Stats(stats)),
                            Frame::Control(Msg::Meter { meter }) => push(Event::Meter(meter)),
                            Frame::Control(Msg::Notice { text }) => push(Event::Notice(text)),
                            Frame::Control(Msg::AudioIs { codec, bitrate }) => {
                                match decoder_for(codec) {
                                    Ok(d) => decoder = d,
                                    Err(e) => push(Event::Notice(e)),
                                }
                                link.now(codec, bitrate);
                            }
                            Frame::Control(Msg::Stopped { reason }) => {
                                if !quitting.load(Ordering::Relaxed) {
                                    push(Event::Failed(reason.unwrap_or_else(|| {
                                        "the server stopped the receiver".into()
                                    })));
                                }
                                break;
                            }
                            Frame::Control(_)
                            | Frame::Cells(_)
                            | Frame::Rows { .. }
                            | Frame::Picture(_) => {}
                        }
                    }
                })
                .map_err(|e| e.to_string())?
        };
        Ok(Self {
            w: Mutex::new(w),
            inbox,
            quitting,
            reader: Some(reader),
            volume,
            native,
            tee,
            _audio: audio,
            tap,
            link,
        })
    }

    /// Take the audio received since the last call (with an audio tap).
    pub fn take_audio(&self) -> Vec<f32> {
        self.tap
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    fn send(&self, m: Msg) {
        let r = self
            .w
            .lock()
            .map_err(|_| "lock".to_string())
            .and_then(|mut w| write_frame(&mut *w, &Frame::Control(m)).map_err(|e| e.to_string()));
        if let Err(e) = r
            && !self.quitting.load(Ordering::Relaxed)
            && let Ok(mut i) = self.inbox.lock()
        {
            i.push(Event::Failed(format!("connection lost: {e}")));
        }
    }
}

impl EngineHandle for RemoteEngine {
    fn command(&self, cmd: Command) {
        match cmd {
            // the audio is played here
            Command::SetVolume(v) => self
                .volume
                .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed),
            cmd => self.send(Msg::Cmd { cmd }),
        }
    }

    fn record(&self, taps: RecTaps) {
        // The native recording is the audio as received; a baseband needs
        // the IQ, which stays on the server.
        if taps.baseband.is_some()
            && let Ok(mut i) = self.inbox.lock()
        {
            i.push(Event::Notice(
                "A baseband recording is not made over the network".into(),
            ));
        }
        if let Ok(mut n) = self.native.lock() {
            *n = taps.native;
        }
    }

    fn records_baseband(&self) -> bool {
        false
    }

    fn net_audio(&self) -> Option<String> {
        Some(self.link.label())
    }

    fn set_net_audio(&self, bitrate: u32) -> Result<(), String> {
        let m = self.link.change(bitrate)?;
        self.send(m);
        Ok(())
    }

    fn set_audio_tee(&self, tx: Option<Sender<Vec<f32>>>) {
        if let Ok(mut t) = self.tee.lock() {
            *t = tx;
        }
    }

    fn poll(&self) -> Vec<Event> {
        self.inbox
            .lock()
            .map(|mut i| std::mem::take(&mut i.events))
            .unwrap_or_default()
    }

    fn stop(mut self: Box<Self>) -> Vec<DataItem> {
        self.quitting.store(true, Ordering::Relaxed);
        self.send(Msg::Stop);
        if let Ok(w) = self.w.lock() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
        if let Ok(mut n) = self.native.lock() {
            n.take();
        }
        self.poll()
            .into_iter()
            .flat_map(|e| match e {
                Event::Data { items, .. } => items,
                _ => Vec::new(),
            })
            .collect()
    }
}

/// How a connection's audio travels, for the user, and asking to change
/// it while connected (0.4.3): shared by the reader thread, which learns
/// what the server sends, and the UI.
pub(super) struct LinkAudio {
    server_opus: bool,
    server_version: String,
    /// The bit rate last asked for (0: PCM).
    asked: AtomicU32,
    label: Mutex<String>,
}

impl LinkAudio {
    pub(super) fn new(info: &ServerInfo, asked: u32) -> Self {
        Self {
            server_opus: info.opus,
            server_version: info.version.clone(),
            asked: AtomicU32::new(asked),
            label: Mutex::new(String::new()),
        }
    }

    /// The codec to ask for at `bitrate` (and remember it was asked).
    pub(super) fn ask(&self, bitrate: u32) -> Codec {
        self.asked.store(bitrate, Ordering::Relaxed);
        super::wire::wanted_codec(bitrate, self.server_opus).0
    }

    /// The server sends `codec` at `bitrate` (0: older servers say none).
    pub(super) fn now(&self, codec: Codec, bitrate: u32) {
        let asked = self.asked.load(Ordering::Relaxed);
        let bitrate = if bitrate == 0 && codec == Codec::Opus {
            asked
        } else {
            bitrate
        };
        let why = (codec == Codec::Pcm16 && asked > 0).then(|| {
            super::wire::wanted_codec(asked, self.server_opus)
                .1
                .unwrap_or("Opus failed on the server")
        });
        if let Ok(mut l) = self.label.lock() {
            *l = super::wire::audio_label(codec, bitrate, why);
        }
    }

    pub(super) fn label(&self) -> String {
        self.label.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// The message changing the audio to `bitrate`, unless the server is
    /// too old for it.
    pub(super) fn change(&self, bitrate: u32) -> Result<Msg, String> {
        if !super::wire::changes_audio(&self.server_version) {
            return Err(format!(
                "the server (ShellSDR {}) changes it only on a new connection",
                self.server_version
            ));
        }
        let codec = self.ask(bitrate);
        Ok(Msg::SetAudio { codec, bitrate })
    }
}

/// A decoder for `codec` (None: PCM).
pub(super) fn decoder_for(codec: Codec) -> Result<Option<opus::Decoder>, String> {
    match codec {
        Codec::Opus => opus::Decoder::new().map(Some),
        Codec::Pcm16 => Ok(None),
    }
}

/// An audio packet's samples: Opus with a decoder, else 16-bit PCM.
pub(super) fn decode_audio(dec: &mut Option<opus::Decoder>, p: &[u8]) -> Vec<f32> {
    match dec {
        Some(d) => d.decode(p),
        None => super::wire::pcm16_decode(p),
    }
}

/// Holds audio back until enough is there to ride over network jitter,
/// again whenever the output ran dry; drops it when too much waits.
#[derive(Default)]
pub(super) struct Jitter {
    held: Vec<f32>,
    primed: bool,
}

impl Jitter {
    pub(super) fn feed(&mut self, samples: Vec<f32>, tx: &Sender<f32>) {
        if self.primed && tx.is_empty() {
            // ran dry: build the cushion again
            self.primed = false;
        }
        if !self.primed {
            self.held.extend(samples);
            if self.held.len() < PREBUFFER {
                return;
            }
            self.primed = true;
            for s in self.held.drain(..) {
                let _ = tx.try_send(s);
            }
            return;
        }
        if tx.len() > MAX_QUEUED {
            return;
        }
        for s in samples {
            let _ = tx.try_send(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jitter_buffer_waits_for_a_cushion_and_caps_the_latency() {
        let (tx, rx) = crossbeam_channel::bounded::<f32>(48_000);
        let mut j = Jitter::default();
        j.feed(vec![0.0; 960], &tx);
        assert!(rx.is_empty(), "held back");
        j.feed(vec![0.0; PREBUFFER], &tx);
        assert_eq!(rx.len(), 960 + PREBUFFER, "released at once");
        j.feed(vec![0.0; 960], &tx);
        assert_eq!(rx.len(), 960 * 2 + PREBUFFER, "then passed on");
        rx.try_iter().count();
        j.feed(vec![0.0; 960], &tx);
        assert!(rx.is_empty(), "ran dry: held back again");
        let mut j = Jitter {
            primed: true,
            ..Jitter::default()
        };
        for _ in 0..40 {
            j.feed(vec![0.0; 960], &tx);
        }
        assert!(rx.len() <= MAX_QUEUED + 960, "{}", rx.len());
    }

    #[test]
    fn the_inbox_keeps_only_the_latest_frame_of_a_kind() {
        let mut i = Inbox::default();
        i.push(Event::Spectrum(vec![1.0]));
        i.push(Event::Data {
            pos: 0,
            items: Vec::new(),
        });
        i.push(Event::Spectrum(vec![2.0]));
        i.push(Event::Data {
            pos: 1,
            items: Vec::new(),
        });
        assert_eq!(i.events.len(), 3, "data is never dropped");
        assert_eq!(i.events[0], Event::Spectrum(vec![2.0]));
    }
}
