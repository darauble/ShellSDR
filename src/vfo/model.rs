//! VFO model and tuning math (pure).
//!
//! Step 3 seam 2. A VFO "sits" at an absolute frequency inside the source
//! band; its offset from the source center drives the DSP NCO. This module
//! holds the demod mode, per-mode settings, snap interval, enable/collapse,
//! and the pure math that maps absolute frequency <-> offset with clamping and
//! snapping.
//
// NOTE(step3->B/4): some helpers (within_band, if_sample_rate) are used by
// tests now and consumed by the Piece B DSP/click-to-tune math and step 4.
#![allow(dead_code)]

use crate::record::RecType;
use crate::record::naming::DEFAULT_TEMPLATE;

/// AM AGC mode (SDR++-style). Carrier AGC levels the complex IF before
/// detection (steady level on a strong carrier); Audio AGC levels the detected
/// audio (can pump on fades); Off applies neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AmAgcMode {
    Off,
    #[default]
    Audio,
    Carrier,
}

impl AmAgcMode {
    pub const ALL: [AmAgcMode; 3] = [AmAgcMode::Off, AmAgcMode::Audio, AmAgcMode::Carrier];

    pub fn label(self) -> &'static str {
        match self {
            AmAgcMode::Off => "Off",
            AmAgcMode::Audio => "Audio",
            AmAgcMode::Carrier => "Carrier",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let n = Self::ALL.len();
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + n - 1) % n]
    }
}

/// CW beat-note pitch range in Hz. SDR++ `cw.h` clamps to 250..1250; ShellSDR
/// allows up to 3000 Hz (user request). Pitches above
/// [`CW_SDRPP_MAX_TONE_HZ`] raise the CW IF rate so the pitch stays below
/// Nyquist (see `dsp::chain::cw_if_rate`).
pub const CW_TONE_MIN_HZ: f64 = 250.0;
pub const CW_TONE_MAX_HZ: f64 = 3_000.0;
/// SDR++'s upper pitch limit: the highest pitch its 3 kHz CW IF carries.
pub const CW_SDRPP_MAX_TONE_HZ: f64 = 1_250.0;

/// Audio demodulator modes (SDR++ set minus RAW).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Nfm,
    Wfm,
    Am,
    Dsb,
    Usb,
    Lsb,
    Cw,
}

impl Mode {
    pub const ALL: [Mode; 7] = [
        Mode::Nfm,
        Mode::Wfm,
        Mode::Am,
        Mode::Dsb,
        Mode::Usb,
        Mode::Lsb,
        Mode::Cw,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Nfm => "NFM",
            Mode::Wfm => "WFM",
            Mode::Am => "AM",
            Mode::Dsb => "DSB",
            Mode::Usb => "USB",
            Mode::Lsb => "LSB",
            Mode::Cw => "CW",
        }
    }

    /// Default channel bandwidth in Hz for the mode (mirrors SDR++ defaults).
    pub fn default_bandwidth(self) -> f64 {
        match self {
            Mode::Nfm => 12_500.0,
            Mode::Wfm => 150_000.0,
            Mode::Am => 10_000.0,
            Mode::Dsb => 4_600.0,
            Mode::Usb | Mode::Lsb => 2_800.0,
            Mode::Cw => 200.0,
        }
    }

    /// Bandwidth edit step in Hz: CW channels are narrow, so they step finer.
    pub fn bandwidth_step(self) -> f64 {
        match self {
            Mode::Cw => 100.0,
            _ => 500.0,
        }
    }

    /// Lowest bandwidth in Hz the Bandwidth field allows for the mode.
    pub fn min_bandwidth(self) -> f64 {
        match self {
            Mode::Cw => 100.0,
            _ => 500.0,
        }
    }

    /// IF sample rate in Hz for the mode (SDR++ values).
    pub fn if_sample_rate(self) -> f64 {
        match self {
            Mode::Nfm => 50_000.0,
            Mode::Wfm => 250_000.0,
            Mode::Am => 15_000.0,
            Mode::Dsb | Mode::Usb | Mode::Lsb => 24_000.0,
            Mode::Cw => 3_000.0,
        }
    }

    /// Default tuning snap interval in Hz (SDR++ per-mode defaults).
    pub fn default_snap_hz(self) -> u64 {
        match self {
            Mode::Nfm => 2_500,
            Mode::Wfm => 100_000,
            Mode::Am => 1_000,
            Mode::Dsb | Mode::Usb | Mode::Lsb => 100,
            Mode::Cw => 10,
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let n = Self::ALL.len();
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + n - 1) % n]
    }
}

