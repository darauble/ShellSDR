//! The Report's data (spec 20.3, 20.4): counters per day and per month,
//! the events of the log, and the clock that says which day a moment
//! belongs to. Pure: no files, no wall clock.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::record::naming::UtcDateTime;

/// Named counters (seconds, counts, bytes).
pub type Counters = BTreeMap<String, u64>;

/// Counter names (spec 20.3). Decoder output from a file playback is
/// counted under `file.` + the name, so a replay does not inflate the day.
pub mod keys {
    pub const TIME_APP: &str = "time.app";
    pub const TIME_SOURCE: &str = "time.source";
    pub const TIME_REC: &str = "time.rec";
    /// + the VFO type: the time the active VFO had that type.
    pub const TIME_TYPE: &str = "time.type.";
    pub const APP_STARTS: &str = "app.starts";
    pub const APP_UNCLEAN: &str = "app.unclean";
    pub const SOURCE_FAILURES: &str = "source.failures";
    pub const DSC_MESSAGES: &str = "dsc.messages";
    /// + `distress`, `urgency`, `safety`, `routine`.
    pub const DSC_SEVERITY: &str = "dsc.";
    pub const DSC_GARBLED: &str = "dsc.garbled";
    pub const HFDL_MESSAGES: &str = "hfdl.messages";
    /// + `emergency`, `caution`, `routine`, `link`.
    pub const HFDL_SEVERITY: &str = "hfdl.";
    /// Frames that failed their FCS.
    pub const HFDL_BAD_FRAMES: &str = "hfdl.bad_frames";
    /// Distinct planes seen and ground stations received (maxima: a day's
    /// and a month's count, spec 27).
    pub const HFDL_AIRCRAFT: &str = "hfdl.aircraft";
    pub const HFDL_STATIONS: &str = "hfdl.stations";
    pub const FAX_PICTURES: &str = "fax.pictures";
    pub const FAX_LINES: &str = "fax.lines";
    pub const FAX_SAVED: &str = "fax.saved";
    pub const DIGITAL_BYTES: &str = "digital.bytes";
    pub const DIGITAL_PICTURES: &str = "digital.pictures";
    pub const DIGITAL_RSID: &str = "digital.rsid";
    pub const DIGITAL_SWITCHES: &str = "digital.switches";
    pub const REC_FILES: &str = "rec.files";
    pub const REC_BYTES: &str = "rec.bytes";
    pub const SCHED_RUNS: &str = "sched.runs";
    pub const SCHED_SKIPPED: &str = "sched.skipped";
    pub const SCHED_FAILED: &str = "sched.failed";
    pub const WARNINGS: &str = "log.warnings";
    pub const ERRORS: &str = "log.errors";
    /// Per station: tuned seconds, and its decoded output.
    pub const STATION_TIME: &str = "time";
    /// Per station: the noise floor, as the sum of minute readings in tenths
    /// of a dB above -200 dB, and their number (the mean is sum / n).
    pub const STATION_NOISE_SUM: &str = "noise.sum";
    pub const STATION_NOISE_N: &str = "noise.n";
    /// Health maxima: DSP load (permille), CPU temperature (tenths of a
    /// degree), memory (kB); dropped IQ blocks (a counter).
    pub const HEALTH_DSP: &str = "health.dsp";
    pub const HEALTH_TEMP: &str = "health.temp";
    pub const HEALTH_MEM: &str = "health.mem";
    pub const HEALTH_DROPS: &str = "health.drops";
    pub const WATCHDOG: &str = "source.watchdog";
    /// The prefix of decoder counters from a file playback.
    pub const FILE: &str = "file.";
}

/// Readings (one per 10 s) before a day's noise floor counts: 5 minutes.
pub const NOISE_MIN_READINGS: u64 = 30;
/// A change of the noise floor worth a warning (spec 20.6).
pub const NOISE_CHANGE_DB: f64 = 6.0;

