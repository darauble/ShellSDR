//! Distances and the user's location (seed spec/04-algorithms/04-support.md
//! §4.4, §4.5).

/// Great-circle distance in km (haversine, mean Earth radius).
pub fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0088;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().min(1.0).asin()
}

/// Kilometres to nautical miles.
pub fn km_to_nm(km: f64) -> f64 {
    km / 1.852
}

/// A location typed by the user: two signed decimal numbers, latitude then
/// longitude, separated by spaces and/or a comma. Nothing else.
pub fn parse_location(text: &str) -> Option<(f64, f64)> {
    let text = text.replace(',', " ");
    let words: Vec<&str> = text.split_whitespace().collect();
    let [lat, lon] = words[..] else { return None };
    let (lat, lon) = (number(lat)?, number(lon)?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// `[+-]? digits [. digits]` or `[+-]? . digits`; nothing else (no
/// exponents, no nan or inf).
fn number(w: &str) -> Option<f64> {
    let body = w.strip_prefix(['+', '-']).unwrap_or(w);
    let (int, frac) = body
        .split_once('.')
        .map_or((body, None), |(a, b)| (a, Some(b)));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let ok = match frac {
        None => digits(int),
        Some(f) => digits(f) && (int.is_empty() || digits(int)),
    };
    if ok { w.parse().ok() } else { None }
}
