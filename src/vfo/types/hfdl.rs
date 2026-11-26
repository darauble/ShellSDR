//! The `HFDL` VFO type (0.4.5, HFDL-ACARS seed): the VFO sits on an HFDL
//! band's center and the processor decodes every ground-station channel
//! inside its span (and inside the IQ band) at once with
//! `crate::hfdl::rx::Receiver`. No audio: the native output is the frames'
//! octets, parsed by the UI (`crate::hfdl::service`).

use num_complex::Complex32;
use serde_json::{Value, json};

use super::{
    DataItem, DecoderMeter, HFDL, NativeOutput, ProcessorMeters, Schema, Settings, VfoProcessor,
    VfoType,
};
use crate::hfdl::rx::{Receiver, channels_in_band};

pub use super::dsc::{MESSAGE_DIR, message_dir};

/// The settings key of the span around the VFO whose channels are decoded.
pub const SPAN_KHZ: &str = "span_khz";
/// The default span: the widest band (5.6 MHz, 272 kHz) fits.
pub const DEFAULT_SPAN_KHZ: f64 = 280.0;
const MIN_SPAN_KHZ: f64 = 10.0;
const MAX_SPAN_KHZ: f64 = 2000.0;
/// The settings keys the engine adds for the processor (absolute Hz).
pub const VFO_HZ_KEY: &str = "_vfo_hz";
pub const CENTER_HZ_KEY: &str = "_center_hz";
/// Band centers end in .0 or .5 kHz.
pub const SNAP_HZ: u64 = 500;
/// Seconds of signal between two channel status items.
const STATUS_EVERY_S: f64 = 0.5;

/// The span of `settings` (kHz).
pub fn span_khz(settings: &Settings) -> f64 {
    settings
        .get(SPAN_KHZ)
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_SPAN_KHZ)
        .clamp(MIN_SPAN_KHZ, MAX_SPAN_KHZ)
}

/// The channels decoded for a VFO at `vfo_hz` with `span_khz` in IQ
/// centered at `center_hz` at `rate`: those of the band whose signal
/// (assigned + 0 to + 3 kHz) lies inside the span.
pub fn channels(vfo_hz: f64, span_khz: f64, center_hz: f64, rate: f64) -> Vec<u32> {
    let half = span_khz * 500.0;
    channels_in_band(center_hz, rate)
        .into_iter()
        .filter(|&k| {
            let lo = k as f64 * 1000.0;
            lo >= vfo_hz - half && lo + 3000.0 <= vfo_hz + half
        })
        .collect()
}

/// The HFDL type.
pub struct HfdlType;

impl VfoType for HfdlType {
    fn id(&self) -> &'static str {
        HFDL
    }

    fn schema(&self, _: &Settings) -> Schema {
        serde_json::from_value(json!({
            "vfo_type": HFDL,
            "fields": [
                {"key": SPAN_KHZ, "label": "Span", "kind": "number", "default": DEFAULT_SPAN_KHZ,
                 "min": MIN_SPAN_KHZ, "max": MAX_SPAN_KHZ, "step": 10.0, "unit": "kHz",
                 "bookmark": true},
                {"key": MESSAGE_DIR, "label": "Messages", "kind": "path", "default": ""}
            ]
        }))
        .expect("hfdl schema is valid")
    }

    fn normalize(&self, settings: &mut Settings, _: Option<&str>) {
        if !settings.get(MESSAGE_DIR).is_some_and(Value::is_string) {
            settings.insert(MESSAGE_DIR.into(), Value::from(""));
        }
        let span = span_khz(settings);
        settings.insert(SPAN_KHZ.into(), json!(span));
    }

    fn native_output(&self) -> NativeOutput {
        NativeOutput::Data
    }

    fn native_always_on(&self) -> bool {
        true
    }

    fn dsp_settings(&self, settings: &Settings) -> Settings {
        let mut s = Settings::new();
        s.insert(SPAN_KHZ.into(), json!(span_khz(settings)));
        s
    }

    fn native_token(&self) -> &'static str {
        "hfdl"
    }

    fn min_baseband_rate(&self, settings: &Settings) -> f64 {
        // the receiver uses 90 % of the band
        span_khz(settings) * 1000.0 / 0.9
    }

    fn bandwidth_hz(&self, settings: &Settings) -> f64 {
        span_khz(settings) * 1000.0
    }

    fn snap_hz(&self, _: &Settings) -> u64 {
        SNAP_HZ
    }

    fn mode_label(&self, _: &Settings) -> String {
        HFDL.to_string()
    }

    fn summary(&self, settings: &Settings) -> String {
        format!("HFDL {:.0} kHz", span_khz(settings))
    }

    fn build_processor(
        &self,
        settings: &Settings,
        source_rate_hz: f64,
        offset_hz: f64,
    ) -> Box<dyn VfoProcessor> {
        let get = |k| settings.get(k).and_then(Value::as_f64);
        let center = get(CENTER_HZ_KEY).unwrap_or(0.0);
        let vfo = get(VFO_HZ_KEY).unwrap_or(center + offset_hz);
        Box::new(HfdlProcessor::new(
            source_rate_hz,
            center,
            &channels(vfo, span_khz(settings), center, source_rate_hz),
        ))
    }
}