/// Counters whose hours are kept for the "Activity by hour" row.
pub const HOURLY: [&str; 4] = [
    keys::DSC_MESSAGES,
    keys::HFDL_MESSAGES,
    keys::FAX_PICTURES,
    keys::DIGITAL_BYTES,
];

/// One day (`2026-11-24`) or one month (`2026-11`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Period {
    pub key: String,
    pub counters: Counters,
    /// Station (bookmark name, or `(manual) 7040 kHz`) -> its counters.
    pub stations: BTreeMap<String, Counters>,
    /// Counter -> 24 hourly values (days only, `HOURLY` counters only).
    pub hourly: BTreeMap<String, Vec<u64>>,
    /// Highest values seen (health: DSP load, temperature, memory).
    pub maxima: Counters,
}

impl Period {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ..Self::default()
        }
    }

    pub fn get(&self, key: &str) -> u64 {
        self.counters.get(key).copied().unwrap_or(0)
    }

    pub fn max_of(&self, key: &str) -> Option<u64> {
        self.maxima.get(key).copied()
    }

    fn raise(&mut self, key: &str, v: u64) {
        let m = self.maxima.entry(key.to_string()).or_insert(v);
        *m = (*m).max(v);
    }

    fn add(&mut self, key: &str, n: u64) {
        *self.counters.entry(key.to_string()).or_default() += n;
    }

    fn add_station(&mut self, station: &str, key: &str, n: u64) {
        *self
            .stations
            .entry(station.to_string())
            .or_default()
            .entry(key.to_string())
            .or_default() += n;
    }

    fn add_hour(&mut self, key: &str, hour: usize, n: u64) {
        let h = self
            .hourly
            .entry(key.to_string())
            .or_insert_with(|| vec![0; 24]);
        h.resize(24, 0);
        h[hour.min(23)] += n;
    }

    /// Add `other`'s counters and stations into this one (the lifetime).
    pub fn merge(&mut self, other: &Period) {
        for (k, v) in &other.counters {
            self.add(k, *v);
        }
        for (s, c) in &other.stations {
            for (k, v) in c {
                self.add_station(s, k, *v);
            }
        }
        for (k, v) in &other.maxima {
            self.raise(k, *v);
        }
    }

    /// As [`Period::merge`], with the hours too (the same day counted in
    /// two places).
    fn absorb(&mut self, other: &Period) {
        self.merge(other);
        for (k, hours) in &other.hourly {
            for (h, v) in hours.iter().enumerate() {
                if *v > 0 {
                    self.add_hour(k, h, *v);
                }
            }
        }
    }
}

/// The persisted counters (`<profile>_report.v1.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportDoc {
    pub min_version: Option<String>,
    /// Set while ShellSDR runs; still set at the next start means it did
    /// not quit cleanly.
    pub running: bool,
    /// The last time the counters were written (Unix s).
    pub last_seen: i64,
    /// The first start counted (Unix s).
    pub first_start: i64,
    /// Days, oldest first.
    pub days: Vec<Period>,
    /// Months, oldest first (kept forever by default).
    pub months: Vec<Period>,
}

/// The `min_version` of the report file.
pub const REPORT_MIN_VERSION: &str = "0.3.0";

impl Default for ReportDoc {
    fn default() -> Self {
        Self {
            min_version: Some(REPORT_MIN_VERSION.to_string()),
            running: false,
            last_seen: 0,
            first_start: 0,
            days: Vec::new(),
            months: Vec::new(),
        }
    }
}

fn period_mut<'a>(list: &'a mut Vec<Period>, key: &str) -> &'a mut Period {
    let i = match list.binary_search_by(|p| p.key.as_str().cmp(key)) {
        Ok(i) => i,
        Err(i) => {
            list.insert(i, Period::new(key));
            i
        }
    };
    &mut list[i]
}

impl ReportDoc {
    pub fn day(&self, key: &str) -> Option<&Period> {
        self.days.iter().find(|p| p.key == key)
    }

    pub fn month(&self, key: &str) -> Option<&Period> {
        self.months.iter().find(|p| p.key == key)
    }

