//! The detail view's fields (spec/07-ui "Detail", VIEW-05; wording is
//! informative). Its text is also what save and copy produce (EXP-01/02).

use super::{
    area_short, category_name, element_text, format_abbr, nature_text, subject, telecommand_text,
};
use crate::dsc::message::Eos;
use crate::dsc::record::{PosSource, Record};
use crate::dsc::stations::Stations;
use crate::dsc::time::{iso_local, iso_utc};

/// Context for records without stored enrichment (schema `v` 1): they are
/// resolved against today's location and station data, and say so (ENR-05).
pub struct Current<'a> {
    pub location: Option<(f64, f64)>,
    pub stations: &'a Stations,
}

/// The fields in order, each `(label, value)`; absent ones left out.
pub fn fields(r: &Record, now: &Current) -> Vec<(String, String)> {
    let m = &r.message;
    let mut f: Vec<(String, String)> = Vec::new();
    let mut add = |l: &str, v: String| f.push((l.to_string(), v));
    // Anything the record does not carry is resolved against today's
    // data, and the reception location then says so (ENR-05).
    let old = r.version < 2 || r.rx.is_none();
    let user = if old { now.location } else { r.rx };

    add("ECC", ecc_text(&m.ecc));
    add("Format", format_abbr(m.format).into());
    if let Some(c) = m.category.and_then(category_name) {
        add("Category", c.into());
    }
    let subject = subject(r);
    if !subject.is_empty() {
        add("Subject", subject);
    }
    if m.self_cancel {
        add("", "** SELF-CANCEL **".into());
    }

    // Parties: stored station data, or today's for an old record.
    let parties = [
        ("From", &m.from, &r.from_st),
        ("To", &m.to, &r.to_st),
        ("Distress", &m.distress_mmsi, &r.dmmsi_st),
    ];
    for (label, id, stored) in parties {
        if label == "Distress"
            && let Some(a) = &m.area
        {
            add("Area", area_short(a));
        }
        let Some(id) = id else { continue };
        let st = if r.version < 2 || stored.is_none() {
            Some(now.stations.info(&id.digits, user))
        } else {
            stored.clone()
        };
        let mut st = st.unwrap_or_default();
        if st.country.is_none() {
            st.country = id.mid.and_then(crate::dsc::domain::mid::administrations);
        }
        let mut v = format!("{} ({})", id.digits, id.kind.name());
        let who: Vec<String> = [st.name.clone(), st.country.clone()]
            .into_iter()
            .flatten()
            .collect();
        if !who.is_empty() {
            v.push(' ');
            v.push_str(&who.join(", "));
        }
        add(label, v);
        if let Some(p) = st.position {
            let mut at = coords(p.lat, p.lon);
            if p.source != PosSource::Builtin {
                at.push_str(&format!(
                    "  [{}{}]",
                    p.source.name(),
                    p.time
                        .map(|t| format!(", {}", iso_utc(t)))
                        .unwrap_or_default()
                ));
            }
            at.push_str(&distance(st.km));
            add(&format!("  {label} at"), at);
        }
    }

    // Distress and message 2/3 contents.
    if let Some((short, _)) = m.distress_nature.and_then(nature_text) {
        add("Nature", short.into());
    }
    if m.position_refused {
        add("Position", "refused".into());
    }
    if let Some(p) = m.position {
        add(
            "Position",
            format!(
                "{:.4}{} {:.4}{}",
                p.lat.abs(),
                p.lat_quad,
                p.lon.abs(),
                p.lon_quad
            ),
        );
        let km = if old {
            user.map(|(a, b)| crate::dsc::geo::distance_km(a, b, p.lat, p.lon))
        } else {
            r.pos_km
        };
        add("  at", format!("{}{}", coords(p.lat, p.lon), distance(km)));
    }
    if let Some((h, mi)) = m.event_time {
        add("Event", format!("{h:02}:{mi:02} UTC"));
    }
    if let Some(fc) = m.freq_channel {
        if let Some(e) = fc.rx {
            add("Rx", element_text(&e));
        }
        if let Some(e) = fc.tx {
            add("Tx", element_text(&e));
        }
    }
    if let Some(t) = m.tc2.filter(|&t| t != 126).and_then(telecommand_text) {
        add("Telecommand 2", t.into());
    }
    if let Some(n) = &m.number {
        add("Number", n.clone());
    }
    if let Some(d) = m.duration_s {
        add(
            "Duration",
            format!("{}:{:02}:{:02}", d / 3600, d / 60 % 60, d % 60),
        );
    }
    if let Some(t) = m.subsequent_comm.and_then(telecommand_text) {
        add("Then", t.into());
    }

    // Expansion (M.821).
    if let Some(x) = &m.expansion {
        if let Some((lat, lon)) = x.precise_position {
            add(
                "Precise pos",
                format!(
                    "{:.6}{} {:.6}{}",
                    lat.abs(),
                    if lat < 0.0 { 'S' } else { 'N' },
                    lon.abs(),
                    if lon < 0.0 { 'W' } else { 'E' }
                ),
            );
            if let Some((a, b)) = user {
                add(
                    "  at",
                    format!(
                        "{}{}",
                        coords(lat, lon),
                        distance(Some(crate::dsc::geo::distance_km(a, b, lat, lon)))
                    ),
                );
            }
        }
        if let Some(v) = x.speed_kn {
            add("Speed", format!("{v:.1} kn"));
        }
        if let Some(v) = x.course_deg {
            add("Course", format!("{v:.1} deg"));
        }
        if let Some(v) = x.hdop {
            add("HDOP", format!("{v:.1}"));
        }
        if let Some(v) = &x.station_id {
            add("Station ID", v.clone());
        }
        if let Some(v) = x.persons_on_board {
            add("On board", v.to_string());
        }
        let list = |v: &[u8]| v.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
        if !x.requested.is_empty() {
            add("Requests", list(&x.requested));
        }
        if !x.unavailable.is_empty() {
            add("Not available", list(&x.unavailable));
        }
        add("Exp. ECC", ecc_text(&x.ecc));
    }

    // Reception.
    if let Some(e) = m.eos {
        add(
            "EOS",
            match e {
                Eos::Req => "REQ",
                Eos::Ack => "ACK",
                Eos::Eos => "EOS",
            }
            .into(),
        );
    }
    add("UTC", iso_utc(r.utc));
    add("Local", iso_local(r.utc, r.offset));
    add("Time from", r.time_source.name().into());
    if let Some(s) = &r.source_label {
        add("Source", s.clone());
    }
    if let Some(file) = &r.file {
        add("File", file.clone());
    }
    match (old, user) {
        (false, Some((a, b))) => add("Rx at", coords(a, b)),
        (true, Some((a, b))) => add(
            "Rx at",
            format!("{}  (current; not recorded)", coords(a, b)),
        ),
        _ => {}
    }
    add(
        "Raw",
        r.symbols
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(" "),
    );
    f
}

