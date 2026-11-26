//! Digital signal processing primitives for the single receive graph.
//!
//! Step 3: NCO frequency translation, decimation, and the demodulators. These
//! are pure/behavioral blocks tested with synthetic signals; the streaming
//! wiring that runs them on a real source thread is assembled in the DSP
//! thread (brought up with the real SoapySDR source).
//
// NOTE(stepA->B): these blocks are fully tested with synthetic signals but are
// not consumed by the running binary until Piece B wires the DSP thread
// (source -> NCO -> decimate -> demod -> resample -> cpal). The allow is
// removed then.
#![allow(dead_code)]

pub mod agc;
pub mod chain;
pub mod channelizer;
pub mod dc_blocker;
pub mod decim_taps;
pub mod deemphasis;
pub mod demod;
pub mod fft;
pub mod fir;
pub mod nco;
pub mod power_decimator;
pub mod resampler;
