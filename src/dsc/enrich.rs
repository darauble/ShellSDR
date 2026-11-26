//! Enrichment at reception (group `enrich`, ENR-02..05): what was known
//! when a message arrived goes into its record (schema `v` 2).

use crate::dsc::record::{PosSource, Record, StationPos};
use crate::dsc::stations::Stations;

/// Sample messages of conformance/scenarios.md (symbols from parse vectors).
pub mod samples {
    pub const SAFETY: [u8; 23] = [
        120, 120, 0, 64, 52, 70, 0, 108, 47, 72, 31, 70, 0, 118, 126, 126, 126, 126, 126, 126, 126,
        117, 101,
    ];
    pub const DISTRESS: [u8; 18] = [
        112, 112, 25, 89, 99, 11, 90, 107, 6, 22, 80, 6, 13, 15, 0, 109, 127, 63,
    ];
    pub const RELAY: [u8; 25] = [
        116, 116, 112, 0, 25, 70, 0, 0, 112, 25, 73, 88, 0, 0, 110, 7, 60, 30, 32, 50, 18, 17, 109,
        127, 107,
    ];
}

/// The position a message reports, and for whom (ENR-04, F-33): the
/// distress identity if there is one, else the sender. Its time is the
/// reception time.
pub fn observed_position(r: &Record) -> Option<(String, StationPos)> {
    let m = &r.message;
    let (lat, lon) = message_position(r)?;
    let who = m.distress_mmsi.as_ref().or(m.from.as_ref())?;
    Some((
        who.digits.clone(),
        StationPos {
            lat,
            lon,
            source: PosSource::Dsc,
            time: Some(r.utc),
        },
    ))
}

/// The position a message carries: the expansion's enhanced position if
/// there is one, else the call's.
pub fn message_position(r: &Record) -> Option<(f64, f64)> {
    let m = &r.message;
    m.expansion
        .as_ref()
        .and_then(|x| x.precise_position)
        .or(m.position.map(|p| (p.lat, p.lon)))
}

/// Fills in the reception context: the user's location, the distance to
/// the message's position, and each identity's station data.
pub fn enrich(r: &mut Record, stations: &Stations, user: Option<(f64, f64)>) {
    r.version = 2;
    r.rx = user;
    r.pos_km = match (user, message_position(r)) {
        (Some((ulat, ulon)), Some((lat, lon))) => {
            Some(crate::dsc::geo::distance_km(ulat, ulon, lat, lon))
        }
        _ => None,
    };
    let info = |id: &Option<crate::dsc::domain::identity::Identity>| {
        id.as_ref()
            .map(|i| stations.info(&i.digits, user))
            .filter(|s| *s != Default::default())
    };
    r.from_st = info(&r.message.from);
    r.to_st = info(&r.message.to);
    r.dmmsi_st = info(&r.message.distress_mmsi);
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;
    use crate::dsc::record::TimeSource;

    const T: i64 = 1792498971;

    fn rec(s: &[u8]) -> Record {
        Record::new(s.to_vec(), T, 10800, TimeSource::Wallclock)
    }

    #[test]
    fn a_distress_alert_reports_its_senders_position() {
        let (id, p) = observed_position(&rec(&DISTRESS)).unwrap();
        assert_eq!(id, "258999119");
        assert_eq!(
            (p.lat, p.lon, p.source, p.time),
            (
                62.46666666666667,
                6.216666666666667,
                PosSource::Dsc,
                Some(T)
            )
        );
    }

    #[test]
    fn a_relay_reports_the_distress_identitys_position_not_the_coast_station() {
        let (id, p) = observed_position(&rec(&RELAY)).unwrap();
        assert_eq!(id, "257388000");
        assert_eq!((p.lat, p.lon), (76.05, 32.833333333333336));
        assert_eq!(observed_position(&rec(&SAFETY)), None);
    }

    #[test]
    fn the_record_gets_location_distance_and_station_data() {
        let mut st = Stations::new();
        st.load_csv("mmsi,name,lat,lon\n258999119,FISHER,62.0,6.0\n");
        let mut r = rec(&DISTRESS);
        r.version = 1;
        enrich(&mut r, &st, Some((54.687157, 25.279652)));
        assert_eq!(r.version, 2);
        assert_eq!(r.rx, Some((54.687157, 25.279652)));
        let km = crate::dsc::geo::distance_km(
            54.687157,
            25.279652,
            62.46666666666667,
            6.216666666666667,
        );
        assert_eq!(r.pos_km, Some(km));
        let from = r.from_st.unwrap();
        assert_eq!(from.name.as_deref(), Some("FISHER"));
        assert_eq!(from.country.as_deref(), Some("Norway"));
        assert!(from.km.is_some());
        assert_eq!(r.to_st, None);
    }

    #[test]
    fn without_a_location_there_are_no_distances() {
        let mut r = rec(&SAFETY);
        enrich(&mut r, &Stations::new(), None);
        assert_eq!((r.rx, r.pos_km), (None, None));
        let to = r.to_st.unwrap();
        assert!(to.country.is_some() && to.km.is_none());
    }
}
