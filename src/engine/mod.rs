//! Runtime engine: the DSP thread and its supervisor (hardware).
//!
//! Piece B2. The engine owns the live receive pipeline: a SoapySDR RX stream
//! feeds `VfoChain` (NCO -> decimate -> demod -> resample to 48 kHz), whose
//! audio is pushed to the cpal output. A `Supervisor` owns the thread's
//! lifecycle (Start/Stop) and a command channel for runtime changes (tune,
//! mode, volume). This module touches hardware and is verified by ear, not by
//! the unit suite; the pure DSP it drives (`VfoChain`) is already tested.
//!
//! The IQ comes from a SoapySDR device or, for the File source (spec 7.2),
//! from a baseband WAV played in real time (`source::file::Pacer`) in SDR++'s
//! block size (sample rate / 200), with seeks and an end-of-file flag.

pub mod audio;
pub mod audio_target;
pub mod silence;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded};
use num_complex::Complex32;

use crate::dsp::fft::{SpectrumAnalyzer, SpectrumFrame};
use crate::dsp::power_decimator::PowerDecimator;
use crate::record::recorder::RecSink;
use crate::source::file::{IqReader, Pacer};
use crate::source::soapy::SoapySource;
use crate::vfo::types::{DataItem, Settings, VfoProcessor};

/// FFT size for the power spectrum (bins). Modest for Pi; adjustable later.
pub const FFT_SIZE: usize = 4096;
/// Spectrum frame rate (frames per second).
pub const FFT_FPS: f64 = 20.0;
/// Audio output (and audio recording) sample rate in Hz.
pub const AUDIO_RATE: u32 = 48_000;
/// Spectrum time-averaging IIR coefficient (1.0 = no averaging).
pub const FFT_AVG_ALPHA: f32 = 0.5;
/// FFT size and frame rate of the VFO view's channel and audio spectra
/// (spec 13.5: small, for the Pi 3).
pub const VIEW_FFT_SIZE: usize = 512;
pub const VIEW_FPS: f64 = 10.0;

/// What the VFO view shows, from the DSP thread (spec 13.5).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VfoViewFrame {
    /// DC-centered dB spectrum of the channel IQ (before the channel
    /// filter), at `zoom_rate`; DC is the channel center.
    pub zoom_db: Vec<f32>,
    pub zoom_rate: f64,
    /// The channel center relative to the VFO's dial (Hz).
    pub center_offset: f64,
    /// DC-centered dB spectrum of the 48 kHz audio (empty for a type
    /// without audio).
    pub audio_db: Vec<f32>,
    /// Channel level after the channel filter, dBFS.
    pub level_db: f32,
    /// The AGC gain, dB (None when the mode has none).
    pub agc_gain_db: Option<f32>,
}

/// Where the engine's IQ comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A SoapySDR device, by its args string.
    Soapy { device_args: String },
    /// A baseband WAV file, starting at frame `start`.
    File { path: String, start: u64 },
    /// Another ShellSDR's input (`args`) over the network: started by
    /// `engine_app::start_engine` as a remote engine, not here.
    Network {
        addr: String,
        token: Option<String>,
        args: String,
        /// Opus bit rate; 0 = 16-bit PCM.
        bitrate: u32,
    },
}

/// Parameters needed to start the engine.
pub struct EngineConfig {
    pub input: Input,
    /// Device (capture) sample rate in Hz.
    pub sample_rate: f64,
    /// Software decimation ratio applied right after capture (1 = none);
    /// everything downstream runs at `sample_rate / decimation`.
    pub decimation: u32,
    pub center_hz: f64,
    pub vfo_hz: f64,
    /// The active VFO's type and its settings (spec 13.2).
    pub vfo_type: String,
    pub settings: Settings,
    /// Output audio target for the active VFO (default / cpal device / PW sink).
    pub audio_target: crate::engine::audio_target::AudioTarget,
    /// Hand the audio to [`Engine::take_audio`] instead of playing it (the
    /// network server, 0.4.0).
    pub audio_tap: bool,
    /// Gain elements to set at startup: (name, dB).
    pub gains: Vec<(String, f64)>,
    pub agc: bool,
    /// Antenna port to select at startup (None = device default).
    pub antenna: Option<String>,
    /// Analog (hardware) filter bandwidth in Hz (None = device default).
    pub hw_bandwidth_hz: Option<f64>,
    /// Whether the FFT/spectrum branch should run at startup (low-fi off).
    pub fft_enabled: bool,
}

/// Commands from the UI to a running engine (0.4.0: plain data, so a remote
/// engine can send them over the network; recording taps, which hold local
/// writers, go through [`EngineHandle::record`] instead).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[allow(clippy::enum_variant_names)]
pub enum Command {
    /// Retune the device center frequency (Hz).
    SetCenter(f64),
    /// Set the active VFO absolute frequency (Hz); the thread derives offset.
    SetVfo(f64),
    /// Change the VFO's type and/or its settings (rebuilds the processor).
    SetVfoSettings(String, Settings),
    /// Set a named gain element (name, dB), applied live to the device.
    SetGain(String, f64),
    /// Toggle the device's hardware AGC, applied live.
    SetAgc(bool),
    /// Select the antenna port, applied live.
    SetAntenna(String),
    /// Set the analog (hardware) filter bandwidth in Hz, applied live.
    SetHwBandwidth(f64),
    /// Change the source decimation ratio; rebuilds the decimator, the FFT
    /// analyzer and the chain at the new effective rate (SDR++ does this live).
    SetDecimation(u32),
    /// File source: continue playing at this frame.
    Seek(u64),
    /// The output volume, 0.0..=1.0 (applied where the audio is played).
    SetVolume(f32),
    /// Whether the VFO view is on screen (its spectra cost CPU only then).
    SetView(bool),
    /// Whether the wideband spectrum runs (low-fi: off when both displays
    /// are off).
    SetFft(bool),
    /// File source: play as fast as the CPU allows (true) or in real time.
    SetFileFast(bool),
    /// File source at Max: pause reading while a decoder is behind.
    SetFileHold(bool),
}

