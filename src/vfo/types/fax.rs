//! The `FAX` VFO type (0.2.2, spec 15): HF weather fax. The VFO sits on the
//! fax's assigned frequency; the processor is the Voice USB chain with its
//! dial 1900 Hz below, so the fax carrier is 1900 Hz audio. The audio plays
//! as usual and goes, as 16-bit samples, to the UI's fax service, which
//! decodes it with the FAX library (`libfax.so.1`).

use num_complex::Complex32;
use serde_json::{Value, json};

use super::{
    Availability, DataItem, FAX, NativeOutput, ProcessorMeters, Schema, Settings, VfoProcessor,
    VfoType,
};
use crate::dsp::chain::VfoChain;
use crate::vfo::model::{AmAgcMode, Mode};

/// The audio frequency of the fax carrier (black 1500, white 2300 Hz).
pub const CARRIER_HZ: f64 = 1900.0;
/// The USB chain's audio bandwidth: the fax with its START/STOP sidebands.
pub const AUDIO_BANDWIDTH_HZ: f64 = 3000.0;
/// The channel width for markers and channel fit, centred on the VFO.
pub const BANDWIDTH_HZ: f64 = 1200.0;
/// Fax frequencies are published to 100 Hz (3855.0, 13882.5 kHz).
pub const SNAP_HZ: u64 = 100;
/// The lowest IQ rate for a minimized baseband: the USB chain's dial is
/// 1900 Hz below the VFO and its channel reaches 1100 Hz above.
pub const MIN_IQ_RATE: f64 = 12_000.0;

/// Settings keys.
pub const LPM: &str = "lpm";
pub const IOC: &str = "ioc";
pub const AUTO_SLANT: &str = "auto_slant";
pub const CLOCK_PPM: &str = "clock_ppm";
/// The view (not in the schema): `Blocks` or `Pixels`.
pub const DISPLAY: &str = "display";
/// The ppm step of the F4/F5 keys (not in the schema): 0.1 or 1.
pub const PPM_STEP: &str = "ppm_step";

/// The clock correction's range (ppm).
pub const MAX_PPM: f64 = 500.0;

/// The typed settings.
#[derive(Debug, Clone, PartialEq)]
pub struct FaxSettings {
    pub lpm: i32,
    pub ioc: i32,
    pub auto_slant: bool,
    pub clock_ppm: f64,
}

impl FaxSettings {
    pub fn from_settings(s: &Settings) -> Self {
        let num = |k: &str| s.get(k).and_then(Value::as_f64);
        Self {
            lpm: num(LPM).map_or(120, |v| v as i32),
            ioc: num(IOC).map_or(576, |v| v as i32),
            auto_slant: s.get(AUTO_SLANT).and_then(Value::as_bool).unwrap_or(true),
            clock_ppm: num(CLOCK_PPM).unwrap_or(0.0),
        }
    }

    /// Pixels per line: IOC x pi, truncated as FAX does (576: 1809, 288: 904).
    pub fn width(&self) -> i32 {
        (f64::from(self.ioc) * std::f64::consts::PI) as i32
    }
}

/// The FAX type.
pub struct FaxType;

impl VfoType for FaxType {
    fn id(&self) -> &'static str {
        FAX
    }

    fn availability(&self) -> Availability {
        match crate::fax::ffi::shared() {
            Ok(_) => Availability::Ok,
            Err(e) => Availability::Unavailable(e),
        }
    }

    fn schema(&self, _: &Settings) -> Schema {
        serde_json::from_value(json!({
            "vfo_type": FAX,
            "fields": [
                {"key": LPM, "label": "LPM", "kind": "choice", "default": 120, "bookmark": true,
                 "options": [{"value": 120, "label": "120"}, {"value": 60, "label": "60"},
                             {"value": 90, "label": "90"}, {"value": 240, "label": "240"}]},
                {"key": IOC, "label": "IOC", "kind": "choice", "default": 576, "bookmark": true,
                 "options": [{"value": 576, "label": "576"}, {"value": 288, "label": "288"}]},
                {"key": AUTO_SLANT, "label": "Auto slant", "kind": "bool", "default": true},
                {"key": CLOCK_PPM, "label": "Clock", "kind": "number", "default": 0.0,
                 "min": -MAX_PPM, "max": MAX_PPM, "step": 0.1, "unit": "ppm"}
            ]
        }))
        .expect("fax schema is valid")
    }

    fn normalize(&self, settings: &mut Settings, _: Option<&str>) {
        for (k, v) in self.defaults() {
            settings.entry(k).or_insert(v);
        }
        let f = FaxSettings::from_settings(settings);
        if ![60, 90, 120, 240].contains(&f.lpm) {
            settings.insert(LPM.into(), json!(120));
        }
        if ![576, 288].contains(&f.ioc) {
            settings.insert(IOC.into(), json!(576));
        }
        let ppm = (f.clock_ppm.clamp(-MAX_PPM, MAX_PPM) * 100.0).round() / 100.0;
        settings.insert(CLOCK_PPM.into(), json!(ppm));
        // F2 Save writes to the recordings folder since 0.2.4
        settings.remove("save_dir");
    }

    fn native_output(&self) -> NativeOutput {
        NativeOutput::Data
    }

    fn plays_audio(&self) -> bool {
        true
    }

    /// Nothing in the settings changes the audio chain: the decoder lives in
    /// the UI's service, so a ppm step never interrupts the stream.
    fn dsp_settings(&self, _: &Settings) -> Settings {
        Settings::new()
    }

    fn native_token(&self) -> &'static str {
        "fax"
    }

    fn min_baseband_rate(&self, _: &Settings) -> f64 {
        MIN_IQ_RATE
    }

    fn bandwidth_hz(&self, _: &Settings) -> f64 {
        BANDWIDTH_HZ
    }

    fn snap_hz(&self, _: &Settings) -> u64 {
        SNAP_HZ
    }

    fn mode_label(&self, _: &Settings) -> String {
        FAX.to_string()
    }

    fn summary(&self, settings: &Settings) -> String {
        let f = FaxSettings::from_settings(settings);
        format!("FAX {}/{}", f.lpm, f.ioc)
    }

    fn build_processor(
        &self,
        _: &Settings,
        source_rate_hz: f64,
        offset_hz: f64,
    ) -> Box<dyn VfoProcessor> {
        Box::new(FaxProcessor::new(source_rate_hz, offset_hz))
    }
}