fn ecc_text(e: &crate::dsc::message::Ecc) -> String {
    match (e.ok, e.received, e.computed) {
        (true, _, _) => "OK".into(),
        (false, Some(rx), Some(cc)) => format!("error (received {rx}, computed {cc})"),
        _ => "missing".into(),
    }
}

fn coords(lat: f64, lon: f64) -> String {
    format!("{lat:.5}, {lon:.5}")
}

/// `  (12.3 km / 6.6 NM)`, or nothing.
fn distance(km: Option<f64>) -> String {
    km.map(|k| format!("  ({k:.1} km / {:.1} NM)", crate::dsc::geo::km_to_nm(k)))
        .unwrap_or_default()
}

/// The saved text: one `Label : value` line per field (spec/05-data).
pub fn text(fields: &[(String, String)]) -> String {
    let width = fields
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    fields
        .iter()
        .map(|(l, v)| format!("{l:width$} : {v}\n"))
        .collect()
}

/// The default file name of a saved message (EXP-01):
/// `<project>_<local YYYY-MM-DDTHH-MM-SS>_<from or na>_<to or na>.txt`.
pub fn save_file_name(project: &str, r: &Record) -> String {
    let local: String = iso_local(r.utc, r.offset)
        .chars()
        .take(19)
        .map(|c| if c == ':' { '-' } else { c })
        .collect();
    let id = |i: &Option<crate::dsc::domain::identity::Identity>| {
        i.as_ref().map_or("na".to_string(), |i| i.digits.clone())
    };
    format!(
        "{project}_{local}_{}_{}.txt",
        id(&r.message.from),
        id(&r.message.to)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::enrich::samples::*;
    use crate::dsc::record::TimeSource;

    fn rec(s: &[u8]) -> Record {
        let mut r = Record::new(s.to_vec(), 1792498971, 10800, TimeSource::Wallclock);
        r.source_label = Some("8414.5 kHz".into());
        r
    }

    fn labels(f: &[(String, String)]) -> Vec<&str> {
        f.iter().map(|(l, _)| l.as_str()).collect()
    }

    #[test]
    fn a_safety_call_shows_identification_parties_reception_and_raw() {
        let st = Stations::new();
        let f = fields(
            &rec(&SAFETY),
            &Current {
                location: None,
                stations: &st,
            },
        );
        assert_eq!(
            labels(&f),
            [
                "ECC",
                "Format",
                "Category",
                "Subject",
                "From",
                "To",
                "EOS",
                "UTC",
                "Local",
                "Time from",
                "Source",
                "Raw"
            ]
        );
        assert_eq!(
            f[4].1,
            "477231700 (ship) China (People's Republic of) - Hong Kong (Special Administrative Region of China)"
        );
        assert_eq!(f[7].1, "2026-10-20T12:22:51Z");
        assert_eq!(
            f[11].1,
            "120 120 0 64 52 70 0 108 47 72 31 70 0 118 126 126 126 126 126 126 126 117 101"
        );
    }

    #[test]
    fn a_distress_alert_shows_nature_position_with_distance_and_event_time() {
        let st = Stations::new();
        let mut r = rec(&DISTRESS);
        crate::dsc::enrich::enrich(&mut r, &st, Some((54.687157, 25.279652)));
        let f = fields(
            &r,
            &Current {
                location: None,
                stations: &st,
            },
        );
        let get = |l: &str| f.iter().find(|(x, _)| x == l).map(|(_, v)| v.as_str());
        assert_eq!(get("Nature"), Some("undesignated"));
        assert_eq!(get("Position"), Some("62.4667N 6.2167E"));
        assert_eq!(
            get("  at"),
            Some("62.46667, 6.21667  (1394.4 km / 752.9 NM)")
        );
        assert_eq!(get("Event"), Some("15:00 UTC"));
        assert_eq!(get("Rx at"), Some("54.68716, 25.27965"));
    }

    #[test]
    fn an_old_record_is_resolved_against_today_and_says_so() {
        let mut st = Stations::new();
        st.load_csv("mmsi,name\n477231700,TEST VESSEL\n");
        let mut r = rec(&SAFETY);
        r.version = 1;
        let f = fields(
            &r,
            &Current {
                location: Some((54.687157, 25.279652)),
                stations: &st,
            },
        );
        let get = |l: &str| f.iter().find(|(x, _)| x == l).map(|(_, v)| v.as_str());
        assert!(get("From").unwrap().contains("TEST VESSEL"));
        assert_eq!(
            get("Rx at"),
            Some("54.68716, 25.27965  (current; not recorded)")
        );
    }

    #[test]
    fn the_saved_text_and_file_name() {
        let st = Stations::new();
        let r = rec(&SAFETY);
        let t = text(&fields(
            &r,
            &Current {
                location: None,
                stations: &st,
            },
        ));
        assert!(t.starts_with("ECC       : OK\nFormat    : INDV\n"), "{t}");
        assert_eq!(
            save_file_name("dsqrust", &r),
            "dsqrust_2026-10-20T15-22-51_477231700_006452700.txt"
        );
        let d = rec(&DISTRESS);
        assert_eq!(
            save_file_name("dsqrust", &d),
            "dsqrust_2026-10-20T15-22-51_258999119_na.txt"
        );
    }
}
