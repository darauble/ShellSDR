//! VFO types (0.2.0, spec 13.2): what a VFO does with its channel.
//!
//! A VFO has one active type (`Voice` = the audio demodulators). A type
//! describes its settings with a JSON [`schema::Schema`], keeps them as a
//! JSON map ([`Settings`]), and builds the DSP processor that turns the
//! VFO's IQ into its native output (audio for Voice). The registry lists the
//! types compiled into this build; a type whose external library cannot be
//! loaded is listed but unavailable.

pub mod digital;
pub mod dsc;
pub mod fax;
pub mod hfdl;
pub mod libs;
pub mod schema;
pub mod voice;

use num_complex::Complex32;

pub use schema::{Field, FieldKind, Schema, Settings};

/// The id of the audio-demodulator type.
pub const VOICE: &str = "Voice";
/// The id of the DSC decoder type (0.2.1).
pub const DSC: &str = "DSC";
/// The id of the weather fax type (0.2.2).
pub const FAX: &str = "FAX";
/// The id of the text modes type (0.2.5: MFSK with pictures, RSID).
pub const DIGITAL: &str = "Digital";
/// The id of the HFDL-ACARS decoder type (0.4.5).
pub const HFDL: &str = "HFDL";

/// What a type's native recording is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeOutput {
    /// 48 kHz mono audio (WAV).
    Audio,
    /// Decoded data (format decided by the type).
    Data,
}

/// Whether a compiled-in type can be used in this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Ok,
    /// Its library could not be loaded (the reason is shown).
    Unavailable(String),
}

/// Live measurements a processor offers to its Work Area view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessorMeters {
    /// Channel power after the channel filter, dBFS.
    pub level_db: f32,
    /// The AGC's current gain, dB (None when the mode has no AGC).
    pub agc_gain_db: Option<f32>,
    /// A decoder's state (None for a type without one).
    pub decoder: Option<DecoderMeter>,
}

/// A data decoder's live state (DSC: seed 06-behaviour 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecoderMeter {
    /// Signal energy where the decoder listens, 0..1.
    pub level: f32,
    /// A message is being read.
    pub decoding: bool,
}

/// Decoded data a `Data` type's processor hands to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum DataItem {
    /// A DSC call: its symbols (seed 02-receiver step 7), and how long ago
    /// (seconds of signal) its phasing was found.
    DscCall { symbols: Vec<u8>, age_s: f64 },
    /// Fax audio (16-bit, `rate` Hz) for the UI's fax decoder.
    FaxAudio { samples: Vec<i16>, rate: f64 },
    /// The channel IQ (`rate` Hz) for the UI's Digital decoder.
    DigitalIq { samples: Vec<Complex32>, rate: f64 },
    /// An HFDL frame (0.4.5): its octets as the frame decoder gave them.
    HfdlFrame {
        freq_khz: u32,
        m1: u8,
        octets: Vec<u8>,
        /// Seconds between the frame's start and the end of the samples processed.
        age_s: f64,
        level_db: f32,
        noise_db: f32,
        ferr_hz: f32,
    },
    /// The HFDL channels' states, about twice a second.
    HfdlStatus {
        channels: Vec<crate::hfdl::rx::ChannelStatus>,
    },
}

/// A VFO type's DSP: consumes the wideband IQ (at the effective rate) and
/// produces the native output.
pub trait VfoProcessor: Send {
    /// Process a block of wideband IQ; returns 48 kHz audio for an `Audio`
    /// type (empty for a `Data` type).
    fn process(&mut self, wideband: &[Complex32]) -> Vec<f32>;

    /// The channel IQ of the last block (before the channel filter) and its
    /// rate, for the zoomed channel view. Empty when not available.
    fn channel_iq(&self) -> (&[Complex32], f64) {
        (&[], 0.0)
    }

    /// Frequency of the channel IQ's DC relative to the VFO frequency (Hz;
    /// for USB the channel is centered `bw/2` above the dial).
    fn channel_center_offset(&self) -> f64 {
        0.0
    }

    /// Live meters for the view.
    fn meters(&self) -> ProcessorMeters {
        ProcessorMeters::default()
    }

    /// Take the data decoded since the last call (a `Data` type).
    fn take_data(&mut self) -> Vec<DataItem> {
        Vec::new()
    }

