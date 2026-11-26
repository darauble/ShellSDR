//! Symbols to a message (seed spec/04-algorithms/03-parser.md). Never fails:
//! whatever cannot be read is ABSENT.

use crate::dsc::domain::identity::{self, Identity};
use crate::dsc::message::*;

const NO_INFO: Symbol = 126;

/// Parse one symbol sequence.
pub fn parse(s: &[Symbol]) -> Message {
    let mut p = Parser {
        s,
        body_end: s.len(),
        m: Message::default(),
    };
    p.run();
    p.m
}

/// The content id: FNV-1a 64 over the symbols, with the non-standard start
/// value of the seed (F-29), as 16 lowercase hex digits.
pub fn content_id(s: &[Symbol]) -> String {
    // Standard FNV-1a would start at 14695981039346656037; the seed keeps
    // the reference's value so ids in existing day files stay valid.
    const START: u64 = 1_469_598_103_934_665_603;
    const PRIME: u64 = 1_099_511_628_211;
    let h = s
        .iter()
        .fold(START, |h, &b| (h ^ u64::from(b)).wrapping_mul(PRIME));
    format!("{h:016x}")
}

struct Parser<'a> {
    s: &'a [Symbol],
    /// Index of the EOS, or the length when there is none.
    body_end: usize,
    m: Message,
}

