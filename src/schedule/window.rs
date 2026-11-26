//! Weekly UTC time windows (pure): the time core shared by bookmarks and the
//! recording scheduler.
//!
//! A window is a UTC start and end (HHMM) plus the weekdays it runs. The
//! time-of-day rule is SDR++'s `bookmarkOnline`: `0000-0000` is all day, a
//! start later than the end wraps past midnight, the end minute is exclusive,
//! and `start == end != 0000` is empty. Bookmarks keep SDR++'s weekday rule
//! (the weekday of *now*); scheduled jobs use the weekday of the
//! occurrence's *start*, so a Monday 22:00-02:00 job runs until Tuesday 02:00
//! even when Tuesday is not checked.
//!
//! Times are seconds since the Unix epoch, so an occurrence has an identity
//! (its start second) that survives restarts and clock jumps: the scheduler
//! re-evaluates "is an occurrence open now?" on every tick (level-triggered),
//! never arms a timer.

use crate::bookmarks::model::DAY_LABELS;

/// Seconds per day.
const DAY: u64 = 86_400;
/// Minutes per day.
const DAY_MIN: i32 = 1_440;

/// Earliest plausible wall-clock time (2025-01-01T00:00Z). A Raspberry Pi has
/// no RTC: before NTP syncs the clock may be far in the past, and nothing
/// should fire on it.
pub const MIN_VALID_UNIX: u64 = 1_735_689_600;

/// Whether the system clock looks set (see [`MIN_VALID_UNIX`]).
pub fn clock_valid(now: u64) -> bool {
    now >= MIN_VALID_UNIX
}

/// Minutes since midnight of an HHMM value.
fn minutes(hhmm: u16) -> i32 {
    i32::from(hhmm / 100) * 60 + i32::from(hhmm % 100)
}

/// HHMM of minutes since midnight (taken modulo a day).
fn hhmm(min: i32) -> u16 {
    let m = min.rem_euclid(DAY_MIN);
    (m / 60 * 100 + m % 60) as u16
}

/// Weekday (0 = Sunday) of a Unix day number (1970-01-01 was a Thursday).
pub fn weekday_of_day(day: u64) -> usize {
    ((day + 4) % 7) as usize
}

/// Whether the time of day `t` (HHMM) is inside `[start, end)` with SDR++'s
/// rules: all day when both are 0000, wrapping past midnight when
/// start > end, never when start == end != 0000.
pub fn time_in_window(start: u16, end: u16, t: u16) -> bool {
    if start == 0 && end == 0 {
        true
    } else if start < end {
        start <= t && t < end
    } else if start > end {
        t >= start || t < end
    } else {
        false
    }
}

/// A weekly window: UTC start/end HHMM and the weekdays (index 0 = Sunday)
/// on which an occurrence *starts*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Window {
    pub start: u16,
    pub end: u16,
    pub days: [bool; 7],
}

impl Default for Window {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            days: [true; 7],
        }
    }
}

impl Window {
    pub fn new(start: u16, end: u16, days: [bool; 7]) -> Self {
        Self { start, end, days }
    }

    /// The first minute of this window (`start`..`start + 1 min`), same days:
    /// when a "Tune only" job fires.
    pub fn first_minute(&self) -> Window {
        Window {
            start: self.start,
            end: hhmm(minutes(self.start) + 1),
            days: self.days,
        }
    }

    /// `0000-0000`: the whole day.
    pub fn is_all_day(&self) -> bool {
        self.start == 0 && self.end == 0
    }

    /// Length of one occurrence in minutes (0 = empty window).
    pub fn duration_min(&self) -> i32 {
        let (s, e) = (minutes(self.start), minutes(self.end));
        if self.is_all_day() {
            DAY_MIN
        } else if s < e {
            e - s
        } else if s > e {
            DAY_MIN - s + e
        } else {
            0
        }
    }

    /// Start second of the occurrence that starts on Unix day `day`, if that
    /// weekday is checked and the window is not empty.
    fn start_on(&self, day: u64) -> Option<u64> {
        (self.days[weekday_of_day(day)] && self.duration_min() > 0)
            .then(|| day * DAY + minutes(self.start) as u64 * 60)
    }

    /// End second (exclusive) of the occurrence starting at `start`.
    pub fn end_of(&self, start: u64) -> u64 {
        start + self.duration_min() as u64 * 60
    }

    /// The start second of the occurrence open at `now`, if any.
    pub fn occurrence_at(&self, now: u64) -> Option<u64> {
        let today = now / DAY;
        // An occurrence lasts at most a day, so only one that started today
        // or yesterday can still be open.
        [today, today.saturating_sub(1)]
            .into_iter()
            .filter_map(|d| self.start_on(d))
            .find(|&s| s <= now && now < self.end_of(s))
    }

