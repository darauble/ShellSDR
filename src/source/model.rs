//! Source module UI state and capability-to-option mapping (pure).
//!
//! Seam 1: the selection/kind state machine the Source module renders and
//! edits. Seam 3: turning a device's `Capabilities` into the option lists the
//! UI presents (sample rates, bandwidths, and IFGR/RFGR as separate controls).

use super::Capabilities;

/// Which kind of source is selected (persisted as `source_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SourceKind {
    #[default]
    SoapySdr,
    File,
    /// Another ShellSDR's receiver over the network (0.4.0, spec 21.2).
    Network,
}

impl SourceKind {
    /// The next kind (SoapySDR, File, Network, around).
    pub fn toggled(self) -> Self {
        self.stepped(1)
    }

    /// The kind `dir` steps away (around).
    pub fn stepped(self, dir: i32) -> Self {
        const ALL: [SourceKind; 3] = [SourceKind::SoapySdr, SourceKind::File, SourceKind::Network];
        let i = ALL.iter().position(|k| *k == self).unwrap_or(0) as i32;
        ALL[(i + dir).rem_euclid(ALL.len() as i32) as usize]
    }

    /// Whether the IQ comes from a live receiver (not a file).
    pub fn is_live(self) -> bool {
        self != SourceKind::File
    }
}

/// Opus bit rates the Network source offers (bits per second; 0 = 16-bit
/// PCM, uncompressed).
pub const NET_BITRATES: [u32; 7] = [16_000, 24_000, 32_000, 48_000, 64_000, 96_000, 0];
/// The default: plenty for a voice channel.
pub const NET_DEFAULT_BITRATE: u32 = 32_000;

/// A label for a bit rate of [`NET_BITRATES`].
pub fn bitrate_label(b: u32) -> String {
    if b == 0 {
        "PCM (uncompressed)".to_string()
    } else {
        format!("Opus {} kb/s", b / 1000)
    }
}

/// [`bitrate_label`] for the Source module's narrow row.
pub fn short_bitrate_label(b: u32) -> String {
    if b == 0 {
        "Off (PCM)".to_string()
    } else {
        bitrate_label(b)
    }
}

/// The device list and view of the side not shown: the local SoapySDR
/// devices while the Network source is selected, the server's inputs
/// otherwise. Swapped in and out so the SoapySDR fields serve both.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeviceSide {
    pub devices: Vec<super::DeviceInfo>,
    pub device_idx: usize,
    pub soapy: Option<SoapyView>,
}

/// The Network source's own state (0.4.0).
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkView {
    /// The server, `host:port` (the default port when none).
    pub addr: String,
    /// The server's secret (empty: none).
    pub token: String,
    /// Opus bit rate; 0 = PCM.
    pub bitrate: u32,
    /// What the last query found: `Ok(server)` or the error.
    pub status: Option<Result<String, String>>,
    /// Each input's fixed center (a served file), by device index.
    pub fixed: Vec<Option<f64>>,
    /// The other side's devices (see [`DeviceSide`]).
    pub other: DeviceSide,
    /// The server is a sharing ShellSDR: it offers its screen (0.4.1).
    pub window: bool,
    /// `[Open window]` was pressed (the run loop opens it).
    pub open_request: bool,
    /// How the running stream's audio travels ("Opus 32 kb/s", or PCM and
    /// why; 0.4.3), None when not running.
    pub link: Option<String>,
}

impl Default for NetworkView {
    fn default() -> Self {
        Self {
            addr: String::new(),
            token: String::new(),
            bitrate: NET_DEFAULT_BITRATE,
            status: None,
            fixed: Vec::new(),
            other: DeviceSide::default(),
            window: false,
            open_request: false,
            link: None,
        }
    }
}

impl NetworkView {
    /// Step the bit rate through [`NET_BITRATES`].
    pub fn step_bitrate(&mut self, dir: i32) {
        let n = NET_BITRATES.len() as i32;
        let i = NET_BITRATES
            .iter()
            .position(|b| *b == self.bitrate)
            .unwrap_or(2) as i32;
        self.bitrate = NET_BITRATES[(i + dir).rem_euclid(n) as usize];
    }

    /// The token to send (None when empty).
    pub fn token(&self) -> Option<String> {
        Some(self.token.trim().to_string()).filter(|t| !t.is_empty())
    }
}

/// One selectable gain control derived from a device gain element.
#[derive(Debug, Clone, PartialEq)]
pub struct GainControl {
    pub name: String,
    pub min_db: f64,
    pub max_db: f64,
    /// Currently selected value (clamped into range).
    pub value_db: f64,
}

/// The option lists and current selections the SoapySDR view renders.
#[derive(Debug, Clone, PartialEq)]
pub struct SoapyView {
    pub sample_rates: Vec<f64>,
    pub sample_rate_idx: usize,
    pub bandwidths: Vec<f64>,
    pub bandwidth_idx: usize,
    pub gains: Vec<GainControl>,
    pub antennas: Vec<String>,
    pub antenna_idx: usize,
    pub agc_supported: bool,
    pub agc_on: bool,
    /// Tunable frequency range (Hz), carried from capabilities.
    pub freq_range: Option<(f64, f64)>,
}

impl SoapyView {
    /// Build the view from device capabilities, selecting defaults the way
    /// SDR++'s `soapy_source` does for a device with no saved settings: each
    /// gain element at its range MINIMUM (`uiGains[i] = gainRanges[i].minimum()`;
    /// for the RSP's IFGR/RFGR reductions that means maximum gain), first
    /// bandwidth, first antenna, and 2 MHz if offered else the first rate.
    pub fn from_caps(caps: &Capabilities) -> Self {
        let sample_rate_idx = caps
            .sample_rates
            .iter()
            .position(|r| *r == 2_000_000.0)
            .unwrap_or(0);
        let gains = caps
            .gains
            .iter()
            .map(|g| GainControl {
                name: g.name.clone(),
                min_db: g.min_db,
                max_db: g.max_db,
                value_db: g.min_db,
            })
            .collect();
        Self {
            sample_rates: caps.sample_rates.clone(),
            sample_rate_idx,
            bandwidths: caps.bandwidths.clone(),
            bandwidth_idx: 0,
            gains,
            antennas: caps.antennas.clone(),
            antenna_idx: 0,
            agc_supported: caps.agc_supported,
            agc_on: false,
            freq_range: Some(caps.freq_range),
        }
    }

    /// Currently selected sample rate, if any.
    pub fn selected_sample_rate(&self) -> Option<f64> {
        self.sample_rates.get(self.sample_rate_idx).copied()
    }

    /// Set the selected sample-rate index (clamped to the list).
    pub fn set_sample_rate_idx(&mut self, idx: usize) {
        if !self.sample_rates.is_empty() {
            self.sample_rate_idx = idx.min(self.sample_rates.len() - 1);
        }
    }