impl Parser<'_> {
    fn run(&mut self) {
        let fmt = self.format_specifier();
        let eos_at = self.eos_and_ecc(fmt);
        self.fields();
        if let Some(at) = eos_at {
            self.m.expansion = self.expansion(at + 2);
            self.m.length = match self.m.expansion.as_ref().and_then(|x| x.end) {
                Some(end) => end,
                None => (at + 2).min(self.s.len()),
            };
        }
        self.derived();
    }

    // --- helpers (03-parser.md "Helpers") -------------------------------

    /// Symbol `i` exists before the EOS and is not garbled.
    fn ok(&self, i: usize) -> bool {
        i < self.body_end && self.s[i] != GARBLED
    }

    /// Symbols `i .. i+n` are ok digit pairs (0..99).
    fn digits(&self, i: usize, n: usize) -> bool {
        (i..i + n).all(|j| self.ok(j) && self.s[j] <= 99)
    }

    /// The symbol, unless it is not ok or "no information" (126).
    fn opt(&self, i: usize) -> Option<u8> {
        (self.ok(i) && self.s[i] != NO_INFO).then(|| self.s[i])
    }

    fn identity(&self, i: usize) -> Option<Identity> {
        if !self.digits(i, 5) {
            return None;
        }
        let ten: String = self.s[i..i + 5].iter().map(|d| format!("{d:02}")).collect();
        Some(identity::classify(&ten[..9]))
    }

    // --- step 1: format specifier ---------------------------------------

    /// Returns the format specifier symbol used for the ECC, if any.
    fn format_specifier(&mut self) -> Option<Symbol> {
        let s = self.s;
        let a = s.first().copied().filter(|&x| x != GARBLED);
        let b = s.get(1).copied().filter(|&x| x != GARBLED);
        let confirmed = a.is_some() && a == b;
        let fmt = match (a, b) {
            (Some(a), Some(b)) if a == b => Some(a),
            (Some(a), Some(b)) => Some(
                if format_of(a) != Format::Unknown || format_of(b) == Format::Unknown {
                    a
                } else {
                    b
                },
            ),
            (a, b) => a.or(b),
        };
        self.m.format = fmt.map_or(Format::Unknown, format_of);
        self.m.format_unconfirmed =
            !confirmed && matches!(self.m.format, Format::Distress | Format::AllShips);
        fmt
    }

    // --- step 2: EOS and ECC ---------------------------------------------

    /// Returns the index of the EOS.
    fn eos_and_ecc(&mut self, fmt: Option<Symbol>) -> Option<usize> {
        let start = match self.m.format {
            Format::Distress => 7,
            Format::AllShips => 10,
            Format::Unknown => 2,
            _ => 15,
        };
        let s = self.s;
        let eos_at = (start..s.len()).find(|&i| matches!(s[i], 117 | 122 | 127));
        if let Some(at) = eos_at {
            self.m.eos = Some(match s[at] {
                117 => Eos::Req,
                122 => Eos::Ack,
                _ => Eos::Eos,
            });
            if let Some(&received) = s.get(at + 1).filter(|&&x| x != GARBLED) {
                let computed = s[2..=at]
                    .iter()
                    .filter(|&&x| x != GARBLED)
                    .fold(fmt.unwrap_or(0), |acc, &x| acc ^ x)
                    & 127;
                self.m.ecc = Ecc {
                    received: Some(received),
                    computed: Some(computed),
                    ok: received == computed,
                };
            }
            self.body_end = at;
        }
        eos_at
    }

    // --- step 7: the expansion sequence (ITU-R M.821-1) ------------------

    /// A digit pair anywhere in the input (the expansion lies past the EOS).
    fn pair(&self, i: usize) -> Option<u32> {
        self.s.get(i).filter(|&&x| x <= 99).map(|&x| u32::from(x))
    }

    /// Four digits from two symbols at `i`.
    fn four(&self, i: usize) -> Option<u32> {
        Some(self.pair(i)? * 100 + self.pair(i + 1)?)
    }

    fn expansion(&self, after: usize) -> Option<Expansion> {
        let s = self.s;
        let is_spec = |i: usize| s.get(i).is_some_and(|&x| (100..=106).contains(&x));
        let mut i = after;
        // The call's closing EOS copies and the RX fillers (M.821 Fig. 1).
        let mut skipped = 0;
        while skipped < 4
            && s.get(i)
                .is_some_and(|&x| matches!(x, 126 | 117 | 122 | 127))
        {
            i += 1;
            skipped += 1;
        }
        if !is_spec(i) {
            return None;
        }
        let start = i;
        let mut x = Expansion::default();
        while is_spec(i) {
            let spec = s[i];
            i += 1;
            if spec != 105 && s.get(i).is_some_and(|&c| c == 110 || c == 126) {
                if s[i] == 110 {
                    x.requested.push(spec)
                } else {
                    x.unavailable.push(spec)
                }
                i += 1;
                continue;
            }
            let len = match spec {
                100 => {
                    x.lat_extra_min = self.four(i).map(|v| f64::from(v) / 10000.0);
                    x.lon_extra_min = self.four(i + 2).map(|v| f64::from(v) / 10000.0);
                    4
                }
                101 => {
                    x.position_source = self.pair(i).filter(|&v| v <= 8).map(|v| v as u8);
                    x.hdop = self.pair(i + 1).map(|v| f64::from(v) / 10.0);
                    x.datum = self.pair(i + 2).filter(|&v| v <= 2).map(|v| v as u8);
                    3
                }
                102 => {
                    x.speed_kn = self.four(i).map(|v| f64::from(v) / 10.0);
                    2
                }
                103 => {
                    x.course_deg = self.four(i).map(|v| f64::from(v) / 10.0); // F-23
                    2
                }
                104 => {
                    let n = (i..s.len()).take(10).take_while(|&j| s[j] <= 99).count();
                    x.station_id = s[i..i + n].iter().map(|&c| station_char(c)).collect();
                    n
                }
                105 => {
                    let hundredths = |j: usize| self.four(j).map(|v| f64::from(v) / 100.0);
                    let tenths = |j: usize| self.four(j).map(|v| f64::from(v) / 10.0);
                    x.area_lat_extra_min = hundredths(i);
                    x.area_lon_extra_min = hundredths(i + 2);
                    x.area_dlat_extra_min = hundredths(i + 4);
                    x.area_dlon_extra_min = hundredths(i + 6);
                    x.area_speed_kn = tenths(i + 8);
                    x.area_course_deg = tenths(i + 10);
                    12
                }
                _ => {
                    x.persons_on_board = self.four(i);
                    2
                }
            };
            i += len;
        }
        if s.get(i).is_some_and(|&e| matches!(e, 117 | 122 | 127)) {
            x.eos = Some(match s[i] {
                117 => Eos::Req,
                122 => Eos::Ack,
                _ => Eos::Eos,
            });
            x.end = Some((i + 2).min(s.len()));
            if let Some(&received) = s.get(i + 1).filter(|&&c| c != GARBLED) {
                let computed = s[start..=i]
                    .iter()
                    .filter(|&&c| c != GARBLED)
                    .fold(0, |a, &c| a ^ c)
                    & 127;
                x.ecc = Ecc {
                    received: Some(received),
                    computed: Some(computed),
                    ok: received == computed,
                };
            }
        }
        if let (Some(p), Some(dlat), Some(dlon)) =
            (self.m.position, x.lat_extra_min, x.lon_extra_min)
        {
            // Add the extra minutes away from the equator / meridian.
            let away = |v: f64, extra: f64| v.signum() * (v.abs() + extra / 60.0);
            x.precise_position = Some((away(p.lat, dlat), away(p.lon, dlon)));
        }
        Some(x)
    }

    // --- step 3: fields by format ------------------------------------------

    fn fields(&mut self) {
        match self.m.format {
            Format::Distress => {
                // Table A1-4.1
                self.m.category = Some(Category::Distress);
                self.m.from = self.identity(2);
                self.m.distress_nature = self.nature(7);
                self.m.position = self.position(8);
                self.m.event_time = self.utc(13);
                self.m.subsequent_comm = self.opt(15);
            }
            Format::AllShips => {
                self.m.category = self.ok(2).then(|| category_of(self.s[2]));
                self.m.from = self.identity(3);
                self.call_body(8);
            }
            _ => {
                self.m.category = self.ok(7).then(|| category_of(self.s[7]));
                if self.m.format == Format::GeographicArea {
                    self.m.area = self.area(2);
                } else {
                    self.m.to = self.identity(2);
                }
                self.m.from = self.identity(8);
                self.call_body(13);
            }
        }
    }

    fn call_body(&mut self, p: usize) {
        if self.m.category == Some(Category::Distress) {
            self.distress_call(p);
        } else {
            self.other_call(p);
        }
    }

    // --- step 4: distress calls (Tables A1-4.2 to A1-4.4) ----------------

    fn distress_call(&mut self, p: usize) {
        self.m.distress_telecommand = self.opt(p);
        // Five 126s ("identity unknown") are not digits: ABSENT.
        self.m.distress_mmsi = self.identity(p + 1);
        self.m.distress_nature = self.nature(p + 6);
        self.m.position = self.position(p + 7);
        self.m.event_time = self.utc(p + 12);
        self.m.subsequent_comm = self.opt(p + 14);
    }

    // --- readers ---------------------------------------------------------

    /// Pos1 (§8.1.2): quadrant, latitude deg/min, longitude deg/min.
    fn position(&self, i: usize) -> Option<Position> {
        if !self.digits(i, 5) {
            return None;
        }
        let d = &self.s[i..i + 5];
        if d.iter().all(|&x| x == 99) {
            return None; // not available, or older than 23.5 h
        }
        let q = d[0] / 10;
        let lat_deg = f64::from(d[0] % 10 * 10 + d[1] / 10);
        let lat_min = d[1] % 10 * 10 + d[2] / 10;
        let lon_deg = f64::from(u16::from(d[2] % 10) * 100 + u16::from(d[3]));
        let lon_min = d[4];
        let lat = lat_deg + f64::from(lat_min) / 60.0;
        let lon = lon_deg + f64::from(lon_min) / 60.0;
        if q > 3 || lat_min >= 60 || lon_min >= 60 || lat > 90.0 || lon > 180.0 {
            return None;
        }
        let (south, west) = quadrant(q);
        Some(Position {
            lat: if south { -lat } else { lat },
            lon: if west { -lon } else { lon },
            lat_quad: if south { 'S' } else { 'N' },
            lon_quad: if west { 'W' } else { 'E' },
        })
    }

    /// Geographic area (§5.3): north-west corner and extent.
    fn area(&self, i: usize) -> Option<Area> {
        if !self.digits(i, 5) {
            return None;
        }
        let d = &self.s[i..i + 5];
        let q = d[0] / 10;
        let lat = i32::from(d[0] % 10 * 10 + d[1] / 10);
        let lon = i32::from(d[1] % 10) * 100 + i32::from(d[2]);
        if q > 3 || lat > 90 || lon > 180 {
            return None;
        }
        let (south, west) = quadrant(q);
        Some(Area {
            corner_lat: if south { -lat } else { lat },
            corner_lon: if west { -lon } else { lon },
            lat_quad: if south { 'S' } else { 'N' },
            lon_quad: if west { 'W' } else { 'E' },
            d_lat: d[3],
            d_lon: d[4],
        })
    }

    /// UTC HH MM (§8.1.3); 88 88 = not available.
    fn utc(&self, i: usize) -> Option<(u8, u8)> {
        if !self.digits(i, 2) {
            return None;
        }
        let (h, m) = (self.s[i], self.s[i + 1]);
        (h <= 23 && m <= 59).then_some((h, m))
    }

    /// Nature of distress: 100..110, or 112 (legacy EPIRB, F-21).
    fn nature(&self, i: usize) -> Option<u8> {
        self.opt(i)
            .filter(|&x| (100..=110).contains(&x) || x == 112)
    }

    // --- step 5: other calls ---------------------------------------------------

    fn other_call(&mut self, p: usize) {
        self.m.tc1 = self.opt(p);
        self.m.tc2 = self.opt(p + 1);
        let m2 = p + 2;
        if self.ok(p) && self.s[p] == 121 {
            // Position telecommand (Table A1-7): Pos4, or a refusal/request.
            if (m2..m2 + 6).all(|j| self.ok(j) && self.s[j] == NO_INFO) {
                self.m.position_refused = self.m.eos == Some(Eos::Ack);
            } else {
                self.m.position = self.position(m2);
            }
            self.m.event_time = self.utc(m2 + 6);
        } else if self.m.format == Format::IndividualAuto
            && self.ok(p)
            && self.s[p] == 105
            && self.m.eos.is_some_and(|e| e != Eos::Req)
        {
            // End-of-call acknowledgement (Tables A1-4.10.1/2, M.689-3,
            // M.1082-1): HH MM SS 126 126 126.
            if self.digits(m2, 3) && self.s[m2 + 1] <= 59 && self.s[m2 + 2] <= 59 {
                let v = |j: usize| u32::from(self.s[m2 + j]);
                self.m.duration_s = Some(v(0) * 3600 + v(1) * 60 + v(2));
            }
        } else if self.ok(m2) && self.s[m2] == 55 {
            self.m.position = self.position(m2 + 1); // Pos2
        } else {
            self.m.freq_channel = self.freq_channel(m2);
        }
        if self.m.format == Format::IndividualAuto {
            self.m.number = self.number(m2);
        }
    }

    /// Public network number (§8.3.3): 105 (odd digit count) or 106 (even),
    /// then digit pairs up to the EOS.
    fn number(&self, i: usize) -> Option<String> {
        let start = (i..self.body_end).find(|&j| self.ok(j) && matches!(self.s[j], 105 | 106))?;
        let pairs = start + 1..self.body_end;
        if !pairs.clone().all(|j| self.ok(j) && self.s[j] <= 99) {
            return None;
        }
        let mut n: String = pairs.map(|j| format!("{:02}", self.s[j])).collect();
        if self.s[start] == 105 && !n.is_empty() {
            n.remove(0); // the leading 0 of an odd digit count
        }
        (!n.is_empty()).then_some(n)
    }

    // --- message 2: frequency or channel (field-coding.md, Table A1-5) ----

    /// RX element at `i`, TX element right after it; ABSENT if both are.
    fn freq_channel(&self, i: usize) -> Option<FreqChannel> {
        let rx_len = self.element_len(i);
        let rx = self.element(i);
        let tx = self.element(i + rx_len);
        (rx.is_some() || tx.is_some()).then_some(FreqChannel { rx, tx })
    }

    /// 4 symbols for the 7-digit form (HM 4) or exactly four 126s, else 3.
    fn element_len(&self, i: usize) -> usize {
        match self.s.get(i) {
            Some(&NO_INFO) => {
                let run = self.s[i..].iter().take_while(|&&x| x == NO_INFO).count();
                if run == 4 { 4 } else { 3 }
            }
            Some(&x) if x <= 99 && x / 10 == 4 => 4,
            _ => 3,
        }
    }

    fn element(&self, i: usize) -> Option<Element> {
        if !self.ok(i) || self.s[i] > 99 {
            return None; // garbled, 126 "no information", or not a digit pair
        }
        let s0 = self.s[i];
        let (hm, tm) = (s0 / 10, s0 % 10);
        let v = |j: usize| u32::from(self.s[i + j]);
        let none = Element {
            kind: ElementKind::Unknown,
            khz: None,
            seven_digit: false,
            channel: None,
            vhf_mode: None,
        };
        Some(match hm {
            0..=2 => {
                if !self.digits(i, 3) {
                    return None;
                }
                let hundreds_hz = v(0) * 10000 + v(1) * 100 + v(2);
                Element {
                    kind: ElementKind::Freq,
                    khz: Some(f64::from(hundreds_hz) / 10.0),
                    ..none
                }
            }
            3 => {
                if !self.digits(i, 3) {
                    return None;
                }
                Element {
                    kind: ElementKind::Hfmf,
                    channel: Some(u32::from(tm) * 10000 + v(1) * 100 + v(2)),
                    ..none
                }
            }
            4 if tm <= 2 => {
                if !self.digits(i, 4) {
                    return None;
                }
                let khz =
                    f64::from(u32::from(tm) * 10000 + v(1) * 100 + v(2)) + f64::from(v(3)) / 100.0;
                Element {
                    kind: ElementKind::Freq,
                    khz: Some(khz),
                    seven_digit: true,
                    ..none
                }
            }
            8 => Element {
                kind: ElementKind::M586,
                ..none
            },
            9 if tm == 0 => {
                if !self.digits(i, 3) {
                    return None;
                }
                Element {
                    kind: ElementKind::Vhf,
                    channel: Some(v(1) * 100 + v(2)),
                    vhf_mode: Some(self.s[i + 1] / 10),
                    ..none
                }
            }
            _ => none,
        })
    }

    // --- step 8: derived values ------------------------------------------------

    fn derived(&mut self) {
        let m = &mut self.m;
        // Step 6 (§8.6): an acknowledgement naming its own sender.
        m.self_cancel = m.distress_telecommand == Some(110)
            && matches!((&m.distress_mmsi, &m.from), (Some(d), Some(f)) if d.digits == f.digits);
        m.severity = if m.format == Format::Distress || m.category == Some(Category::Distress) {
            Severity::Distress
        } else {
            match m.category {
                Some(Category::Urgency) => Severity::Urgency,
                Some(Category::Safety) => Severity::Safety,
                _ => Severity::Routine,
            }
        };
        m.garbled_count = self.s.iter().filter(|&&x| x == GARBLED).count();
        m.content_id = content_id(self.s);
    }
}

