//! The RSID (Reed-Solomon mode identifier) detector (fldigi `rsid/rsid.cxx`,
//! receive half): 15 tones of 10.77 Hz spacing and 93 ms each, found in a
//! 2048-point FFT at 11025 Hz taken every 512 samples. A code of the first
//! table names a mode; the ESCAPE code says one of the second table follows.
//!
//! Ported from fldigi 4.1.23 (GPL v3 or later; Dave Freese W1HKJ, Stelios
//! Bounanos M0GLD, John Douyere VK2ETA; RSID by Patrick Lindecker F6CTE).
//! The input is complex: positive frequencies are the audio (IQ of the upper
//! sideband, or a real signal).

use num_complex::Complex64;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

use super::filters::hamming;

/// The rate the detector runs at.
pub const SAMPLE_RATE: f64 = 11025.0;
const FFT_SAMPLES: usize = 512;
const FFT_SIZE: usize = 1024;
const ARRAY_SIZE: usize = 2048;
pub const NSYMBOLS: usize = 15;
const NTIMES: usize = NSYMBOLS * 2;
/// One RSID tone's length (s).
pub const SYMLEN_S: f64 = 1024.0 / SAMPLE_RATE;
/// The ESCAPE code of the first table.
pub const ESCAPE: u16 = 6;
/// The end-of-transmission code.
pub const EOT: u16 = 263;
/// Errors (of 15 symbols) a match may have: fldigi's default "medium".
const HAMMING_RESOLUTION: usize = 1;