/// A single VFO.
#[derive(Debug, Clone, PartialEq)]
pub struct Vfo {
    /// Display name. The mandatory first VFO is "Master".
    pub name: String,
    /// Absolute tuned frequency in Hz.
    pub frequency_hz: u64,
    pub mode: Mode,
    /// Channel bandwidth in Hz.
    pub bandwidth_hz: f64,
    /// Tuning snap interval in Hz (0 = no snap).
    pub snap_hz: u64,
    /// CW beat-note tone in Hz (only meaningful for CW).
    pub cw_tone_hz: f64,
    /// AM AGC mode (only meaningful for AM).
    pub am_agc: AmAgcMode,
    /// Output audio device name (None = system default). Per-VFO so each VFO
    /// can route to a different output.
    pub audio_device: Option<String>,
    /// Whether the VFO is enabled (disabled = torn down, no CPU).
    pub enabled: bool,
    /// Whether the VFO row is collapsed in the UI.
    pub collapsed: bool,
    /// What the recorder records for the active type: native output,
    /// baseband, or both, and whether the baseband is minimized (spec 13.6).
    pub rec: crate::record::RecChoice,
    /// Recording file-name template (SDR++ tokens + `$n`).
    pub rec_template: String,
    /// Whether recording is switched on (runtime only, never persisted: a
    /// restart must not silently start writing files).
    pub recording: bool,
    /// The VFO's audio output is muted; recording is not affected (it taps
    /// the audio before the volume).
    pub muted: bool,
    /// The active VFO type (spec 13.2). For `Voice` the typed fields above
    /// (mode, bandwidth, snap, CW tone, AM AGC) are its settings.
    pub vfo_type: String,
    /// Settings of the other types this VFO has used (and of types this
    /// build lacks), kept so switching back restores them (spec 13.4). Keys
    /// `rec_native` / `rec_baseband` / `rec_minimize` hold that type's
    /// recording choices.
    pub stash: std::collections::BTreeMap<String, crate::vfo::types::Settings>,
}

impl Vfo {
    /// A new VFO with mode defaults applied.
    pub fn new(name: impl Into<String>, frequency_hz: u64, mode: Mode) -> Self {
        Self {
            name: name.into(),
            frequency_hz,
            mode,
            bandwidth_hz: mode.default_bandwidth(),
            snap_hz: mode.default_snap_hz(),
            cw_tone_hz: 800.0,
            am_agc: AmAgcMode::default(),
            audio_device: None,
            enabled: true,
            collapsed: false,
            rec: crate::record::RecChoice::default(),
            rec_template: DEFAULT_TEMPLATE.to_string(),
            recording: false,
            muted: false,
            vfo_type: crate::vfo::types::VOICE.to_string(),
            stash: Default::default(),
        }
    }

    /// The Voice settings held in the typed fields.
    pub fn voice(&self) -> crate::vfo::types::voice::VoiceSettings {
        crate::vfo::types::voice::VoiceSettings {
            mode: self.mode,
            bandwidth_hz: self.bandwidth_hz,
            snap_hz: self.snap_hz,
            cw_tone_hz: self.cw_tone_hz,
            am_agc: self.am_agc,
        }
    }

    /// Set the typed Voice fields.
    pub fn set_voice(&mut self, v: crate::vfo::types::voice::VoiceSettings) {
        self.mode = v.mode;
        self.bandwidth_hz = v.bandwidth_hz;
        self.snap_hz = v.snap_hz;
        self.cw_tone_hz = v.cw_tone_hz;
        self.am_agc = v.am_agc;
    }

    /// Whether the active type is Voice.
    pub fn is_voice(&self) -> bool {
        self.vfo_type == crate::vfo::types::VOICE
    }

