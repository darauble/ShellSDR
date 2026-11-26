//! The `Digital` VFO type (0.2.5, spec 16): the text modes RSID knows, in
//! 0.2.5 the MFSK family with pictures. The VFO is the dial (an AM carrier,
//! a USB dial) and the signal sits at an audio offset (`carrier_hz`) in the
//! sideband chosen. The processor only shifts and decimates the IQ; the
//! decoder runs on the UI's worker (`crate::digital::service`), so a change
//! of mode or carrier never interrupts the stream. No audio is made.

use num_complex::Complex32;
use serde_json::{Value, json};

use super::{
    DIGITAL, DataItem, NativeOutput, ProcessorMeters, Schema, Settings, VfoProcessor, VfoType,
};
use crate::dsp::channelizer::Channelizer;
use crate::dsp::nco::Nco;
use crate::mfsk::decoder::{CARRIER_RANGE, Config, DEFAULT_SQUELCH};
use crate::mfsk::frontend::Sideband;
use crate::mfsk::modes::{self, MODES};

/// Settings keys.
pub const MODE: &str = "mode";
pub const SIDEBAND: &str = "sideband";
pub const CARRIER: &str = "carrier_hz";
pub const AFC: &str = "afc";
pub const RSID: &str = "rsid";
pub const SQUELCH: &str = "squelch";

/// The channel rate's floor: the sidebands reach 3.8 kHz each side.
pub const MIN_IQ_RATE: f64 = 8000.0;
/// The channel width for markers and channel fit, centred on the VFO.
pub const BANDWIDTH_HZ: f64 = 7600.0;
pub const SNAP_HZ: u64 = 100;
pub const DEFAULT_CARRIER: f64 = 1500.0;

/// The receiver's settings from a VFO's `Digital` table.
pub fn config(s: &Settings) -> Config {
    let num = |k: &str| s.get(k).and_then(Value::as_f64);
    let flag = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(true);
    Config {
        mode: s
            .get(MODE)
            .and_then(Value::as_str)
            .and_then(modes::by_name)
            .unwrap_or_else(|| modes::by_name(modes::DEFAULT).expect("the default mode")),
        carrier: num(CARRIER).unwrap_or(DEFAULT_CARRIER),
        sideband: s
            .get(SIDEBAND)
            .and_then(Value::as_str)
            .and_then(Sideband::from_name)
            .unwrap_or_default(),
        afc: flag(AFC),
        rsid: flag(RSID),
        squelch: num(SQUELCH).unwrap_or(DEFAULT_SQUELCH),
    }
}

/// The Digital type.
pub struct DigitalType;

impl VfoType for DigitalType {
    fn id(&self) -> &'static str {
        DIGITAL
    }

    fn schema(&self, _: &Settings) -> Schema {
        let modes: Vec<Value> = MODES
            .iter()
            .map(|m| json!({"value": m.name, "label": m.name}))
            .collect();
        let sides: Vec<Value> = Sideband::ALL
            .iter()
            .map(|b| json!({"value": b.name(), "label": b.name()}))
            .collect();
        serde_json::from_value(json!({
            "vfo_type": DIGITAL,
            "fields": [
                {"key": MODE, "label": "Mode", "kind": "choice", "default": modes::DEFAULT,
                 "bookmark": true, "options": modes},
                {"key": SIDEBAND, "label": "Sideband", "kind": "choice", "default": "Upper",
                 "bookmark": true, "options": sides},
                {"key": CARRIER, "label": "Carrier", "kind": "number", "default": DEFAULT_CARRIER,
                 "min": CARRIER_RANGE.0, "max": CARRIER_RANGE.1, "step": 10.0, "unit": "Hz",
                 "bookmark": true},
                {"key": AFC, "label": "AFC", "kind": "bool", "default": true},
                {"key": RSID, "label": "RSID", "kind": "bool", "default": true},
                {"key": SQUELCH, "label": "Squelch", "kind": "integer", "default": DEFAULT_SQUELCH as i64,
                 "min": 0, "max": 100, "step": 5}
            ]
        }))
        .expect("digital schema is valid")
    }

    fn normalize(&self, settings: &mut Settings, _: Option<&str>) {
        for (k, v) in self.defaults() {
            settings.entry(k).or_insert(v);
        }
        let c = config(settings);
        settings.insert(MODE.into(), json!(c.mode.name));
        settings.insert(SIDEBAND.into(), json!(c.sideband.name()));
        let carrier = (c.carrier.clamp(CARRIER_RANGE.0, CARRIER_RANGE.1) * 10.0).round() / 10.0;
        settings.insert(CARRIER.into(), json!(carrier));
        settings.insert(
            SQUELCH.into(),
            json!(c.squelch.clamp(0.0, 100.0).round() as i64),
        );
    }

    fn native_output(&self) -> NativeOutput {
        NativeOutput::Data
    }

    /// No setting changes the processor (the worker takes the sideband
    /// too), so nothing interrupts the stream.
    fn dsp_settings(&self, _: &Settings) -> Settings {
        Settings::new()
    }

    fn native_token(&self) -> &'static str {
        "digital"
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

    fn mode_label(&self, settings: &Settings) -> String {
        config(settings).mode.name.to_string()
    }

    fn summary(&self, settings: &Settings) -> String {
        let c = config(settings);
        format!("{} {} {:.0} Hz", c.mode.name, c.sideband.name(), c.carrier)
    }

    fn build_processor(
        &self,
        _: &Settings,
        source_rate_hz: f64,
        offset_hz: f64,
    ) -> Box<dyn VfoProcessor> {
        Box::new(DigitalProcessor::new(source_rate_hz, offset_hz))
    }
}

