//! The `Voice` VFO type: the audio demodulators (NFM/WFM/AM/DSB/USB/LSB/CW)
//! with SDR++'s chain (`dsp::chain::VfoChain`).

use num_complex::Complex32;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{NativeOutput, ProcessorMeters, Schema, Settings, VOICE, VfoProcessor, VfoType};
use crate::dsp::chain::VfoChain;
use crate::vfo::model::{AmAgcMode, CW_TONE_MAX_HZ, CW_TONE_MIN_HZ, Mode};

/// Highest bandwidth the Bandwidth field allows (Hz; WFM is 150 kHz).
pub const MAX_BANDWIDTH_HZ: f64 = 500_000.0;
/// Snap field step (Hz).
pub const SNAP_STEP_HZ: i64 = 500;

/// Voice settings as a typed struct (the JSON object's keys).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    pub mode: Mode,
    pub bandwidth_hz: f64,
    pub snap_hz: u64,
    pub cw_tone_hz: f64,
    pub am_agc: AmAgcMode,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self::for_mode(Mode::Nfm)
    }
}

impl VoiceSettings {
    /// The defaults of `mode`.
    pub fn for_mode(mode: Mode) -> Self {
        Self {
            mode,
            bandwidth_hz: mode.default_bandwidth(),
            snap_hz: mode.default_snap_hz(),
            cw_tone_hz: crate::dsp::chain::DEFAULT_CW_TONE_HZ,
            am_agc: AmAgcMode::default(),
        }
    }

    /// From a settings object (missing or bad keys = defaults).
    pub fn from_settings(s: &Settings) -> Self {
        let mut out = Self::default();
        if let Some(m) = s
            .get("mode")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            out = Self::for_mode(m);
        }
        if let Some(bw) = s.get("bandwidth_hz").and_then(Value::as_f64) {
            out.bandwidth_hz = bw;
        }
        if let Some(sn) = s.get("snap_hz").and_then(Value::as_u64) {
            out.snap_hz = sn;
        }
        if let Some(t) = s.get("cw_tone_hz").and_then(Value::as_f64) {
            out.cw_tone_hz = t;
        }
        if let Some(a) = s
            .get("am_agc")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            out.am_agc = a;
        }
        out
    }

    /// As a settings object.
    pub fn to_settings(&self) -> Settings {
        match serde_json::to_value(self) {
            Ok(Value::Object(m)) => m,
            _ => Settings::new(),
        }
    }

    /// Clamp into the allowed ranges.
    pub fn clamped(mut self) -> Self {
        self.bandwidth_hz = self
            .bandwidth_hz
            .clamp(self.mode.min_bandwidth(), MAX_BANDWIDTH_HZ);
        self.cw_tone_hz = self.cw_tone_hz.clamp(CW_TONE_MIN_HZ, CW_TONE_MAX_HZ);
        self
    }

    /// The lowest IQ rate these settings decode from (spec 13.6): AM/FM need
    /// the channel (1.25 x bandwidth); SSB/DSB sit beside the dial and are
    /// shifted by bw/2 before `Re()`, so they need 2.2 x bandwidth; CW is
    /// shifted up by the pitch, so it needs 2.2 x (pitch + bw/2).
    pub fn min_baseband_rate(&self) -> f64 {
        match self.mode {
            Mode::Am | Mode::Nfm | Mode::Wfm => 1.25 * self.bandwidth_hz,
            Mode::Dsb | Mode::Usb | Mode::Lsb => 2.2 * self.bandwidth_hz,
            Mode::Cw => 2.2 * (self.cw_tone_hz + self.bandwidth_hz / 2.0),
        }
    }
}

/// The Voice type.
pub struct VoiceType;

fn options<T: Serialize + Copy>(all: &[T], label: impl Fn(T) -> &'static str) -> Value {
    Value::Array(
        all.iter()
            .map(|m| json!({"value": serde_json::to_value(m).unwrap(), "label": label(*m)}))
            .collect(),
    )
}

