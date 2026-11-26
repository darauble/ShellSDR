//! A received message with its reception context: one line of a day file
//! (seed spec/05-data/record.schema.json). All message fields derive from
//! the symbols (STO-05).

use crate::dsc::decoder::parser;
use crate::dsc::message::{Message, Symbol};

/// Where the time of a message came from (TIM-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSource {
    Filename,
    Mtime,
    Wallclock,
}

impl TimeSource {
    pub fn name(self) -> &'static str {
        match self {
            TimeSource::Filename => "filename",
            TimeSource::Mtime => "mtime",
            TimeSource::Wallclock => "wallclock",
        }
    }
}

/// Where a station position came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosSource {
    Builtin,
    Csv,
    Dsc,
    AprsFi,
}

impl PosSource {
    pub fn name(self) -> &'static str {
        match self {
            PosSource::Builtin => "builtin",
            PosSource::Csv => "csv",
            PosSource::Dsc => "dsc",
            PosSource::AprsFi => "aprs.fi",
        }
    }
}

/// A known station position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StationPos {
    pub lat: f64,
    pub lon: f64,
    pub source: PosSource,
    /// UTC epoch the position was observed; `None` for a fixed position.
    pub time: Option<i64>,
}

/// What was known about an identity at reception (ENR-05).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StationInfo {
    pub name: Option<String>,
    pub country: Option<String>,
    pub position: Option<StationPos>,
    /// Distance from the user's location, km.
    pub km: Option<f64>,
}

/// One received message.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub symbols: Vec<Symbol>,
    pub message: Message,
    /// Reception instant, UTC epoch seconds (TIM-01).
    pub utc: i64,
    /// Local offset at reception, seconds (local - UTC).
    pub offset: i32,
    pub time_source: TimeSource,
    /// Channel label.
    pub source_label: Option<String>,
    /// WAV file the message was decoded from.
    pub file: Option<String>,
    /// Schema version: 2 carries the reception-time enrichment.
    pub version: u8,
    /// The user's location at reception.
    pub rx: Option<(f64, f64)>,
    /// Distance from `rx` to the message's position, km.
    pub pos_km: Option<f64>,
    pub from_st: Option<StationInfo>,
    pub to_st: Option<StationInfo>,
    pub dmmsi_st: Option<StationInfo>,
    /// Signal-to-noise estimate, dB, when known (seed F-34).
    pub snr: Option<f64>,
}

impl Record {
    /// A record of decoded symbols, without enrichment.
    pub fn new(symbols: Vec<Symbol>, utc: i64, offset: i32, time_source: TimeSource) -> Record {
        Record {
            message: parser::parse(&symbols),
            symbols,
            utc,
            offset,
            time_source,
            source_label: None,
            file: None,
            version: 2,
            rx: None,
            pos_km: None,
            from_st: None,
            to_st: None,
            dmmsi_st: None,
            snr: None,
        }
    }
}
