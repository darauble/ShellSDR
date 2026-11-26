//! The `DSC` VFO type (0.2.1, spec 14): a maritime DSC decoder fed by the
//! VFO's IQ. The VFO sits on the DSC frequency itself; the processor shifts
//! it to DC, decimates to a channel of at least 8 kHz, normalizes the level
//! and runs the seed's demodulator and receiver (`crate::dsc::decoder`) on
//! the complex samples. No audio: the native output is the decoded calls.

use num_complex::Complex32;
use serde_json::{Value, json};

use super::{
    DSC, DataItem, DecoderMeter, NativeOutput, ProcessorMeters, Schema, Settings, VfoProcessor,
    VfoType,
};
use crate::dsc::decoder::{Decoder, demod::MIN_IQ_RATE};
use crate::dsp::channelizer::Channelizer;
use crate::dsp::nco::Nco;

/// The settings key of the message folder (the only schema field).
pub const MESSAGE_DIR: &str = "message_dir";
/// RMS the channel is normalized to before the demodulator (16-bit scale:
/// the discriminator takes the log of integer levels).
pub const AGC_TARGET_RMS: f64 = 8000.0;
/// The AGC's averaging time (s).
const AGC_TIME_S: f64 = 1.0;
/// Channel width for markers and channel fit (Hz): the two tones at +/-85 Hz
/// with their 100 Bd keying.
pub const BANDWIDTH_HZ: f64 = 400.0;
/// DSC frequencies end in .0 or .5 kHz.
pub const SNAP_HZ: u64 = 500;

/// The message folder of `settings` (`~` expanded); empty = `default`,
/// the DSC type's recordings folder.
pub fn message_dir(settings: &Settings, default: &std::path::Path) -> std::path::PathBuf {
    match settings
        .get(MESSAGE_DIR)
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some(d) if !d.is_empty() => std::path::PathBuf::from(crate::app::expand_home(d)),
        _ => default.to_path_buf(),
    }
}

/// The DSC type.
pub struct DscType;

impl VfoType for DscType {
    fn id(&self) -> &'static str {
        DSC
    }

    fn schema(&self, _: &Settings) -> Schema {
        serde_json::from_value(json!({
            "vfo_type": DSC,
            "fields": [
                {"key": MESSAGE_DIR, "label": "Messages", "kind": "path", "default": ""}
            ]
        }))
        .expect("dsc schema is valid")
    }

    fn normalize(&self, settings: &mut Settings, _: Option<&str>) {
        if !settings.get(MESSAGE_DIR).is_some_and(Value::is_string) {
            settings.insert(MESSAGE_DIR.into(), Value::from(""));
        }
    }

    fn native_output(&self) -> NativeOutput {
        NativeOutput::Data
    }

    fn native_always_on(&self) -> bool {
        true
    }

    fn dsp_settings(&self, _: &Settings) -> Settings {
        Settings::new()
    }

    fn native_token(&self) -> &'static str {
        "dsc"
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
        DSC.to_string()
    }

    fn summary(&self, _: &Settings) -> String {
        DSC.to_string()
    }

    fn build_processor(
        &self,
        _: &Settings,
        source_rate_hz: f64,
        offset_hz: f64,
    ) -> Box<dyn VfoProcessor> {
        Box::new(DscProcessor::new(source_rate_hz, offset_hz))
    }
}

/// The DSC DSP: NCO -> channelizer -> AGC -> decoder.
pub struct DscProcessor {
    nco: Nco,
    chan: Channelizer,
    rate: f64,
    decoder: Decoder,
    /// Mean channel power (slow), None before the first block.
    power: Option<f64>,
    /// Channel samples fed to the decoder so far.
    fed: u64,
    last_chan: Vec<Complex32>,
    found: Vec<DataItem>,
}

impl DscProcessor {
    /// For wideband IQ at `source_rate` with the DSC frequency `offset` Hz
    /// from its centre.
    pub fn new(source_rate: f64, offset: f64) -> Self {
        let chan = Channelizer::new(source_rate, MIN_IQ_RATE);
        let rate = chan.out_rate();
        Self {
            nco: Nco::new(-offset, source_rate),
            chan,
            rate,
            decoder: Decoder::new_iq(rate),
            power: None,
            fed: 0,
            last_chan: Vec::new(),
            found: Vec::new(),
        }
    }

