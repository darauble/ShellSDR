//! `shellsdr --serve`: the headless network server (spec 21.2).
//!
//! It owns the SDR (or serves baseband files given with `--serve-file`)
//! and runs the engine for one client at a time: the client starts it with
//! its settings, sends commands, and gets the engine's events and the audio
//! back. Nothing is recorded or decoded into files here; the client does
//! that with what it receives.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::opus;
use super::wire::{
    DEFAULT_PORT, Frame, Msg, PROTO, RemoteInput, StartReq, read_frame, spectrum_payload,
    write_frame, write_raw,
};
use crate::engine::{self, Command, EngineConfig, EngineHandle, Event, Input};
use crate::source::Capabilities;

/// What `--serve` was given.
#[derive(Debug, Clone, Default)]
pub struct ServeOptions {
    /// Address to listen on (`host:port`, or a host: the default port).
    pub addr: String,
    /// A shared secret the client must send (None: anyone who can connect).
    pub token: Option<String>,
    /// Baseband WAV files offered as inputs (looped).
    pub files: Vec<PathBuf>,
}

/// The listen address with the default port added when none is given.
pub fn with_port(addr: &str) -> String {
    let a = addr.trim();
    let has_port = match a.rsplit_once(':') {
        // `[::1]:7373`, `host:7373`; a bare IPv6 address has several colons
        Some((h, p)) => p.parse::<u16>().is_ok() && (!h.contains(':') || h.ends_with(']')),
        None => false,
    };
    if a.is_empty() {
        format!("127.0.0.1:{DEFAULT_PORT}")
    } else if has_port {
        a.to_string()
    } else {
        format!("{a}:{DEFAULT_PORT}")
    }
}

/// Inputs served by this server.
struct Ctx {
    token: Option<String>,
    files: Vec<(PathBuf, crate::source::file::FileMeta)>,
}

const FILE_ARGS: &str = "shellsdr-file=";

impl Ctx {
    fn inputs(&self) -> Vec<RemoteInput> {
        // driver modules print while enumerating
        let found = {
            let _hush = crate::engine::silence::StdioSilencer::new();
            crate::source::soapy::enumerate_with_timeout(Duration::from_secs(10))
        };
        let mut out: Vec<RemoteInput> = found
            .unwrap_or_default()
            .into_iter()
            .map(|d| RemoteInput {
                label: d.label,
                args: d.args,
                fixed_center_hz: None,
            })
            .collect();
        for (i, (p, m)) in self.files.iter().enumerate() {
            out.push(RemoteInput {
                label: format!(
                    "File: {}",
                    p.file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                ),
                args: format!("{FILE_ARGS}{i}"),
                fixed_center_hz: Some(m.center_hz.unwrap_or(0) as f64),
            });
        }
        out
    }

    fn file(&self, args: &str) -> Option<&(PathBuf, crate::source::file::FileMeta)> {
        let i: usize = args.strip_prefix(FILE_ARGS)?.parse().ok()?;
        self.files.get(i)
    }

    fn probe(&self, args: &str) -> Result<Capabilities, String> {
        if let Some((_, m)) = self.file(args) {
            let c = m.center_hz.unwrap_or(0) as f64;
            return Ok(Capabilities {
                sample_rates: vec![f64::from(m.sample_rate)],
                bandwidths: Vec::new(),
                gains: Vec::new(),
                antennas: Vec::new(),
                freq_range: (c, c),
                agc_supported: false,
            });
        }
        let _hush = crate::engine::silence::StdioSilencer::new();
        crate::source::soapy::SoapySource::open(args)
            .map(|s| s.capabilities().clone())
            .map_err(|e| e.to_string())
    }
}

/// Print a line to stderr with the time.
fn say(msg: impl std::fmt::Display) {
    let t = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S");
    eprintln!("{t} {msg}");
}

/// A bound server, not yet accepting.
pub struct Server {
    listener: TcpListener,
    ctx: Arc<Ctx>,
}

impl Server {
    /// Check the served files and listen.
    pub fn bind(opts: &ServeOptions) -> Result<Self, String> {
        let addr = with_port(&opts.addr);
        let mut files = Vec::new();
        for p in &opts.files {
            let meta =
                crate::source::file::probe(p).map_err(|e| format!("{}: {e}", p.display()))?;
            files.push((p.clone(), meta));
        }
        let listener =
            TcpListener::bind(&addr).map_err(|e| format!("cannot listen on {addr}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self {
            listener,
            ctx: Arc::new(Ctx {
                token: opts.token.clone(),
                files,
            }),
        })
    }