    /// The active type, if compiled into this build.
    pub fn type_def(&self) -> Option<&'static dyn crate::vfo::types::VfoType> {
        crate::vfo::types::lookup(&self.vfo_type)
    }

    /// The active type's settings as a JSON object.
    pub fn type_settings(&self) -> crate::vfo::types::Settings {
        if self.is_voice() {
            let mut s = self.voice().to_settings();
            // Keys a newer build added to Voice are kept in the stash entry.
            if let Some(extra) = self.stash.get(&self.vfo_type) {
                for (k, v) in extra {
                    if !s.contains_key(k) && !REC_KEYS.contains(&k.as_str()) {
                        s.insert(k.clone(), v.clone());
                    }
                }
            }
            s
        } else {
            let mut s = self.stash.get(&self.vfo_type).cloned().unwrap_or_default();
            s.retain(|k, _| !REC_KEYS.contains(&k.as_str()));
            s
        }
    }

    /// Replace the active type's settings (normalized by the type).
    pub fn set_type_settings(&mut self, settings: &crate::vfo::types::Settings) {
        let s = crate::vfo::types::normalized(&self.vfo_type, settings);
        if self.is_voice() {
            self.set_voice(crate::vfo::types::voice::VoiceSettings::from_settings(&s));
            let extra: crate::vfo::types::Settings = s
                .into_iter()
                .filter(|(k, _)| !VOICE_KEYS.contains(&k.as_str()))
                .collect();
            let entry = self.stash.entry(self.vfo_type.clone()).or_default();
            entry.retain(|k, _| REC_KEYS.contains(&k.as_str()));
            entry.extend(extra);
            if entry.is_empty() {
                self.stash.remove(&self.vfo_type);
            }
        } else {
            let entry = self.stash.entry(self.vfo_type.clone()).or_default();
            entry.retain(|k, _| REC_KEYS.contains(&k.as_str()));
            entry.extend(s);
        }
    }

    /// The channel bandwidth of the active type (Hz).
    pub fn bandwidth(&self) -> f64 {
        match self.type_def() {
            Some(t) if !self.is_voice() => t.bandwidth_hz(&self.type_settings()),
            _ => self.bandwidth_hz,
        }
    }

    /// The tuning snap of the active type (Hz).
    pub fn snap(&self) -> u64 {
        match self.type_def() {
            Some(t) if !self.is_voice() => t.snap_hz(&self.type_settings()),
            _ => self.snap_hz,
        }
    }

    /// Short mode label of the active type (`USB`; the type id when the
    /// type is not in this build).
    pub fn mode_label(&self) -> String {
        match self.type_def() {
            Some(t) => t.mode_label(&self.type_settings()),
            None => self.vfo_type.clone(),
        }
    }

    /// Switch the active type to `id` (no-op when already active): the
    /// current type's settings are kept for later, and `id`'s are restored
    /// (or its defaults). The frequency stays.
    pub fn switch_type(&mut self, id: &str) {
        if self.vfo_type == id {
            return;
        }
        // Park the current type's settings (recording choices included).
        let mut parked = self.type_settings();
        parked.extend(self.rec.to_keys());
        self.stash.insert(self.vfo_type.clone(), parked);
        self.vfo_type = id.to_string();
        let restored = self.stash.get(id).cloned().unwrap_or_default();
        self.rec = crate::record::RecChoice::from_keys(&restored);
        self.set_type_settings(&restored);
    }

    /// The Master VFO: mandatory, cannot be removed.
    pub fn master(frequency_hz: u64) -> Self {
        Self::new("Master", frequency_hz, Mode::Nfm)
    }

    /// Whether this is the Master VFO (by name).
    pub fn is_master(&self) -> bool {
        self.name == "Master"
    }

    /// Change mode, resetting bandwidth to the mode default.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.bandwidth_hz = mode.default_bandwidth();
        self.snap_hz = mode.default_snap_hz();
    }

    /// Offset in Hz from the source center frequency (signed). Positive means
    /// the VFO is above center.
    pub fn offset_from(&self, source_center_hz: u64) -> i64 {
        self.frequency_hz as i64 - source_center_hz as i64
    }

    /// Snapshot this VFO for the profile (spec 13.4).
    pub fn to_entry(&self) -> crate::config::VfoEntry {
        let mut types = self.stash.clone();
        let mut active = self.type_settings();
        active.extend(self.rec.to_keys());
        types.insert(self.vfo_type.clone(), active);
        crate::config::VfoEntry {
            frequency_hz: self.frequency_hz,
            vfo_type: self.vfo_type.clone(),
            enabled: self.enabled,
            collapsed: self.collapsed,
            audio_device: self.audio_device.clone(),
            muted: self.muted,
            rec_template: self.rec_template.clone(),
            types,
        }
    }

    /// Build the VFO `name` from its profile entry.
    pub fn from_entry(name: &str, e: &crate::config::VfoEntry) -> Self {
        let mut v = Self::new(name, e.frequency_hz, Mode::Nfm);
        v.enabled = e.enabled;
        v.collapsed = e.collapsed;
        v.audio_device = e.audio_device.clone();
        v.muted = e.muted;
        if !e.rec_template.trim().is_empty() {
            v.rec_template = e.rec_template.clone();
        }
        v.stash = e.types.clone();
        let active = if e.vfo_type.is_empty() {
            crate::vfo::types::VOICE.to_string()
        } else {
            e.vfo_type.clone()
        };
        let settings = v.stash.get(&active).cloned().unwrap_or_default();
        v.rec = crate::record::RecChoice::from_keys(&settings);
        v.vfo_type = active;
        v.set_type_settings(&settings);
        v
    }

    /// Snapshot this VFO's settings in the 0.1.x shape (tests of the
    /// migration).
    pub fn to_config(&self) -> crate::config::VfoConfig {
        crate::config::VfoConfig {
            name: self.name.clone(),
            frequency_hz: self.frequency_hz,
            mode: self.mode,
            bandwidth_hz: self.bandwidth_hz,
            snap_hz: self.snap_hz,
            cw_tone_hz: self.cw_tone_hz,
            am_agc: self.am_agc,
            audio_device: self.audio_device.clone(),
            enabled: self.enabled,
            collapsed: self.collapsed,
            rec_type: if self.rec.baseband && !self.rec.native {
                RecType::Baseband
            } else {
                RecType::Audio
            },
            rec_template: self.rec_template.clone(),
            muted: self.muted,
        }
    }

    /// Build a VFO from persisted settings.
    pub fn from_config(c: &crate::config::VfoConfig) -> Self {
        Self {
            name: c.name.clone(),
            frequency_hz: c.frequency_hz,
            mode: c.mode,
            bandwidth_hz: c.bandwidth_hz,
            snap_hz: c.snap_hz,
            cw_tone_hz: c.cw_tone_hz,
            am_agc: c.am_agc,
            audio_device: c.audio_device.clone(),
            enabled: c.enabled,
            collapsed: c.collapsed,
            rec: crate::record::RecChoice::from_legacy(c.rec_type),
            rec_template: if c.rec_template.trim().is_empty() {
                DEFAULT_TEMPLATE.to_string()
            } else {
                c.rec_template.clone()
            },
            recording: false,
            muted: c.muted,
            vfo_type: crate::vfo::types::VOICE.to_string(),
            stash: Default::default(),
        }
    }
}

