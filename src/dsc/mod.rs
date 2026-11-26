//! Maritime DSC (ITU-R M.493-16) decoding and message data (0.2.1, spec
//! 14.1): ported from DSQrust, which was built from the DSC seed
//! (`DSC-Decoder-Seed`). The DSP front end feeds it complex IQ from a VFO
//! (`vfo::types::dsc`); this module knows nothing of the radio or the UI.

pub mod alert;
pub mod csv;
pub mod decoder;
pub mod domain;
pub mod enrich;
pub mod entries;
pub mod geo;
pub mod lookup;
pub mod message;
pub mod notify;
pub mod presentation;
pub mod record;
pub mod service;
pub mod settings;
pub mod stations;
pub mod store;
pub mod time;
pub mod view;
pub mod wav;
