//! The parsed message (seed spec/04-algorithms/03-parser.md, "Output fields").
//! `None` is ABSENT (PAR-02).

use crate::dsc::domain::identity::Identity;

/// A symbol as received: 0..127, or [`GARBLED`].
pub type Symbol = u8;
/// A symbol that could not be received.
pub const GARBLED: Symbol = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    GeographicArea,
    Distress,
    CommonInterest,
    AllShips,
    Individual,
    IndividualAuto,
    Vts,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Routine,
    Vts,
    Acs,
    Safety,
    Urgency,
    Distress,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Severity {
    #[default]
    Routine,
    Safety,
    Urgency,
    Distress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eos {
    /// 117: acknowledgement requested.
    Req,
    /// 122: acknowledgement.
    Ack,
    /// 127: any other call.
    Eos,
}

/// A position, signed by quadrant (south and west negative).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
    /// 'N' or 'S'.
    pub lat_quad: char,
    /// 'E' or 'W'.
    pub lon_quad: char,
}

/// A geographic area: north-west corner (signed) and extent in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub corner_lat: i32,
    pub corner_lon: i32,
    pub lat_quad: char,
    pub lon_quad: char,
    pub d_lat: u8,
    pub d_lon: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    Freq,
    Hfmf,
    Vhf,
    M586,
    Unknown,
}

/// One frequency/channel element of message 2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Element {
    pub kind: ElementKind,
    pub khz: Option<f64>,
    pub seven_digit: bool,
    pub channel: Option<u32>,
    pub vhf_mode: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreqChannel {
    pub rx: Option<Element>,
    pub tx: Option<Element>,
}

/// The error-check character: received and computed, both absent when the
/// ECC itself is garbled or missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ecc {
    pub received: Option<u8>,
    pub computed: Option<u8>,
    pub ok: bool,
}

/// An ITU-R M.821 expansion sequence.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Expansion {
    pub lat_extra_min: Option<f64>,
    pub lon_extra_min: Option<f64>,
    pub precise_position: Option<(f64, f64)>,
    pub position_source: Option<u8>,
    pub hdop: Option<f64>,
    pub datum: Option<u8>,
    pub speed_kn: Option<f64>,
    pub course_deg: Option<f64>,
    pub station_id: Option<String>,
    pub area_lat_extra_min: Option<f64>,
    pub area_lon_extra_min: Option<f64>,
    pub area_dlat_extra_min: Option<f64>,
    pub area_dlon_extra_min: Option<f64>,
    pub area_speed_kn: Option<f64>,
    pub area_course_deg: Option<f64>,
    pub persons_on_board: Option<u32>,
    pub requested: Vec<u8>,
    pub unavailable: Vec<u8>,
    pub eos: Option<Eos>,
    pub ecc: Ecc,
    /// Index one past the expansion's ECC, when its EOS was found.
    pub end: Option<usize>,
}

/// A parsed call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Message {
    pub format: Format,
    pub format_unconfirmed: bool,
    pub category: Option<Category>,
    pub severity: Severity,
    pub to: Option<Identity>,
    pub from: Option<Identity>,
    pub distress_mmsi: Option<Identity>,
    pub area: Option<Area>,
    pub distress_telecommand: Option<u8>,
    pub distress_nature: Option<u8>,
    pub subsequent_comm: Option<u8>,
    pub tc1: Option<u8>,
    pub tc2: Option<u8>,
    pub position: Option<Position>,
    pub position_refused: bool,
    /// UTC hours and minutes.
    pub event_time: Option<(u8, u8)>,
    pub freq_channel: Option<FreqChannel>,
    pub number: Option<String>,
    pub duration_s: Option<u32>,
    pub expansion: Option<Expansion>,
    pub eos: Option<Eos>,
    pub ecc: Ecc,
    pub self_cancel: bool,
    pub garbled_count: usize,
    /// 16 lowercase hex digits (PAR-05).
    pub content_id: String,
    /// Symbols the transmission occupies (for the receiver's output cut).
    pub length: usize,
}
