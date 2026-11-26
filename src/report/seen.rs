//! Distinct things heard (spec 27): HFDL planes seen and ground stations
//! received. A count of distinct things is not a sum, so they are kept
//! apart from the counters: one file per month,
//! `<profile>_seen_YYYY-MM.json`, each thing with a mask of the days of the
//! month it was heard on (bit 0 = the 1st). A day's count is the things
//! with its bit, a month's the things in its file, the lifetime's the
//! things in all of them.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::model::Moment;
use super::store::ReportFiles;

/// What is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeenKind {
    /// HFDL aircraft heard (ICAO address, else registration).
    Aircraft,
    /// HFDL ground stations heard (their ID).
    Station,
}

/// One month's file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeenMonth {
    pub aircraft: BTreeMap<String, u32>,
    pub stations: BTreeMap<String, u32>,
}

impl SeenMonth {
    fn map(&self, kind: SeenKind) -> &BTreeMap<String, u32> {
        match kind {
            SeenKind::Aircraft => &self.aircraft,
            SeenKind::Station => &self.stations,
        }
    }

    fn map_mut(&mut self, kind: SeenKind) -> &mut BTreeMap<String, u32> {
        match kind {
            SeenKind::Aircraft => &mut self.aircraft,
            SeenKind::Station => &mut self.stations,
        }
    }

    /// Add `other`'s days to this month's.
    fn absorb(&mut self, other: &SeenMonth) {
        for kind in [SeenKind::Aircraft, SeenKind::Station] {
            let m = self.map_mut(kind);
            for (id, days) in other.map(kind) {
                *m.entry(id.clone()).or_default() |= days;
            }
        }
    }
}

/// The day of the month of `2026-11-26` as a bit.
fn day_bit(day: &str) -> u32 {
    let d: u32 = day.get(8..10).and_then(|s| s.parse().ok()).unwrap_or(1);
    1 << (d.clamp(1, 31) - 1)
}

/// The current month in memory and the lifetime's things.
#[derive(Debug, Default)]
pub struct Seen {
    month: String,
    cur: SeenMonth,
    dirty: bool,
    lifetime: [HashSet<String>; 2],
}

fn slot(kind: SeenKind) -> usize {
    match kind {
        SeenKind::Aircraft => 0,
        SeenKind::Station => 1,
    }
}

impl Seen {
    /// Read every month of the profile (the lifetime) and `month`.
    pub fn open(files: &ReportFiles, month: &str) -> Self {
        let mut s = Self::default();
        for m in files.seen_months() {
            if let Ok(Some(f)) = files.load_seen(&m) {
                s.learn(&f);
                if m == month {
                    s.cur = f;
                }
            }
        }
        s.month = month.to_string();
        s
    }

    fn learn(&mut self, f: &SeenMonth) {
        for kind in [SeenKind::Aircraft, SeenKind::Station] {
            self.lifetime[slot(kind)].extend(f.map(kind).keys().cloned());
        }
    }

    /// `id` heard at `at`; whether it was new for that day.
    pub fn mark(&mut self, files: &ReportFiles, at: &Moment, kind: SeenKind, id: &str) -> bool {
        if at.month != self.month {
            // a new month: the old one to its file, then the new one's
            let _ = self.save(files);
            self.month = at.month.clone();
            self.cur = files
                .load_seen(&at.month)
                .ok()
                .flatten()
                .unwrap_or_default();
            self.dirty = false;
        }
        let bit = day_bit(&at.day);
        let days = self.cur.map_mut(kind).entry(id.to_string()).or_default();
        if *days & bit != 0 {
            return false;
        }
        *days |= bit;
        self.dirty = true;
        self.lifetime[slot(kind)].insert(id.to_string());
        true
    }

    /// Things heard on `day` (of the current month).
    pub fn day_count(&self, kind: SeenKind, day: &str) -> u64 {
        let bit = day_bit(day);
        self.cur
            .map(kind)
            .values()
            .filter(|d| *d & bit != 0)
            .count() as u64
    }