/// Keys of a type's recording choices inside its stash entry.
pub const REC_KEYS: [&str; 3] = ["rec_native", "rec_baseband", "rec_minimize"];
/// Keys the typed Voice fields hold.
const VOICE_KEYS: [&str; 5] = ["mode", "bandwidth_hz", "snap_hz", "cw_tone_hz", "am_agc"];

/// Snap a frequency to the nearest multiple of `snap_hz`. A snap of 0 leaves
/// the value unchanged.
pub fn snap(freq_hz: u64, snap_hz: u64) -> u64 {
    if snap_hz == 0 {
        return freq_hz;
    }
    let half = snap_hz / 2;
    ((freq_hz + half) / snap_hz) * snap_hz
}

/// Whether `vfo_freq` is inside the source passband `[center - sr/2, center +
/// sr/2]` for a source sample rate `sr_hz`.
pub fn within_band(vfo_freq_hz: u64, source_center_hz: u64, sr_hz: f64) -> bool {
    let half = (sr_hz / 2.0) as i64;
    let off = vfo_freq_hz as i64 - source_center_hz as i64;
    off >= -half && off <= half
}

/// New source center for tuning a VFO of bandwidth `vfo_bw_hz` to
/// `target_hz` in Free mode, ported from SDR++ `tuner::normalTuning` (no
/// zoom, so the view equals the whole span): if the channel still fits
/// strictly inside the span, the center is unchanged and only the VFO moves;
/// if it falls below, the source retunes so the VFO lands near the TOP edge
/// (offset `span/2 - bw/2 - span/10`); if above, near the BOTTOM edge.
pub fn normal_tuning_center(center_hz: f64, span_hz: f64, vfo_bw_hz: f64, target_hz: f64) -> f64 {
    let offset = target_hz - center_hz;
    let (lo, hi) = (offset - vfo_bw_hz / 2.0, offset + vfo_bw_hz / 2.0);
    let (bottom, top) = (-span_hz / 2.0, span_hz / 2.0);
    if lo > bottom && hi < top {
        return center_hz;
    }
    let new_offset = if lo <= bottom {
        span_hz / 2.0 - vfo_bw_hz / 2.0 - span_hz / 10.0
    } else {
        vfo_bw_hz / 2.0 - span_hz / 2.0 + span_hz / 10.0
    };
    target_hz - new_offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_tuning_moves_only_the_vfo_inside_the_span() {
        // 100 MHz center, 2 MHz span, 10 kHz channel at +500 kHz: fits.
        assert_eq!(normal_tuning_center(100e6, 2e6, 10e3, 100.5e6), 100e6);
    }

    #[test]
    fn normal_tuning_below_the_span_puts_the_vfo_near_the_top() {
        // SDR++: newVFOOffset = 1e6 - 5e3 - 2e5 = 795 kHz; center = f - 795k.
        assert_eq!(normal_tuning_center(100e6, 2e6, 10e3, 98e6), 97_205_000.0);
    }

    #[test]
    fn normal_tuning_above_the_span_puts_the_vfo_near_the_bottom() {
        // newVFOOffset = 5e3 - 1e6 + 2e5 = -795 kHz; center = f + 795k.
        assert_eq!(normal_tuning_center(100e6, 2e6, 10e3, 103e6), 103_795_000.0);
        // A channel straddling the top edge also retunes.
        assert_eq!(
            normal_tuning_center(100e6, 2e6, 10e3, 100_996_000.0),
            101_791_000.0
        );
    }

    #[test]
    fn mode_defaults_match_spec() {
        assert_eq!(Mode::Nfm.default_bandwidth(), 12_500.0);
        assert_eq!(Mode::Am.default_bandwidth(), 10_000.0);
        assert_eq!(Mode::Cw.default_bandwidth(), 200.0);
    }

    #[test]
    fn cw_bandwidth_steps_and_floor_are_100_hz() {
        assert_eq!(Mode::Cw.bandwidth_step(), 100.0);
        assert_eq!(Mode::Cw.min_bandwidth(), 100.0);
    }

    #[test]
    fn non_cw_bandwidth_steps_and_floor_stay_500_hz() {
        for m in [
            Mode::Nfm,
            Mode::Wfm,
            Mode::Am,
            Mode::Dsb,
            Mode::Usb,
            Mode::Lsb,
        ] {
            assert_eq!(m.bandwidth_step(), 500.0, "{m:?} step");
            assert_eq!(m.min_bandwidth(), 500.0, "{m:?} floor");
        }
    }

    #[test]
    fn defaults_match_sdrpp_radio_module() {
        // decoder_modules/radio/src/demodulators/*.h getDefaultBandwidth().
        assert_eq!(Mode::Wfm.default_bandwidth(), 150_000.0);
        assert_eq!(Mode::Dsb.default_bandwidth(), 4_600.0, "dsb.h: 4600");
        assert_eq!(Mode::Usb.default_bandwidth(), 2_800.0);
        assert_eq!(Mode::Lsb.default_bandwidth(), 2_800.0);
        // cw.h: tone = 800.
        assert_eq!(Vfo::master(7_000_000).cw_tone_hz, 800.0);
    }

    #[test]
    fn mode_cycles_through_all_seven() {
        let mut m = Mode::Nfm;
        for _ in 0..Mode::ALL.len() {
            m = m.next();
        }
        assert_eq!(m, Mode::Nfm, "cycling through all returns to start");
        assert_eq!(Mode::ALL.len(), 7);
    }

    #[test]
    fn master_is_named_master_and_nfm() {
        let v = Vfo::master(100_000_000);
        assert!(v.is_master());
        assert_eq!(v.mode, Mode::Nfm);
        assert!(v.enabled);
        assert_eq!(v.audio_device, None, "default audio device by default");
    }

    #[test]
    fn audio_device_round_trips_through_config() {
        let mut v = Vfo::master(100_000_000);
        v.audio_device = Some("USB Headphones".to_string());
        let c = v.to_config();
        let back = Vfo::from_config(&c);
        assert_eq!(back.audio_device.as_deref(), Some("USB Headphones"));
    }

    #[test]
    fn mute_round_trips_through_config_and_old_configs_load_unmuted() {
        let mut v = Vfo::master(100_000_000);
        assert!(!v.muted, "unmuted by default");
        v.muted = true;
        assert!(Vfo::from_config(&v.to_config()).muted);
        let old: crate::config::VfoConfig = toml::from_str("name = \"Master\"").unwrap();
        assert!(!Vfo::from_config(&old).muted);
    }

    #[test]
    fn set_mode_resets_bandwidth_to_default() {
        let mut v = Vfo::master(100_000_000);
        v.bandwidth_hz = 999.0;
        v.set_mode(Mode::Am);
        assert_eq!(v.bandwidth_hz, 10_000.0);
    }

    #[test]
    fn offset_is_signed_distance_from_center() {
        let v = Vfo::new("v", 100_200_000, Mode::Nfm);
        assert_eq!(v.offset_from(100_000_000), 200_000);
        assert_eq!(v.offset_from(100_500_000), -300_000);
    }

    #[test]
    fn snap_rounds_to_nearest_multiple() {
        // 100_012_345: 345 above the 100_012_500? no, below by 155 -> but
        // nearest multiple of 12_500 is 100_012_500 (345 below it vs 12_155
        // above 100_000_000). Check: 100_012_345 is 12_345 above 100_000_000
        // and 155 below 100_012_500 -> nearest is 100_012_500.
        assert_eq!(snap(100_012_345, 12_500), 100_012_500);
        // 100_007_000 is 7_000 above 100_000_000 and 5_500 below 100_012_500
        // -> nearest is 100_012_500.
        assert_eq!(snap(100_007_000, 12_500), 100_012_500);
        // 100_006_000 is 6_000 above 100_000_000 and 6_500 below 100_012_500
        // -> nearest is 100_000_000.
        assert_eq!(snap(100_006_000, 12_500), 100_000_000);
        assert_eq!(snap(145_000_000, 0), 145_000_000, "snap 0 is identity");
    }

    #[test]
    fn within_band_checks_passband() {
        let center = 100_000_000;
        let sr = 2_000_000.0; // +/- 1 MHz
        assert!(within_band(100_500_000, center, sr));
        assert!(within_band(99_000_000, center, sr), "edge inside");
        assert!(!within_band(101_500_000, center, sr), "outside");
    }

    #[test]
    fn switching_type_keeps_each_types_settings_and_the_frequency() {
        use crate::vfo::types::{VOICE, test_type};
        let mut v = Vfo::new("Master", 7_100_000, Mode::Usb);
        v.bandwidth_hz = 2_400.0;
        assert!(v.is_voice());
        assert_eq!(v.type_settings()["mode"], "Usb");
        v.switch_type(test_type::ID);
        assert_eq!(v.vfo_type, test_type::ID);
        assert_eq!(
            v.type_settings()["speed"],
            "Slow",
            "defaults of the new type"
        );
        let mut s = v.type_settings();
        s.insert("speed".into(), "Fast".into());
        v.set_type_settings(&s);
        assert_eq!(v.mode_label(), "Fast");
        assert_eq!(v.bandwidth(), 500.0);
        v.switch_type(VOICE);
        assert_eq!(v.mode, Mode::Usb);
        assert_eq!(v.bandwidth_hz, 2_400.0, "Voice settings came back");
        assert_eq!(v.frequency_hz, 7_100_000);
        v.switch_type(test_type::ID);
        assert_eq!(v.type_settings()["speed"], "Fast", "and so did Test's");
    }

    #[test]
    fn unknown_keys_and_recording_keys_survive_set_type_settings() {
        let mut v = Vfo::master(100_000_000);
        v.stash.insert(
            "Voice".into(),
            serde_json::json!({"rec_baseband": true})
                .as_object()
                .unwrap()
                .clone(),
        );
        let mut s = v.type_settings();
        s.insert("future".into(), 1.into());
        v.set_type_settings(&s);
        assert_eq!(v.type_settings()["future"], 1);
        assert_eq!(v.stash["Voice"]["rec_baseband"], true);
        assert!(!v.type_settings().contains_key("rec_baseband"));
    }
}