/// fldigi's first table (`rsid_ids_1`), in its order.
pub const TABLE1: [(u16, &str); 145] = [
    (263, "EOT"),
    (6, "ESCAPE"),
    (1, "BPSK31"),
    (110, "QPSK31"),
    (2, "BPSK63"),
    (3, "QPSK63"),
    (4, "BPSK125"),
    (5, "QPSK125"),
    (126, "BPSK250"),
    (127, "QPSK250"),
    (173, "BPSK500"),
    (183, "PSK125R"),
    (186, "PSK250R"),
    (187, "PSK500R"),
    (7, "PSKFEC31"),
    (8, "PSK10"),
    (9, "MT63_500_LG"),
    (10, "MT63_500_ST"),
    (11, "MT63_500_VST"),
    (12, "MT63_1000_LG"),
    (13, "MT63_1000_ST"),
    (14, "MT63_1000_VST"),
    (15, "MT63_2000_LG"),
    (17, "MT63_2000_ST"),
    (18, "MT63_2000_VST"),
    (19, "PSKAM10"),
    (20, "PSKAM31"),
    (21, "PSKAM50"),
    (22, "PSK63F"),
    (23, "PSK220F"),
    (24, "CHIP64"),
    (25, "CHIP128"),
    (26, "CW"),
    (27, "CCW_OOK_12"),
    (28, "CCW_OOK_24"),
    (29, "CCW_OOK_48"),
    (30, "CCW_FSK_12"),
    (31, "CCW_FSK_24"),
    (33, "CCW_FSK_48"),
    (34, "PACTOR1_FEC"),
    (113, "PACKET_110"),
    (35, "PACKET_300"),
    (36, "PACKET_1200"),
    (37, "RTTY_ASCII_7"),
    (38, "RTTY_ASCII_8"),
    (39, "RTTY_45"),
    (40, "RTTY_50"),
    (41, "RTTY_75"),
    (42, "AMTOR_FEC"),
    (43, "THROB_1"),
    (44, "THROB_2"),
    (45, "THROB_4"),
    (46, "THROBX_1"),
    (47, "THROBX_2"),
    (146, "THROBX_4"),
    (204, "CONTESTIA_4_125"),
    (55, "CONTESTIA_4_250"),
    (54, "CONTESTIA_4_500"),
    (255, "CONTESTIA_4_1000"),
    (254, "CONTESTIA_4_2000"),
    (169, "CONTESTIA_8_125"),
    (49, "CONTESTIA_8_250"),
    (52, "CONTESTIA_8_500"),
    (117, "CONTESTIA_8_1000"),
    (247, "CONTESTIA_8_2000"),
    (275, "CONTESTIA_16_250"),
    (50, "CONTESTIA_16_500"),
    (53, "CONTESTIA_16_1000"),
    (259, "CONTESTIA_16_2000"),
    (51, "CONTESTIA_32_1000"),
    (201, "CONTESTIA_32_2000"),
    (194, "CONTESTIA_64_500"),
    (193, "CONTESTIA_64_1000"),
    (191, "CONTESTIA_64_2000"),
    (56, "VOICE"),
    (60, "MFSK8"),
    (57, "MFSK16"),
    (147, "MFSK32"),
    (148, "MFSK11"),
    (152, "MFSK22"),
    (61, "RTTYM_8_250"),
    (62, "RTTYM_16_500"),
    (63, "RTTYM_32_1000"),
    (65, "RTTYM_8_500"),
    (66, "RTTYM_16_1000"),
    (67, "RTTYM_4_500"),
    (68, "RTTYM_4_250"),
    (119, "RTTYM_8_1000"),
    (170, "RTTYM_8_125"),
    (203, "OLIVIA_4_125"),
    (75, "OLIVIA_4_250"),
    (74, "OLIVIA_4_500"),
    (229, "OLIVIA_4_1000"),
    (238, "OLIVIA_4_2000"),
    (163, "OLIVIA_8_125"),
    (69, "OLIVIA_8_250"),
    (72, "OLIVIA_8_500"),
    (116, "OLIVIA_8_1000"),
    (214, "OLIVIA_8_2000"),
    (70, "OLIVIA_16_500"),
    (73, "OLIVIA_16_1000"),
    (234, "OLIVIA_16_2000"),
    (71, "OLIVIA_32_1000"),
    (221, "OLIVIA_32_2000"),
    (211, "OLIVIA_64_2000"),
    (76, "PAX"),
    (77, "PAX2"),
    (78, "DOMINOF"),
    (79, "FAX"),
    (81, "SSTV"),
    (84, "DOMINOEX_4"),
    (85, "DOMINOEX_5"),
    (86, "DOMINOEX_8"),
    (87, "DOMINOEX_11"),
    (88, "DOMINOEX_16"),
    (90, "DOMINOEX_22"),
    (92, "DOMINOEX_4_FEC"),
    (93, "DOMINOEX_5_FEC"),
    (97, "DOMINOEX_8_FEC"),
    (98, "DOMINOEX_11_FEC"),
    (99, "DOMINOEX_16_FEC"),
    (101, "DOMINOEX_22_FEC"),
    (104, "FELD_HELL"),
    (105, "PSK_HELL"),
    (106, "HELL_80"),
    (107, "FM_HELL_105"),
    (108, "FM_HELL_245"),
    (114, "MODE_141A"),
    (123, "DTMF"),
    (125, "ALE400"),
    (131, "FDMDV"),
    (132, "JT65_A"),
    (134, "JT65_B"),
    (135, "JT65_C"),
    (136, "THOR_4"),
    (137, "THOR_8"),
    (138, "THOR_16"),
    (139, "THOR_5"),
    (143, "THOR_11"),
    (145, "THOR_22"),
    (153, "CALL_ID"),
    (155, "PACKET_PSK1200"),
    (156, "PACKET_PSK250"),
    (159, "PACKET_PSK63"),
    (172, "MODE_188_110A_8N1"),
];