/// What the DSP thread receives: a command, or new recording taps (empty =
/// stop): the native output takes the active VFO's audio before the volume
/// (SDR++), the baseband the decimated IQ, whole or minimized. Dropping a
/// sink lets its writer thread finish the file.
enum Ctl {
    Cmd(Command),
    Record(RecTaps),
}

/// Counters a running engine keeps (sent as [`Event::Stats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EngineStats {
    /// Driver overflows since start (lost IQ blocks).
    pub overflows: u64,
    /// IQ samples read from the source since it started (the watchdog).
    pub samples: u64,
    /// DSP load over the last second, 0.0..=1.0+ of real time.
    pub load: f32,
    /// File source: play position in frames, and whether it ended.
    pub file_pos: u64,
    pub file_eof: bool,
}

/// What a running engine reports to the UI, in order (0.4.0).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A new wideband spectrum frame (DC-centered dB); only the latest.
    Spectrum(SpectrumFrame),
    /// A new VFO view frame; only the latest.
    View(VfoViewFrame),
    /// Decoded data, dated by the file position it was published at (a
    /// file played at Max moves on far between two UI frames).
    Data { pos: u64, items: Vec<DataItem> },
    /// The decoder's latest state (a `Data` type).
    Meter(Option<crate::vfo::types::DecoderMeter>),
    /// The counters, once per poll.
    Stats(EngineStats),
    /// The screen must be repainted in full: the DSP thread had stdout
    /// silenced while the UI may have drawn.
    Repaint,
    /// The engine failed and stopped producing (a remote connection lost);
    /// the UI stops it.
    Failed(String),
    /// Something the user should know that does not stop the engine.
    Notice(String),
}

/// A running engine as the UI sees it: the local DSP thread or a remote
/// server (0.4.0). Commands in, events out.
pub trait EngineHandle {
    /// Apply a command (best effort).
    fn command(&self, cmd: Command);
    /// Replace the recording taps (empty = stop).
    fn record(&self, taps: RecTaps);
    /// Whether a baseband recording can be made (not over the network).
    fn records_baseband(&self) -> bool {
        true
    }
    /// Send a copy of the audio (48 kHz, before the volume) to `tx` as well
    /// (a viewer of the shared screen, 0.4.1); None stops it.
    fn set_audio_tee(&self, _tx: Option<Sender<Vec<f32>>>) {}
    /// How the audio travels from a server (a Network source, 0.4.3):
    /// "Opus 32 kb/s", or PCM and why; None for a local source.
    fn net_audio(&self) -> Option<String> {
        None
    }
    /// Change a Network source's audio while it runs (`bitrate` 0: PCM).
    fn set_net_audio(&self, _bitrate: u32) -> Result<(), String> {
        Ok(())
    }
    /// The events since the last call.
    fn poll(&self) -> Vec<Event>;
    /// Stop and release the source and audio. Returns the data decoded but
    /// not yet taken, including what the processor held back (a DSC call
    /// waiting for its expansion, seed NFR-03).
    fn stop(self: Box<Self>) -> Vec<DataItem>;
}

/// Where the DSP thread writes a recording's outputs (spec 13.6).
#[derive(Debug, Default)]
pub struct RecTaps {
    pub native: Option<RecSink>,
    pub baseband: Option<BasebandTap>,
}

/// The baseband output of a recording.
#[derive(Debug)]
pub struct BasebandTap {
    pub sink: RecSink,
    /// Set for a minimized baseband.
    pub minimize: Option<crate::record::Minimize>,
}

/// Shifts the IQ to a fixed absolute frequency and decimates it by powers of
/// two to the rate a type needs (a minimized baseband).
pub struct Minimizer {
    freq_hz: f64,
    min_rate: f64,
    nco: crate::dsp::nco::Nco,
    stages: crate::dsp::channelizer::Channelizer,
}

impl Minimizer {
    /// For IQ at `rate` centered on `center_hz`.
    pub fn new(m: crate::record::Minimize, center_hz: f64, rate: f64) -> Self {
        Self {
            freq_hz: m.freq_hz,
            min_rate: m.min_rate,
            nco: crate::dsp::nco::Nco::new(-(m.freq_hz - center_hz), rate),
            stages: crate::dsp::channelizer::Channelizer::new(rate, m.min_rate),
        }
    }

    /// The output rate.
    pub fn out_rate(&self) -> f64 {
        self.stages.out_rate()
    }

    /// Follow a source retune: the recording stays on its frequency.
    pub fn set_center(&mut self, center_hz: f64, rate: f64) {
        self.nco.set_freq(-(self.freq_hz - center_hz), rate);
    }

    /// Rebuild for a new input rate (Source decimation change).
    pub fn rebuilt(&self, center_hz: f64, rate: f64) -> Self {
        Self::new(
            crate::record::Minimize {
                freq_hz: self.freq_hz,
                min_rate: self.min_rate,
            },
            center_hz,
            rate,
        )
    }

    /// Shift and decimate one block.
    pub fn process(&mut self, iq: &[Complex32]) -> Vec<Complex32> {
        let mut x = iq.to_vec();
        self.nco.mix(&mut x);
        self.stages.process(&x)
    }
}

