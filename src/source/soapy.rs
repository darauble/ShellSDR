//! Real SoapySDR-backed source (hardware).
//!
//! Piece B2. Wraps a `soapysdr::Device`, exposes its capabilities (sample
//! rates, bandwidths, named gain elements incl. IFGR/RFGR, antennas, freq
//! range, AGC) through the same `Capabilities` type the mock uses, and
//! provides a blocking `read` for the DSP thread to pull IQ blocks.
//!
//! This module talks to hardware and is not exercised by the unit suite; it is
//! verified on the RSPduo by ear. Enumeration is a free function so the Source
//! module can list real devices.

use num_complex::Complex32;
use soapysdr::{Device, Direction, RxStream};

use super::{Capabilities, DeviceInfo, GainElement, SourceError};

const RX: Direction = Direction::Rx;
const CH: usize = 0;

/// Enumerate available SoapySDR devices as `DeviceInfo` (label + args string).
pub fn enumerate() -> Vec<DeviceInfo> {
    match soapysdr::enumerate("") {
        Ok(list) => list
            .into_iter()
            .map(|args| {
                let label = args
                    .get("label")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| args.to_string());
                DeviceInfo {
                    label,
                    args: args.to_string(),
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Like [`enumerate`], but gives up after `timeout` and returns `None`.
///
/// `SoapySDR::Device::enumerate("")` asks *every* installed driver module,
/// and a single misbehaving one can block for minutes (e.g. the UHD module
/// probing `localhost:5444` over IPv6 when `::1` is not on `lo`, so the TCP
/// connect has to time out). Running it on a helper thread keeps startup
/// responsive; on timeout the thread is left to finish (or not) on its own.
pub fn enumerate_with_timeout(timeout: std::time::Duration) -> Option<Vec<DeviceInfo>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("soapy-enumerate".into())
        .spawn(move || {
            let _ = tx.send(enumerate());
        })
        .ok()?;
    rx.recv_timeout(timeout).ok()
}

/// An opened SoapySDR device plus its probed capabilities.
pub struct SoapySource {
    dev: Device,
    caps: Capabilities,
    sample_rate: f64,
}

impl SoapySource {
    /// Open the device described by `args` (e.g. `driver=sdrplay,mode=ST`).
    pub fn open(args: &str) -> Result<Self, SourceError> {
        let dev = Device::new(args).map_err(|e| SourceError::Open(e.to_string()))?;
        let caps = Self::probe(&dev)?;
        let sample_rate = caps.sample_rates.first().copied().unwrap_or(2_000_000.0);
        dev.set_sample_rate(RX, CH, sample_rate)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        Ok(Self {
            dev,
            caps,
            sample_rate,
        })
    }

    fn probe(dev: &Device) -> Result<Capabilities, SourceError> {
        let map_err = |e: soapysdr::Error| SourceError::Open(e.to_string());

        let sample_rates = dev
            .get_sample_rate_range(RX, CH)
            .map_err(map_err)?
            .into_iter()
            .map(|r| r.minimum)
            .collect::<Vec<_>>();

        let bandwidths = dev
            .bandwidth_range(RX, CH)
            .map_err(map_err)?
            .into_iter()
            .map(|r| r.minimum)
            .collect::<Vec<_>>();

        let mut gains = Vec::new();
        if let Ok(names) = dev.list_gains(RX, CH) {
            for name in names {
                if let Ok(r) = dev.gain_element_range(RX, CH, name.as_str()) {
                    gains.push(GainElement {
                        name,
                        min_db: r.minimum,
                        max_db: r.maximum,
                    });
                }
            }
        }

        let antennas = dev.antennas(RX, CH).unwrap_or_default();

        let freq_range = dev
            .frequency_range(RX, CH)
            .ok()
            .and_then(|rs| {
                let lo = rs.iter().map(|r| r.minimum).fold(f64::INFINITY, f64::min);
                let hi = rs
                    .iter()
                    .map(|r| r.maximum)
                    .fold(f64::NEG_INFINITY, f64::max);
                if lo.is_finite() && hi.is_finite() {
                    Some((lo, hi))
                } else {
                    None
                }
            })
            .unwrap_or((0.0, 2_000_000_000.0));

        let agc_supported = dev.has_gain_mode(RX, CH).unwrap_or(false);

        Ok(Capabilities {
            sample_rates,
            bandwidths,
            gains,
            antennas,
            freq_range,
            agc_supported,
        })
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn set_sample_rate(&mut self, hz: f64) -> Result<(), SourceError> {
        self.dev
            .set_sample_rate(RX, CH, hz)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        self.sample_rate = hz;
        Ok(())
    }

    pub fn set_frequency(&mut self, hz: f64) -> Result<(), SourceError> {
        self.dev
            .set_frequency(RX, CH, hz, ())
            .map_err(|e| SourceError::Config(e.to_string()))
    }

    pub fn set_bandwidth(&mut self, hz: f64) -> Result<(), SourceError> {
        self.dev
            .set_bandwidth(RX, CH, hz)
            .map_err(|e| SourceError::Config(e.to_string()))
    }

    pub fn set_gain_element(&mut self, name: &str, db: f64) -> Result<(), SourceError> {
        self.dev
            .set_gain_element(RX, CH, name, db)
            .map_err(|e| SourceError::Config(e.to_string()))
    }

    pub fn set_agc(&mut self, on: bool) -> Result<(), SourceError> {
        self.dev
            .set_gain_mode(RX, CH, on)
            .map_err(|e| SourceError::Config(e.to_string()))
    }

    /// Select the RX antenna port by name (e.g. `Tuner 1 50 ohm`).
    pub fn set_antenna(&mut self, name: &str) -> Result<(), SourceError> {
        self.dev
            .set_antenna(RX, CH, name)
            .map_err(|e| SourceError::Config(e.to_string()))
    }

    /// Create and activate an RX stream of complex float samples.
    pub fn start_stream(&self) -> Result<RxStream<Complex32>, SourceError> {
        let mut stream = self
            .dev
            .rx_stream::<Complex32>(&[CH])
            .map_err(|e| SourceError::Open(e.to_string()))?;
        stream
            .activate(None)
            .map_err(|e| SourceError::Open(e.to_string()))?;
        Ok(stream)
    }
}