/// fldigi's second table (`rsid_ids_2`), after ESCAPE.
pub const TABLE2: [(u16, &str); 57] = [
    (450, "PSK63RX4"),
    (457, "PSK63RX5"),
    (458, "PSK63RX10"),
    (460, "PSK63RX20"),
    (462, "PSK63RX32"),
    (467, "PSK125RX4"),
    (497, "PSK125RX5"),
    (513, "PSK125RX10"),
    (519, "PSK125X12"),
    (522, "PSK125RX12"),
    (527, "PSK125RX16"),
    (529, "PSK250RX2"),
    (533, "PSK250RX3"),
    (539, "PSK250RX5"),
    (541, "PSK250X6"),
    (545, "PSK250RX6"),
    (551, "PSK250RX7"),
    (553, "PSK500RX2"),
    (558, "PSK500RX3"),
    (564, "PSK500RX4"),
    (566, "PSK500X2"),
    (569, "PSK500X4"),
    (570, "PSK1000"),
    (580, "PSK1000R"),
    (587, "PSK1000X2"),
    (595, "PSK1000RX2"),
    (604, "PSK800RX2"),
    (610, "PSK800X2"),
    (620, "MFSK64"),
    (625, "MFSK128"),
    (639, "THOR25x4"),
    (649, "THOR50x1"),
    (653, "THOR50x2"),
    (658, "THOR100"),
    (662, "DOMINOEX_44"),
    (681, "DOMINOEX_88"),
    (687, "MFSK31"),
    (691, "DOMINOEX_MICRO"),
    (693, "THOR_MICRO"),
    (1026, "MFSK64L"),
    (1029, "MFSK128L"),
    (1066, "PSK8P125"),
    (1071, "PSK8P250"),
    (1076, "PSK8P500"),
    (1047, "PSK8P1000"),
    (1037, "PSK8P125F"),
    (1038, "PSK8P250F"),
    (1043, "PSK8P500F"),
    (1078, "PSK8P1000F"),
    (1058, "PSK8P1200F"),
    (1239, "PSK8P125FL"),
    (2052, "PSK8P250FL"),
    (2053, "OFDM500F"),
    (2094, "OFDM7F0F"),
    (2118, "OFDM2000"),
    (2110, "OFDM2000F"),
    (1171, "IFKP"),
];

const SQUARES: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13,
    14, 15, 0, 2, 4, 6, 8, 10, 12, 14, 9, 11, 13, 15, 1, 3, 5, 7, 0, 3, 6, 5, 12, 15, 10, 9, 1, 2,
    7, 4, 13, 14, 11, 8, 0, 4, 8, 12, 9, 13, 1, 5, 11, 15, 3, 7, 2, 6, 10, 14, 0, 5, 10, 15, 13, 8,
    7, 2, 3, 6, 9, 12, 14, 11, 4, 1, 0, 6, 12, 10, 1, 7, 13, 11, 2, 4, 14, 8, 3, 5, 15, 9, 0, 7,
    14, 9, 5, 2, 11, 12, 10, 13, 4, 3, 15, 8, 1, 6, 0, 8, 9, 1, 11, 3, 2, 10, 15, 7, 6, 14, 4, 12,
    13, 5, 0, 9, 11, 2, 15, 6, 4, 13, 7, 14, 12, 5, 8, 1, 3, 10, 0, 10, 13, 7, 3, 9, 14, 4, 6, 12,
    11, 1, 5, 15, 8, 2, 0, 11, 15, 4, 7, 12, 8, 3, 14, 5, 1, 10, 9, 2, 6, 13, 0, 12, 1, 13, 2, 14,
    3, 15, 4, 8, 5, 9, 6, 10, 7, 11, 0, 13, 3, 14, 6, 11, 5, 8, 12, 1, 15, 2, 10, 7, 9, 4, 0, 14,
    5, 11, 10, 4, 15, 1, 13, 3, 8, 6, 7, 9, 2, 12, 0, 15, 7, 8, 14, 1, 9, 6, 5, 10, 2, 13, 11, 4,
    12, 3,
];

const INDICES: [u8; 12] = [2, 4, 8, 9, 11, 15, 7, 14, 5, 10, 13, 3];

/// The 15 tones (0..15) of `code` (fldigi `cRsId::Encode`).
pub fn encode(code: u16) -> [u8; NSYMBOLS] {
    let mut r = [0u8; NSYMBOLS];
    r[0] = (code >> 8) as u8;
    r[1] = ((code >> 4) & 0x0f) as u8;
    r[2] = (code & 0x0f) as u8;
    for &ix in &INDICES {
        for j in (1..NSYMBOLS).rev() {
            r[j] = r[j - 1] ^ SQUARES[((r[j] as usize) << 4) + ix as usize];
        }
        r[0] = SQUARES[((r[0] as usize) << 4) + ix as usize];
    }
    r
}