    /// The latest occurrence start at or before `now` (within a week).
    pub fn last_start_at_or_before(&self, now: u64) -> Option<u64> {
        let today = now / DAY;
        (today.saturating_sub(7)..=today)
            .rev()
            .filter_map(|d| self.start_on(d))
            .find(|&s| s <= now)
    }

    /// The first occurrence start strictly after `now` (within a week).
    pub fn next_start_after(&self, now: u64) -> Option<u64> {
        let today = now / DAY;
        (today..today + 8)
            .filter_map(|d| self.start_on(d))
            .find(|&s| s > now)
    }

    /// Whether some occurrence of `self` overlaps some occurrence of `other`
    /// in time (checked over two weeks, enough for weekly windows that last
    /// at most a day).
    pub fn overlaps(&self, other: &Window) -> bool {
        // Unix day 3 (1970-01-04) was a Sunday.
        let occurrences = |w: &Window| -> Vec<(u64, u64)> {
            (3..3 + 15)
                .filter_map(|d| w.start_on(d))
                .map(|s| (s, w.end_of(s)))
                .collect()
        };
        let a = occurrences(self);
        let b = occurrences(other);
        a.iter()
            .any(|&(s1, e1)| b.iter().any(|&(s2, e2)| s1 < e2 && s2 < e1))
    }

    /// The same window seen from a clock `offset_min` minutes ahead of UTC
    /// (e.g. local time): start and end move by the offset, and the weekdays
    /// rotate when the start crosses midnight. `shifted(-offset)` undoes it.
    /// An all-day window stays all day.
    pub fn shifted(&self, offset_min: i32) -> Window {
        if self.is_all_day() || offset_min == 0 {
            return *self;
        }
        let s = minutes(self.start) + offset_min;
        let day_shift = s.div_euclid(DAY_MIN);
        let mut days = [false; 7];
        for (i, on) in self.days.iter().enumerate() {
            days[(i as i32 + day_shift).rem_euclid(7) as usize] = *on;
        }
        Window {
            start: hhmm(s),
            end: hhmm(minutes(self.end) + offset_min),
            days,
        }
    }

    /// The weekdays as text: "" when every day, else "MoTuWe".
    pub fn days_label(&self) -> String {
        if self.days.iter().all(|d| *d) {
            return String::new();
        }
        DAY_LABELS
            .iter()
            .zip(self.days)
            .filter(|(_, on)| *on)
            .map(|(l, _)| *l)
            .collect()
    }

    /// "0600-0630" or "all day".
    pub fn time_label(&self) -> String {
        if self.is_all_day() {
            "all day".to_string()
        } else {
            format!("{:04}-{:04}", self.start, self.end)
        }
    }
}