/// Shared runtime parameters read by the DSP loop and audio callback without
/// rebuilding. `volume` is an `Arc` so the audio output can read it directly.
struct Shared {
    /// Audio volume (f32 bits in an AtomicU32), 0.0..=1.0.
    volume: Arc<AtomicU32>,
    /// Whether the FFT branch should run (false in low-fi = both displays off).
    fft_on: AtomicBool,
    /// Latest finished spectrum frame (DC-centered dB), drop-oldest. The DSP
    /// thread overwrites it; the UI takes it each draw.
    latest_spectrum: Mutex<Option<SpectrumFrame>>,
    /// Whether the VFO view is on screen (its spectra cost CPU only then).
    view_on: AtomicBool,
    /// Latest VFO view frame, drop-oldest like the spectrum.
    latest_view: Mutex<Option<VfoViewFrame>>,
    /// Data decoded by a `Data` type (DSC calls) not yet taken by the UI,
    /// with the file position it was published at; never dropped (every
    /// call is recorded, seed STO-01).
    data: Mutex<Vec<(u64, DataItem)>>,
    /// The latest decoder meters, kept fresh while the view is hidden too.
    decoder: Mutex<Option<crate::vfo::types::DecoderMeter>>,
    /// Stream overflows reported by the driver: each one is a block of IQ
    /// the device delivered but we did not read in time (an audible click).
    overflows: AtomicU64,
    /// IQ samples read from the source (the Report's watchdog, 0.3.0).
    samples: AtomicU64,
    /// DSP load over the last second, in permille of real time (time spent
    /// processing / wall time). Near 1000 the loop cannot keep up.
    load_permille: AtomicU32,
    /// File source: play position (frames) and whether the end was reached.
    file_pos: AtomicU64,
    file_eof: AtomicBool,
    /// File source: play as fast as the CPU allows (0.2.6 [Max]).
    file_fast: AtomicBool,
    /// File source at Max: a decoder downstream is behind, wait for it.
    file_hold: AtomicBool,
    /// A copy of the audio for a viewer (0.4.1).
    tee: Mutex<Option<Sender<Vec<f32>>>>,
    /// Set by the DSP thread after its stdout was silenced (the stream
    /// start): a frame the UI drew meanwhile went to `/dev/null`, so the UI
    /// must repaint the whole screen.
    repaint: AtomicBool,
}

impl Shared {
    fn new(fft_on: bool, file_pos: u64) -> Self {
        Self {
            volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            fft_on: AtomicBool::new(fft_on),
            latest_spectrum: Mutex::new(None),
            view_on: AtomicBool::new(false),
            latest_view: Mutex::new(None),
            data: Mutex::new(Vec::new()),
            decoder: Mutex::new(None),
            overflows: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            load_permille: AtomicU32::new(0),
            file_pos: AtomicU64::new(file_pos),
            file_eof: AtomicBool::new(false),
            file_fast: AtomicBool::new(false),
            file_hold: AtomicBool::new(false),
            repaint: AtomicBool::new(false),
            tee: Mutex::new(None),
        }
    }
}

/// Handle to a running engine: lets the UI post commands and stop it.
pub struct Engine {
    cmd_tx: Sender<Ctl>,
    quit: Arc<AtomicBool>,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
    /// Keep the audio output alive for the engine's lifetime (None with an
    /// audio tap).
    _audio: Option<audio::AudioOutput>,
    /// The audio tap's receiving end.
    tap: Option<Receiver<f32>>,
}

impl Engine {
    /// Take the audio produced since the last call (48 kHz mono, before the
    /// volume); empty without an audio tap.
    pub fn take_audio(&self) -> Vec<f32> {
        self.tap
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    /// The counters.
    pub fn stats(&self) -> EngineStats {
        let sh = &self.shared;
        EngineStats {
            overflows: sh.overflows.load(Ordering::Relaxed),
            samples: sh.samples.load(Ordering::Relaxed),
            load: sh.load_permille.load(Ordering::Relaxed) as f32 / 1000.0,
            file_pos: sh.file_pos.load(Ordering::Relaxed),
            file_eof: sh.file_eof.load(Ordering::Relaxed),
        }
    }

    /// Take the data decoded since the last call, in batches with the file
    /// position each was published at.
    fn take_data_at(&self) -> Vec<(u64, Vec<DataItem>)> {
        let items = self
            .shared
            .data
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default();
        batches(items)
    }
}

impl EngineHandle for Engine {
    fn command(&self, cmd: Command) {
        let sh = &self.shared;
        match cmd {
            // read by the audio callback and the DSP loop directly
            Command::SetVolume(v) => sh
                .volume
                .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed),
            Command::SetView(on) => sh.view_on.store(on, Ordering::Relaxed),
            Command::SetFft(on) => sh.fft_on.store(on, Ordering::Relaxed),
            Command::SetFileFast(on) => sh.file_fast.store(on, Ordering::Relaxed),
            Command::SetFileHold(on) => sh.file_hold.store(on, Ordering::Relaxed),
            cmd => {
                let _ = self.cmd_tx.send(Ctl::Cmd(cmd));
            }
        }
    }

    fn record(&self, taps: RecTaps) {
        let _ = self.cmd_tx.send(Ctl::Record(taps));
    }

    fn set_audio_tee(&self, tx: Option<Sender<Vec<f32>>>) {
        if let Ok(mut t) = self.shared.tee.lock() {
            *t = tx;
        }
    }

    fn poll(&self) -> Vec<Event> {
        let sh = &self.shared;
        let mut out = Vec::new();
        if sh.repaint.swap(false, Ordering::Relaxed) {
            out.push(Event::Repaint);
        }
        out.extend(
            self.take_data_at()
                .into_iter()
                .map(|(pos, items)| Event::Data { pos, items }),
        );
        out.push(Event::Meter(sh.decoder.lock().ok().and_then(|d| *d)));
        if let Some(v) = sh.latest_view.lock().ok().and_then(|mut s| s.take()) {
            out.push(Event::View(v));
        }
        if let Some(f) = sh.latest_spectrum.lock().ok().and_then(|mut g| g.take()) {
            out.push(Event::Spectrum(f));
        }
        out.push(Event::Stats(self.stats()));
        out
    }