    /// Add `n` to counter `key` of the moment `at` (its day, hour and month).
    pub fn count(&mut self, at: &Moment, key: &str, n: u64) {
        if n == 0 {
            return;
        }
        let day = period_mut(&mut self.days, &at.day);
        day.add(key, n);
        if HOURLY.contains(&key) {
            day.add_hour(key, at.hour, n);
        }
        period_mut(&mut self.months, &at.month).add(key, n);
    }

    /// Keep the highest `v` of `key` for the day and the month.
    pub fn maximum(&mut self, at: &Moment, key: &str, v: u64) {
        period_mut(&mut self.days, &at.day).raise(key, v);
        period_mut(&mut self.months, &at.month).raise(key, v);
    }

    /// Keep the highest of `key` for the day (`day_v`) and the month
    /// (`month_v`) apart: distinct counts (spec 27).
    pub fn maximum_apart(&mut self, at: &Moment, key: &str, day_v: u64, month_v: u64) {
        period_mut(&mut self.days, &at.day).raise(key, day_v);
        period_mut(&mut self.months, &at.month).raise(key, month_v);
    }

    /// Add `n` to `station`'s counter `key`.
    pub fn count_station(&mut self, at: &Moment, station: &str, key: &str, n: u64) {
        if n == 0 || station.is_empty() {
            return;
        }
        period_mut(&mut self.days, &at.day).add_station(station, key, n);
        period_mut(&mut self.months, &at.month).add_station(station, key, n);
    }

    /// Add everything `other` counted (kept in memory while the folder
    /// could not be read) to this document read from the folder.
    pub fn absorb(&mut self, other: &ReportDoc) {
        for p in &other.days {
            period_mut(&mut self.days, &p.key).absorb(p);
        }
        for p in &other.months {
            period_mut(&mut self.months, &p.key).absorb(p);
        }
        if self.first_start == 0 || (other.first_start > 0 && other.first_start < self.first_start)
        {
            self.first_start = other.first_start;
        }
    }

    /// `station`'s mean noise floor on `day` (dB), from at least `min_n`
    /// readings.
    pub fn noise(&self, day: &str, station: &str, min_n: u64) -> Option<f64> {
        let c = self.day(day)?.stations.get(station)?;
        let n = *c.get(keys::STATION_NOISE_N)?;
        let sum = *c.get(keys::STATION_NOISE_SUM)?;
        (n >= min_n).then(|| sum as f64 / n as f64 / 10.0 - 200.0)
    }

    /// Today's noise floor of `station` against the median of its last 7
    /// days (at least 3 of them): the change in dB when it is 6 dB or more.
    pub fn noise_change(&self, today: &str, station: &str) -> Option<f64> {
        let now = self.noise(today, station, NOISE_MIN_READINGS)?;
        let mut past: Vec<f64> = (1..=7)
            .filter_map(|n| day_minus(today, n))
            .filter_map(|d| self.noise(&d, station, NOISE_MIN_READINGS))
            .collect();
        if past.len() < 3 {
            return None;
        }
        past.sort_by(f64::total_cmp);
        let median = past[past.len() / 2];
        let change = now - median;
        (change.abs() >= NOISE_CHANGE_DB).then_some(change)
    }

    /// Everything counted: the months summed.
    pub fn lifetime(&self) -> Period {
        let mut all = Period::new("lifetime");
        for m in &self.months {
            all.merge(m);
        }
        all
    }

    /// Drop days older than `keep_days` before `today`, months older than
    /// `keep_months` (0 keeps forever).
    pub fn prune(&mut self, today: &str, keep_days: u32, keep_months: u32) {
        if keep_days > 0
            && let Some(first) = day_minus(today, keep_days)
        {
            self.days.retain(|p| p.key.as_str() > first.as_str());
        }
        if keep_months > 0
            && let Some(first) = month_minus(&today[..7.min(today.len())], keep_months)
        {
            self.months.retain(|p| p.key.as_str() > first.as_str());
        }
    }
}