/// The HFDL DSP: the band receiver over the chosen channels.
pub struct HfdlProcessor {
    rx: Option<Receiver>,
    rate: f64,
    /// Input samples processed.
    fed: u64,
    next_status: u64,
    found: Vec<DataItem>,
}

impl HfdlProcessor {
    /// For IQ at `rate` centered at `center_hz`, decoding `channels_khz`.
    pub fn new(rate: f64, center_hz: f64, channels_khz: &[u32]) -> Self {
        Self {
            rx: (!channels_khz.is_empty()).then(|| Receiver::new(rate, center_hz, channels_khz)),
            rate,
            fed: 0,
            next_status: 0,
            found: Vec::new(),
        }
    }

    /// The channels decoded (kHz).
    pub fn channels(&self) -> Vec<u32> {
        self.rx.as_ref().map(Receiver::channels).unwrap_or_default()
    }
}

impl VfoProcessor for HfdlProcessor {
    fn process(&mut self, wideband: &[Complex32]) -> Vec<f32> {
        let Some(rx) = self.rx.as_mut() else {
            return Vec::new();
        };
        let frames = rx.process(wideband);
        self.fed += wideband.len() as u64;
        let now = self.fed as f64 / self.rate;
        for f in frames {
            self.found.push(DataItem::HfdlFrame {
                freq_khz: f.freq_khz,
                m1: f.m1 as u8,
                octets: f.octets,
                age_s: (now - f.t).max(0.0),
                level_db: f.level_db,
                noise_db: f.noise_db,
                ferr_hz: f.freq_err_hz,
            });
        }
        if self.fed >= self.next_status {
            self.next_status = self.fed + (STATUS_EVERY_S * self.rate) as u64;
            self.found.push(DataItem::HfdlStatus {
                channels: rx.status(),
            });
        }
        Vec::new()
    }

    fn meters(&self) -> ProcessorMeters {
        let st = self.rx.as_ref().map(Receiver::status).unwrap_or_default();
        let level = st.iter().map(|c| c.level_db).fold(-200.0f32, f32::max);
        let snr = st
            .iter()
            .map(|c| c.level_db - c.noise_db)
            .fold(0.0f32, f32::max);
        ProcessorMeters {
            level_db: level,
            agc_gain_db: None,
            decoder: Some(DecoderMeter {
                level: (snr / 30.0).clamp(0.0, 1.0),
                decoding: st.iter().any(|c| c.decoding),
            }),
        }
    }

    fn take_data(&mut self) -> Vec<DataItem> {
        std::mem::take(&mut self.found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_span_picks_the_bands_channels_inside_it() {
        // the 8.9 MHz band at its suggested center, 250 kHz of IQ
        let all = channels(8_902_500.0, 280.0, 8_902_500.0, 250_000.0);
        assert!(all.contains(&8825) && all.contains(&8977), "{all:?}");
        let near = channels(8_942_000.0, 20.0, 8_902_500.0, 250_000.0);
        assert!(near.iter().all(|&k| (8932..=8949).contains(&k)), "{near:?}");
        assert!(near.contains(&8942));
        assert!(channels(7_000_000.0, 280.0, 7_000_000.0, 250_000.0).is_empty());
    }

    #[test]
    fn no_channels_is_a_silent_processor() {
        let mut p = HfdlProcessor::new(48_000.0, 7_000_000.0, &[]);
        assert!(p.process(&[Complex32::new(0.1, 0.0); 4800]).is_empty());
        assert!(p.take_data().is_empty());
        assert!(p.channels().is_empty());
    }

    #[test]
    fn the_processor_reports_channel_status() {
        let mut p = HfdlProcessor::new(250_000.0, 8_902_500.0, &[8942, 8977]);
        p.process(&vec![Complex32::new(0.0, 0.0); 25_000]);
        let d = p.take_data();
        let Some(DataItem::HfdlStatus { channels }) = d.last() else {
            panic!("{d:?}");
        };
        assert_eq!(
            channels.iter().map(|c| c.freq_khz).collect::<Vec<_>>(),
            [8942, 8977]
        );
    }
}