    fn stop(mut self: Box<Self>) -> Vec<DataItem> {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.take_data_at()
            .into_iter()
            .flat_map(|(_, items)| items)
            .collect()
    }
}

/// Start the engine: open the device, spawn the DSP thread, open audio.
/// Returns an `Engine` handle or an error string.
pub fn start(cfg: EngineConfig) -> Result<Engine, String> {
    // Open the input: configure the device, or open the file at its start
    // position.
    let src = match &cfg.input {
        Input::Soapy { device_args } => Feed::Soapy(open_soapy(device_args, &cfg)?),
        Input::File { path, start } => {
            let mut reader = IqReader::open(std::path::Path::new(path))?;
            reader.seek(*start).map_err(|e| e.to_string())?;
            Feed::File(reader)
        }
        Input::Network { .. } => return Err("a network input runs on its server".into()),
    };

    let (cmd_tx, cmd_rx) = bounded::<Ctl>(64);
    let quit = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared::new(
        cfg.fft_enabled,
        match &cfg.input {
            Input::File { start, .. } => *start,
            Input::Soapy { .. } | Input::Network { .. } => 0,
        },
    ));

    // Audio output: 48 kHz mono, fed by a bounded sample channel. Volume is
    // applied in the audio callback from the shared atomic.
    let (audio_tx, audio_rx) = bounded::<f32>(48_000); // ~1 s buffer cap
    let (audio, tap) = if cfg.audio_tap {
        (None, Some(audio_rx))
    } else {
        let out = audio::AudioOutput::new(
            48_000,
            audio_rx,
            Arc::clone(&shared.volume),
            &cfg.audio_target,
        )
        .map_err(|e| e.to_string())?;
        (Some(out), None)
    };

    let quit_thread = Arc::clone(&quit);
    let shared_thread = Arc::clone(&shared);
    let handle = std::thread::Builder::new()
        .name("shellsdr-dsp".into())
        .spawn(move || {
            dsp_loop(src, cfg, cmd_rx, audio_tx, shared_thread, quit_thread);
        })
        .map_err(|e| e.to_string())?;

    Ok(Engine {
        cmd_tx,
        quit,
        shared,
        handle: Some(handle),
        _audio: audio,
        tap,
    })
}

/// Open + configure a SoapySDR device. A stdout/stderr silencer is scoped
/// around it: sdrplay prints `[INFO] devIdx: ... / rspDuoSampleFreq: ...`
/// here, and any line that bypasses the no-op `log` handler would scroll and
/// corrupt the ratatui display. The guard restores the fds on drop.
fn open_soapy(args: &str, cfg: &EngineConfig) -> Result<SoapySource, String> {
    let _hush = silence::StdioSilencer::new();
    let mut src = SoapySource::open(args).map_err(|e| e.to_string())?;
    src.set_sample_rate(cfg.sample_rate)
        .map_err(|e| e.to_string())?;
    src.set_frequency(cfg.center_hz)
        .map_err(|e| e.to_string())?;
    src.set_agc(cfg.agc).map_err(|e| e.to_string())?;
    // Antenna and analog bandwidth before gains (SDR++ start order).
    if let Some(ant) = &cfg.antenna {
        let _ = src.set_antenna(ant);
    }
    if let Some(bw) = cfg.hw_bandwidth_hz {
        let _ = src.set_bandwidth(bw);
    }
    for (name, db) in &cfg.gains {
        let _ = src.set_gain_element(name, *db);
    }
    Ok(src)
}

/// The opened IQ input, before its stream starts.
enum Feed {
    Soapy(SoapySource),
    File(IqReader<std::io::BufReader<std::fs::File>>),
}

/// A running IQ input the DSP loop reads blocks from.
enum Stream {
    Soapy {
        src: SoapySource,
        stream: soapysdr::RxStream<Complex32>,
    },
    File {
        reader: IqReader<std::io::BufReader<std::fs::File>>,
        pacer: Pacer,
        t0: std::time::Instant,
        /// SDR++ file source block: sample rate / 200 (5 ms).
        block: usize,
        /// The last read was at Max (the real-time clock restarts after).
        was_fast: bool,
    },
}

impl Stream {
    fn start(feed: Feed, rate: f64) -> Option<Self> {
        Some(match feed {
            Feed::Soapy(src) => {
                let _hush = silence::StdioSilencer::new();
                let stream = src.start_stream().ok()?;
                Stream::Soapy { src, stream }
            }
            Feed::File(reader) => Stream::File {
                reader,
                pacer: Pacer::new(rate),
                t0: std::time::Instant::now(),
                block: ((rate / 200.0) as usize).max(1),
                was_fast: false,
            },
        })
    }

    /// A buffer size that fits one read.
    fn block_len(&self) -> usize {
        match self {
            Stream::Soapy { stream, .. } => stream.mtu().unwrap_or(65536).max(1024),
            Stream::File { block, .. } => *block * FAST_BLOCKS,
        }
    }

    fn soapy(&mut self) -> Option<&mut SoapySource> {
        match self {
            Stream::Soapy { src, .. } => Some(src),
            Stream::File { .. } => None,
        }
    }