/// The local clock's offset from UTC in minutes at `now` (e.g. +120 for
/// EET in summer), from the C library (`TZ` and `/etc/localtime`). 0 when
/// unknown.
pub fn local_offset_min(now: u64) -> i32 {
    let Ok(t) = libc::time_t::try_from(now) else {
        return 0;
    };
    // SAFETY: `tm` is plain data that `localtime_r` fills; both pointers are
    // valid for the duration of the call.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        (tm.tm_gmtoff / 60) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-09 (a Monday) 00:00 UTC.
    const MON: u64 = 1_794_182_400;
    const H: u64 = 3_600;
    const M: u64 = 60;

    fn days(on: &[usize]) -> [bool; 7] {
        let mut d = [false; 7];
        for &i in on {
            d[i] = true;
        }
        d
    }

    #[test]
    fn reference_monday_is_a_monday() {
        assert_eq!(weekday_of_day(MON / DAY), 1);
    }

    #[test]
    fn time_rule_matches_sdrpp_bookmark_online() {
        assert!(time_in_window(0, 0, 1234), "0000-0000 is all day");
        assert!(time_in_window(530, 630, 530));
        assert!(!time_in_window(530, 630, 630), "end exclusive");
        assert!(time_in_window(2200, 200, 2330), "wraps past midnight");
        assert!(time_in_window(2200, 200, 130));
        assert!(!time_in_window(2200, 200, 200));
        assert!(!time_in_window(900, 900, 900), "start == end != 0 is empty");
    }

    #[test]
    fn occurrence_is_open_inside_the_window_only() {
        let w = Window::new(600, 630, [true; 7]);
        assert_eq!(w.occurrence_at(MON + 6 * H), Some(MON + 6 * H));
        assert_eq!(w.occurrence_at(MON + 6 * H + 29 * M), Some(MON + 6 * H));
        assert_eq!(w.occurrence_at(MON + 6 * H + 30 * M), None, "end exclusive");
        assert_eq!(w.occurrence_at(MON + 5 * H + 59 * M), None);
    }

    #[test]
    fn unchecked_weekday_has_no_occurrence() {
        let w = Window::new(600, 630, days(&[2])); // Tuesday only
        assert_eq!(w.occurrence_at(MON + 6 * H), None, "Monday unchecked");
        let tue = MON + DAY + 6 * H;
        assert_eq!(w.occurrence_at(tue), Some(tue));
    }

    #[test]
    fn wrapping_occurrence_belongs_to_its_start_day() {
        // Monday 22:00 to Tuesday 02:00, only Monday checked: still open on
        // Tuesday at 01:00 (unlike the bookmark on-air rule).
        let w = Window::new(2200, 200, days(&[1]));
        let start = MON + 22 * H;
        assert_eq!(w.occurrence_at(MON + DAY + H), Some(start));
        assert_eq!(w.occurrence_at(MON + DAY + 2 * H), None);
        // Sunday was not checked, so Monday 01:00 is outside.
        assert_eq!(w.occurrence_at(MON + H), None);
    }

    #[test]
    fn all_day_runs_midnight_to_midnight_on_checked_days() {
        let w = Window::new(0, 0, days(&[1]));
        assert_eq!(w.duration_min(), 1_440);
        assert_eq!(w.occurrence_at(MON), Some(MON));
        assert_eq!(w.occurrence_at(MON + DAY - 1), Some(MON));
        assert_eq!(w.occurrence_at(MON + DAY), None);
    }

    #[test]
    fn empty_window_never_opens() {
        let w = Window::new(900, 900, [true; 7]);
        assert_eq!(w.duration_min(), 0);
        assert_eq!(w.occurrence_at(MON + 9 * H), None);
        assert_eq!(w.next_start_after(MON), None);
    }

    #[test]
    fn next_start_skips_unchecked_days_and_the_open_occurrence() {
        let w = Window::new(600, 630, days(&[3])); // Wednesday
        assert_eq!(w.next_start_after(MON), Some(MON + 2 * DAY + 6 * H));
        let daily = Window::new(600, 630, [true; 7]);
        assert_eq!(
            daily.next_start_after(MON + 6 * H),
            Some(MON + DAY + 6 * H),
            "strictly after now"
        );
    }

    #[test]
    fn overlap_considers_days_and_wrapping() {
        let a = Window::new(600, 700, [true; 7]);
        assert!(a.overlaps(&Window::new(630, 730, [true; 7])));
        assert!(!a.overlaps(&Window::new(700, 800, [true; 7])), "touching");
        assert!(!Window::new(600, 700, days(&[1])).overlaps(&Window::new(600, 700, days(&[2]))));
        // Monday 23:00-01:00 overlaps Tuesday 00:30-01:30.
        assert!(Window::new(2300, 100, days(&[1])).overlaps(&Window::new(30, 130, days(&[2]))));
        // Saturday 23:00-01:00 wraps into Sunday (week boundary).
        assert!(Window::new(2300, 100, days(&[6])).overlaps(&Window::new(30, 130, days(&[0]))));
    }

    #[test]
    fn shifting_to_local_time_rotates_days_across_midnight() {
        // 23:00 UTC Monday is 01:00 Tuesday at UTC+2.
        let utc = Window::new(2300, 2330, days(&[1]));
        let local = utc.shifted(120);
        assert_eq!(local, Window::new(100, 130, days(&[2])));
        assert_eq!(local.shifted(-120), utc, "round trip");
        // 01:00 UTC Sunday is 20:00 Saturday at UTC-5.
        let w = Window::new(100, 200, days(&[0]));
        assert_eq!(w.shifted(-300), Window::new(2000, 2100, days(&[6])));
        // All day stays all day.
        assert_eq!(Window::default().shifted(120), Window::default());
    }

    #[test]
    fn labels() {
        assert_eq!(Window::new(600, 630, [true; 7]).time_label(), "0600-0630");
        assert_eq!(Window::default().time_label(), "all day");
        assert_eq!(Window::new(600, 630, [true; 7]).days_label(), "");
        assert_eq!(Window::new(600, 630, days(&[1, 3])).days_label(), "MoWe");
    }

    #[test]
    fn clock_before_2025_is_not_trusted() {
        assert!(!clock_valid(0));
        assert!(!clock_valid(MIN_VALID_UNIX - 1));
        assert!(clock_valid(MON));
    }
}