/// `YYYY-MM-DD` `n` days before `day`.
pub fn day_minus(day: &str, n: u32) -> Option<String> {
    let t = day_start_utc(day)? - i64::from(n) * 86_400;
    let d = UtcDateTime::from_unix(t.max(0) as u64);
    Some(format!("{:04}-{:02}-{:02}", d.year, d.month, d.day))
}

/// `YYYY-MM` `n` months before `month`.
pub fn month_minus(month: &str, n: u32) -> Option<String> {
    let y: i32 = month.get(..4)?.parse().ok()?;
    let m: i32 = month.get(5..7)?.parse().ok()?;
    let idx = y * 12 + (m - 1) - n as i32;
    Some(format!(
        "{:04}-{:02}",
        idx.div_euclid(12),
        idx.rem_euclid(12) + 1
    ))
}

/// Midnight UTC of `YYYY-MM-DD` (Unix s).
pub fn day_start_utc(day: &str) -> Option<i64> {
    let y: i64 = day.get(..4)?.parse().ok()?;
    let m: i64 = day.get(5..7)?.parse().ok()?;
    let d: i64 = day.get(8..10)?.parse().ok()?;
    // days from civil (H. Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400)
}

/// Where a moment falls: its day, month and hour in the report's clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    pub day: String,
    pub month: String,
    pub hour: usize,
}

/// The report's clock: local time (the system's offset) or UTC (spec 20.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub local: bool,
    /// The local offset from UTC in minutes.
    pub offset_min: i32,
}

impl Clock {
    /// The offset used (0 in UTC).
    pub fn offset(&self) -> i32 {
        if self.local { self.offset_min } else { 0 }
    }

    fn shifted(&self, t: i64) -> UtcDateTime {
        UtcDateTime::from_unix((t + i64::from(self.offset()) * 60).max(0) as u64)
    }

    pub fn moment(&self, t: i64) -> Moment {
        let d = self.shifted(t);
        Moment {
            day: format!("{:04}-{:02}-{:02}", d.year, d.month, d.day),
            month: format!("{:04}-{:02}", d.year, d.month),
            hour: d.hour as usize,
        }
    }

    /// `2026-11-24T20:04:12+02:00`, or `...Z` in UTC.
    pub fn stamp(&self, t: i64) -> String {
        let d = self.shifted(t);
        let off = self.offset();
        let zone = if off == 0 {
            "Z".to_string()
        } else {
            let a = off.abs();
            format!(
                "{}{:02}:{:02}",
                if off < 0 { '-' } else { '+' },
                a / 60,
                a % 60
            )
        };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{zone}",
            d.year, d.month, d.day, d.hour, d.minute, d.second
        )
    }

    /// `20:04:12` for the log page.
    pub fn time_of_day(&self, t: i64) -> String {
        let d = self.shifted(t);
        format!("{:02}:{:02}:{:02}", d.hour, d.minute, d.second)
    }
}

/// An event's level (spec 20.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    /// The mark in the log: nothing, `!`, `!!`.
    pub fn mark(self) -> &'static str {
        match self {
            Level::Info => "",
            Level::Warning => "!",
            Level::Error => "!!",
        }
    }

    fn from_mark(s: &str) -> Option<Self> {
        match s {
            "" | "-" => Some(Level::Info),
            "!" => Some(Level::Warning),
            "!!" => Some(Level::Error),
            _ => None,
        }
    }
}

/// An event's category (spec 20.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    App,
    Source,
    User,
    Sched,
    Rec,
    Dsc,
    Hfdl,
    Fax,
    Digital,
    Health,
}

