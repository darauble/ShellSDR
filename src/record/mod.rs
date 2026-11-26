//! Recording (Step 8): WAV files with RIFF ICMT metadata, SDR++-style file
//! names, and a writer thread so the DSP thread never touches the disk.
//!
//! A VFO records its type's native output (Voice: the demodulated audio,
//! before the output volume), the baseband (the source IQ after the Source
//! decimation, i.e. at the effective sample rate), or both at once (0.2.0,
//! spec 13.6). A minimized baseband is shifted to the VFO frequency and
//! decimated by powers of two to the lowest rate the type decodes from.
//!
//! Files are 16-bit PCM WAV (SDR++'s default sample type): Audio is mono at
//! 48 kHz, Baseband is I/Q as a stereo pair. ICMT keys (`format_version` 2,
//! spec 8.4 + 13.6), one `key=value` per line:
//!
//! | key | value |
//! |---|---|
//! | `format_version` | `2` (1 = 0.1.x, still read) |
//! | `rec_type` | the type's native token (`audio`) or `baseband` |
//! | `vfo_type` | e.g. `Voice` |
//! | `vfo_settings` | the type's settings, compact JSON |
//! | `minimized` | `1` for a minimized baseband, else `0` |
//! | `center_freq` | the IQ's center, Hz: the source center, or the VFO frequency for a minimized baseband (playback tunes to this) |
//! | `vfo_freq` | VFO frequency, Hz |
//! | `vfo_name` | e.g. `Master` |
//! | `mode` | `NFM`, `USB`, ... |
//! | `bandwidth` | VFO channel bandwidth, Hz |
//! | `source_sample_rate` | device capture rate, Hz |
//! | `decimation` | Source decimation ratio (1 = none) |
//! | `start_time` | UTC, ISO 8601 (`2026-11-05T13:40:29Z`) |
//! | `timezone` | `UTC` |
//! | `sample_rate` | this file's rate, Hz |
//! | `part` | 1, 2, ... (split at 4 GiB or on a rate change) |
//! | `end_time` | UTC, ISO 8601, when this part was closed |

pub mod naming;
pub mod recorder;
pub mod wav;

use serde::{Deserialize, Serialize};

/// One output of a recording (spec 13.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecKind {
    /// The type's native output (Voice: audio).
    Native,
    /// The IQ (whole, or minimized around the VFO).
    Baseband,
}

impl RecKind {
    /// Added to a file name whose template has no `$t` (before `.wav`).
    pub fn suffix(self) -> &'static str {
        match self {
            RecKind::Native => "_n",
            RecKind::Baseband => "_b",
        }
    }
}

/// What a VFO records, per type (persisted as `rec_native`, `rec_baseband`,
/// `rec_minimize` in the type's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecChoice {
    pub native: bool,
    pub baseband: bool,
    /// Decimate the baseband to the type's minimum around the VFO.
    pub minimize: bool,
}

impl Default for RecChoice {
    fn default() -> Self {
        Self {
            native: true,
            baseband: false,
            minimize: false,
        }
    }
}

impl RecChoice {
    /// The outputs it records, native first.
    pub fn kinds(&self) -> Vec<RecKind> {
        let mut k = Vec::new();
        if self.native {
            k.push(RecKind::Native);
        }
        if self.baseband {
            k.push(RecKind::Baseband);
        }
        k
    }

    /// From the 0.1.x recorder type.
    pub fn from_legacy(t: RecType) -> Self {
        Self {
            native: t == RecType::Audio,
            baseband: t == RecType::Baseband,
            minimize: false,
        }
    }

    /// As settings keys (`rec_native`, ...) for a type's table.
    pub fn to_keys(&self) -> crate::vfo::types::Settings {
        [
            ("rec_native", self.native),
            ("rec_baseband", self.baseband),
            ("rec_minimize", self.minimize),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::from(v)))
        .collect()
    }

    /// From a type's table (missing keys = defaults).
    pub fn from_keys(s: &crate::vfo::types::Settings) -> Self {
        let d = Self::default();
        let get = |k: &str, def: bool| s.get(k).and_then(|v| v.as_bool()).unwrap_or(def);
        Self {
            native: get("rec_native", d.native),
            baseband: get("rec_baseband", d.baseband),
            minimize: get("rec_minimize", d.minimize),
        }
    }
}

/// What a 0.1.x VFO or job recorded (read only to migrate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RecType {
    #[default]
    Audio,
    Baseband,
}

impl RecType {
    /// The `$t` / `rec_type` token (SDR++ `audio` / `baseband`).
    pub fn token(self) -> &'static str {
        match self {
            RecType::Audio => "audio",
            RecType::Baseband => "baseband",
        }
    }

    /// UI label.
    pub fn label(self) -> &'static str {
        match self {
            RecType::Audio => "Audio",
            RecType::Baseband => "Baseband",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            RecType::Audio => RecType::Baseband,
            RecType::Baseband => RecType::Audio,
        }
    }
}

/// Default recordings folder: `~/ShellSDR/recordings` (SDR++ uses
/// `<root>/recordings`; ours is outside the config dir so it is easy to find).
pub fn default_dir() -> String {
    directories::UserDirs::new()
        .map(|u| u.home_dir().join("ShellSDR").join("recordings"))
        .unwrap_or_else(|| std::path::PathBuf::from("recordings"))
        .to_string_lossy()
        .into_owned()
}

/// Version of the ICMT key set below; bump when keys change meaning.
pub const FORMAT_VERSION: u32 = 2;

/// A minimized baseband: the absolute frequency it is centered on and the
/// rate the power-of-two decimation reaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Minimize {
    pub freq_hz: f64,
    /// The lowest rate the type needs (the decimation stops at or above it).
    pub min_rate: f64,
}

/// The rate a power-of-two decimation of `source_rate` reaches for a
/// minimum of `min_rate`: the smallest `source_rate / 2^k >= min_rate`
/// (`source_rate` itself when it is already below).
pub fn minimized_rate(source_rate: f64, min_rate: f64) -> f64 {
    let mut r = source_rate;
    while r / 2.0 >= min_rate {
        r /= 2.0;
    }
    r
}

/// Everything needed to start one output of a recording, decided from the
/// UI state.
#[derive(Debug, Clone, PartialEq)]
pub struct RecPlan {
    pub kind: RecKind,
    /// Set for a minimized baseband.
    pub minimize: Option<Minimize>,
    /// Expanded file name, without folder and `.wav`.
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
    /// ICMT metadata (the recorder adds `sample_rate`, `part`, `end_time`).
    pub info: Vec<(String, String)>,
}
