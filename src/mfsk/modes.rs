//! The MFSK modes (fldigi `mfsk::mfsk`, `globals.cxx` names, `rsid_defs.cxx`
//! codes).

/// An MFSK mode's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode {
    /// fldigi's name (`MFSK-32`): the setting value and the marks.
    pub name: &'static str,
    pub sample_rate: u32,
    /// Samples per symbol.
    pub symlen: usize,
    /// Bits per symbol.
    pub symbits: usize,
    /// Interleaver depth.
    pub depth: usize,
    /// The DFT bin of the lowest tone.
    pub basetone: usize,
    pub numtones: usize,
    /// Zero bits sent after a picture (and the lead-in).
    pub preamble: usize,
    /// Pictures can be sent.
    pub pictures: bool,
    /// The RSID code: (table 1 or 2, code). None: no RSID.
    pub rsid: Option<(u8, u16)>,
}

impl Mode {
    /// Hz between tones.
    pub fn tone_spacing(&self) -> f64 {
        f64::from(self.sample_rate) / self.symlen as f64
    }

    /// The signal's width (first to last tone), Hz.
    pub fn bandwidth(&self) -> f64 {
        (self.numtones as f64 - 1.0) * self.tone_spacing()
    }

    /// Symbols per second.
    pub fn baud(&self) -> f64 {
        self.tone_spacing()
    }
}

#[allow(clippy::too_many_arguments)]
const fn m(
    name: &'static str,
    sample_rate: u32,
    symlen: usize,
    symbits: usize,
    depth: usize,
    basetone: usize,
    numtones: usize,
    preamble: usize,
    pictures: bool,
    rsid: Option<(u8, u16)>,
) -> Mode {
    Mode {
        name,
        sample_rate,
        symlen,
        symbits,
        depth,
        basetone,
        numtones,
        preamble,
        pictures,
        rsid,
    }
}

/// Every mode, in the order of the `Mode` choice.
pub const MODES: [Mode; 11] = [
    m("MFSK-4", 8000, 2048, 5, 5, 256, 32, 107, false, None),
    m(
        "MFSK-8",
        8000,
        1024,
        5,
        5,
        128,
        32,
        107,
        false,
        Some((1, 60)),
    ),
    m(
        "MFSK-11",
        11025,
        1024,
        4,
        10,
        93,
        16,
        107,
        false,
        Some((1, 148)),
    ),
    m(
        "MFSK-16",
        8000,
        512,
        4,
        10,
        64,
        16,
        107,
        true,
        Some((1, 57)),
    ),
    m(
        "MFSK-22",
        11025,
        512,
        4,
        10,
        46,
        16,
        107,
        false,
        Some((1, 152)),
    ),
    m(
        "MFSK-31",
        8000,
        256,
        3,
        10,
        32,
        8,
        107,
        false,
        Some((2, 687)),
    ),
    m(
        "MFSK-32",
        8000,
        256,
        4,
        10,
        32,
        16,
        107,
        true,
        Some((1, 147)),
    ),
    m(
        "MFSK-64",
        8000,
        128,
        4,
        10,
        16,
        16,
        180,
        true,
        Some((2, 620)),
    ),
    m(
        "MFSK-128",
        8000,
        64,
        4,
        20,
        8,
        16,
        214,
        true,
        Some((2, 625)),
    ),
    m(
        "MFSK-64L",
        8000,
        128,
        4,
        400,
        16,
        16,
        2500,
        false,
        Some((2, 1026)),
    ),
    m(
        "MFSK-128L",
        8000,
        64,
        4,
        800,
        8,
        16,
        5000,
        false,
        Some((2, 1029)),
    ),
];

/// The default mode.
pub const DEFAULT: &str = "MFSK-32";

/// The mode called `name`.
pub fn by_name(name: &str) -> Option<Mode> {
    MODES.iter().copied().find(|m| m.name == name)
}

/// The mode an RSID code stands for, if this build decodes it.
pub fn by_rsid(table: u8, code: u16) -> Option<Mode> {
    MODES
        .iter()
        .copied()
        .find(|m| m.rsid == Some((table, code)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_rates_and_codes() {
        let m = by_name("MFSK-32").unwrap();
        assert_eq!(m.tone_spacing(), 31.25);
        assert_eq!(m.bandwidth(), 468.75);
        assert_eq!(by_rsid(2, 620).unwrap().name, "MFSK-64");
        assert_eq!(by_rsid(1, 620), None);
        assert!(by_name(DEFAULT).is_some());
    }
}