impl VfoType for VoiceType {
    fn id(&self) -> &'static str {
        VOICE
    }

    fn schema(&self, settings: &Settings) -> Schema {
        let v = VoiceSettings::from_settings(settings);
        let d = VoiceSettings::for_mode(v.mode);
        serde_json::from_value(json!({
            "vfo_type": VOICE,
            "fields": [
                {"key": "mode", "label": "Mode", "kind": "choice", "default": "Nfm",
                 "bookmark": true, "options": options(&Mode::ALL, Mode::label)},
                {"key": "bandwidth_hz", "label": "Bandwidth", "kind": "number",
                 "default": d.bandwidth_hz, "min": v.mode.min_bandwidth(),
                 "max": MAX_BANDWIDTH_HZ, "step": v.mode.bandwidth_step(), "unit": "Hz",
                 "bookmark": true},
                {"key": "snap_hz", "label": "Snap", "kind": "integer",
                 "default": d.snap_hz, "min": 0, "max": 1_000_000, "step": SNAP_STEP_HZ,
                 "unit": "Hz"},
                {"key": "cw_tone_hz", "label": "CW tone", "kind": "number",
                 "default": d.cw_tone_hz, "min": CW_TONE_MIN_HZ, "max": CW_TONE_MAX_HZ,
                 "step": 10.0, "unit": "Hz", "bookmark": true,
                 "visible_if": {"mode": ["Cw"]}},
                {"key": "am_agc", "label": "AM AGC", "kind": "choice", "default": "Audio",
                 "bookmark": true, "options": options(&AmAgcMode::ALL, AmAgcMode::label),
                 "visible_if": {"mode": ["Am"]}}
            ]
        }))
        .expect("voice schema is valid")
    }

    fn normalize(&self, settings: &mut Settings, changed: Option<&str>) {
        let mut v = VoiceSettings::from_settings(settings);
        if changed == Some("mode") {
            // A mode change resets bandwidth and snap to the mode defaults.
            let d = VoiceSettings::for_mode(v.mode);
            v.bandwidth_hz = d.bandwidth_hz;
            v.snap_hz = d.snap_hz;
        }
        let keep: Vec<(String, Value)> = settings
            .iter()
            .filter(|(k, _)| !VOICE_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        *settings = v.clamped().to_settings();
        // Keys this build does not know are kept (forward compatibility).
        settings.extend(keep);
    }

    fn native_output(&self) -> NativeOutput {
        NativeOutput::Audio
    }

    fn native_token(&self) -> &'static str {
        "audio"
    }

    fn min_baseband_rate(&self, settings: &Settings) -> f64 {
        VoiceSettings::from_settings(settings).min_baseband_rate()
    }

    fn bandwidth_hz(&self, settings: &Settings) -> f64 {
        VoiceSettings::from_settings(settings).bandwidth_hz
    }

    fn snap_hz(&self, settings: &Settings) -> u64 {
        VoiceSettings::from_settings(settings).snap_hz
    }

    fn mode_label(&self, settings: &Settings) -> String {
        VoiceSettings::from_settings(settings)
            .mode
            .label()
            .to_string()
    }

    fn summary(&self, settings: &Settings) -> String {
        let v = VoiceSettings::from_settings(settings);
        let mut s = format!(
            "{} {}",
            v.mode.label(),
            super::schema::hz_label(v.bandwidth_hz)
        );
        match v.mode {
            Mode::Cw => s.push_str(&format!(" {}Hz", super::schema::fmt_num(v.cw_tone_hz))),
            Mode::Am => s.push_str(&format!(" {}", v.am_agc.label())),
            _ => {}
        }
        s
    }

    fn build_processor(
        &self,
        settings: &Settings,
        source_rate_hz: f64,
        offset_hz: f64,
    ) -> Box<dyn VfoProcessor> {
        let v = VoiceSettings::from_settings(settings);
        Box::new(VfoChain::new_with_tone(
            source_rate_hz,
            offset_hz,
            v.mode,
            v.bandwidth_hz,
            v.am_agc,
            v.cw_tone_hz,
        ))
    }
}

/// The keys Voice owns.
const VOICE_KEYS: [&str; 5] = ["mode", "bandwidth_hz", "snap_hz", "cw_tone_hz", "am_agc"];

impl VfoProcessor for VfoChain {
    fn process(&mut self, wideband: &[Complex32]) -> Vec<f32> {
        VfoChain::process(self, wideband)
    }

    fn channel_iq(&self) -> (&[Complex32], f64) {
        (self.last_channel(), self.if_rate())
    }

    fn channel_center_offset(&self) -> f64 {
        self.channel_center_offset()
    }

