//! Time: calendar arithmetic, ISO 8601, recording start times
//! (seed spec/04-algorithms/04-support.md §4.1-4.3).

/// A calendar date and time of day (proleptic Gregorian).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// The calendar date and time of `epoch` seconds since 1970-01-01 00:00.
pub fn civil(epoch: i64) -> Civil {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400) as u32;
    // Days to civil date (H. Hinnant's algorithm), eras of 400 years.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    Civil {
        year,
        month,
        day,
        hour: secs / 3600,
        minute: secs / 60 % 60,
        second: secs % 60,
    }
}

pub use jiff::tz::TimeZone;

/// The offset (local - UTC, seconds) of a zone at an instant.
pub fn offset_at(tz: &TimeZone, utc: i64) -> i32 {
    jiff::Timestamp::from_second(utc).map_or(0, |t| tz.to_offset(t).seconds())
}

/// A recording's start time from its file name (§4.1): (UTC epoch,
/// offset), the name's time read as local time in `tz`.
pub fn base_time_from_name(name: &str, tz: &TimeZone) -> Option<(i64, i32)> {
    let b = name.as_bytes();
    (0..b.len())
        .filter(|&i| b[i].is_ascii_digit())
        .find_map(|start| date_time_at(b, start))
        .map(|(y, mo, d, h, mi, se)| local_to_utc(epoch_of(y, mo, d, h, mi, se), tz))
}

/// `YYYY MM DD`, up to 3 separators, `HH MM SS` starting at `start`.
fn date_time_at(b: &[u8], start: usize) -> Option<(i64, i64, i64, i64, i64, i64)> {
    let mut i = start;
    let y = take(b, &mut i, 4)?;
    let mo = take(b, &mut i, 2)?;
    let d = take(b, &mut i, 2)?;
    if !((1970..=2099).contains(&y) && (1..=12).contains(&mo) && (1..=31).contains(&d)) {
        return None;
    }
    let mut seps = 0;
    while i < b.len() && !b[i].is_ascii_digit() {
        seps += 1;
        i += 1;
        if seps > 3 {
            return None;
        }
    }
    if i >= b.len() {
        return None;
    }
    let h = take(b, &mut i, 2)?;
    let mi = take(b, &mut i, 2)?;
    let se = take(b, &mut i, 2)?;
    (h < 24 && mi < 60 && se < 60).then_some((y, mo, d, h, mi, se))
}

/// `n` digits as a number: one delimiter may precede the group, and one
/// non-digit may stand between two of its digits.
fn take(b: &[u8], i: &mut usize, n: usize) -> Option<i64> {
    if *i < b.len() && !b[*i].is_ascii_digit() {
        *i += 1;
    }
    let mut v = 0i64;
    let mut got = 0;
    while got < n {
        match b.get(*i) {
            Some(c) if c.is_ascii_digit() => {
                v = v * 10 + i64::from(c - b'0');
                got += 1;
                *i += 1;
            }
            Some(_) if got > 0 && b.get(*i + 1).is_some_and(u8::is_ascii_digit) => *i += 1,
            _ => return None,
        }
    }
    Some(v)
}

/// A wall-clock time read as if it were UTC (the day is not checked
/// against the month: 31 February is 3 March).
fn epoch_of(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + h * 3600 + mi * 60 + s
}

/// Local wall time to (UTC, offset) with explicit DST handling (§4.1): the
/// earlier instant in a repeated hour, the offset before the change in a
/// missing hour.
pub fn local_to_utc(wall: i64, tz: &TimeZone) -> (i64, i32) {
    let before = wall - i64::from(offset_at(tz, wall - 86_400));
    let after = wall - i64::from(offset_at(tz, wall + 86_400));
    let t = [before, after]
        .into_iter()
        .filter(|&t| i64::from(offset_at(tz, t)) == wall - t)
        .min()
        .unwrap_or(before);
    (t, offset_at(tz, t))
}

/// `YYYY-MM-DDTHH:MM:SSZ` (§4.2).
pub fn iso_utc(utc: i64) -> String {
    let c = civil(utc);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

/// The local date and time of `utc + offset`, then `±HH:MM` (§4.2).
pub fn iso_local(utc: i64, offset: i32) -> String {
    let c = civil(utc + i64::from(offset));
    let (sign, off) = if offset < 0 {
        ('-', -offset)
    } else {
        ('+', offset)
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        c.year,
        c.month,
        c.day,
        c.hour,
        c.minute,
        c.second,
        off / 3600,
        off / 60 % 60
    )
}

/// `YYYY-MM-DDTHH:MM:SSZ` back to an epoch; `None` for anything else.
pub fn parse_iso_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let shape = b.len() == 20
        && b.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        });
    if !shape {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (
        n(0..4)?,
        n(5..7)?,
        n(8..10)?,
        n(11..13)?,
        n(14..16)?,
        n(17..19)?,
    );
    ((1..=12).contains(&mo) && (1..=31).contains(&d) && h < 24 && mi < 60 && se < 60)
        .then(|| epoch_of(y, mo, d, h, mi, se))
}

/// The local date `YYYY-MM-DD` of `utc + offset`.
pub fn local_date(utc: i64, offset: i32) -> String {
    let c = civil(utc + i64::from(offset));
    format!("{:04}-{:02}-{:02}", c.year, c.month, c.day)
}

/// The local time of day `HH:MM:SS` of `utc + offset`.
pub fn local_time(utc: i64, offset: i32) -> String {
    let c = civil(utc + i64::from(offset));
    format!("{:02}:{:02}:{:02}", c.hour, c.minute, c.second)
}

/// The day file of a message: its local date + `.ndjson` (§4.3).
pub fn day_file_name(utc: i64, offset: i32) -> String {
    format!("{}.ndjson", local_date(utc, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Civil {
        Civil {
            year: y,
            month: mo,
            day: d,
            hour: h,
            minute: mi,
            second: s,
        }
    }

    #[test]
    fn civil_of_known_instants() {
        assert_eq!(civil(0), c(1970, 1, 1, 0, 0, 0));
        // 04-support.md §4.2: 1792498968 is 2026-10-20T12:22:48Z.
        assert_eq!(civil(1792498968), c(2026, 10, 20, 12, 22, 48));
        assert_eq!(civil(951_782_400), c(2000, 2, 29, 0, 0, 0)); // leap day
        assert_eq!(civil(-1), c(1969, 12, 31, 23, 59, 59));
        assert_eq!(civil(4_102_444_799), c(2099, 12, 31, 23, 59, 59));
    }
}