    /// Set the selected bandwidth index (clamped to the list).
    pub fn set_bandwidth_idx(&mut self, idx: usize) {
        if !self.bandwidths.is_empty() {
            self.bandwidth_idx = idx.min(self.bandwidths.len() - 1);
        }
    }

    /// Nudge a gain element by `delta` dB, clamped to its range and rounded to
    /// a whole value (RSP IFGR steps in 1 dB, RFGR is an integer LNA state).
    pub fn adjust_gain(&mut self, gain_idx: usize, delta: f64) {
        if let Some(g) = self.gains.get_mut(gain_idx) {
            g.value_db = (g.value_db + delta).round().clamp(g.min_db, g.max_db);
        }
    }

    /// Toggle hardware AGC (only meaningful when `agc_supported`).
    pub fn toggle_agc(&mut self) {
        if self.agc_supported {
            self.agc_on = !self.agc_on;
        }
    }

    /// Snapshot the current selections as persistable `DeviceSettings`
    /// (by value/name, not list index).
    pub fn to_settings(&self) -> crate::config::DeviceSettings {
        crate::config::DeviceSettings {
            sample_rate_hz: self.selected_sample_rate(),
            bandwidth_hz: self.bandwidths.get(self.bandwidth_idx).copied(),
            antenna: self.antennas.get(self.antenna_idx).cloned(),
            gains: self
                .gains
                .iter()
                .map(|g| (g.name.clone(), g.value_db))
                .collect(),
            agc: self.agc_on,
        }
    }

    /// Apply persisted `DeviceSettings` onto this view, matching by value/name.
    /// Entries not present in the current capabilities are ignored so a config
    /// from a different device/driver version degrades gracefully.
    pub fn apply_settings(&mut self, s: &crate::config::DeviceSettings) {
        if let Some(sr) = s.sample_rate_hz
            && let Some(i) = self.sample_rates.iter().position(|r| *r == sr)
        {
            self.sample_rate_idx = i;
        }
        if let Some(bw) = s.bandwidth_hz
            && let Some(i) = self.bandwidths.iter().position(|b| *b == bw)
        {
            self.bandwidth_idx = i;
        }
        if let Some(ant) = &s.antenna
            && let Some(i) = self.antennas.iter().position(|a| a == ant)
        {
            self.antenna_idx = i;
        }
        for (name, db) in &s.gains {
            if let Some(g) = self.gains.iter_mut().find(|g| &g.name == name) {
                g.value_db = db.clamp(g.min_db, g.max_db);
            }
        }
        if self.agc_supported {
            self.agc_on = s.agc;
        }
    }
}

/// Skip button lengths in seconds (spec 7.2: 5 s / 10 s / 30 s / 1 min).
pub const SKIP_SECONDS: [u64; 4] = [5, 10, 30, 60];

/// A transport button of the File view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileButton {
    /// [Start] / [Stop]: the same switch as the global Start/Stop.
    StartStop,
    /// [Max] / [1×]: play as fast as the CPU allows, or back to real time
    /// (0.2.6); starts the playback when stopped.
    Max,
    /// [Rewind]: back to the beginning.
    Rewind,
    /// Skip back by `SKIP_SECONDS[i]`.
    Back(usize),
    /// Skip forward by `SKIP_SECONDS[i]`.
    Fwd(usize),
}

impl FileButton {
    /// Label of a skip button ("5s", "1m").
    pub fn skip_label(i: usize) -> String {
        match SKIP_SECONDS[i] {
            s if s % 60 == 0 => format!("{}m", s / 60),
            s => format!("{s}s"),
        }
    }
}

/// Measures the speed of a file played at Max: × real time over the last
/// [`SpeedMeter::WINDOW_S`] or more of wall time (the position moves in UI
/// frame steps, so a shorter window jitters).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SpeedMeter {
    /// Wall time (s) and play position (frames) the window began at.
    start: Option<(f64, u64)>,
}

impl SpeedMeter {
    pub const WINDOW_S: f64 = 2.0;

    /// Feed the play position at wall time `now_s`; returns a new speed when
    /// a window closes. A position going back (a seek) restarts it.
    pub fn observe(&mut self, now_s: f64, pos: u64, rate: f64) -> Option<f64> {
        match self.start {
            Some((t0, p0)) if pos >= p0 => {
                let dt = now_s - t0;
                if dt < Self::WINDOW_S {
                    return None;
                }
                self.start = Some((now_s, pos));
                Some((pos - p0) as f64 / rate.max(1.0) / dt)
            }
            _ => {
                self.start = Some((now_s, pos));
                None
            }
        }
    }

    /// Forget the window (not at Max, or a seek).
    pub fn reset(&mut self) {
        self.start = None;
    }
}

/// Baseband file source state: the loaded file and the play position.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileView {
    /// Path as the user gave it (persisted).
    pub path: Option<String>,
    /// The loaded file's description (None until a file loads).
    pub meta: Option<super::file::FileMeta>,
    /// Why the last load failed, shown in the module.
    pub error: Option<String>,
    /// Play position in frames: where Start begins, and (while running)
    /// what the engine reports.
    pub position: u64,
    /// A seek the running engine has not been told about yet.
    pub pending_seek: Option<u64>,
    /// Playing at Max (as fast as the CPU allows); off again on Stop.
    pub fast: bool,
    /// The speed reached at Max, × real time (None until measured).
    pub speed: Option<f64>,
    /// Selected button within the transport row (0 = Start/Stop, 1 = Max,
    /// 2 = Rewind) and the two skip rows.
    pub transport_btn: usize,
    pub back_btn: usize,
    pub fwd_btn: usize,
}

impl FileView {
    /// Move the play position to `frame` (clamped to the file), remembering
    /// it for the running engine.
    pub fn seek_to(&mut self, frame: u64) {
        let end = self.meta.as_ref().map(|m| m.frames).unwrap_or(0);
        self.position = frame.min(end);
        self.pending_seek = Some(self.position);
    }

    /// Skip by `seconds` (negative = back), clamped to the file.
    pub fn skip(&mut self, seconds: i64) {
        let Some(m) = &self.meta else { return };
        let delta = seconds.unsigned_abs() * m.sample_rate as u64;
        let target = if seconds < 0 {
            self.position.saturating_sub(delta)
        } else {
            self.position.saturating_add(delta)
        };
        self.seek_to(target);
    }

    /// Play position and duration in seconds.
    pub fn position_s(&self) -> f64 {
        self.meta
            .as_ref()
            .map(|m| self.position as f64 / m.sample_rate.max(1) as f64)
            .unwrap_or(0.0)
    }