    fn meters(&self) -> ProcessorMeters {
        ProcessorMeters {
            level_db: self.level_db(),
            agc_gain_db: self.agc_gain_db(),
            decoder: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_json() {
        let v = VoiceSettings {
            mode: Mode::Usb,
            bandwidth_hz: 2700.0,
            snap_hz: 100,
            cw_tone_hz: 700.0,
            am_agc: AmAgcMode::Carrier,
        };
        let s = v.to_settings();
        assert_eq!(s["mode"], "Usb");
        assert_eq!(s["am_agc"], "Carrier");
        assert_eq!(VoiceSettings::from_settings(&s), v);
    }

    #[test]
    fn normalize_resets_on_mode_change_and_clamps() {
        let t = VoiceType;
        let mut s = VoiceSettings::for_mode(Mode::Usb).to_settings();
        s.insert("mode".into(), json!("Am"));
        t.normalize(&mut s, Some("mode"));
        assert_eq!(s["bandwidth_hz"], 10_000.0);
        assert_eq!(s["snap_hz"], 1_000);
        s.insert("cw_tone_hz".into(), json!(9999.0));
        s.insert("bandwidth_hz".into(), json!(1.0));
        t.normalize(&mut s, None);
        assert_eq!(s["cw_tone_hz"], CW_TONE_MAX_HZ);
        assert_eq!(s["bandwidth_hz"], Mode::Am.min_bandwidth());
    }

    #[test]
    fn normalize_fills_missing_keys_and_keeps_unknown_ones() {
        let mut s = Settings::new();
        s.insert("mode".into(), json!("Cw"));
        s.insert("future_key".into(), json!(true));
        VoiceType.normalize(&mut s, None);
        assert_eq!(s["bandwidth_hz"], 200.0);
        assert_eq!(s["future_key"], true);
    }

    #[test]
    fn schema_shows_cw_tone_only_in_cw_and_am_agc_only_in_am() {
        let t = VoiceType;
        let keys = |m: Mode| -> Vec<String> {
            let s = VoiceSettings::for_mode(m).to_settings();
            t.schema(&s)
                .visible_fields(&s, false)
                .iter()
                .map(|f| f.key.clone())
                .collect()
        };
        assert_eq!(keys(Mode::Usb), ["mode", "bandwidth_hz", "snap_hz"]);
        assert!(keys(Mode::Cw).contains(&"cw_tone_hz".to_string()));
        assert!(keys(Mode::Am).contains(&"am_agc".to_string()));
        let s = VoiceSettings::for_mode(Mode::Cw).to_settings();
        let bw = t.schema(&s);
        let bw = bw.field("bandwidth_hz").unwrap();
        assert_eq!(
            bw.step(&json!(200.0), -1),
            json!(100.0),
            "CW steps by 100 Hz"
        );
    }

    #[test]
    fn bookmark_part_excludes_snap() {
        let s = VoiceSettings::for_mode(Mode::Usb).to_settings();
        let part = VoiceType.schema(&s).bookmark_part(&s);
        assert!(part.contains_key("mode") && part.contains_key("cw_tone_hz"));
        assert!(!part.contains_key("snap_hz"));
    }

    #[test]
    fn min_baseband_rate_per_mode() {
        let r = |m: Mode| VoiceSettings::for_mode(m).min_baseband_rate();
        assert_eq!(r(Mode::Am), 12_500.0);
        assert_eq!(r(Mode::Usb), 2.2 * 2_800.0);
        assert_eq!(r(Mode::Cw), 2.2 * (800.0 + 100.0));
    }

    #[test]
    fn summary_and_mode_label() {
        let mut v = VoiceSettings::for_mode(Mode::Cw);
        v.cw_tone_hz = 650.0;
        assert_eq!(VoiceType.summary(&v.to_settings()), "CW 200 Hz 650Hz");
        assert_eq!(VoiceType.mode_label(&v.to_settings()), "CW");
    }

    #[test]
    fn the_registry_processor_sounds_exactly_like_the_chain() {
        // 0.2-2 review: the type seam must not change the audio.
        let sr = 250_000.0;
        let iq: Vec<Complex32> = (0..50_000)
            .map(|n| {
                let p = std::f64::consts::TAU * 31_000.0 * n as f64 / sr;
                Complex32::new(p.cos() as f32, p.sin() as f32) * 0.3
            })
            .collect();
        for mode in Mode::ALL {
            let v = VoiceSettings::for_mode(mode);
            let mut p = VoiceType.build_processor(&v.to_settings(), sr, 30_000.0);
            let mut c = VfoChain::new_with_tone(
                sr,
                30_000.0,
                v.mode,
                v.bandwidth_hz,
                v.am_agc,
                v.cw_tone_hz,
            );
            let (a, b): (Vec<f32>, Vec<f32>) = iq
                .chunks(1250)
                .map(|blk| (p.process(blk), c.process(blk)))
                .fold((vec![], vec![]), |(mut x, mut y), (pa, cb)| {
                    x.extend(pa);
                    y.extend(cb);
                    (x, y)
                });
            assert!(!a.is_empty(), "{mode:?} produced audio");
            assert_eq!(a, b, "{mode:?} identical");
        }
    }

    #[test]
    fn the_voice_processor_feeds_the_view() {
        let sr = 250_000.0;
        let v = VoiceSettings::for_mode(Mode::Usb);
        let mut p = VoiceType.build_processor(&v.to_settings(), sr, 10_000.0);
        assert_eq!(
            p.channel_center_offset(),
            1_400.0,
            "USB: bw/2 above the dial"
        );
        let iq: Vec<Complex32> = (0..25_000)
            .map(|n| {
                let ph = std::f64::consts::TAU * 11_000.0 * n as f64 / sr;
                Complex32::new(ph.cos() as f32, ph.sin() as f32) * 0.1
            })
            .collect();
        for blk in iq.chunks(1250) {
            p.process(blk);
        }
        let (ch, rate) = p.channel_iq();
        assert!(!ch.is_empty());
        assert!((24_000.0..48_000.0).contains(&rate), "the IF rate: {rate}");
        let m = p.meters();
        assert!(m.level_db > -40.0 && m.level_db < 0.0, "{}", m.level_db);
        assert!(m.agc_gain_db.is_some(), "SSB has an AGC");
    }
}
