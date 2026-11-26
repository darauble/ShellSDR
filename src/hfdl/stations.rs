//! The ground-station table (HFDL seed `ground-stations.md`): built in
//! from `data/ground-stations.csv` (system table version 52).

use std::sync::OnceLock;

pub struct Station {
    pub id: u8,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub freqs_khz: Vec<f64>,
}

fn table() -> &'static Vec<Station> {
    static T: OnceLock<Vec<Station>> = OnceLock::new();
    T.get_or_init(|| {
        crate::hfdl::apps::cpdlc::csv_rows(include_str!("data/ground-stations.csv"))
            .into_iter()
            .skip(1)
            .filter(|r| r.len() >= 5)
            .map(|r| Station {
                id: r[0].parse().unwrap_or(0),
                name: r[1].clone(),
                lat: r[2].parse().unwrap_or(0.0),
                lon: r[3].parse().unwrap_or(0.0),
                freqs_khz: r[4]
                    .split_whitespace()
                    .filter_map(|f| f.parse().ok())
                    .collect(),
            })
            .collect()
    })
}

pub fn station(id: u8) -> Option<&'static Station> {
    table().iter().find(|s| s.id == id)
}

/// The HFDL bands (`ground-stations.md` §2): name, a center that covers
/// every channel with no channel's signal on it (Hz), and the span from the
/// lowest channel to 3 kHz above the highest (kHz).
pub const BANDS: [(&str, u64, u32); 13] = [
    ("2.9 MHz", 2_980_000, 78),
    ("3.5 MHz", 3_477_500, 45),
    ("3.9 MHz", 3_904_500, 3),
    ("4.7 MHz", 4_672_000, 36),
    ("5.6 MHz", 5_587_500, 272),
    ("6.6 MHz", 6_623_500, 186),
    ("8.9 MHz", 8_902_500, 155),
    ("10 MHz", 10_058_500, 69),
    ("11.3 MHz", 11_287_000, 206),
    ("13.3 MHz", 13_309_000, 90),
    ("15 MHz", 15_029_500, 3),
    ("17.9 MHz", 17_944_500, 87),
    ("22 MHz", 21_964_000, 72),
];

/// The number of stations in the table.
pub fn count() -> usize {
    table().len()
}

/// The stations, by ID.
pub fn all() -> &'static [Station] {
    table()
}

/// A station's frequency by index, kHz.
pub fn frequency_khz(gs: u8, index: usize) -> Option<f64> {
    station(gs)?.freqs_khz.get(index).copied()
}

/// Every frequency of every station, kHz.
pub fn all_frequencies_khz() -> Vec<u32> {
    table()
        .iter()
        .flat_map(|s| s.freqs_khz.iter().map(|f| f.round() as u32))
        .collect()
}
