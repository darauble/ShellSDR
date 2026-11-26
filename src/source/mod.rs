//! Source abstraction: the SDR device (or baseband file) that produces IQ.
//!
//! Step 2 scope: the `SdrSource` trait contract, shared capability/descriptor
//! types, and a deterministic mock implementation used by `--test` and by the
//! unit suite. The real SoapySDR-backed source and its streaming thread arrive
//! in step 3 (option b); this module defines the seam they must honor.
//
// NOTE(step2->3): the streaming half of the `SdrSource` seam (IqSink, the
// start/stop trait methods, MockSource::enumerate/generate, DeviceInfo) is
// fully tested here but not consumed by the running binary until step 3 wires
// the real SoapySDR streaming thread and a DSP consumer. The allow is removed
// then.
#![allow(dead_code)]

pub mod file;
mod mock;
pub mod model;
pub mod soapy;

pub use mock::MockSource;

/// IQ sample type used across ShellSDR: 32-bit complex float.
pub type IqSample = num_complex::Complex32;

/// A device as surfaced by enumeration: a human label plus the opaque args
/// string used to open it (e.g. `driver=sdrplay,mode=ST`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
    pub label: String,
    pub args: String,
}

/// A named gain element with its range in dB (e.g. RSPduo `IFGR` [20,59],
/// `RFGR` [0,9]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GainElement {
    pub name: String,
    pub min_db: f64,
    pub max_db: f64,
}

/// The capabilities a source advertises once opened.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    /// Discrete sample rates in Hz (capture widths).
    pub sample_rates: Vec<f64>,
    /// Discrete analog filter bandwidths in Hz.
    pub bandwidths: Vec<f64>,
    /// Named gain elements (order as the device reports them).
    pub gains: Vec<GainElement>,
    /// Antenna port names.
    pub antennas: Vec<String>,
    /// Tunable frequency range in Hz (inclusive).
    pub freq_range: (f64, f64),
    /// Whether hardware AGC is supported.
    pub agc_supported: bool,
}

/// Errors a source operation can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// The args did not match any device / could not open.
    Open(String),
    /// A configuration value was rejected (e.g. unsupported rate).
    Config(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::Open(m) => write!(f, "open error: {m}"),
            SourceError::Config(m) => write!(f, "config error: {m}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// A sink that receives IQ sample buffers from a running source. The source
/// calls `push` from its streaming context; implementations must not block.
pub trait IqSink: Send {
    fn push(&mut self, samples: &[IqSample]);
}

/// The contract every SDR source (real or mock) must honor.
pub trait SdrSource {
    /// Capabilities of the opened device.
    fn capabilities(&self) -> &Capabilities;

    /// Current configured sample rate (Hz).
    fn sample_rate(&self) -> f64;

    /// Set the sample rate; must be one of `capabilities().sample_rates`.
    fn set_sample_rate(&mut self, hz: f64) -> Result<(), SourceError>;

    /// Set the center/tuned frequency (Hz).
    fn set_frequency(&mut self, hz: f64) -> Result<(), SourceError>;

    /// Set a named gain element to `db`.
    fn set_gain(&mut self, element: &str, db: f64) -> Result<(), SourceError>;

    /// Whether the source is currently streaming.
    fn is_running(&self) -> bool;

    /// Start streaming; produce `count` sample buffers into `sink`. In the
    /// real source this spawns a thread; the mock produces synchronously for
    /// deterministic tests. Idempotent: starting a running source is a no-op.
    fn start(&mut self, sink: &mut dyn IqSink, buffers: usize);

    /// Stop streaming. Idempotent.
    fn stop(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test sink that records every buffer it receives.
    #[derive(Default)]
    struct CollectSink {
        frames: Vec<Vec<IqSample>>,
    }

    impl IqSink for CollectSink {
        fn push(&mut self, samples: &[IqSample]) {
            self.frames.push(samples.to_vec());
        }
    }

    #[test]
    fn mock_enumerate_open_configure_start_stop() {
        // Enumerate returns at least one deterministic device.
        let devs = MockSource::enumerate();
        assert!(!devs.is_empty(), "mock enumerates at least one device");

        // Open the first device.
        let mut src = MockSource::open(&devs[0].args).expect("open mock");

        // Capabilities include the RSP-like sample rate set.
        let caps = src.capabilities().clone();
        assert!(caps.sample_rates.contains(&2_000_000.0));

        // Configure a valid sample rate.
        src.set_sample_rate(2_000_000.0).expect("valid rate");
        assert_eq!(src.sample_rate(), 2_000_000.0);

        // An unsupported rate is rejected.
        assert!(matches!(
            src.set_sample_rate(1.234e6),
            Err(SourceError::Config(_))
        ));

        // Start produces the requested number of non-empty IQ buffers.
        let mut sink = CollectSink::default();
        assert!(!src.is_running());
        src.start(&mut sink, 3);
        assert!(src.is_running());
        assert_eq!(sink.frames.len(), 3, "produced 3 buffers");
        assert!(sink.frames.iter().all(|f| !f.is_empty()));

        // Start again while running is a no-op (idempotent).
        src.start(&mut sink, 3);
        assert_eq!(
            sink.frames.len(),
            3,
            "no extra buffers while already running"
        );

        // Stop is idempotent.
        src.stop();
        assert!(!src.is_running());
        src.stop();
        assert!(!src.is_running());
    }
}