    /// End of input (the processor is about to be dropped): emit what is
    /// still held back, e.g. a call waiting for a possible expansion.
    fn finish(&mut self) {}
}

/// A VFO type: its settings and how it processes a channel.
pub trait VfoType: Send + Sync {
    /// The persisted type name (e.g. `"Voice"`).
    fn id(&self) -> &'static str;

    /// Whether it can be used (its library loaded).
    fn availability(&self) -> Availability {
        Availability::Ok
    }

    /// The settings schema for the current `settings`.
    fn schema(&self, settings: &Settings) -> Schema;

    /// Default settings.
    fn defaults(&self) -> Settings {
        self.schema(&Settings::new()).defaults()
    }

    /// Apply dependent rules after `changed` (a field key; `None` = a whole
    /// object was set, e.g. from a bookmark) and clamp values into range.
    /// Missing keys are filled from the defaults.
    fn normalize(&self, settings: &mut Settings, changed: Option<&str>);

    /// The native recording kind.
    fn native_output(&self) -> NativeOutput;

    /// Whether its processor's audio is played (the Audio and Mute rows):
    /// an `Audio` type, or a data type you can listen to (FAX).
    fn plays_audio(&self) -> bool {
        self.native_output() == NativeOutput::Audio
    }

    /// Whether the native output is always recorded (DSC: every decoded
    /// call goes to its day file), so the user cannot switch it off and
    /// [Record] records only the baseband.
    fn native_always_on(&self) -> bool {
        false
    }

    /// The part of `settings` the processor depends on: only a change in it
    /// rebuilds the processor (DSC: none, so its Tools settings never
    /// interrupt a call being read).
    fn dsp_settings(&self, settings: &Settings) -> Settings {
        settings.clone()
    }

    /// The `$t` / ICMT `rec_type` value of a native recording.
    fn native_token(&self) -> &'static str;

    /// The lowest IQ rate it decodes from, for a minimized baseband (Hz).
    fn min_baseband_rate(&self, settings: &Settings) -> f64;

    /// The channel bandwidth (Hz): channel fit, markers, tuning.
    fn bandwidth_hz(&self, settings: &Settings) -> f64;

    /// The tuning snap (Hz, 0 = none).
    fn snap_hz(&self, settings: &Settings) -> u64;

    /// A short mode label for names and the status bar (`$r`, e.g. `USB`).
    fn mode_label(&self, settings: &Settings) -> String;

    /// A compact summary for lists ("USB 2800 Hz").
    fn summary(&self, settings: &Settings) -> String;

    /// Build the processor for a VFO at `offset_hz` from the center of IQ at
    /// `source_rate_hz`. The engine adds the absolute frequencies to
    /// `settings` as `_vfo_hz` and `_center_hz` (HFDL picks its channels).
    fn build_processor(
        &self,
        settings: &Settings,
        source_rate_hz: f64,
        offset_hz: f64,
    ) -> Box<dyn VfoProcessor>;
}

static VOICE_TYPE: voice::VoiceType = voice::VoiceType;
static DSC_TYPE: dsc::DscType = dsc::DscType;
static FAX_TYPE: fax::FaxType = fax::FaxType;
static DIGITAL_TYPE: digital::DigitalType = digital::DigitalType;
static HFDL_TYPE: hfdl::HfdlType = hfdl::HfdlType;
#[cfg(test)]
static TEST_TYPE: test_type::TestType = test_type::TestType;

/// The types compiled into this build, in menu order.
pub fn registry() -> Vec<&'static dyn VfoType> {
    #[cfg(test)]
    {
        vec![
            &VOICE_TYPE,
            &TEST_TYPE,
            &DSC_TYPE,
            &FAX_TYPE,
            &DIGITAL_TYPE,
            &HFDL_TYPE,
        ]
    }
    #[cfg(not(test))]
    {
        vec![&VOICE_TYPE, &DSC_TYPE, &FAX_TYPE, &DIGITAL_TYPE, &HFDL_TYPE]
    }
}

/// The type with `id`, if compiled in.
pub fn lookup(id: &str) -> Option<&'static dyn VfoType> {
    registry().into_iter().find(|t| t.id() == id)
}