impl Category {
    pub const ALL: [Category; 10] = [
        Category::App,
        Category::Source,
        Category::User,
        Category::Sched,
        Category::Rec,
        Category::Dsc,
        Category::Hfdl,
        Category::Fax,
        Category::Digital,
        Category::Health,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::App => "APP",
            Category::Source => "SOURCE",
            Category::User => "USER",
            Category::Sched => "SCHED",
            Category::Rec => "REC",
            Category::Dsc => "DSC",
            Category::Hfdl => "HFDL",
            Category::Fax => "FAX",
            Category::Digital => "DIGITAL",
            Category::Health => "HEALTH",
        }
    }

    fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.label() == s)
    }
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Unix s.
    pub time: i64,
    pub level: Level,
    pub category: Category,
    pub text: String,
    /// A file it made (a recording, a picture, a saved message).
    pub file: Option<String>,
}

impl Event {
    /// The log file line: `<stamp> <mark> <CATEGORY> <text>[ | <file>]`,
    /// with `-` for the info mark so the columns split on spaces.
    pub fn line(&self, clock: &Clock) -> String {
        let mark = match self.level {
            Level::Info => "-",
            l => l.mark(),
        };
        let text = self.text.replace(['\n', '\r'], " ");
        match &self.file {
            Some(f) => format!(
                "{} {mark} {} {text} | {f}",
                clock.stamp(self.time),
                self.category.label()
            ),
            None => format!(
                "{} {mark} {} {text}",
                clock.stamp(self.time),
                self.category.label()
            ),
        }
    }

    /// Read a log line back (either clock's stamps).
    pub fn parse(line: &str) -> Option<Event> {
        let mut parts = line.splitn(4, ' ');
        let stamp = parts.next()?;
        let level = Level::from_mark(parts.next()?)?;
        let category = Category::from_label(parts.next()?)?;
        let rest = parts.next().unwrap_or("");
        let (text, file) = match rest.rsplit_once(" | ") {
            Some((t, f)) => (t.to_string(), Some(f.to_string())),
            None => (rest.to_string(), None),
        };
        Some(Event {
            time: parse_stamp(stamp)?,
            level,
            category,
            text,
            file,
        })
    }
}