/// M.821 Table 2: 0-9 digits, 11-36 A-Z, 37 '.', 38 ',', 39 '-', 40 '/',
/// 41 space; anything else has no character.
fn station_char(c: Symbol) -> Option<char> {
    match c {
        0..=9 => Some(char::from(b'0' + c)),
        11..=36 => Some(char::from(b'A' + c - 11)),
        37 => Some('.'),
        38 => Some(','),
        39 => Some('-'),
        40 => Some('/'),
        41 => Some(' '),
        _ => None,
    }
}

/// Quadrant digit -> (south, west): 0 NE, 1 NW, 2 SE, 3 SW.
fn quadrant(q: u8) -> (bool, bool) {
    (q >= 2, q % 2 == 1)
}

/// Format specifier symbols (symbols.csv).
pub fn format_of(x: Symbol) -> Format {
    match x {
        102 => Format::GeographicArea,
        112 => Format::Distress,
        114 => Format::CommonInterest,
        116 => Format::AllShips,
        120 => Format::Individual,
        123 => Format::IndividualAuto,
        103 => Format::Vts,
        _ => Format::Unknown,
    }
}

/// Category symbols (symbols.csv).
pub fn category_of(x: Symbol) -> Category {
    match x {
        112 => Category::Distress,
        110 => Category::Urgency,
        108 => Category::Safety,
        100 => Category::Routine,
        103 => Category::Vts,
        106 => Category::Acs,
        _ => Category::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_id_uses_the_seed_start_value() {
        // 03-parser.md step 8: "individual-safety gives 584c203a0db32c6b".
        let s = [
            120, 120, 0, 64, 52, 70, 0, 108, 47, 72, 31, 70, 0, 118, 126, 126, 126, 126, 126, 126,
            126, 117, 101,
        ];
        assert_eq!(content_id(&s), "584c203a0db32c6b");
        // Empty input: the start value itself (parse vector `empty`).
        assert_eq!(content_id(&[]), "14650fb0739d0383");
    }

    const SAFETY: [u8; 23] = [
        120, 120, 0, 64, 52, 70, 0, 108, 47, 72, 31, 70, 0, 118, 126, 126, 126, 126, 126, 126, 126,
        117, 101,
    ];

    // Step 8: length = the expansion's end, else EOS + ECC, else 0.
    #[test]
    fn length_ends_at_the_ecc() {
        assert_eq!(parse(&SAFETY).length, 23);
        let mut repeated = SAFETY.to_vec();
        repeated.extend([117, 117]); // closing EOS repetitions (F-25)
        assert_eq!(parse(&repeated).length, 23);
    }

    #[test]
    fn length_includes_the_expansion() {
        let mut s = SAFETY.to_vec();
        s.extend([102, 1, 24, 117, 105 ^ 102 ^ 1 ^ 24 ^ 117 ^ 105, 117, 117]);
        assert_eq!(parse(&s).length, 28);
    }

    #[test]
    fn length_is_zero_without_eos() {
        assert_eq!(parse(&SAFETY[..15]).length, 0);
        assert_eq!(parse(&[]).length, 0);
    }

    #[test]
    fn length_never_past_the_input_when_the_ecc_is_missing() {
        assert_eq!(parse(&SAFETY[..22]).length, 22);
    }
}