    /// The channel rate the decoder runs at.
    pub fn channel_rate(&self) -> f64 {
        self.rate
    }

    fn push_call(
        found: &mut Vec<DataItem>,
        c: crate::dsc::decoder::receiver::Call,
        fed: u64,
        rate: f64,
    ) {
        found.push(DataItem::DscCall {
            age_s: fed.saturating_sub(c.start_sample) as f64 / rate,
            symbols: c.symbols,
        });
    }
}

impl VfoProcessor for DscProcessor {
    fn process(&mut self, wideband: &[Complex32]) -> Vec<f32> {
        let mut x = wideband.to_vec();
        self.nco.mix(&mut x);
        let y = self.chan.process(&x);
        if y.is_empty() {
            return Vec::new();
        }
        let block = y.iter().map(|z| f64::from(z.norm_sqr())).sum::<f64>() / y.len() as f64;
        let w = (y.len() as f64 / (self.rate * AGC_TIME_S)).min(1.0);
        let p = match self.power {
            None => block,
            Some(p) => p + (block - p) * w,
        };
        self.power = Some(p);
        let gain = if p > 0.0 {
            (AGC_TARGET_RMS / p.sqrt()) as f32
        } else {
            0.0
        };
        let z: Vec<Complex32> = y.iter().map(|s| s * gain).collect();
        self.fed += z.len() as u64;
        let (fed, rate, found) = (self.fed, self.rate, &mut self.found);
        self.decoder
            .feed_iq(&z, &mut |c| Self::push_call(found, c, fed, rate));
        self.last_chan = y;
        Vec::new()
    }

    fn channel_iq(&self) -> (&[Complex32], f64) {
        (&self.last_chan, self.rate)
    }

    fn meters(&self) -> ProcessorMeters {
        let p = self.power.unwrap_or(0.0);
        ProcessorMeters {
            level_db: if p > 0.0 {
                (10.0 * p.log10()) as f32
            } else {
                -200.0
            },
            agc_gain_db: None,
            decoder: Some(DecoderMeter {
                level: self.decoder.level() as f32,
                decoding: self.decoder.is_decoding(),
            }),
        }
    }

    fn take_data(&mut self) -> Vec<DataItem> {
        std::mem::take(&mut self.found)
    }

