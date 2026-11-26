//! Presentation helpers: pure functions from a record to texts, used by
//! every front end (seed spec/02-architecture.md, spec/07-ui).

pub mod detail;

use crate::dsc::domain::mid;
use crate::dsc::message::{Category, Element, ElementKind, Format, Severity};
use crate::dsc::record::Record;
use crate::dsc::time::{local_date, local_time};

/// The searchable terms of a message (spec/07-ui/search-terms.md, F-11),
/// in order, each distinct text once. Absent fields give no term.
pub fn search_terms(r: &Record) -> Vec<String> {
    let m = &r.message;
    let mut terms: Vec<String> = Vec::new();
    let mut add = |t: Option<String>| {
        if let Some(t) = t.filter(|t| !t.is_empty())
            && !terms.contains(&t)
        {
            terms.push(t);
        }
    };
    add(Some(local_date(r.utc, r.offset)));
    add(Some(local_time(r.utc, r.offset)));
    add(r.source_label.clone());
    add(Some(format_abbr(m.format).into()));
    add(Some(format_name(m.format).into()));
    add(m.category.and_then(category_name).map(String::from));
    for (id, st) in [
        (&m.from, &r.from_st),
        (&m.to, &r.to_st),
        (&m.distress_mmsi, &r.dmmsi_st),
    ] {
        if let Some(id) = id {
            add(Some(id.digits.clone()));
            add(id.mid.and_then(mid::administrations));
        }
        add(st.as_ref().and_then(|s| s.name.clone()));
    }
    for tc in [m.distress_telecommand, m.tc1, m.tc2] {
        add(tc.and_then(telecommand_text).map(String::from));
    }
    if let Some((short, long)) = m.distress_nature.and_then(nature_text) {
        add(Some(short.into()));
        add(Some(long.into()));
    }
    if m.self_cancel {
        add(Some("self-cancel".into()));
    }
    if let Some(fc) = m.freq_channel {
        add(fc.rx.map(|e| element_text(&e)));
        add(fc.tx.map(|e| element_text(&e)));
    }
    add(m.number.clone());
    add(Some(if m.ecc.ok { "OK" } else { "!!" }.into()));
    terms
}

/// Table A: the format abbreviation.
pub fn format_abbr(f: Format) -> &'static str {
    match f {
        Format::GeographicArea => "AREA",
        Format::Distress => "DIST",
        Format::CommonInterest => "COMM",
        Format::AllShips => "ALL",
        Format::Individual => "INDV",
        Format::IndividualAuto => "IAUT",
        Format::Vts => "VTS",
        Format::Unknown => "UNK",
    }
}

/// Table A: the format name.
pub fn format_name(f: Format) -> &'static str {
    match f {
        Format::GeographicArea => "GEOGRAPHIC AREA",
        Format::Distress => "DISTRESS",
        Format::CommonInterest => "COMMON INTEREST",
        Format::AllShips => "ALL SHIPS",
        Format::Individual => "INDIVIDUAL",
        Format::IndividualAuto => "INDIVIDUAL (AUTO)",
        Format::Vts => "VTS AREA",
        Format::Unknown => "UNKNOWN",
    }
}

/// Table B: the category name; none for an unknown category.
pub fn category_name(c: Category) -> Option<&'static str> {
    Some(match c {
        Category::Routine => "ROUTINE",
        Category::Vts => "VTS",
        Category::Acs => "ACS",
        Category::Safety => "SAFETY",
        Category::Urgency => "URGENCY",
        Category::Distress => "DISTRESS",
        Category::Unknown => return None,
    })
}

/// Table C: telecommand texts.
pub fn telecommand_text(x: u8) -> Option<&'static str> {
    Some(match x {
        100 => "all modes TP",
        101 => "duplex TP",
        103 => "polling",
        104 => "unable to comply",
        105 => "end of call",
        106 => "data",
        109 => "J3E TP",
        110 => "distress ack",
        112 => "distress relay",
        113 => "TTY-FEC",
        115 => "TTY-ARQ",
        118 => "test",
        121 => "position",
        126 => "no info",
        _ => return None,
    })
}

/// Table D: nature of distress, short and long text.
pub fn nature_text(x: u8) -> Option<(&'static str, &'static str)> {
    Some(match x {
        100 => ("fire/explosion", "fire/explosion"),
        101 => ("flooding", "flooding"),
        102 => ("collision", "collision"),
        103 => ("grounding", "grounding"),
        104 => ("listing", "listing/capsizing"),
        105 => ("sinking", "sinking"),
        106 => ("disabled/adrift", "disabled/adrift"),
        107 => ("undesignated", "undesignated distress"),
        108 => ("abandoning ship", "abandoning ship"),
        109 => ("piracy", "piracy/armed robbery"),
        110 => ("man overboard", "man overboard"),
        112 => ("EPIRB", "EPIRB emission"),
        _ => return None,
    })
}