/// The fax DSP: a USB receiver 1900 Hz below the VFO; its audio plays and is
/// handed to the UI as 16-bit samples.
pub struct FaxProcessor {
    chain: VfoChain,
    pending: Vec<i16>,
}

impl FaxProcessor {
    pub fn new(source_rate: f64, offset: f64) -> Self {
        Self {
            chain: VfoChain::new(
                source_rate,
                offset - CARRIER_HZ,
                Mode::Usb,
                AUDIO_BANDWIDTH_HZ,
                AmAgcMode::default(),
            ),
            pending: Vec::new(),
        }
    }
}

/// Audio (-1..1) as 16-bit samples.
pub fn to_i16(audio: &[f32]) -> impl Iterator<Item = i16> + '_ {
    audio
        .iter()
        .map(|&a| (a * 32767.0).round().clamp(-32768.0, 32767.0) as i16)
}

impl VfoProcessor for FaxProcessor {
    fn process(&mut self, wideband: &[Complex32]) -> Vec<f32> {
        let audio = self.chain.process(wideband);
        self.pending.extend(to_i16(&audio));
        audio
    }

    fn channel_iq(&self) -> (&[Complex32], f64) {
        (self.chain.last_channel(), self.chain.if_rate())
    }

    fn channel_center_offset(&self) -> f64 {
        self.chain.channel_center_offset() - CARRIER_HZ
    }

    fn meters(&self) -> ProcessorMeters {
        ProcessorMeters {
            level_db: self.chain.level_db(),
            agc_gain_db: self.chain.agc_gain_db(),
            decoder: None,
        }
    }

    fn take_data(&mut self) -> Vec<DataItem> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        vec![DataItem::FaxAudio {
            samples: std::mem::take(&mut self.pending),
            rate: crate::engine::AUDIO_RATE as f64,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_pictures_folder_is_dropped() {
        // F2 Save writes to the recordings folder since 0.2.4
        let mut s = Settings::new();
        s.insert("save_dir".into(), json!("/old/pictures"));
        FaxType.normalize(&mut s, None);
        assert!(!s.contains_key("save_dir"));
        let schema = FaxType.schema(&s);
        assert!(schema.fields.iter().all(|f| f.key != "save_dir"));
    }

    #[test]
    fn settings_fill_in_and_clamp() {
        let t = FaxType;
        let mut s = Settings::new();
        s.insert(LPM.into(), json!(77));
        s.insert(CLOCK_PPM.into(), json!(-900.123));
        s.insert("future".into(), json!(1));
        t.normalize(&mut s, None);
        let f = FaxSettings::from_settings(&s);
        assert_eq!(f.lpm, 120);
        assert_eq!(f.ioc, 576);
        assert_eq!(f.width(), 1809);
        assert!(f.auto_slant);
        assert_eq!(f.clock_ppm, -MAX_PPM);
        assert_eq!(s["future"], 1, "unknown keys are kept");
        s.insert(IOC.into(), json!(288));
        assert_eq!(FaxSettings::from_settings(&s).width(), 904);
        assert!(
            t.dsp_settings(&s).is_empty(),
            "no setting rebuilds the chain"
        );
    }

    #[test]
    fn the_carrier_lands_on_1900_hz_audio_and_reaches_the_ui_as_samples() {
        // a carrier 300 Hz above the VFO must come out at 2200 Hz
        let rate = 48_000.0;
        let n = 48_000;
        let iq: Vec<Complex32> = (0..n)
            .map(|i| {
                let ph = 2.0 * std::f64::consts::PI * 300.0 * i as f64 / rate;
                Complex32::new(ph.cos() as f32, ph.sin() as f32) * 0.1
            })
            .collect();
        let mut p = FaxProcessor::new(rate, 0.0);
        let mut audio = Vec::new();
        for b in iq.chunks(4800) {
            audio.extend(p.process(b));
        }
        let data = p.take_data();
        let DataItem::FaxAudio { samples, rate: r } = &data[0] else {
            panic!("fax audio expected");
        };
        assert_eq!(*r, 48_000.0);
        assert_eq!(samples.len(), audio.len());
        assert!(p.take_data().is_empty(), "taken once");
        // count zero crossings in the second half (after the AGC settles)
        let half = &audio[audio.len() / 2..];
        let crossings = half
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let hz = crossings as f64 / 2.0 / (half.len() as f64 / 48_000.0);
        assert!((hz - 2200.0).abs() < 30.0, "{hz} Hz");
    }

    #[test]
    fn samples_are_scaled_and_clipped() {
        let v: Vec<i16> = to_i16(&[0.0, 0.5, -1.0, 2.0, -2.0]).collect();
        assert_eq!(v, [0, 16384, -32767, 32767, -32768]);
    }
}