    fn finish(&mut self) {
        let (fed, rate, found) = (self.fed, self.rate, &mut self.found);
        self.decoder
            .finish(&mut |c| Self::push_call(found, c, fed, rate));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::decoder::tx;

    /// The call of the conformance `real-recording` (parse vectors).
    const CALL: [u8; 23] = [
        120, 120, 0, 27, 11, 0, 10, 108, 35, 20, 3, 67, 50, 118, 126, 126, 126, 126, 126, 126, 126,
        117, 54,
    ];

    /// Wideband IQ at `rate` with the call keyed at `offset` Hz, between
    /// noise-free silences.
    fn call_iq(rate: f64, offset: f64, amp: f32) -> Vec<Complex32> {
        let mut bits = vec![0u8; 0];
        bits.extend(tx::call_bits(&CALL, 200));
        let mut iq = vec![Complex32::new(0.0, 0.0); (rate * 0.5) as usize];
        iq.extend(tx::fsk_iq(&bits, rate, offset, amp));
        iq.extend(vec![Complex32::new(0.0, 0.0); (rate * 2.0) as usize]);
        iq
    }

    fn run(p: &mut DscProcessor, iq: &[Complex32], block: usize) -> Vec<DataItem> {
        let mut out = Vec::new();
        for b in iq.chunks(block) {
            assert!(p.process(b).is_empty(), "no audio");
            out.extend(p.take_data());
        }
        p.finish();
        out.extend(p.take_data());
        out
    }

    fn symbols(items: &[DataItem]) -> Vec<Vec<u8>> {
        items
            .iter()
            .filter_map(|i| match i {
                DataItem::DscCall { symbols, .. } => Some(symbols.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_channel_runs_at_eight_khz_or_more() {
        assert_eq!(DscProcessor::new(2_000_000.0, 0.0).channel_rate(), 15_625.0);
        assert_eq!(DscProcessor::new(250_000.0, 0.0).channel_rate(), 15_625.0);
        assert_eq!(DscProcessor::new(48_000.0, 0.0).channel_rate(), 12_000.0);
        assert_eq!(DscProcessor::new(8_000.0, 0.0).channel_rate(), 8_000.0);
    }

    #[test]
    fn a_call_off_the_source_centre_is_decoded_from_iq() {
        let rate = 250_000.0;
        let iq = call_iq(rate, 37_500.0, 0.01);
        let mut p = DscProcessor::new(rate, 37_500.0);
        let found = run(&mut p, &iq, 4096);
        assert_eq!(symbols(&found), vec![CALL.to_vec()]);
    }

    #[test]
    fn the_level_does_not_matter_thanks_to_the_agc() {
        for amp in [1e-4f32, 0.5] {
            let rate = 96_000.0;
            let mut p = DscProcessor::new(rate, -12_000.0);
            let found = run(&mut p, &call_iq(rate, -12_000.0, amp), 2048);
            assert_eq!(symbols(&found), vec![CALL.to_vec()], "amplitude {amp}");
        }
    }

    #[test]
    fn a_call_on_another_frequency_is_not_decoded() {
        let rate = 96_000.0;
        let mut p = DscProcessor::new(rate, 0.0);
        assert!(run(&mut p, &call_iq(rate, 1_000.0, 0.1), 2048).is_empty());
    }

    #[test]
    fn the_age_is_the_time_since_the_phasing() {
        let rate = 96_000.0;
        let mut p = DscProcessor::new(rate, 0.0);
        let iq = call_iq(rate, 0.0, 0.1);
        let mut got = None;
        for b in iq.chunks(960) {
            p.process(b);
            if let Some(DataItem::DscCall { age_s, .. }) = p.take_data().pop() {
                got = Some(age_s);
                break;
            }
        }
        // The call is 200 dots (2 s) + the call itself; its phasing lies
        // just after the dots, so by the time the call (or its look-ahead)
        // ends, a few seconds of signal have passed since then.
        let age = got.expect("decoded");
        assert!((2.0..12.0).contains(&age), "{age}");
    }

    #[test]
    fn meters_report_the_tone_level_and_reading() {
        let rate = 96_000.0;
        let mut p = DscProcessor::new(rate, 0.0);
        let iq = call_iq(rate, 0.0, 0.1);
        let mut decoding = false;
        let mut level: f32 = 0.0;
        for b in iq.chunks(960) {
            p.process(b);
            let m = p.meters().decoder.unwrap();
            decoding |= m.decoding;
            level = level.max(m.level);
        }
        assert!(decoding, "DECODING while the call is read");
        assert!(level > 0.6, "{level}");
    }

    #[test]
    fn settings_hold_only_the_message_folder_and_never_rebuild_the_dsp() {
        let t = DscType;
        let d = t.defaults();
        assert_eq!(d.get(MESSAGE_DIR), Some(&Value::from("")));
        let mut s = Settings::new();
        s.insert("api_key".into(), Value::from("k"));
        t.normalize(&mut s, None);
        assert_eq!(s["api_key"], "k", "Tools settings are kept");
        assert_eq!(s[MESSAGE_DIR], "");
        assert!(t.dsp_settings(&s).is_empty());
        assert!(t.native_always_on());
        assert_eq!(t.native_output(), NativeOutput::Data);
        assert_eq!(t.min_baseband_rate(&s), 8000.0);
    }

    #[test]
    fn the_message_folder_defaults_to_the_recordings_folder() {
        let rec = std::path::Path::new("/data/dsc-rec");
        let mut s = Settings::new();
        assert_eq!(message_dir(&s, rec), rec);
        s.insert(MESSAGE_DIR.into(), Value::from("/srv/dsc"));
        assert_eq!(message_dir(&s, rec), std::path::PathBuf::from("/srv/dsc"));
    }
}