/// The type with `id` if compiled in and available.
pub fn usable(id: &str) -> Option<&'static dyn VfoType> {
    lookup(id).filter(|t| t.availability() == Availability::Ok)
}

/// Ids of the types the user may select (compiled in and available).
pub fn selectable_ids() -> Vec<&'static str> {
    registry()
        .into_iter()
        .filter(|t| t.availability() == Availability::Ok)
        .map(|t| t.id())
        .collect()
}

/// Normalized settings of `id` from `raw` (defaults filled in); `raw` as is
/// for a type this build lacks.
pub fn normalized(id: &str, raw: &Settings) -> Settings {
    let mut s = raw.clone();
    if let Some(t) = lookup(id) {
        t.normalize(&mut s, None);
    }
    s
}

/// A second type for unit tests only (type switching, unknown keys, data
/// output): one choice and one number.
#[cfg(test)]
pub mod test_type {
    use super::*;
    use serde_json::json;

    pub const ID: &str = "Test";

    pub struct TestType;

    struct Silent;
    impl VfoProcessor for Silent {
        fn process(&mut self, _: &[Complex32]) -> Vec<f32> {
            Vec::new()
        }
    }

    impl VfoType for TestType {
        fn id(&self) -> &'static str {
            ID
        }
        fn schema(&self, _: &Settings) -> Schema {
            serde_json::from_value(json!({
                "vfo_type": ID,
                "fields": [
                    {"key": "speed", "label": "Speed", "kind": "choice", "default": "Slow",
                     "bookmark": true,
                     "options": [{"value": "Slow", "label": "Slow"}, {"value": "Fast", "label": "Fast"}]},
                    {"key": "width_hz", "label": "Width", "kind": "number", "default": 500.0,
                     "min": 100.0, "max": 5000.0, "step": 100.0, "unit": "Hz", "bookmark": true}
                ]
            }))
            .unwrap()
        }
        fn normalize(&self, s: &mut Settings, _: Option<&str>) {
            for (k, v) in self.defaults() {
                s.entry(k).or_insert(v);
            }
        }
        fn native_output(&self) -> NativeOutput {
            NativeOutput::Data
        }
        fn native_token(&self) -> &'static str {
            "test"
        }
        fn min_baseband_rate(&self, s: &Settings) -> f64 {
            1.25 * self.bandwidth_hz(s)
        }
        fn bandwidth_hz(&self, s: &Settings) -> f64 {
            s.get("width_hz").and_then(|v| v.as_f64()).unwrap_or(500.0)
        }
        fn snap_hz(&self, _: &Settings) -> u64 {
            100
        }
        fn mode_label(&self, s: &Settings) -> String {
            s.get("speed")
                .and_then(|v| v.as_str())
                .unwrap_or("Slow")
                .to_string()
        }
        fn summary(&self, s: &Settings) -> String {
            format!("Test {}", self.mode_label(s))
        }
        fn build_processor(&self, _: &Settings, _: f64, _: f64) -> Box<dyn VfoProcessor> {
            Box::new(Silent)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_voice_first_and_lookup_finds_by_id() {
        assert_eq!(registry()[0].id(), VOICE);
        assert!(lookup(VOICE).is_some());
        assert!(lookup(DSC).is_some());
        assert!(lookup(FAX).is_some());
        assert!(lookup(DIGITAL).is_some());
        assert!(lookup(HFDL).is_some());
        assert!(lookup("NAVTEX").is_none(), "not compiled into this build");
        assert!(selectable_ids().contains(&VOICE));
    }

    #[test]
    fn every_types_defaults_pass_its_own_validation() {
        for t in registry() {
            let d = t.defaults();
            assert_eq!(t.schema(&d).validate(&d), Ok(()), "{}", t.id());
            let mut n = d.clone();
            t.normalize(&mut n, None);
            assert_eq!(t.schema(&n).validate(&n), Ok(()), "{} normalized", t.id());
        }
    }

    #[test]
    fn normalized_fills_defaults_and_keeps_unknown_types_raw() {
        let s = normalized(VOICE, &Settings::new());
        assert_eq!(s["mode"], "Nfm");
        let mut raw = Settings::new();
        raw.insert("channel".into(), 70.into());
        assert_eq!(normalized("NAVTEX", &raw), raw);
    }
}