    /// The address it listens on.
    pub fn local_addr(&self) -> String {
        self.listener
            .local_addr()
            .map_or_else(|_| String::new(), |a| a.to_string())
    }

    /// Accept clients until `quit` is set.
    pub fn run(self, quit: Arc<AtomicBool>) -> Result<(), String> {
        say(format!(
            "ShellSDR {} serving on {}; audio: {}",
            crate::version::VERSION,
            self.local_addr(),
            match opus::available() {
                Ok(v) => v,
                Err(e) => format!("16-bit PCM ({e})"),
            }
        ));
        let loopback = self
            .listener
            .local_addr()
            .is_ok_and(|a| a.ip().is_loopback());
        if !loopback && self.ctx.token.is_none() {
            say(
                "warning: listening beyond this machine without --token: anyone who can reach the port can use the receiver",
            );
        }
        for (p, m) in &self.ctx.files {
            say(format!(
                "serving file {} ({} Hz, center {} Hz)",
                p.display(),
                m.sample_rate,
                m.center_hz.unwrap_or(0)
            ));
        }
        accept_loop(self.listener, self.ctx, quit);
        say("server stopped");
        Ok(())
    }
}

/// Run the server until `quit` is set.
pub fn serve(opts: ServeOptions, quit: Arc<AtomicBool>) -> Result<(), String> {
    Server::bind(&opts)?.run(quit)
}