/// The Digital DSP: the VFO to DC, decimated to at least 8 kHz, handed to
/// the UI as it is.
pub struct DigitalProcessor {
    nco: Nco,
    chan: Channelizer,
    rate: f64,
    last: Vec<Complex32>,
    pending: Vec<Complex32>,
    power: f32,
}

impl DigitalProcessor {
    pub fn new(source_rate: f64, offset: f64) -> Self {
        let chan = Channelizer::new(source_rate, MIN_IQ_RATE);
        Self {
            nco: Nco::new(-offset, source_rate),
            rate: chan.out_rate(),
            chan,
            last: Vec::new(),
            pending: Vec::new(),
            power: 0.0,
        }
    }

    pub fn channel_rate(&self) -> f64 {
        self.rate
    }
}

impl VfoProcessor for DigitalProcessor {
    fn process(&mut self, wideband: &[Complex32]) -> Vec<f32> {
        let mut x = wideband.to_vec();
        self.nco.mix(&mut x);
        let y = self.chan.process(&x);
        if !y.is_empty() {
            self.power = y.iter().map(|z| z.norm_sqr()).sum::<f32>() / y.len() as f32;
        }
        self.pending.extend_from_slice(&y);
        self.last = y;
        Vec::new()
    }

    fn channel_iq(&self) -> (&[Complex32], f64) {
        (&self.last, self.rate)
    }

    fn meters(&self) -> ProcessorMeters {
        ProcessorMeters {
            level_db: if self.power > 0.0 {
                10.0 * self.power.log10()
            } else {
                -200.0
            },
            agc_gain_db: None,
            decoder: None,
        }
    }

    fn take_data(&mut self) -> Vec<DataItem> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        vec![DataItem::DigitalIq {
            samples: std::mem::take(&mut self.pending),
            rate: self.rate,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_fill_in_and_clamp() {
        let mut s = Settings::new();
        s.insert(MODE.into(), json!("MFSK-999"));
        s.insert(CARRIER.into(), json!(9000.0));
        s.insert(SQUELCH.into(), json!(140));
        s.insert("future".into(), json!(1));
        DigitalType.normalize(&mut s, None);
        let c = config(&s);
        assert_eq!(c.mode.name, "MFSK-32");
        assert_eq!(c.carrier, CARRIER_RANGE.1);
        assert_eq!(c.sideband, Sideband::Upper);
        assert!(c.afc && c.rsid);
        assert_eq!(c.squelch, 100.0);
        assert_eq!(s["future"], 1, "unknown keys are kept");
        assert_eq!(DigitalType.summary(&s), "MFSK-32 Upper 3800 Hz");
        assert!(DigitalType.dsp_settings(&s).is_empty());
        let bookmark: Vec<String> = DigitalType
            .schema(&s)
            .fields
            .iter()
            .filter(|f| f.bookmark)
            .map(|f| f.key.clone())
            .collect();
        assert_eq!(bookmark, [MODE, SIDEBAND, CARRIER]);
    }

    #[test]
    fn the_channel_reaches_the_ui_at_8_khz_or_more() {
        let rate = 2_000_000.0;
        // a tone 1500 Hz above a VFO that is 100 kHz above the centre
        let iq: Vec<Complex32> = (0..200_000)
            .map(|i| {
                let ph = std::f64::consts::TAU * 101_500.0 * i as f64 / rate;
                Complex32::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        let mut p = DigitalProcessor::new(rate, 100_000.0);
        assert_eq!(p.channel_rate(), 15_625.0);
        for b in iq.chunks(20_000) {
            assert!(p.process(b).is_empty(), "no audio");
        }
        let data = p.take_data();
        let DataItem::DigitalIq { samples, rate: r } = &data[0] else {
            panic!("IQ expected");
        };
        assert_eq!(*r, 15_625.0);
        let tail = &samples[samples.len() / 2..];
        let turn: f64 = tail
            .windows(2)
            .map(|w| f64::from((w[1] * w[0].conj()).arg()))
            .sum::<f64>()
            / (tail.len() - 1) as f64;
        let hz = turn * r / std::f64::consts::TAU;
        assert!((hz - 1500.0).abs() < 1.0, "{hz}");
        assert!(p.take_data().is_empty(), "taken once");
    }
}