    /// The button currently selected in `field`'s row, if it is a button row.
    pub fn selected_button(&self, field: SourceField) -> Option<FileButton> {
        match field {
            SourceField::FileTransport => Some(match self.transport_btn {
                0 => FileButton::StartStop,
                1 => FileButton::Max,
                _ => FileButton::Rewind,
            }),
            SourceField::FileBack => Some(FileButton::Back(self.back_btn)),
            SourceField::FileFwd => Some(FileButton::Fwd(self.fwd_btn)),
            _ => None,
        }
    }

    /// Left/Right within a button row (wraps).
    pub fn step_button(&mut self, field: SourceField, dir: i32) {
        let step = |i: usize, n: usize| ((i as i32 + dir).rem_euclid(n as i32)) as usize;
        match field {
            SourceField::FileTransport => self.transport_btn = step(self.transport_btn, 3),
            SourceField::FileBack => self.back_btn = step(self.back_btn, SKIP_SECONDS.len()),
            SourceField::FileFwd => self.fwd_btn = step(self.fwd_btn, SKIP_SECONDS.len()),
            _ => {}
        }
    }

    /// Make `b` the selected button of its row (mouse click).
    pub fn select_button(&mut self, b: FileButton) {
        match b {
            FileButton::StartStop => self.transport_btn = 0,
            FileButton::Max => self.transport_btn = 1,
            FileButton::Rewind => self.transport_btn = 2,
            FileButton::Back(i) => self.back_btn = i,
            FileButton::Fwd(i) => self.fwd_btn = i,
        }
    }

    /// The row of `b` (for selecting the field on click).
    pub fn button_field(b: FileButton) -> SourceField {
        match b {
            FileButton::StartStop | FileButton::Max | FileButton::Rewind => {
                SourceField::FileTransport
            }
            FileButton::Back(_) => SourceField::FileBack,
            FileButton::Fwd(_) => SourceField::FileFwd,
        }
    }
}

/// The navigable controls in the Source module (SoapySDR view), in order.
/// `Gain(i)` indexes into the current view's gain list, so the set of fields
/// depends on the opened device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceField {
    /// The SoapySDR/File kind toggle.
    Kind,
    /// The device dropdown.
    Device,
    /// The sample-rate dropdown.
    SampleRate,
    /// The software decimation dropdown (SDR++ "Decimation").
    Decimation,
    /// The bandwidth dropdown.
    Bandwidth,
    /// The antenna dropdown (only when the device has more than one port).
    Antenna,
    /// A gain element by index into the view's gain list.
    Gain(usize),
    /// The hardware AGC toggle (only when supported).
    Agc,
    /// File view: the file path (ENTER opens a prompt).
    FilePath,
    /// File view: the [Start]/[Stop] [Rewind] row.
    FileTransport,
    /// File view: the skip-back button row.
    FileBack,
    /// File view: the skip-forward button row.
    FileFwd,
    /// Network view: the server address, its token, the audio bit rate.
    NetServer,
    NetToken,
    NetAudio,
    /// `[Open window]`: the server is a sharing ShellSDR (0.4.1).
    NetOpen,
    /// Sharing this ShellSDR (0.4.1, every kind): on/off, the address it
    /// listens on, its token, muting here while viewed, disconnecting the
    /// viewer.
    ShareOn,
    ShareListen,
    ShareToken,
    ShareMute,
    ShareDisconnect,
}

/// Sharing this ShellSDR's screen (0.4.1, spec 22.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ShareView {
    pub on: bool,
    /// `host:port` it listens on (the default port when none).
    pub listen: String,
    /// The secret a viewer must give (empty: none).
    pub token: String,
    /// Mute the audio here while a viewer is connected.
    pub mute_while_viewed: bool,
    /// `Ok(state)` or the error (e.g. the port in use).
    pub status: Option<Result<String, String>>,
    /// The connected viewer ("192.168.1.20 (terminal, 120x40)").
    pub viewer: Option<String>,
    /// `[Disconnect]` was pressed (the run loop acts on it).
    pub disconnect: bool,
}

impl Default for ShareView {
    fn default() -> Self {
        Self {
            on: false,
            listen: format!("127.0.0.1:{}", crate::remote::wire::DEFAULT_PORT),
            token: String::new(),
            mute_while_viewed: false,
            status: None,
            viewer: None,
            disconnect: false,
        }
    }
}

impl ShareView {
    /// The token to require (None when empty).
    pub fn token(&self) -> Option<String> {
        Some(self.token.trim().to_string()).filter(|t| !t.is_empty())
    }

    /// The sharing rows: On, Listen, Token, the status line, and while a
    /// viewer is connected Mute and Disconnect.
    pub fn rows(&self) -> u16 {
        if self.viewer.is_some() { 6 } else { 4 }
    }
}

/// File view rows: Kind, File, info, position, transport, back, forward.
/// The info and position rows are read-only text between the fields.
pub const FILE_VIEW_ROWS: u16 = 7;

/// The Source module's full state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SourceModule {
    pub kind: SourceKind,
    pub soapy: Option<SoapyView>,
    pub file: FileView,
    /// Enumerated SoapySDR devices (label + args) and the selected index.
    pub devices: Vec<super::DeviceInfo>,
    pub device_idx: usize,
    /// Which field the in-module cursor is on.
    field: Option<SourceField>,
    /// Software decimation ratio applied after capture (one of
    /// `power_decimator::RATIOS`; 0 is treated as 1 for `Default`).
    decimation: u32,
    /// The Network source (0.4.0).
    pub network: NetworkView,
    /// Sharing this ShellSDR (0.4.1).
    pub share: ShareView,
}

impl SourceModule {
    /// Populate the SoapySDR view from opened-device capabilities.
    pub fn set_soapy_caps(&mut self, caps: &Capabilities) {
        self.soapy = Some(SoapyView::from_caps(caps));
    }

    /// Set the enumerated device list.
    pub fn set_devices(&mut self, devices: Vec<super::DeviceInfo>) {
        self.devices = devices;
        if self.device_idx >= self.devices.len() {
            self.device_idx = 0;
        }
    }

    /// Args string of the selected device, if any.
    pub fn selected_device_args(&self) -> Option<String> {
        self.devices.get(self.device_idx).map(|d| d.args.clone())
    }

    /// Human label of the selected device, if any.
    pub fn selected_device_label(&self) -> Option<&str> {
        self.devices.get(self.device_idx).map(|d| d.label.as_str())
    }

    /// The source sample rate (Hz): the selected device rate, or the loaded
    /// file's rate in the File view.
    pub fn selected_sample_rate(&self) -> Option<f64> {
        match self.kind {
            SourceKind::SoapySdr | SourceKind::Network => {
                self.soapy.as_ref().and_then(|v| v.selected_sample_rate())
            }
            SourceKind::File => self.file.meta.as_ref().map(|m| m.sample_rate as f64),
        }
    }