fn accept_loop(listener: TcpListener, ctx: Arc<Ctx>, quit: Arc<AtomicBool>) {
    let busy = Arc::new(AtomicBool::new(false));
    let mut sessions = Vec::new();
    while !quit.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let _ = stream.set_nonblocking(false);
                // a client that just left (a query before the start) may
                // not be noticed yet: give its session a moment to end
                let t0 = Instant::now();
                while busy.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if busy.swap(true, Ordering::SeqCst) {
                    say(format!("{peer}: refused, another client is connected"));
                    refuse(stream, "busy: another client is connected");
                    continue;
                }
                say(format!("{peer}: connected"));
                let (ctx, busy, quit) = (Arc::clone(&ctx), Arc::clone(&busy), Arc::clone(&quit));
                sessions.push(std::thread::spawn(move || {
                    match session(stream, &ctx, &quit) {
                        Ok(()) => say(format!("{peer}: disconnected")),
                        Err(e) => say(format!("{peer}: {e}")),
                    }
                    busy.store(false, Ordering::SeqCst);
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => say(format!("accept: {e}")),
        }
        sessions.retain(|h| !h.is_finished());
    }
    for h in sessions {
        let _ = h.join();
    }
}

pub(super) fn refuse(mut stream: TcpStream, reason: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let _ = write_frame(
        &mut stream,
        &Frame::Control(Msg::Refused {
            reason: reason.to_string(),
        }),
    );
}

/// Whether two secrets are equal, in time that does not depend on where
/// they differ.
pub(super) fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// One client: the handshake, then its requests until it leaves.
fn session(stream: TcpStream, ctx: &Ctx, quit: &AtomicBool) -> Result<(), String> {
    let _ = stream.set_nodelay(true);
    let mut rd = stream.try_clone().map_err(|e| e.to_string())?;
    let mut w = stream;
    w.set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    rd.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let hello = read_frame(&mut rd).map_err(|e| format!("no hello: {e}"))?;
    let Frame::Control(Msg::Hello {
        proto,
        version,
        token,
        want,
    }) = hello
    else {
        return Err("the first message was not a hello".into());
    };
    if proto != PROTO {
        refuse(
            w,
            &format!("protocol {proto} is not {PROTO}: update ShellSDR on both ends"),
        );
        return Err(format!("client {version} speaks protocol {proto}"));
    }
    if let Some(want) = &ctx.token
        && !token.as_deref().is_some_and(|t| same_secret(t, want))
    {
        refuse(w, "wrong token");
        return Err("refused: wrong token".into());
    }
    if want.as_deref() == Some(super::wire::WANT_WINDOW) {
        refuse(
            w,
            "this server is headless (shellsdr --serve): use the Network source's devices",
        );
        return Err("refused: a window was asked of a headless server".into());
    }
    rd.set_read_timeout(None).map_err(|e| e.to_string())?;
    send(
        &mut w,
        Msg::Welcome {
            proto: PROTO,
            version: crate::version::VERSION.to_string(),
            host: hostname(),
            opus: opus::available().is_ok(),
            kind: Some(super::wire::WANT_ENGINE.to_string()),
        },
    )?;
    say(format!("client ShellSDR {version} accepted"));

    // The client's messages arrive on a reader thread.
    let (tx, rx) = crossbeam_channel::unbounded::<Msg>();
    std::thread::spawn(move || {
        while let Ok(f) = read_frame(&mut rd) {
            if let Frame::Control(m) = f
                && tx.send(m).is_err()
            {
                break;
            }
        }
    });

    let mut run: Option<Running> = None;
    let result = loop {
        if quit.load(Ordering::Relaxed) {
            break Ok(());
        }
        let mut idle = true;
        loop {
            match rx.try_recv() {
                Ok(m) => {
                    idle = false;
                    if let Err(e) = handle(m, ctx, &mut run, &mut w) {
                        stop(&mut run);
                        return Err(e);
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    stop(&mut run);
                    return Ok(());
                }
            }
        }
        if let Some(r) = &mut run {
            match r.pump(&mut w) {
                Ok(sent) => idle &= !sent,
                Err(e) => break Err(e),
            }
        }
        if idle {
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    stop(&mut run);
    result
}

fn send(w: &mut TcpStream, m: Msg) -> Result<(), String> {
    write_frame(w, &Frame::Control(m)).map_err(|e| format!("send: {e}"))
}

pub(super) fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "server".into())
}

fn stop(run: &mut Option<Running>) {
    if let Some(r) = run.take() {
        let _ = Box::new(r.engine).stop();
        say("engine stopped");
    }
}

/// Act on one client message.
fn handle(m: Msg, ctx: &Ctx, run: &mut Option<Running>, w: &mut TcpStream) -> Result<(), String> {
    match m {
        Msg::ListInputs => send(
            w,
            Msg::Inputs {
                inputs: ctx.inputs(),
            },
        ),
        Msg::Probe { args } => {
            // a running device cannot be opened twice: its capabilities
            // are known
            let caps = match run.as_ref().filter(|r| r.args == args) {
                Some(r) => Ok(r.caps.clone()),
                None => ctx.probe(&args),
            };
            let (caps, error) = match caps {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(e)),
            };
            send(w, Msg::Caps { args, caps, error })
        }
        Msg::Start { req } => {
            stop(run);
            match Running::start(req, ctx) {
                Ok(r) => {
                    let (codec, bitrate) = (r.audio.codec(), r.audio.bitrate());
                    say(format!(
                        "engine started: {} at {} Hz, audio {}",
                        r.label,
                        r.rate,
                        super::wire::audio_label(codec, bitrate, None)
                    ));
                    *run = Some(r);
                    send(w, Msg::Started { codec, bitrate })
                }
                Err(e) => {
                    say(format!("start failed: {e}"));
                    send(w, Msg::Stopped { reason: Some(e) })
                }
            }
        }
        Msg::Cmd { cmd } => {
            if let Some(r) = run {
                r.engine.command(cmd);
            }
            Ok(())
        }
        Msg::SetAudio { codec, bitrate } => {
            let Some(r) = run else { return Ok(()) };
            r.audio = opus::Packer::new(codec, bitrate);
            let (codec, bitrate) = (r.audio.codec(), r.audio.bitrate());
            say(format!(
                "audio now {}",
                super::wire::audio_label(codec, bitrate, None)
            ));
            send(w, Msg::AudioIs { codec, bitrate })
        }
        Msg::Stop => {
            stop(run);
            send(w, Msg::Stopped { reason: None })
        }
        // nothing else is a client's
        _ => Ok(()),
    }
}

/// The engine running for the client.
struct Running {
    engine: engine::Engine,
    args: String,
    label: String,
    rate: f64,
    caps: Capabilities,
    /// A served file starts again at its end.
    looped: bool,
    /// The audio as sent.
    audio: opus::Packer,
    bins: usize,
    meter: Option<Option<crate::vfo::types::DecoderMeter>>,
    stats_at: Option<Instant>,
}

impl Running {
    fn start(req: StartReq, ctx: &Ctx) -> Result<Self, String> {
        let (input, label, looped) = match ctx.file(&req.args) {
            Some((p, _)) => (
                Input::File {
                    path: p.to_string_lossy().into_owned(),
                    start: 0,
                },
                p.display().to_string(),
                true,
            ),
            None => (
                Input::Soapy {
                    device_args: req.args.clone(),
                },
                req.args.clone(),
                false,
            ),
        };
        let caps = ctx.probe(&req.args)?;
        let audio = opus::Packer::new(req.codec, req.bitrate);
        let engine = engine::start(EngineConfig {
            input,
            sample_rate: req.sample_rate,
            decimation: req.decimation,
            center_hz: req.center_hz,
            vfo_hz: req.vfo_hz,
            vfo_type: req.vfo_type,
            settings: req.settings,
            audio_target: Default::default(),
            audio_tap: true,
            gains: req.gains,
            agc: req.agc,
            antenna: req.antenna,
            hw_bandwidth_hz: req.hw_bandwidth_hz,
            fft_enabled: req.fft_enabled,
        })?;
        Ok(Self {
            engine,
            args: req.args,
            label,
            rate: req.sample_rate,
            caps,
            looped,
            audio,
            bins: req.spectrum_bins as usize,
            meter: None,
            stats_at: None,
        })
    }

    /// Send what the engine produced; returns whether anything was sent.
    fn pump(&mut self, w: &mut TcpStream) -> Result<bool, String> {
        let mut sent = false;
        let audio = self.engine.take_audio();
        if !audio.is_empty() {
            for p in self.audio.pack(&audio) {
                write_frame(w, &Frame::Audio(p)).map_err(|e| format!("send: {e}"))?;
                sent = true;
            }
        }
        for ev in self.engine.poll() {
            let r = match ev {
                Event::Spectrum(s) => {
                    let (kind, payload) = spectrum_payload(&s, self.bins);
                    write_raw(w, kind, &payload)
                }
                Event::View(v) => write_frame(w, &Frame::View(v)),
                Event::Data { pos, items } => write_frame(w, &Frame::Data { pos, items }),
                Event::Meter(m) => {
                    if self.meter == Some(m) {
                        continue;
                    }
                    self.meter = Some(m);
                    write_frame(w, &Frame::Control(Msg::Meter { meter: m }))
                }
                Event::Stats(st) => {
                    if self.looped && st.file_eof {
                        // a served file plays on from the start
                        self.engine.command(Command::Seek(0));
                    }
                    if self
                        .stats_at
                        .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
                    {
                        continue;
                    }
                    self.stats_at = Some(Instant::now());
                    write_frame(w, &Frame::Control(Msg::Stats { stats: st }))
                }
                Event::Notice(text) => write_frame(w, &Frame::Control(Msg::Notice { text })),
                Event::Failed(reason) => write_frame(
                    w,
                    &Frame::Control(Msg::Stopped {
                        reason: Some(reason),
                    }),
                ),
                Event::Repaint => continue,
            };
            r.map_err(|e| format!("send: {e}"))?;
            sent = true;
        }
        if sent {
            let _ = w.flush();
        }
        Ok(sent)
    }
}

/// Set by SIGINT / SIGTERM.
static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::Relaxed);
}

/// `shellsdr --serve`: serve until Ctrl+C (or SIGTERM), releasing the
/// device cleanly.
pub fn run_until_signal(opts: ServeOptions) -> Result<(), String> {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    let quit = Arc::new(AtomicBool::new(false));
    let q = Arc::clone(&quit);
    std::thread::spawn(move || {
        while !SIGNALLED.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        q.store(true, Ordering::Relaxed);
    });
    serve(opts, quit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_port_is_added_when_missing() {
        assert_eq!(with_port(""), "127.0.0.1:7373");
        assert_eq!(with_port("0.0.0.0"), "0.0.0.0:7373");
        assert_eq!(with_port("pi4:9000"), "pi4:9000");
        assert_eq!(with_port("[::1]:9000"), "[::1]:9000");
        assert_eq!(with_port("[::1]"), "[::1]:7373");
    }

    #[test]
    fn secrets_compare_whole() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "abcd"));
    }
}