    /// Read one block into `buf`; 0 when nothing is available (timeout,
    /// overflow, or the end of a file).
    fn read(&mut self, buf: &mut [Complex32], shared: &Shared) -> usize {
        match self {
            Stream::Soapy { stream, .. } => match stream.read(&mut [buf], 200_000) {
                Ok(n) => n,
                Err(e) => {
                    // Overflow = the driver dropped samples because we read
                    // too late; count it. Timeouts and other transient
                    // errors: keep going unless asked to quit.
                    if e.code == soapysdr::ErrorCode::Overflow {
                        shared.overflows.fetch_add(1, Ordering::Relaxed);
                    }
                    0
                }
            },
            Stream::File {
                reader,
                pacer,
                t0,
                block,
                was_fast,
            } => {
                if shared.file_eof.load(Ordering::Relaxed) {
                    // Idle until the UI stops us (or seeks back).
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    return 0;
                }
                let fast = shared.file_fast.load(Ordering::Relaxed);
                let n = if fast {
                    *was_fast = true;
                    // Max: no pacing, larger blocks; wait only for a decoder
                    // that is behind (its queue would grow without bound).
                    if must_wait(shared) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        return 0;
                    }
                    buf.len().min(*block * FAST_BLOCKS)
                } else {
                    if std::mem::take(was_fast) {
                        // Back to real time from here.
                        *pacer = Pacer::new(reader.meta().sample_rate as f64);
                        *t0 = std::time::Instant::now();
                    }
                    // Real time: wait until this block is due.
                    let wait = pacer.wait_for(t0.elapsed().as_secs_f64(), *block as u64);
                    if wait > 0.0 {
                        std::thread::sleep(std::time::Duration::from_secs_f64(wait.min(0.05)));
                        if pacer.due(t0.elapsed().as_secs_f64()) < *block as u64 {
                            return 0;
                        }
                    }
                    buf.len().min(*block)
                };
                let got = reader.read(&mut buf[..n]).unwrap_or(0);
                pacer.sent(got as u64);
                shared.file_pos.store(reader.position(), Ordering::Relaxed);
                if got == 0 {
                    shared.file_eof.store(true, Ordering::Relaxed);
                }
                got
            }
        }
    }

    /// File source: continue at `frame`, restarting the real-time clock.
    fn seek(&mut self, frame: u64, shared: &Shared) {
        if let Stream::File {
            reader, pacer, t0, ..
        } = self
        {
            let _ = reader.seek(frame);
            *pacer = Pacer::new(reader.meta().sample_rate as f64);
            *t0 = std::time::Instant::now();
            shared.file_pos.store(reader.position(), Ordering::Relaxed);
            shared.file_eof.store(false, Ordering::Relaxed);
        }
    }

    fn stop(self) {
        if let Stream::Soapy { mut stream, .. } = self {
            let _ = stream.deactivate(None);
        }
    }
}

/// File blocks read at once at Max (5 ms each: 100 ms).
const FAST_BLOCKS: usize = 20;
/// At Max, reading waits while this many data batches (100 ms each) are not
/// taken yet: the UI decodes FAX itself, so it can fall behind. The UI
/// takes them once a frame, so this also bounds the speed per UI frame.
const MAX_PENDING_DATA: usize = 256;

/// Whether reading at Max must wait for a decoder downstream.
fn must_wait(shared: &Shared) -> bool {
    shared.file_hold.load(Ordering::Relaxed)
        || shared
            .data
            .lock()
            .is_ok_and(|d| d.len() >= MAX_PENDING_DATA)
}

/// Group `(position, item)` pairs into batches of equal position, in order.
fn batches(items: Vec<(u64, DataItem)>) -> Vec<(u64, Vec<DataItem>)> {
    let mut out: Vec<(u64, Vec<DataItem>)> = Vec::new();
    for (pos, item) in items {
        match out.last_mut() {
            Some((p, v)) if *p == pos => v.push(item),
            _ => out.push((pos, vec![item])),
        }
    }
    out
}

/// The processor of `vfo_type` (None when the type is not in this build or
/// its library did not load: the VFO is then silent).
fn build_processor(
    vfo_type: &str,
    settings: &Settings,
    rate: f64,
    vfo: f64,
    center: f64,
) -> Option<Box<dyn VfoProcessor>> {
    let mut s = settings.clone();
    s.insert("_vfo_hz".into(), vfo.into());
    s.insert("_center_hz".into(), center.into());
    crate::vfo::types::usable(vfo_type).map(|t| t.build_processor(&s, rate, vfo - center))
}

