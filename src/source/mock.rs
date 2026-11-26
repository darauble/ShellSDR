//! Deterministic mock SDR source for `--test` and the unit suite.
//!
//! Capability values mirror what the step-0b spike observed on the RSPduo so
//! the capability-to-UI mapping can be exercised against realistic data
//! without hardware. Streaming synthesizes a simple complex tone so downstream
//! DSP steps have a known input.

use super::{Capabilities, DeviceInfo, GainElement, IqSample, IqSink, SdrSource, SourceError};

/// Number of IQ samples the mock emits per buffer.
const BUFFER_LEN: usize = 1024;

pub struct MockSource {
    caps: Capabilities,
    sample_rate: f64,
    frequency: f64,
    running: bool,
    /// Running sample index, so generated tone is continuous across buffers.
    phase_idx: u64,
}

impl MockSource {
    /// Enumerate deterministic devices resembling an RSPduo's mode variants.
    pub fn enumerate() -> Vec<DeviceInfo> {
        vec![
            DeviceInfo {
                label: "Mock RSPduo - Single Tuner".to_string(),
                args: "driver=mock,mode=ST".to_string(),
            },
            DeviceInfo {
                label: "Mock RSPduo - Master".to_string(),
                args: "driver=mock,mode=MA".to_string(),
            },
        ]
    }

    /// Open a mock device by args. Any args beginning with `driver=mock` open.
    pub fn open(args: &str) -> Result<Self, SourceError> {
        if !args.starts_with("driver=mock") {
            return Err(SourceError::Open(format!("not a mock device: {args}")));
        }
        Ok(Self {
            caps: Self::rsp_like_caps(),
            sample_rate: 2_000_000.0,
            frequency: 100_000_000.0,
            running: false,
            phase_idx: 0,
        })
    }

    /// RSPduo-like capabilities captured from the step-0b spike.
    fn rsp_like_caps() -> Capabilities {
        Capabilities {
            sample_rates: vec![
                62_500.0,
                96_000.0,
                125_000.0,
                192_000.0,
                250_000.0,
                384_000.0,
                500_000.0,
                768_000.0,
                1_000_000.0,
                2_000_000.0,
            ],
            bandwidths: vec![
                200_000.0,
                300_000.0,
                600_000.0,
                1_536_000.0,
                5_000_000.0,
                6_000_000.0,
                7_000_000.0,
                8_000_000.0,
            ],
            gains: vec![
                GainElement {
                    name: "IFGR".to_string(),
                    min_db: 20.0,
                    max_db: 59.0,
                },
                GainElement {
                    name: "RFGR".to_string(),
                    min_db: 0.0,
                    max_db: 9.0,
                },
            ],
            antennas: vec![
                "Tuner 1 50 ohm".to_string(),
                "Tuner 1 Hi-Z".to_string(),
                "Tuner 2 50 ohm".to_string(),
            ],
            freq_range: (1_000.0, 2_000_000_000.0),
            agc_supported: true,
        }
    }

    /// Generate one buffer of a continuous complex tone at ~1/8 the sample
    /// rate, advancing `phase_idx` so buffers stitch together.
    fn generate(&mut self) -> Vec<IqSample> {
        let mut out = Vec::with_capacity(BUFFER_LEN);
        let tone_hz = self.sample_rate / 8.0;
        let two_pi = std::f32::consts::TAU;
        for _ in 0..BUFFER_LEN {
            let t = self.phase_idx as f32 / self.sample_rate as f32;
            let phase = two_pi * tone_hz as f32 * t;
            out.push(IqSample::new(phase.cos(), phase.sin()));
            self.phase_idx = self.phase_idx.wrapping_add(1);
        }
        out
    }
}

impl SdrSource for MockSource {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn set_sample_rate(&mut self, hz: f64) -> Result<(), SourceError> {
        if self.caps.sample_rates.contains(&hz) {
            self.sample_rate = hz;
            Ok(())
        } else {
            Err(SourceError::Config(format!("unsupported sample rate {hz}")))
        }
    }

    fn set_frequency(&mut self, hz: f64) -> Result<(), SourceError> {
        let (lo, hi) = self.caps.freq_range;
        if (lo..=hi).contains(&hz) {
            self.frequency = hz;
            Ok(())
        } else {
            Err(SourceError::Config(format!("frequency {hz} out of range")))
        }
    }

    fn set_gain(&mut self, element: &str, db: f64) -> Result<(), SourceError> {
        match self.caps.gains.iter().find(|g| g.name == element) {
            Some(g) if (g.min_db..=g.max_db).contains(&db) => Ok(()),
            Some(g) => Err(SourceError::Config(format!(
                "{element} {db} out of [{}, {}]",
                g.min_db, g.max_db
            ))),
            None => Err(SourceError::Config(format!("no gain element {element}"))),
        }
    }

    fn is_running(&self) -> bool {
        self.running
    }

    fn start(&mut self, sink: &mut dyn IqSink, buffers: usize) {
        if self.running {
            return;
        }
        self.running = true;
        for _ in 0..buffers {
            let buf = self.generate();
            sink.push(&buf);
        }
    }

    fn stop(&mut self) {
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_rejects_non_mock_args() {
        assert!(matches!(
            MockSource::open("driver=sdrplay"),
            Err(SourceError::Open(_))
        ));
    }

    #[test]
    fn frequency_and_gain_bounds_enforced() {
        let mut src = MockSource::open("driver=mock,mode=ST").unwrap();
        assert!(src.set_frequency(100.0e6).is_ok());
        assert!(src.set_frequency(5.0e9).is_err(), "above range rejected");
        assert!(src.set_gain("IFGR", 30.0).is_ok());
        assert!(
            src.set_gain("IFGR", 10.0).is_err(),
            "below IFGR min rejected"
        );
        assert!(
            src.set_gain("NOPE", 0.0).is_err(),
            "unknown element rejected"
        );
    }
}