    /// The file's center frequency while the File source is selected (the
    /// center is then locked to it, like SDR++'s file source).
    pub fn file_center(&self) -> Option<u64> {
        match self.kind {
            SourceKind::File => self.file.meta.as_ref().and_then(|m| m.center_hz),
            // a served file
            SourceKind::Network => self
                .network
                .fixed
                .get(self.device_idx)
                .copied()
                .flatten()
                .map(|c| c.round() as u64),
            SourceKind::SoapySdr => None,
        }
    }

    /// Status-bar label of the current source.
    pub fn source_label(&self) -> Option<String> {
        match self.kind {
            SourceKind::SoapySdr => self.selected_device_label().map(str::to_string),
            SourceKind::Network => self
                .selected_device_label()
                .map(|d| format!("{d} @ {}", self.network.addr)),
            SourceKind::File => self.file.path.as_ref().map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.clone())
            }),
        }
    }

    /// What identifies the stream: a change while running restarts the
    /// engine (another device, rate, source kind or file).
    pub fn stream_key(&self) -> (SourceKind, Option<String>, Option<f64>) {
        let id = match self.kind {
            SourceKind::SoapySdr => self.selected_device_args(),
            SourceKind::File => self.file.path.clone(),
            // another server, input, secret or audio rate: reconnect
            SourceKind::Network => self.selected_device_args().map(|a| {
                let n = &self.network;
                format!("{}|{a}|{}|{}", n.addr, n.token, n.bitrate)
            }),
        };
        (self.kind, id, self.selected_sample_rate())
    }

    /// The software decimation ratio (1 = none).
    pub fn decimation(&self) -> u32 {
        self.decimation.max(1)
    }

    /// Set the decimation ratio; values SDR++ does not offer are ignored.
    pub fn set_decimation(&mut self, ratio: u32) {
        if crate::dsp::power_decimator::RATIOS.contains(&ratio) {
            self.decimation = ratio;
        }
    }

    /// Step to the previous/next ratio in the list (wraps).
    pub fn step_decimation(&mut self, dir: i32) {
        let ratios = &crate::dsp::power_decimator::RATIOS;
        let n = ratios.len() as i32;
        let i = ratios
            .iter()
            .position(|r| *r == self.decimation())
            .unwrap_or(0) as i32;
        self.decimation = ratios[((i + dir) % n + n) as usize % ratios.len()];
    }

    /// The rate everything downstream of the source runs at: the selected
    /// sample rate divided by the decimation (SDR++ `getEffectiveSamplerate`).
    pub fn effective_sample_rate(&self) -> Option<f64> {
        self.selected_sample_rate()
            .map(|r| r / self.decimation() as f64)
    }

    /// The selected antenna port name, if any.
    pub fn selected_antenna(&self) -> Option<String> {
        self.soapy
            .as_ref()
            .and_then(|v| v.antennas.get(v.antenna_idx).cloned())
    }

    /// The selected analog (hardware) filter bandwidth in Hz, if any.
    pub fn selected_hw_bandwidth(&self) -> Option<f64> {
        self.soapy
            .as_ref()
            .and_then(|v| v.bandwidths.get(v.bandwidth_idx).copied())
    }

    /// The configured gain elements as (name, dB).
    pub fn gain_settings(&self) -> Vec<(String, f64)> {
        self.soapy
            .as_ref()
            .map(|v| {
                v.gains
                    .iter()
                    .map(|g| (g.name.clone(), g.value_db))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether AGC is enabled in the SoapySDR view.
    pub fn agc_on(&self) -> bool {
        self.soapy.as_ref().map(|v| v.agc_on).unwrap_or(false)
    }

    /// The current device's settings snapshot (if a device view exists).
    pub fn device_settings(&self) -> Option<crate::config::DeviceSettings> {
        self.soapy.as_ref().map(|v| v.to_settings())
    }

    /// Apply persisted settings to the current device view (if present).
    pub fn apply_device_settings(&mut self, s: &crate::config::DeviceSettings) {
        if let Some(v) = &mut self.soapy {
            v.apply_settings(s);
        }
    }

    /// Toggle the source kind.
    pub fn toggle_kind(&mut self) {
        self.set_kind(self.kind.toggled());
    }

    /// Select a source kind; entering or leaving the Network source swaps
    /// the device lists (see [`DeviceSide`]).
    pub fn set_kind(&mut self, kind: SourceKind) {
        if (self.kind == SourceKind::Network) != (kind == SourceKind::Network) {
            let o = &mut self.network.other;
            std::mem::swap(&mut self.devices, &mut o.devices);
            std::mem::swap(&mut self.device_idx, &mut o.device_idx);
            std::mem::swap(&mut self.soapy, &mut o.soapy);
        }
        self.kind = kind;
    }

    /// The local SoapySDR side, wherever it is now.
    pub fn local_side(&self) -> (&[super::DeviceInfo], usize, Option<&SoapyView>) {
        if self.kind == SourceKind::Network {
            let o = &self.network.other;
            (&o.devices, o.device_idx, o.soapy.as_ref())
        } else {
            (&self.devices, self.device_idx, self.soapy.as_ref())
        }
    }

    /// The Network side, wherever it is now.
    pub fn network_side(&self) -> (&[super::DeviceInfo], usize, Option<&SoapyView>) {
        if self.kind == SourceKind::Network {
            (&self.devices, self.device_idx, self.soapy.as_ref())
        } else {
            let o = &self.network.other;
            (&o.devices, o.device_idx, o.soapy.as_ref())
        }
    }

    /// The key a device's saved settings are stored under: its args, or for
    /// a server's input the server and its args (the same model of radio
    /// may be on both machines).
    pub fn network_key(addr: &str, args: &str) -> String {
        format!("net:{addr}|{args}")
    }

    /// Every device's current settings by key: the shown side's and the
    /// other side's selected device.
    pub fn all_device_settings(&self) -> Vec<(String, crate::config::DeviceSettings)> {
        let mut out = Vec::new();
        let (d, i, v) = self.local_side();
        if let (Some(dev), Some(v)) = (d.get(i), v) {
            out.push((dev.args.clone(), v.to_settings()));
        }
        let (d, i, v) = self.network_side();
        if let (Some(dev), Some(v)) = (d.get(i), v) {
            out.push((
                Self::network_key(&self.network.addr, &dev.args),
                v.to_settings(),
            ));
        }
        out
    }

    /// The local device's args (whichever side is shown).
    pub fn local_device_args(&self) -> Option<String> {
        let (d, i, _) = self.local_side();
        d.get(i).map(|d| d.args.clone())
    }

    /// The selected server input's args (whichever side is shown).
    pub fn network_device_args(&self) -> Option<String> {
        let (d, i, _) = self.network_side();
        d.get(i).map(|d| d.args.clone())
    }

    /// Take what a server query found: its inputs (the selection kept by
    /// args when still there) and the selected input's capabilities. Only
    /// while the Network source is shown.
    pub fn set_network_inputs(
        &mut self,
        inputs: Vec<crate::remote::wire::RemoteInput>,
        selected: Option<&str>,
        caps: Option<&Capabilities>,
    ) {
        debug_assert_eq!(self.kind, SourceKind::Network);
        self.network.fixed = inputs.iter().map(|i| i.fixed_center_hz).collect();
        self.devices = inputs
            .into_iter()
            .map(|i| super::DeviceInfo {
                label: i.label,
                args: i.args,
            })
            .collect();
        self.device_idx = selected
            .and_then(|a| self.devices.iter().position(|d| d.args == a))
            .unwrap_or(0);
        self.soapy = caps.map(SoapyView::from_caps);
    }

    /// The engine input of the Network source (None: no server or input).
    pub fn network_input(&self) -> Option<crate::engine::Input> {
        let n = &self.network;
        if n.addr.trim().is_empty() || self.kind != SourceKind::Network || n.window {
            return None;
        }
        Some(crate::engine::Input::Network {
            addr: n.addr.trim().to_string(),
            token: n.token(),
            args: self.selected_device_args()?,
            bitrate: n.bitrate,
        })
    }

    /// The rows of the current kind's view (the sharing rows follow).
    pub fn kind_rows(&self) -> u16 {
        let device = |v: &SoapyView| {
            let agc = u16::from(v.agc_supported);
            let ant = u16::from(v.antennas.len() > 1);
            3 + ant + v.gains.len() as u16 + agc
        };
        match self.kind {
            // Kind + Device, then the device's rows or a placeholder line.
            SourceKind::SoapySdr => 2 + self.soapy.as_ref().map_or(1, device),
            // Kind, File, info; then position, transport and two skip rows
            // once a file is loaded.
            SourceKind::File if self.file.meta.is_some() => FILE_VIEW_ROWS,
            SourceKind::File => 3,
            // Kind, Server, its status, Token, Audio, Device, then the
            // device's rows (or a placeholder line).
            // a sharing server: Kind, Server, status, Token, Audio, [Open]
            SourceKind::Network if self.network.window => 6,
            SourceKind::Network => 6 + self.soapy.as_ref().map_or(1, device),
        }
    }

    /// Every content row: the kind's and the sharing rows.
    pub fn content_rows(&self) -> u16 {
        self.kind_rows() + self.share.rows()
    }

    /// The sharing fields.
    fn share_fields(&self) -> Vec<SourceField> {
        let mut f = vec![
            SourceField::ShareOn,
            SourceField::ShareListen,
            SourceField::ShareToken,
        ];
        if self.share.viewer.is_some() {
            f.extend([SourceField::ShareMute, SourceField::ShareDisconnect]);
        }
        f
    }

    // ── Field navigation ─────────────────────────────────────────────────

    /// The navigable fields for the current SoapySDR view, in order. `Kind`
    /// and `Device` are always present; sample-rate/bandwidth/gains/AGC appear
    /// once a device view exists (AGC only when the device supports it).
    pub fn fields(&self) -> Vec<SourceField> {
        if self.kind == SourceKind::File {
            let mut f = vec![SourceField::Kind, SourceField::FilePath];
            if self.file.meta.is_some() {
                f.extend([
                    SourceField::FileTransport,
                    SourceField::FileBack,
                    SourceField::FileFwd,
                ]);
            }
            f.extend(self.share_fields());
            return f;
        }
        let mut f = vec![SourceField::Kind, SourceField::Device];
        if self.kind == SourceKind::Network && self.network.window {
            f = vec![
                SourceField::Kind,
                SourceField::NetServer,
                SourceField::NetToken,
                SourceField::NetAudio,
                SourceField::NetOpen,
            ];
            f.extend(self.share_fields());
            return f;
        }
        if self.kind == SourceKind::Network {
            f = vec![
                SourceField::Kind,
                SourceField::NetServer,
                SourceField::NetToken,
                SourceField::NetAudio,
                SourceField::Device,
            ];
        }
        if let Some(v) = &self.soapy {
            f.push(SourceField::SampleRate);
            f.push(SourceField::Decimation);
            f.push(SourceField::Bandwidth);
            if v.antennas.len() > 1 {
                f.push(SourceField::Antenna);
            }
            for i in 0..v.gains.len() {
                f.push(SourceField::Gain(i));
            }
            if v.agc_supported {
                f.push(SourceField::Agc);
            }
        }
        f.extend(self.share_fields());
        f
    }

    /// Content row (0-based, inside the frame) that `field` is drawn on.
    /// SoapySDR rows follow `fields()`; the File view has read-only text rows
    /// (file info, position) between its fields.
    pub fn field_row(&self, field: SourceField) -> u16 {
        let k = self.kind_rows();
        match field {
            // below the kind's rows; the status line after Token
            SourceField::ShareOn => k,
            SourceField::ShareListen => k + 1,
            SourceField::ShareToken => k + 2,
            SourceField::ShareMute => k + 4,
            SourceField::ShareDisconnect => k + 5,
            SourceField::FilePath => 1,
            SourceField::FileTransport => 4,
            SourceField::FileBack => 5,
            SourceField::FileFwd => 6,
            // the Network view has the server's status below its address
            SourceField::Kind | SourceField::NetServer => {
                self.fields().iter().position(|x| *x == field).unwrap_or(0) as u16
            }
            f if self.kind == SourceKind::Network => {
                self.fields().iter().position(|x| *x == f).unwrap_or(0) as u16 + 1
            }
            f => self.fields().iter().position(|x| *x == f).unwrap_or(0) as u16,
        }
    }

    /// The field the cursor is on (defaults to the first field).
    pub fn field(&self) -> SourceField {
        self.field
            .filter(|f| self.fields().contains(f))
            .unwrap_or(SourceField::Kind)
    }

    /// Move the field cursor to the next field (wraps).
    pub fn field_next(&mut self) {
        let fields = self.fields();
        let cur = self.field();
        let i = fields.iter().position(|f| *f == cur).unwrap_or(0);
        self.field = Some(fields[(i + 1) % fields.len()]);
    }

    /// Move the field cursor to the previous field (wraps).
    pub fn field_prev(&mut self) {
        let fields = self.fields();
        let n = fields.len();
        let cur = self.field();
        let i = fields.iter().position(|f| *f == cur).unwrap_or(0);
        self.field = Some(fields[(i + n - 1) % n]);
    }

    /// Put the field cursor on `field` if it is currently navigable.
    pub fn select_field(&mut self, field: SourceField) {
        if self.fields().contains(&field) {
            self.field = Some(field);
        }
    }

    /// Select the device at `idx` and re-probe it to rebuild the capability
    /// view, mirroring SDR++'s open-probe-close-on-select behavior. `probe`
    /// opens the device by args, reads its capabilities, and closes it; it
    /// returns `None` if the device could not be opened/probed. On a failed
    /// probe the index still moves (so the UI reflects the choice) but the
    /// existing view is kept. Returns whether the probe succeeded.
    pub fn select_device<F>(&mut self, idx: usize, mut probe: F) -> bool
    where
        F: FnMut(&str) -> Option<Capabilities>,
    {
        if idx >= self.devices.len() {
            return false;
        }
        self.device_idx = idx;
        let args = self.devices[idx].args.clone();
        match probe(&args) {
            Some(caps) => {
                self.set_soapy_caps(&caps);
                true
            }
            None => false,
        }
    }

    /// Select the device whose args match `args`, re-probing it. Returns
    /// whether a matching device was found (probe success is independent).
    /// Used at startup to restore the persisted device; a missing device
    /// leaves the current selection unchanged.
    pub fn select_device_by_args<F>(&mut self, args: &str, probe: F) -> bool
    where
        F: FnMut(&str) -> Option<Capabilities>,
    {
        if let Some(idx) = self.devices.iter().position(|d| d.args == args) {
            self.select_device(idx, probe);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::MockSource;
    use super::super::SdrSource;
    use super::*;

    fn rsp_caps() -> Capabilities {
        // Open the mock to get the RSP-like capabilities captured from the spike.
        let src = MockSource::open("driver=mock,mode=ST").unwrap();
        src.capabilities().clone()
    }

    #[test]
    fn soapy_view_round_trips_device_settings() {
        let mut v = SoapyView::from_caps(&rsp_caps());
        // Change several controls.
        let sr_idx = v.sample_rates.iter().position(|r| *r == 250_000.0).unwrap();
        v.set_sample_rate_idx(sr_idx);
        v.set_bandwidth_idx(2);
        v.antenna_idx = 1;
        v.adjust_gain(0, -5.0); // IFGR down from mid
        v.agc_on = true;

        let s = v.to_settings();
        assert_eq!(s.sample_rate_hz, Some(250_000.0));
        assert_eq!(s.bandwidth_hz, v.bandwidths.get(2).copied());
        assert_eq!(s.antenna.as_deref(), Some(v.antennas[1].as_str()));
        assert!(s.gains.iter().any(|(n, _)| n == "IFGR"));
        assert!(s.agc);

        // Apply onto a fresh view (as if the device was just re-probed).
        let mut v2 = SoapyView::from_caps(&rsp_caps());
        v2.apply_settings(&s);
        assert_eq!(
            v2.selected_sample_rate(),
            Some(250_000.0),
            "sample rate restored by value"
        );
        assert_eq!(v2.bandwidth_idx, 2, "bandwidth restored by value");
        assert_eq!(v2.antenna_idx, 1, "antenna restored by name");
        assert!(v2.agc_on);
        let ifgr = v2.gains.iter().find(|g| g.name == "IFGR").unwrap();
        let orig_ifgr = v.gains.iter().find(|g| g.name == "IFGR").unwrap();
        assert!(
            (ifgr.value_db - orig_ifgr.value_db).abs() < 1e-9,
            "IFGR restored by name"
        );
    }

    #[test]
    fn apply_settings_ignores_values_not_in_caps() {
        use crate::config::DeviceSettings;
        // A persisted sample rate/antenna/gain the current device lacks is
        // skipped; the view keeps its defaults rather than breaking.
        let mut v = SoapyView::from_caps(&rsp_caps());
        let default_sr = v.selected_sample_rate();
        v.apply_settings(&DeviceSettings {
            sample_rate_hz: Some(9_999_999.0),
            bandwidth_hz: Some(123.0),
            antenna: Some("Nonexistent".into()),
            gains: vec![("NOPE".into(), 5.0)],
            agc: false,
        });
        assert_eq!(v.selected_sample_rate(), default_sr, "unknown rate ignored");
        assert_eq!(v.antenna_idx, 0, "unknown antenna ignored");
    }

    #[test]
    fn gain_adjust_steps_to_whole_values_within_range() {
        let mut v = SoapyView::from_caps(&rsp_caps());
        // A fractional persisted value (old midpoint default) snaps to whole.
        v.gains[1].value_db = 4.5; // RFGR
        v.adjust_gain(1, -1.0);
        assert_eq!(
            v.gains[1].value_db, 4.0,
            "4.5 - 1 rounds to the LNA state 4"
        );
        for _ in 0..20 {
            v.adjust_gain(1, -1.0);
        }
        assert_eq!(v.gains[1].value_db, 0.0, "clamped at the RFGR minimum");
        for _ in 0..60 {
            v.adjust_gain(0, 1.0);
        }
        assert_eq!(v.gains[0].value_db, 59.0, "IFGR clamped at its maximum");
    }

    #[test]
    fn decimation_defaults_to_none_and_divides_the_effective_rate() {
        let mut m = SourceModule::default();
        m.set_soapy_caps(&rsp_caps());
        assert_eq!(m.decimation(), 1, "SDR++ default: None");
        assert_eq!(m.effective_sample_rate(), Some(2_000_000.0));
        m.set_decimation(8);
        assert_eq!(m.effective_sample_rate(), Some(250_000.0));
        m.set_decimation(3);
        assert_eq!(m.decimation(), 8, "only SDR++ ratios are accepted");
    }

    #[test]
    fn decimation_steps_through_the_sdrpp_ratios() {
        let mut m = SourceModule::default();
        m.step_decimation(1);
        assert_eq!(m.decimation(), 2);
        m.step_decimation(-1);
        m.step_decimation(-1);
        assert_eq!(m.decimation(), 64, "wraps like the other source lists");
    }

    /// The kind's own fields (without the sharing rows).
    fn own(m: &SourceModule) -> Vec<SourceField> {
        m.fields()
            .into_iter()
            .filter(|f| {
                !matches!(
                    f,
                    SourceField::ShareOn
                        | SourceField::ShareListen
                        | SourceField::ShareToken
                        | SourceField::ShareMute
                        | SourceField::ShareDisconnect
                )
            })
            .collect()
    }

    #[test]
    fn default_kind_is_soapysdr() {
        assert_eq!(SourceModule::default().kind, SourceKind::SoapySdr);
    }

    #[test]
    fn soapy_fields_list_device_rate_bandwidth_gains_agc() {
        // With RSP-like caps (3 antennas, 2 gains + AGC supported), the
        // navigable fields are: Kind, Device, SampleRate, Bandwidth, Antenna,
        // IFGR, RFGR, Agc (render order matches).
        let mut m = SourceModule::default();
        m.set_soapy_caps(&rsp_caps());
        let fields = own(&m);
        assert_eq!(
            fields,
            vec![
                SourceField::Kind,
                SourceField::Device,
                SourceField::SampleRate,
                SourceField::Decimation,
                SourceField::Bandwidth,
                SourceField::Antenna,
                SourceField::Gain(0),
                SourceField::Gain(1),
                SourceField::Agc,
            ]
        );
    }

    #[test]
    fn antenna_field_hidden_for_single_antenna_devices() {
        // SDR++ only shows the antenna combo when there is more than one.
        let caps = Capabilities {
            antennas: vec!["RX".into()],
            ..rsp_caps()
        };
        let mut m = SourceModule::default();
        m.set_soapy_caps(&caps);
        assert!(!m.fields().contains(&SourceField::Antenna));
    }

    #[test]
    fn selected_antenna_and_hw_bandwidth_follow_the_view() {
        let mut m = SourceModule::default();
        m.set_soapy_caps(&rsp_caps());
        assert_eq!(m.selected_antenna().as_deref(), Some("Tuner 1 50 ohm"));
        assert_eq!(m.selected_hw_bandwidth(), Some(200_000.0));
        if let Some(v) = &mut m.soapy {
            v.antenna_idx = 1;
            v.set_bandwidth_idx(1);
        }
        assert_eq!(m.selected_antenna().as_deref(), Some("Tuner 1 Hi-Z"));
        assert_eq!(m.selected_hw_bandwidth(), Some(300_000.0));
    }

    #[test]
    fn field_cursor_cycles_and_starts_on_kind() {
        let mut m = SourceModule::default();
        m.set_soapy_caps(&rsp_caps());
        assert_eq!(m.field(), SourceField::Kind);
        m.field_next();
        assert_eq!(m.field(), SourceField::Device);
        m.field_prev();
        assert_eq!(m.field(), SourceField::Kind);
        m.field_prev();
        assert_eq!(m.field(), SourceField::ShareToken, "wraps to last");
    }

    #[test]
    fn selecting_device_reprobes_and_rebuilds_the_view() {
        // Two devices: a 2-gain RSP caps and a stripped 1-gain caps. Selecting
        // the second re-probes (via the injected closure) and rebuilds the
        // view so it now reflects the second device's single gain.
        let mut m = SourceModule::default();
        m.set_devices(vec![
            super::super::DeviceInfo {
                label: "Dev A".into(),
                args: "driver=mock,mode=ST".into(),
            },
            super::super::DeviceInfo {
                label: "Dev B".into(),
                args: "driver=mock,mode=MA".into(),
            },
        ]);
        m.set_soapy_caps(&rsp_caps());
        assert_eq!(m.soapy.as_ref().unwrap().gains.len(), 2);

        let stripped = Capabilities {
            gains: vec![super::super::GainElement {
                name: "IFGR".into(),
                min_db: 20.0,
                max_db: 59.0,
            }],
            ..rsp_caps()
        };
        // Select device index 1; the probe closure returns the stripped caps.
        let probed = std::cell::Cell::new(String::new());
        let ok = m.select_device(1, |args| {
            probed.set(args.to_string());
            Some(stripped.clone())
        });
        assert!(ok, "selection with a successful probe returns true");
        assert_eq!(m.device_idx, 1, "device index updated");
        assert_eq!(
            probed.into_inner(),
            "driver=mock,mode=MA",
            "probed the chosen args"
        );
        assert_eq!(
            m.soapy.as_ref().unwrap().gains.len(),
            1,
            "view rebuilt from the new device's caps"
        );
    }

    #[test]
    fn selecting_device_keeps_old_view_when_probe_fails() {
        let mut m = SourceModule::default();
        m.set_devices(vec![
            super::super::DeviceInfo {
                label: "Dev A".into(),
                args: "driver=mock,mode=ST".into(),
            },
            super::super::DeviceInfo {
                label: "Dev B".into(),
                args: "bad".into(),
            },
        ]);
        m.set_soapy_caps(&rsp_caps());
        let ok = m.select_device(1, |_| None); // probe fails
        assert!(!ok, "failed probe returns false");
        assert_eq!(m.device_idx, 1, "index still moves to the chosen device");
        assert_eq!(
            m.soapy.as_ref().unwrap().gains.len(),
            2,
            "view unchanged when the new device could not be probed"
        );
    }

    #[test]
    fn select_device_by_args_restores_persisted_choice() {
        let mut m = SourceModule::default();
        m.set_devices(vec![
            super::super::DeviceInfo {
                label: "A".into(),
                args: "driver=mock,mode=ST".into(),
            },
            super::super::DeviceInfo {
                label: "B".into(),
                args: "driver=mock,mode=MA".into(),
            },
        ]);
        let found = m.select_device_by_args("driver=mock,mode=MA", |_| Some(rsp_caps()));
        assert!(found, "matching args resolves the device");
        assert_eq!(m.device_idx, 1);
        // A missing device leaves the selection unchanged.
        let missing = m.select_device_by_args("driver=none", |_| Some(rsp_caps()));
        assert!(!missing);
        assert_eq!(m.device_idx, 1, "unchanged when args not found");
    }

    #[test]
    fn toggle_kind_switches_views() {
        let mut m = SourceModule::default();
        m.toggle_kind();
        assert_eq!(m.kind, SourceKind::File);
        m.toggle_kind();
        assert_eq!(m.kind, SourceKind::Network);
        m.toggle_kind();
        assert_eq!(m.kind, SourceKind::SoapySdr);
        assert_eq!(SourceKind::SoapySdr.stepped(-1), SourceKind::Network);
    }

    #[test]
    fn the_network_source_swaps_in_its_own_devices() {
        use crate::remote::wire::RemoteInput;
        let mut m = SourceModule::default();
        m.set_devices(vec![super::super::DeviceInfo {
            label: "RSPduo".into(),
            args: "driver=sdrplay".into(),
        }]);
        m.set_soapy_caps(&rsp_caps());
        m.set_kind(SourceKind::Network);
        assert!(
            m.devices.is_empty() && m.soapy.is_none(),
            "nothing asked yet"
        );
        assert_eq!(m.local_device_args().as_deref(), Some("driver=sdrplay"));
        m.network.addr = "pi4:7373".into();
        m.set_network_inputs(
            vec![
                RemoteInput {
                    label: "RSPduo".into(),
                    args: "driver=sdrplay".into(),
                    fixed_center_hz: None,
                },
                RemoteInput {
                    label: "File: x".into(),
                    args: "shellsdr-file=0".into(),
                    fixed_center_hz: Some(13_264_000.0),
                },
            ],
            Some("shellsdr-file=0"),
            Some(&rsp_caps()),
        );
        assert_eq!(m.device_idx, 1);
        assert_eq!(m.file_center(), Some(13_264_000), "a served file's center");
        assert!(m.fields().contains(&SourceField::NetServer));
        assert_eq!(
            m.field_row(SourceField::NetToken),
            3,
            "below the status line"
        );
        let keys: Vec<String> = m
            .all_device_settings()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            keys,
            vec![
                "driver=sdrplay".to_string(),
                "net:pi4:7373|shellsdr-file=0".to_string()
            ],
            "the same radio here and there is saved apart"
        );
        assert!(matches!(
            m.network_input(),
            Some(crate::engine::Input::Network { ref args, .. }) if args == "shellsdr-file=0"
        ));
        m.set_kind(SourceKind::File);
        assert_eq!(m.selected_device_args().as_deref(), Some("driver=sdrplay"));
        assert_eq!(m.network_device_args().as_deref(), Some("shellsdr-file=0"));
        assert_eq!(m.network_input(), None, "only while shown");
    }

    #[test]
    fn soapy_view_maps_rsp_sample_rates_and_defaults_to_2mhz() {
        let v = SoapyView::from_caps(&rsp_caps());
        assert!(
            v.sample_rates.contains(&250_000.0),
            "250 kHz option present"
        );
        assert!(
            v.sample_rates.contains(&2_000_000.0),
            "2 MHz option present"
        );
        assert_eq!(
            v.selected_sample_rate(),
            Some(2_000_000.0),
            "defaults to 2 MHz"
        );
    }

    #[test]
    fn soapy_view_exposes_ifgr_and_rfgr_as_separate_controls() {
        let v = SoapyView::from_caps(&rsp_caps());
        let names: Vec<&str> = v.gains.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["IFGR", "RFGR"], "both gain elements, separate");
        let ifgr = v.gains.iter().find(|g| g.name == "IFGR").unwrap();
        assert_eq!(
            (ifgr.min_db, ifgr.max_db),
            (20.0, 59.0),
            "IFGR range from spike"
        );
        let rfgr = v.gains.iter().find(|g| g.name == "RFGR").unwrap();
        assert_eq!(
            (rfgr.min_db, rfgr.max_db),
            (0.0, 9.0),
            "RFGR range from spike"
        );
        // SDR++ default for a device with no saved settings: range minimum.
        assert_eq!(ifgr.value_db, 20.0, "IFGR defaults to its minimum");
        assert_eq!(rfgr.value_db, 0.0, "RFGR defaults to its minimum");
    }

    #[test]
    fn soapy_view_maps_rsp_bandwidths_and_antennas() {
        let v = SoapyView::from_caps(&rsp_caps());
        assert_eq!(v.bandwidths.first().copied(), Some(200_000.0));
        assert_eq!(v.bandwidths.last().copied(), Some(8_000_000.0));
        assert_eq!(v.antennas.len(), 3);
        assert!(v.agc_supported);
    }

    fn file_module() -> SourceModule {
        let mut m = SourceModule {
            kind: SourceKind::File,
            ..Default::default()
        };
        m.file.path = Some("/rec/20261104T100029_baseband_13264000Hz.wav".into());
        m.file.meta = Some(crate::source::file::FileMeta {
            sample_rate: 250_000,
            frames: 250_000 * 40,
            center_hz: Some(13_264_000),
            format: crate::source::file::SampleFormat::I16,
            tuning: None,
        });
        m
    }

    #[test]
    fn file_view_fields_appear_once_a_file_is_loaded() {
        let mut m = SourceModule {
            kind: SourceKind::File,
            ..Default::default()
        };
        assert_eq!(own(&m), vec![SourceField::Kind, SourceField::FilePath]);
        m = file_module();
        assert_eq!(
            own(&m),
            vec![
                SourceField::Kind,
                SourceField::FilePath,
                SourceField::FileTransport,
                SourceField::FileBack,
                SourceField::FileFwd
            ]
        );
        // Info and position rows sit between the path and the transport.
        assert_eq!(m.field_row(SourceField::FilePath), 1);
        assert_eq!(m.field_row(SourceField::FileTransport), 4);
        assert_eq!(m.field_row(SourceField::FileFwd), FILE_VIEW_ROWS - 1);
    }

    #[test]
    fn file_rate_center_label_and_stream_key_follow_the_kind() {
        let mut m = file_module();
        assert_eq!(m.selected_sample_rate(), Some(250_000.0));
        assert_eq!(m.file_center(), Some(13_264_000));
        assert_eq!(
            m.source_label().as_deref(),
            Some("20261104T100029_baseband_13264000Hz.wav")
        );
        let file_key = m.stream_key();
        m.toggle_kind();
        assert_eq!(m.file_center(), None, "SoapySDR is not locked");
        assert_ne!(m.stream_key(), file_key, "switching kind restarts");
    }

    #[test]
    fn skips_move_and_clamp_the_position_and_queue_a_seek() {
        let mut f = file_module().file;
        f.skip(10);
        assert_eq!(f.position, 2_500_000);
        assert_eq!(f.pending_seek, Some(2_500_000));
        f.skip(-30);
        assert_eq!(f.position, 0, "clamped at the start");
        f.skip(60);
        assert_eq!(f.position, 250_000 * 40, "clamped at the end");
        assert_eq!(f.position_s(), 40.0);
    }

    #[test]
    fn the_speed_meter_measures_over_its_window_and_restarts_on_a_seek() {
        let mut m = SpeedMeter::default();
        assert_eq!(m.observe(10.0, 0, 1000.0), None, "the window opens");
        assert_eq!(m.observe(11.5, 30_000, 1000.0), None, "window not closed");
        assert_eq!(m.observe(12.0, 40_000, 1000.0), Some(20.0));
        assert_eq!(m.observe(16.0, 80_000, 1000.0), Some(10.0));
        assert_eq!(m.observe(17.0, 5_000, 1000.0), None, "a seek back restarts");
        assert_eq!(m.observe(19.0, 11_000, 1000.0), Some(3.0));
    }

    #[test]
    fn button_rows_step_and_select() {
        let mut f = FileView::default();
        assert_eq!(
            f.selected_button(SourceField::FileTransport),
            Some(FileButton::StartStop)
        );
        f.step_button(SourceField::FileTransport, 1);
        assert_eq!(
            f.selected_button(SourceField::FileTransport),
            Some(FileButton::Max)
        );
        f.step_button(SourceField::FileTransport, 1);
        assert_eq!(
            f.selected_button(SourceField::FileTransport),
            Some(FileButton::Rewind)
        );
        f.step_button(SourceField::FileTransport, 1);
        assert_eq!(
            f.selected_button(SourceField::FileTransport),
            Some(FileButton::StartStop),
            "wraps"
        );
        f.step_button(SourceField::FileTransport, -1);
        f.step_button(SourceField::FileBack, -1);
        assert_eq!(
            f.selected_button(SourceField::FileBack),
            Some(FileButton::Back(3))
        );
        f.select_button(FileButton::Fwd(2));
        assert_eq!(
            f.selected_button(SourceField::FileFwd),
            Some(FileButton::Fwd(2))
        );
        assert_eq!(FileButton::skip_label(0), "5s");
        assert_eq!(FileButton::skip_label(3), "1m");
        assert_eq!(f.selected_button(SourceField::Kind), None);
    }
}
