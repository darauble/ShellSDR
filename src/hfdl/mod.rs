//! HFDL (ARINC 635) with full ACARS decoding (0.4.5): the HFDL-ACARS seed
//! (`../HFDL-ACARS-Decoder-Seed`) built into ShellSDR as a VFO type.
//!
//! - `phy`: the physical layer's constants and the frame decoder.
//! - `pdu`: the PDU layers and the session parser.
//! - `acars`, `apps`: ACARS and its applications.
//! - `stations`: the ground-station table.

pub mod acars;
pub mod apps;
pub mod enrich;
pub mod lookup;
pub mod msg;
pub mod pdu;
pub mod phy;
pub mod rx;
pub mod service;
pub mod stations;