/// Table E: a frequency or channel element.
pub fn element_text(e: &Element) -> String {
    match e.kind {
        ElementKind::Freq => match e.khz {
            Some(khz) if e.seven_digit => format!("{khz:.2} kHz"),
            Some(khz) => format!("{khz:.1} kHz"),
            None => "unknown".into(),
        },
        ElementKind::Hfmf => format!("HF/MF ch {}", e.channel.unwrap_or_default()),
        ElementKind::Vhf => format!("VHF ch {}", e.channel.unwrap_or_default()),
        ElementKind::M586 => "M.586".into(),
        ElementKind::Unknown => "unknown".into(),
    }
}

/// Search (04-support.md §4.6, VIEW-06): every whitespace-separated word
/// of the query is a case-insensitive (ASCII) substring of one term.
pub fn matches(query: &str, terms: &[String]) -> bool {
    let terms: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    query.split_whitespace().all(|w| {
        let w = w.to_ascii_lowercase();
        terms.iter().any(|t| t.contains(&w))
    })
}

/// The distress-only filter (VIEW-07): distress and urgency.
pub fn distress_only(r: &Record) -> bool {
    matches!(r.message.severity, Severity::Distress | Severity::Urgency)
}

/// The texts of one message-list row (VIEW-03; wording free, F-11).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListRow {
    pub date: String,
    pub time: String,
    pub source: String,
    pub format: String,
    pub category: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub ecc: String,
    /// Severity letter D, U, S, R (VIEW-04: not colour alone).
    pub severity: char,
}

/// The list row of a record.
pub fn list_row(r: &Record) -> ListRow {
    let m = &r.message;
    let to = match (&m.to, &m.area) {
        (Some(id), _) => id.digits.clone(),
        (None, Some(a)) => area_short(a),
        (None, None) => PLACEHOLDER.into(),
    };
    ListRow {
        date: local_date(r.utc, r.offset),
        time: local_time(r.utc, r.offset),
        source: r.source_label.clone().unwrap_or_else(|| PLACEHOLDER.into()),
        format: format_abbr(m.format).into(),
        category: match m.category {
            Some(c) => category_name(c).unwrap_or("UNKNOWN").into(),
            None => PLACEHOLDER.into(),
        },
        from: m
            .from
            .as_ref()
            .map_or_else(|| PLACEHOLDER.into(), |i| i.digits.clone()),
        to,
        subject: subject(r),
        ecc: if m.ecc.ok { "OK" } else { "!!" }.into(),
        severity: severity_letter(m.severity),
    }
}

/// Shown for an absent value (not searchable).
pub const PLACEHOLDER: &str = "--";

/// D, U, S or R.
pub fn severity_letter(s: Severity) -> char {
    match s {
        Severity::Distress => 'D',
        Severity::Urgency => 'U',
        Severity::Safety => 'S',
        Severity::Routine => 'R',
    }
}

/// Distress telecommand + nature, else nature, else first telecommand;
/// `[self-cancel]` appended (spec/07-ui, message list).
pub fn subject(r: &Record) -> String {
    let m = &r.message;
    let nature = m
        .distress_nature
        .and_then(nature_text)
        .map(|(short, _)| short);
    let mut s = match (m.distress_telecommand.and_then(telecommand_text), nature) {
        (Some(tc), Some(n)) => format!("{tc}: {n}"),
        (Some(tc), None) => tc.to_string(),
        (None, Some(n)) => n.to_string(),
        (None, None) => m
            .tc1
            .and_then(telecommand_text)
            .unwrap_or_default()
            .to_string(),
    };
    if m.self_cancel {
        s.push_str(" [self-cancel]");
    }
    s
}

/// A short area text: NW corner and extent, e.g. `60N003E+8/14`.
pub fn area_short(a: &crate::dsc::message::Area) -> String {
    format!(
        "{:02}{}{:03}{}+{}/{}",
        a.corner_lat.abs(),
        a.lat_quad,
        a.corner_lon.abs(),
        a.lon_quad,
        a.d_lat,
        a.d_lon
    )
}

/// A position as `62.467N 6.217E`.
pub fn position_short(lat: f64, lon: f64) -> String {
    format!(
        "{:.3}{} {:.3}{}",
        lat.abs(),
        if lat < 0.0 { 'S' } else { 'N' },
        lon.abs(),
        if lon < 0.0 { 'W' } else { 'E' }
    )
}

/// Alert texts: (summary, body). Informative wording.
pub fn alert_text(r: &Record) -> (String, String) {
    let m = &r.message;
    let summary = format!(
        "DSC {}",
        match m.severity {
            Severity::Distress => "DISTRESS",
            Severity::Urgency => "URGENCY",
            Severity::Safety => "SAFETY",
            Severity::Routine => "ROUTINE",
        }
    );
    let mut body = format!(
        "From {}",
        m.from.as_ref().map_or(PLACEHOLDER, |i| i.digits.as_str())
    );
    if let Some((_, long)) = m.distress_nature.and_then(nature_text) {
        body.push_str(&format!(" — {long}"));
    }
    if let Some(p) = m.position {
        body.push_str(&format!("  ({})", position_short(p.lat, p.lon)));
    }
    if m.self_cancel {
        body.push_str("  [SELF-CANCEL]");
    } else if m.distress_telecommand == Some(112) {
        body.push_str("  [RELAY]");
    } else if m.distress_telecommand == Some(110) {
        body.push_str("  [ACKNOWLEDGEMENT]");
    }
    (summary, body)
}