/// The DSP thread body: read IQ, run the chain, push audio + spectrum, apply
/// commands.
fn dsp_loop(
    src: Feed,
    cfg: EngineConfig,
    cmd_rx: Receiver<Ctl>,
    audio_tx: Sender<f32>,
    shared: Arc<Shared>,
    quit: Arc<AtomicBool>,
) {
    // Source decimation (SDR++ IQFrontEnd preproc): everything below runs at
    // the effective rate.
    let mut decim = PowerDecimator::new(cfg.decimation);
    let mut sr = cfg.sample_rate / decim.ratio() as f64;
    let mut dec_buf: Vec<Complex32> = Vec::new();
    let mut rec = RecTaps::default();
    let mut minimizer: Option<Minimizer> = None;
    let mut center = cfg.center_hz;
    let mut vfo = cfg.vfo_hz;
    let mut vfo_type = cfg.vfo_type.clone();
    let mut settings = cfg.settings.clone();
    let mut chain = build_processor(&vfo_type, &settings, sr, vfo, center);

    // Power-spectrum analyzer: fed the wideband IQ when the FFT branch is on.
    let mut analyzer = SpectrumAnalyzer::new(FFT_SIZE, sr, FFT_FPS, FFT_AVG_ALPHA);

    // The VFO view's spectra (only while the view is on screen).
    let mut zoom_an: Option<(f64, SpectrumAnalyzer)> = None;
    let mut audio_an = SpectrumAnalyzer::new(VIEW_FFT_SIZE, AUDIO_RATE as f64, VIEW_FPS, 0.5);
    let mut view = VfoViewFrame::default();

    // The stream start silences stdout for the whole process, the UI's
    // frames too: the UI repaints after it.
    let stream = Stream::start(src, cfg.sample_rate);
    shared.repaint.store(true, Ordering::Relaxed);
    let Some(mut stream) = stream else {
        return;
    };
    let mut buf = vec![Complex32::new(0.0, 0.0); stream.block_len()];
    // Load measurement window.
    let mut win_start = std::time::Instant::now();
    let mut busy = std::time::Duration::ZERO;

    while !quit.load(Ordering::Relaxed) {
        // Apply any pending commands (may rebuild the chain).
        let mut rebuild = false;
        while let Ok(ctl) = cmd_rx.try_recv() {
            let cmd = match ctl {
                Ctl::Record(r) => {
                    minimizer = r
                        .baseband
                        .as_ref()
                        .and_then(|b| b.minimize)
                        .map(|m| Minimizer::new(m, center, sr));
                    rec = r;
                    continue;
                }
                Ctl::Cmd(cmd) => cmd,
            };
            match cmd {
                Command::SetCenter(hz) => {
                    center = hz;
                    if let Some(m) = &mut minimizer {
                        m.set_center(hz, sr);
                    }
                    if let Some(src) = stream.soapy() {
                        let _ = src.set_frequency(hz);
                    }
                    rebuild = true;
                }
                Command::SetVfo(hz) => {
                    vfo = hz;
                    rebuild = true;
                }
                Command::SetVfoSettings(t, s) => {
                    vfo_type = t;
                    settings = s;
                    rebuild = true;
                }
                Command::SetGain(name, db) => {
                    if let Some(src) = stream.soapy() {
                        let _ = src.set_gain_element(&name, db);
                    }
                }
                Command::SetAgc(on) => {
                    if let Some(src) = stream.soapy() {
                        let _ = src.set_agc(on);
                    }
                }
                Command::SetAntenna(name) => {
                    if let Some(src) = stream.soapy() {
                        let _ = src.set_antenna(&name);
                    }
                }
                Command::SetHwBandwidth(bw) => {
                    if let Some(src) = stream.soapy() {
                        let _ = src.set_bandwidth(bw);
                    }
                }
                Command::Seek(frame) => stream.seek(frame, &shared),
                Command::SetDecimation(r) => {
                    decim = PowerDecimator::new(r);
                    sr = cfg.sample_rate / decim.ratio() as f64;
                    analyzer = SpectrumAnalyzer::new(FFT_SIZE, sr, FFT_FPS, FFT_AVG_ALPHA);
                    // A baseband recording continues in a new part file at
                    // the new rate (a WAV has one rate).
                    if let Some(b) = &mut rec.baseband {
                        match &mut minimizer {
                            Some(m) => {
                                *m = m.rebuilt(center, sr);
                                b.sink.set_sample_rate(m.out_rate().round() as u32);
                            }
                            None => b.sink.set_sample_rate(sr.round() as u32),
                        }
                    }
                    rebuild = true;
                }
                // applied by `Engine::command` on the shared state
                Command::SetVolume(_)
                | Command::SetView(_)
                | Command::SetFft(_)
                | Command::SetFileFast(_)
                | Command::SetFileHold(_) => {}
            }
        }
        if rebuild {
            flush_processor(chain.as_deref_mut(), &shared);
            chain = build_processor(&vfo_type, &settings, sr, vfo, center);
        }

        // Read one block of IQ.
        match stream.read(&mut buf, &shared) {
            n if n > 0 => {
                shared.samples.fetch_add(n as u64, Ordering::Relaxed);
                let t0 = std::time::Instant::now();
                decim.process(&buf[..n], &mut dec_buf);
                if dec_buf.is_empty() {
                    busy += t0.elapsed();
                    continue;
                }
                let iq = &dec_buf[..];
                if let Some(b) = &rec.baseband {
                    let out = match &mut minimizer {
                        Some(m) => m.process(iq),
                        None => iq.to_vec(),
                    };
                    if !out.is_empty() {
                        b.sink.push(out.iter().flat_map(|c| [c.re, c.im]).collect());
                    }
                }
                // Spectrum tap on the wideband IQ (before per-VFO translation).
                // Skipped entirely in low-fi (both displays off) so it costs
                // zero CPU.
                // At Max, frames the UI would never see are skipped: the
                // analyzer is fed only after the last frame was taken.
                let fast = shared.file_fast.load(Ordering::Relaxed);
                let spectrum_taken =
                    || !fast || shared.latest_spectrum.lock().is_ok_and(|s| s.is_none());
                if shared.fft_on.load(Ordering::Relaxed) && spectrum_taken() {
                    let frames = analyzer.feed(iq);
                    if let Some(frame) = frames.into_iter().last()
                        && let Ok(mut slot) = shared.latest_spectrum.lock()
                    {
                        // Drop-oldest: keep only the most recent frame.
                        *slot = Some(frame);
                    }
                }

                let audio = chain.as_mut().map(|p| p.process(iq)).unwrap_or_default();
                if let Some(p) = chain.as_mut() {
                    publish_data(p.take_data(), &shared);
                    if let Ok(mut d) = shared.decoder.lock() {
                        *d = p.meters().decoder;
                    }
                }
                if let Some(sink) = &rec.native
                    && !audio.is_empty()
                {
                    sink.push(audio.clone());
                }
                if shared.view_on.load(Ordering::Relaxed)
                    && (!fast || shared.latest_view.lock().is_ok_and(|s| s.is_none()))
                    && let Some(p) = chain.as_ref()
                {
                    let (ch, rate) = p.channel_iq();
                    if rate > 0.0 && zoom_an.as_ref().is_none_or(|(r, _)| *r != rate) {
                        zoom_an = Some((
                            rate,
                            SpectrumAnalyzer::new(VIEW_FFT_SIZE, rate, VIEW_FPS, 0.5),
                        ));
                    }
                    let mut fresh = false;
                    if let Some((rate, an)) = &mut zoom_an
                        && let Some(f) = an.feed(ch).into_iter().last()
                    {
                        view.zoom_db = f;
                        view.zoom_rate = *rate;
                        fresh = true;
                    }
                    let a: Vec<Complex32> = audio.iter().map(|&x| Complex32::new(x, 0.0)).collect();
                    if let Some(f) = audio_an.feed(&a).into_iter().last() {
                        view.audio_db = f;
                        fresh = true;
                    }
                    if fresh {
                        let m = p.meters();
                        view.center_offset = p.channel_center_offset();
                        view.level_db = m.level_db;
                        view.agc_gain_db = m.agc_gain_db;
                        if let Ok(mut slot) = shared.latest_view.lock() {
                            *slot = Some(view.clone());
                        }
                    }
                }
                if !fast
                    && !audio.is_empty()
                    && let Ok(t) = shared.tee.lock()
                    && let Some(tx) = t.as_ref()
                {
                    let _ = tx.try_send(audio.clone());
                }
                // Nothing is played at Max (it would only fill the buffer).
                for s in audio.into_iter().filter(|_| !fast) {
                    // Drop samples if the consumer is behind (bounded channel).
                    // Volume is applied in the audio callback.
                    let _ = audio_tx.try_send(s);
                }
                busy += t0.elapsed();
            }
            _ => {}
        }
        let wall = win_start.elapsed();
        if wall >= std::time::Duration::from_secs(1) {
            let permille = (busy.as_secs_f64() / wall.as_secs_f64() * 1000.0) as u32;
            shared.load_permille.store(permille, Ordering::Relaxed);
            win_start = std::time::Instant::now();
            busy = std::time::Duration::ZERO;
        }
    }

    stream.stop();
    flush_processor(chain.as_deref_mut(), &shared);
}