    pub fn month_count(&self, kind: SeenKind) -> u64 {
        self.cur.map(kind).len() as u64
    }

    pub fn lifetime_count(&self, kind: SeenKind) -> u64 {
        self.lifetime[slot(kind)].len() as u64
    }

    /// Write the month when it changed, merged with what its file holds
    /// (another instance, or what was written before the folder failed).
    pub fn save(&mut self, files: &ReportFiles) -> Result<(), String> {
        if !self.dirty || self.month.is_empty() {
            return Ok(());
        }
        if let Some(disk) = files.load_seen(&self.month)? {
            self.cur.absorb(&disk);
            self.learn(&disk);
        }
        files.save_seen(&self.month, &self.cur)?;
        self.dirty = false;
        Ok(())
    }

    /// Forget the lifetime and read it again (after months were deleted
    /// or the folder changed).
    pub fn reopen(&mut self, files: &ReportFiles) {
        let _ = self.save(files);
        let month = std::mem::take(&mut self.month);
        *self = Self::open(files, &month);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: &str) -> Moment {
        Moment {
            day: day.to_string(),
            month: day[..7].to_string(),
            hour: 12,
        }
    }

    fn tmp(name: &str) -> ReportFiles {
        let d = std::env::temp_dir().join(format!("shellsdr-seen-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        ReportFiles::new(d, "p")
    }

    #[test]
    fn days_months_and_the_lifetime_count_each_thing_once() {
        let files = tmp("count");
        let mut s = Seen::open(&files, "2026-11");
        let (a, st) = (SeenKind::Aircraft, SeenKind::Station);
        assert!(s.mark(&files, &at("2026-11-25"), a, "A1B2C3"));
        assert!(!s.mark(&files, &at("2026-11-25"), a, "A1B2C3"));
        assert!(s.mark(&files, &at("2026-11-26"), a, "A1B2C3"));
        assert!(s.mark(&files, &at("2026-11-26"), a, "4CA123"));
        assert!(s.mark(&files, &at("2026-11-26"), st, "7"));
        assert_eq!(s.day_count(a, "2026-11-25"), 1);
        assert_eq!(s.day_count(a, "2026-11-26"), 2);
        assert_eq!(s.month_count(a), 2);
        assert_eq!(s.month_count(st), 1);
        // a new month: its own file; the lifetime keeps both
        assert!(s.mark(&files, &at("2026-12-01"), a, "A1B2C3"));
        assert!(s.mark(&files, &at("2026-12-01"), a, "ABCDEF"));
        assert_eq!(s.month_count(a), 2);
        assert_eq!(s.lifetime_count(a), 3);
        s.save(&files).unwrap();
        // read back
        let s2 = Seen::open(&files, "2026-12");
        assert_eq!(s2.lifetime_count(a), 3);
        assert_eq!(s2.month_count(a), 2);
        assert_eq!(s2.day_count(a, "2026-12-01"), 2);
        let nov = files.load_seen("2026-11").unwrap().unwrap();
        assert_eq!(nov.aircraft["A1B2C3"], (1 << 24) | (1 << 25));
        let _ = std::fs::remove_dir_all(&files.dir);
    }

    #[test]
    fn saving_merges_with_the_file() {
        let files = tmp("merge");
        let mut one = Seen::open(&files, "2026-11");
        let mut two = Seen::open(&files, "2026-11");
        one.mark(&files, &at("2026-11-26"), SeenKind::Aircraft, "AAAAAA");
        two.mark(&files, &at("2026-11-26"), SeenKind::Aircraft, "BBBBBB");
        one.save(&files).unwrap();
        two.save(&files).unwrap();
        let f = files.load_seen("2026-11").unwrap().unwrap();
        assert_eq!(f.aircraft.len(), 2);
        assert_eq!(two.month_count(SeenKind::Aircraft), 2);
        let _ = std::fs::remove_dir_all(&files.dir);
    }
}
