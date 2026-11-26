//! Maritime identities (seed spec/03-domain/identities.md, ITU-R M.585-10).

/// The form of an identity: what kind of station it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ship,
    Group,
    Coast,
    SarAircraft,
    Handheld,
    ParentCraft,
    Aton,
    AisSart,
    Mob,
    EpirbAis,
    AmrdB,
    Unknown,
}

impl Kind {
    /// The vocabulary name (conformance vectors, `kind`).
    pub fn name(self) -> &'static str {
        match self {
            Kind::Ship => "ship",
            Kind::Group => "group",
            Kind::Coast => "coast",
            Kind::SarAircraft => "sar_aircraft",
            Kind::Handheld => "handheld",
            Kind::ParentCraft => "parent_craft",
            Kind::Aton => "aton",
            Kind::AisSart => "ais_sart",
            Kind::Mob => "mob",
            Kind::EpirbAis => "epirb_ais",
            Kind::AmrdB => "amrd_b",
            Kind::Unknown => "unknown",
        }
    }
}

/// A 9-digit identity, kept as text (PAR-04), with its form and MID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub digits: String,
    pub kind: Kind,
    pub mid: Option<u16>,
}

impl Identity {
    pub fn is_coast(&self) -> bool {
        self.kind == Kind::Coast
    }
}

/// Classify an identity by its leading digits.
pub fn classify(digits: &str) -> Identity {
    let (kind, mid_at) = form(digits);
    let mid = mid_at
        .and_then(|i| digits.get(i..i + 3))
        .and_then(|m| m.parse().ok());
    Identity {
        digits: digits.to_string(),
        kind,
        mid,
    }
}

/// The kind, and the index where the MID starts.
fn form(d: &str) -> (Kind, Option<usize>) {
    if d.len() != 9 || !d.bytes().all(|b| b.is_ascii_digit()) {
        return (Kind::Unknown, None);
    }
    // In the order of identities.md: 111 before ship, 00 before 0.
    const FORMS: [(&str, Kind, Option<usize>); 10] = [
        ("970", Kind::AisSart, None),
        ("972", Kind::Mob, None),
        ("974", Kind::EpirbAis, None),
        ("979", Kind::AmrdB, None),
        ("111", Kind::SarAircraft, Some(3)),
        ("00", Kind::Coast, Some(2)),
        ("0", Kind::Group, Some(1)),
        ("98", Kind::ParentCraft, Some(2)),
        ("99", Kind::Aton, Some(2)),
        ("8", Kind::Handheld, Some(1)),
    ];
    if let Some((_, kind, mid_at)) = FORMS.iter().find(|(prefix, _, _)| d.starts_with(prefix)) {
        return (*kind, *mid_at);
    }
    match d.as_bytes()[0] {
        b'2'..=b'7' => (Kind::Ship, Some(0)),
        _ => (Kind::Unknown, None),
    }
}