/// Hand decoded data to the UI.
fn publish_data(items: Vec<DataItem>, shared: &Shared) {
    if !items.is_empty()
        && let Ok(mut d) = shared.data.lock()
    {
        let pos = shared.file_pos.load(Ordering::Relaxed);
        d.extend(items.into_iter().map(|i| (pos, i)));
    }
}

/// End a processor's input before it is replaced or dropped: what it held
/// back is published.
fn flush_processor(p: Option<&mut (dyn VfoProcessor + 'static)>, shared: &Shared) {
    if let Some(p) = p {
        p.finish();
        publish_data(p.take_data(), shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Minimize, minimized_rate};
    use crate::vfo::model::Mode;
    use crate::vfo::types::VfoType;
    use crate::vfo::types::voice::{VoiceSettings, VoiceType};

    /// Emits one call per block, and one more when its input ends.
    struct Calls(u8);
    impl VfoProcessor for Calls {
        fn process(&mut self, _: &[Complex32]) -> Vec<f32> {
            self.0 += 1;
            Vec::new()
        }
        fn take_data(&mut self) -> Vec<DataItem> {
            let n = std::mem::take(&mut self.0);
            (0..n)
                .map(|_| DataItem::DscCall {
                    symbols: vec![n],
                    age_s: 0.0,
                })
                .collect()
        }
        fn finish(&mut self) {
            self.0 = 9;
        }
    }

    #[test]
    fn decoded_data_queues_until_taken_and_a_flush_adds_what_was_held_back() {
        let shared = Shared::new(false, 0);
        let mut p: Box<dyn VfoProcessor> = Box::new(Calls(0));
        p.process(&[]);
        publish_data(p.take_data(), &shared);
        publish_data(Vec::new(), &shared);
        flush_processor(Some(p.as_mut()), &shared);
        let got = std::mem::take(&mut *shared.data.lock().unwrap());
        assert_eq!(got.len(), 10, "1 call + 9 held back");
        assert!(shared.data.lock().unwrap().is_empty());
    }

    #[test]
    fn data_is_dated_by_the_file_position_it_was_published_at() {
        let shared = Shared::new(false, 100);
        let call = |n| DataItem::DscCall {
            symbols: vec![n],
            age_s: 0.0,
        };
        publish_data(vec![call(1), call(2)], &shared);
        shared.file_pos.store(900, Ordering::Relaxed);
        publish_data(vec![call(3)], &shared);
        let items = std::mem::take(&mut *shared.data.lock().unwrap());
        let got = batches(items);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].0, got[0].1.len()), (100, 2));
        assert_eq!((got[1].0, got[1].1.len()), (900, 1));
    }

    #[test]
    fn max_waits_for_a_decoder_behind() {
        let shared = Shared::new(false, 0);
        assert!(!must_wait(&shared));
        shared.file_hold.store(true, Ordering::Relaxed);
        assert!(must_wait(&shared), "the UI says Digital is behind");
        shared.file_hold.store(false, Ordering::Relaxed);
        for _ in 0..MAX_PENDING_DATA {
            publish_data(
                vec![DataItem::FaxAudio {
                    samples: vec![0],
                    rate: 11025.0,
                }],
                &shared,
            );
        }
        assert!(must_wait(&shared), "the UI has not taken the data");
        shared.data.lock().unwrap().clear();
        assert!(!must_wait(&shared));
    }

    /// Relative power of `x` at `f` (Goertzel-style correlation).
    fn energy_at(x: &[f32], f: f64, sr: f64) -> f64 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let p = std::f64::consts::TAU * f * i as f64 / sr;
            re += v as f64 * p.cos();
            im += v as f64 * p.sin();
        }
        (re * re + im * im).sqrt() / x.len().max(1) as f64
    }

    /// Wideband IQ at `sr` with `f(t)` returning the complex sample.
    fn signal(sr: f64, secs: f64, f: impl Fn(f64) -> Complex32) -> Vec<Complex32> {
        (0..(sr * secs) as usize)
            .map(|n| f(n as f64 / sr))
            .collect()
    }

    fn tone(hz: f64, t: f64) -> Complex32 {
        let p = std::f64::consts::TAU * hz * t;
        Complex32::new(p.cos() as f32, p.sin() as f32)
    }

    /// Minimize `iq` (center 0) around `vfo_off`, play it back through the
    /// Voice chain at the minimized rate, and return (audio, rate).
    fn minimize_and_play(
        iq: &[Complex32],
        sr: f64,
        vfo_off: f64,
        v: VoiceSettings,
    ) -> (Vec<f32>, f64) {
        let s = v.to_settings();
        let m = Minimize {
            freq_hz: vfo_off,
            min_rate: VoiceType.min_baseband_rate(&s),
        };
        let mut min = Minimizer::new(m, 0.0, sr);
        assert_eq!(min.out_rate(), minimized_rate(sr, m.min_rate));
        let small: Vec<Complex32> = iq.chunks(10_000).flat_map(|b| min.process(b)).collect();
        let rate = min.out_rate();
        // Playback: the file's center is the VFO frequency, the VFO at 0.
        let mut chain = VoiceType.build_processor(&s, rate, 0.0);
        let block = (rate / 200.0).max(1.0) as usize;
        let audio: Vec<f32> = small.chunks(block).flat_map(|b| chain.process(b)).collect();
        (audio, rate)
    }

    /// The strongest of `candidates` in the second half of `audio` (after
    /// AGC and filters settle).
    fn dominant(audio: &[f32], candidates: &[f64]) -> f64 {
        let tail = &audio[audio.len() / 2..];
        *candidates
            .iter()
            .max_by(|a, b| {
                energy_at(tail, **a, 48_000.0)
                    .partial_cmp(&energy_at(tail, **b, 48_000.0))
                    .unwrap()
            })
            .unwrap()
    }

    const SR: f64 = 2_000_000.0;
    const VFO: f64 = 300_000.0;

    #[test]
    fn a_minimized_usb_baseband_plays_back_at_its_low_rate() {
        // A USB carrier at dial + 1 kHz, plus a strong signal 20 kHz away.
        let iq = signal(SR, 0.5, |t| {
            tone(VFO + 1_000.0, t) * 0.2 + tone(VFO + 20_000.0, t) * 0.5
        });
        let (audio, rate) = minimize_and_play(&iq, SR, VFO, VoiceSettings::for_mode(Mode::Usb));
        assert_eq!(rate, 7_812.5, "2 MHz / 256 >= 2.2 x 2800");
        assert!(!audio.is_empty());
        assert_eq!(
            dominant(&audio, &[700.0, 1_000.0, 1_500.0, 2_000.0]),
            1_000.0
        );
    }

    #[test]
    fn a_minimized_am_baseband_plays_back() {
        // 1 kHz tone AM on a carrier at the dial.
        let iq = signal(SR, 0.5, |t| {
            let m = 1.0 + 0.5 * (std::f64::consts::TAU * 1_000.0 * t).cos();
            tone(VFO, t) * (0.2 * m as f32)
        });
        let (audio, rate) = minimize_and_play(&iq, SR, VFO, VoiceSettings::for_mode(Mode::Am));
        assert_eq!(rate, 15_625.0);
        assert_eq!(dominant(&audio, &[500.0, 1_000.0, 2_500.0]), 1_000.0);
    }

    #[test]
    fn a_minimized_cw_baseband_keeps_its_pitch() {
        let iq = signal(SR, 0.5, |t| tone(VFO, t) * 0.2);
        let v = VoiceSettings::for_mode(Mode::Cw);
        let (audio, rate) = minimize_and_play(&iq, SR, VFO, v);
        assert!(rate >= 2.2 * (v.cw_tone_hz + v.bandwidth_hz / 2.0));
        assert_eq!(
            dominant(&audio, &[400.0, 800.0, 1_200.0]),
            800.0,
            "the 800 Hz pitch"
        );
    }

    #[test]
    fn the_minimizer_stays_on_its_frequency_when_the_center_moves() {
        let mut m = Minimizer::new(
            Minimize {
                freq_hz: 1_000_000.0,
                min_rate: 10_000.0,
            },
            900_000.0,
            SR,
        );
        // Retune the source: the signal at 1 MHz is now 50 kHz below center.
        m.set_center(1_050_000.0, SR);
        let iq = signal(SR, 0.05, |t| tone(-50_000.0 + 500.0, t));
        let out: Vec<Complex32> = iq.chunks(4096).flat_map(|b| m.process(b)).collect();
        let rate = m.out_rate();
        let tail = &out[out.len() / 2..];
        // The tone sits at +500 Hz in the minimized IQ.
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, c) in tail.iter().enumerate() {
            let p = -std::f64::consts::TAU * 500.0 * i as f64 / rate;
            re += (c.re as f64) * p.cos() - (c.im as f64) * p.sin();
            im += (c.re as f64) * p.sin() + (c.im as f64) * p.cos();
        }
        let mag = (re * re + im * im).sqrt() / tail.len() as f64;
        assert!(mag > 0.8, "tone kept at +500 Hz: {mag}");
    }
}