/// `2026-11-24T20:04:12+02:00` or `...Z` -> Unix s.
pub fn parse_stamp(s: &str) -> Option<i64> {
    let day = day_start_utc(s.get(..10)?)?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let m: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    let zone = s.get(19..)?;
    let off = if zone == "Z" {
        0
    } else {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let zh: i64 = zone.get(1..3)?.parse().ok()?;
        let zm: i64 = zone.get(4..6)?.parse().ok()?;
        sign * (zh * 60 + zm)
    };
    Some(day + h * 3600 + m * 60 + sec - off * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-24 18:04:12 UTC.
    const T: i64 = 1_795_543_452;

    #[test]
    fn the_clock_puts_a_moment_on_its_local_or_utc_day() {
        let utc = Clock {
            local: false,
            offset_min: 120,
        };
        let local = Clock {
            local: true,
            offset_min: 120,
        };
        assert_eq!(utc.stamp(T), "2026-11-24T18:04:12Z");
        assert_eq!(local.stamp(T), "2026-11-24T20:04:12+02:00");
        // 23:30 UTC is already the next local day at UTC+2.
        let late = T + 5 * 3600 + 26 * 60;
        assert_eq!(utc.moment(late).day, "2026-11-24");
        assert_eq!(local.moment(late).day, "2026-11-25");
        assert_eq!(local.moment(late).hour, 1);
        assert_eq!(local.moment(T).month, "2026-11");
        let west = Clock {
            local: true,
            offset_min: -330,
        };
        assert_eq!(west.stamp(T), "2026-11-24T12:34:12-05:30");
    }

    #[test]
    fn stamps_read_back_to_the_same_instant() {
        for c in [
            Clock {
                local: false,
                offset_min: 0,
            },
            Clock {
                local: true,
                offset_min: 120,
            },
            Clock {
                local: true,
                offset_min: -330,
            },
        ] {
            assert_eq!(parse_stamp(&c.stamp(T)), Some(T), "{}", c.stamp(T));
        }
        assert_eq!(day_start_utc("1970-01-01"), Some(0));
        assert_eq!(day_minus("2026-03-01", 1).unwrap(), "2026-02-28");
    }

    #[test]
    fn events_write_and_read_back_as_lines() {
        let clock = Clock {
            local: true,
            offset_min: 120,
        };
        let e = Event {
            time: T,
            level: Level::Warning,
            category: Category::Source,
            text: "RSPduo lost (read error), retry in 10 s".into(),
            file: None,
        };
        let line = e.line(&clock);
        assert_eq!(
            line,
            "2026-11-24T20:04:12+02:00 ! SOURCE RSPduo lost (read error), retry in 10 s"
        );
        assert_eq!(Event::parse(&line), Some(e));
        let info = Event {
            time: T,
            level: Level::Info,
            category: Category::Rec,
            text: "Closed: 20 min, 41 MB".into(),
            file: Some("/r/a b.wav".into()),
        };
        let line = info.line(&Clock {
            local: false,
            offset_min: 0,
        });
        assert_eq!(
            line,
            "2026-11-24T18:04:12Z - REC Closed: 20 min, 41 MB | /r/a b.wav"
        );
        assert_eq!(Event::parse(&line), Some(info));
        assert_eq!(Event::parse("garbage"), None);
    }

    #[test]
    fn counts_go_to_the_day_its_hour_and_the_month_and_add_up_to_the_lifetime() {
        let clock = Clock {
            local: false,
            offset_min: 0,
        };
        let mut d = ReportDoc::default();
        let a = clock.moment(T);
        let b = clock.moment(T + 86_400 * 7);
        d.count(&a, keys::DSC_MESSAGES, 3);
        d.count(&a, keys::DSC_MESSAGES, 2);
        d.count(&b, keys::DSC_MESSAGES, 1);
        d.count(&a, keys::TIME_APP, 0);
        d.count_station(&a, "DSC 2", keys::STATION_TIME, 60);
        assert_eq!(d.day("2026-11-24").unwrap().get(keys::DSC_MESSAGES), 5);
        assert_eq!(
            d.day("2026-11-24").unwrap().hourly[keys::DSC_MESSAGES][18],
            5
        );
        assert!(
            !d.day("2026-11-24")
                .unwrap()
                .counters
                .contains_key(keys::TIME_APP)
        );
        assert_eq!(d.month("2026-11").unwrap().get(keys::DSC_MESSAGES), 5);
        assert_eq!(d.month("2026-12").unwrap().get(keys::DSC_MESSAGES), 1);
        let life = d.lifetime();
        assert_eq!(life.get(keys::DSC_MESSAGES), 6);
        assert_eq!(life.stations["DSC 2"][keys::STATION_TIME], 60);
        assert_eq!(d.days.len(), 2);
        d.prune("2026-12-01", 5, 0);
        assert_eq!(d.days.len(), 1, "the old day goes, months stay");
        assert_eq!(d.months.len(), 2);
        d.prune("2026-12-01", 5, 1);
        assert_eq!(d.months.len(), 1);
    }

    #[test]
    fn a_noise_floor_6_db_off_its_week_is_a_change() {
        let clock = Clock {
            local: false,
            offset_min: 0,
        };
        let mut d = ReportDoc::default();
        let put = |d: &mut ReportDoc, days_back: i64, db: f64| {
            let at = clock.moment(T - days_back * 86_400);
            let tenths = ((db + 200.0) * 10.0) as u64;
            d.count_station(&at, "DSC 2", keys::STATION_NOISE_SUM, tenths * 40);
            d.count_station(&at, "DSC 2", keys::STATION_NOISE_N, 40);
        };
        for back in 1..=4 {
            put(&mut d, back, -110.0);
        }
        put(&mut d, 0, -107.0);
        assert_eq!(
            d.noise_change("2026-11-24", "DSC 2"),
            None,
            "3 dB is normal"
        );
        put(&mut d, 0, -94.0); // the day's mean becomes -100.5
        let c = d.noise_change("2026-11-24", "DSC 2").unwrap();
        assert!((c - 9.5).abs() < 0.01, "{c}");
        assert_eq!(d.noise_change("2026-11-24", "other"), None);
    }
}