/// The name of `code` in `table` (1 or 2).
pub fn name(table: u8, code: u16) -> Option<&'static str> {
    let t: &[(u16, &str)] = if table == 1 { &TABLE1 } else { &TABLE2 };
    t.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// A code found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// 1 or 2.
    pub table: u8,
    pub code: u16,
    /// The signal's centre (Hz of the input).
    pub freq_hz: f64,
}

pub struct Detector {
    codes1: Vec<[u8; NSYMBOLS]>,
    codes2: Vec<[u8; NSYMBOLS]>,
    window: Vec<f64>,
    fft: Arc<dyn Fft<f64>>,
    input: Vec<Complex64>,
    work: Vec<Complex64>,
    buckets: Vec<Vec<u8>>,
    bin_low: usize,
    bin_high: usize,
    secondary_timeout: f64,
    /// The last power spectrum, 0..5512 Hz in 1024 bins.
    spectrum: Vec<f32>,
    spectra: u64,
    found: Vec<Found>,
    /// Both sidebands of carrier-locked AM: the power of each audio
    /// frequency is that of its upper and its mirrored lower sideband added.
    fold: bool,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        let mut d = Self {
            codes1: TABLE1.iter().map(|(c, _)| encode(*c)).collect(),
            codes2: TABLE2.iter().map(|(c, _)| encode(*c)).collect(),
            window: (0..ARRAY_SIZE)
                .map(|i| hamming(i as f64 / ARRAY_SIZE as f64))
                .collect(),
            fft: FftPlanner::new().plan_fft_forward(ARRAY_SIZE),
            input: Vec::with_capacity(ARRAY_SIZE),
            work: vec![Complex64::new(0.0, 0.0); ARRAY_SIZE],
            buckets: vec![vec![0; FFT_SIZE]; NTIMES],
            bin_low: 3,
            bin_high: FFT_SIZE - 32,
            secondary_timeout: 0.0,
            spectrum: vec![0.0; FFT_SIZE],
            spectra: 0,
            found: Vec::new(),
            fold: false,
        };
        d.reset();
        d
    }

    /// Search for signals centred between `lo` and `hi` Hz only.
    pub fn set_range(&mut self, lo: f64, hi: f64) {
        let bpf = ARRAY_SIZE as f64 / SAMPLE_RATE;
        self.bin_low = ((lo * bpf) as i64 - NSYMBOLS as i64).max(3) as usize;
        self.bin_high = ((hi * bpf) as i64 + NSYMBOLS as i64).min((FFT_SIZE - 32) as i64) as usize;
    }

    /// Add the mirrored negative frequencies to the positive ones (the input
    /// is AM turned to its carrier: diversity of the two sidebands, which a
    /// fade in one of them does not cancel).
    pub fn set_fold(&mut self, fold: bool) {
        self.fold = fold;
    }

    pub fn reset(&mut self) {
        self.secondary_timeout = 0.0;
        self.input.clear();
        self.input.resize(FFT_SIZE, Complex64::new(0.0, 0.0));
        for row in &mut self.buckets {
            row.iter_mut().for_each(|b| *b = 0);
        }
    }

    /// The last power spectrum (1024 bins over 0..5512.5 Hz) and how many
    /// were taken so far.
    pub fn spectrum(&self) -> (&[f32], u64) {
        (&self.spectrum, self.spectra)
    }

    pub fn take_found(&mut self) -> Vec<Found> {
        std::mem::take(&mut self.found)
    }

    /// Samples at 11025 Hz in.
    pub fn process(&mut self, samples: &[Complex64]) {
        if self.secondary_timeout > 0.0 {
            self.secondary_timeout -= samples.len() as f64 / SAMPLE_RATE;
            if self.secondary_timeout <= 0.0 {
                self.reset();
            }
        }
        for &s in samples {
            self.input.push(s);
            if self.input.len() >= ARRAY_SIZE {
                self.search();
                if self.input.len() >= ARRAY_SIZE {
                    self.input.drain(..FFT_SAMPLES);
                }
            }
        }
    }

    fn calculate_buckets(&mut self, spectrum: &[f64], begin: usize, end: usize) {
        let row = NTIMES - 1;
        let mut amp_max = 0.0;
        let mut bucket_max = begin as i64 - 2;
        let mut i = begin;
        while i < end {
            if bucket_max == i as i64 - 2 {
                amp_max = spectrum[i];
                bucket_max = i as i64;
                let mut j = i + 2;
                while j < i + NTIMES + 2 {
                    if spectrum[j] > amp_max {
                        amp_max = spectrum[j];
                        bucket_max = j as i64;
                    }
                    j += 2;
                }
            } else {
                let j = i + NTIMES;
                if spectrum[j] > amp_max {
                    amp_max = spectrum[j];
                    bucket_max = j as i64;
                }
            }
            self.buckets[row][i] = ((bucket_max - i as i64) >> 1) as u8;
            i += 2;
        }
    }

    fn distance(&self, bucket: usize, code: &[u8; NSYMBOLS]) -> usize {
        let mut dist = 0;
        for (i, &c) in code.iter().enumerate() {
            if self.buckets[2 * i + 1][bucket] != c {
                dist += 1;
                if dist > HAMMING_RESOLUTION {
                    break;
                }
            }
        }
        dist
    }

    fn search_amp(&self, second: bool) -> Option<(usize, u16)> {
        let (codes, table): (&[[u8; NSYMBOLS]], &[(u16, &str)]) = if second {
            (&self.codes2, &TABLE2)
        } else {
            (&self.codes1, &TABLE1)
        };
        let mut best = 1000;
        let mut found = None;
        for (i, code) in codes.iter().enumerate() {
            for j in self.bin_low..self.bin_high.saturating_sub(NTIMES) {
                let d = self.distance(j, code);
                if d < best {
                    best = d;
                    found = Some((j, table[i].0));
                    if d == 0 {
                        break;
                    }
                }
            }
        }
        found.filter(|_| best <= HAMMING_RESOLUTION)
    }

    fn search(&mut self) {
        for (w, (s, win)) in self
            .work
            .iter_mut()
            .zip(self.input.iter().zip(&self.window))
        {
            *w = s * win;
        }
        self.fft.process(&mut self.work);
        let pscale = 4.0 / (FFT_SIZE * FFT_SIZE) as f64;
        let ampl: Vec<f64> = (0..FFT_SIZE)
            .map(|i| {
                let mirror = if self.fold && i > 0 {
                    self.work[ARRAY_SIZE - i].norm_sqr()
                } else {
                    0.0
                };
                (self.work[i].norm_sqr() + mirror) * pscale
            })
            .collect();
        for (s, a) in self.spectrum.iter_mut().zip(&ampl) {
            *s = *a as f32;
        }
        self.spectra += 1;

        self.buckets.rotate_left(1);
        self.buckets[NTIMES - 1].iter_mut().for_each(|b| *b = 0);
        let (lo, hi) = (3, FFT_SIZE - 32);
        self.calculate_buckets(&ampl, lo, hi - NTIMES);
        self.calculate_buckets(&ampl, lo + 1, hi - NTIMES);

        let freq = |bin: usize| (bin as f64 + NSYMBOLS as f64 - 0.5) * SAMPLE_RATE / 2048.0;
        if self.secondary_timeout <= 0.0 {
            if let Some((bin, code)) = self.search_amp(false) {
                if code == ESCAPE {
                    // 10 symbols of gap, 15 symbols, 2 for timing errors
                    self.secondary_timeout = 27.0 * SYMLEN_S;
                } else {
                    self.found.push(Found {
                        table: 1,
                        code,
                        freq_hz: freq(bin),
                    });
                    self.reset();
                }
            }
            return;
        }
        if let Some((bin, code)) = self.search_amp(true) {
            self.found.push(Found {
                table: 2,
                code,
                freq_hz: freq(bin),
            });
            self.reset();
        }
    }
}
