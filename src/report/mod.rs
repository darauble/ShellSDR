//! The Report (0.3.0, spec 20): what ShellSDR did, for mostly unattended
//! operation. Counters per day and month (lifetime = the months summed),
//! the event log (today in memory, every day in a file), kept in the
//! report folder with the profile's prefix. Writing never blocks the app:
//! a folder that cannot be written (a NAS down) keeps the counts and lines
//! in memory and is retried every minute.

pub mod health;
pub mod hook;
pub mod markdown;
pub mod model;
pub mod seen;
pub mod store;

use serde::{Deserialize, Serialize};

use model::{Category, Clock, Event, Level, Period, ReportDoc, keys};
use seen::{Seen, SeenKind};
use store::ReportFiles;

/// The Report's settings (profile TOML, `[report]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportConfig {
    /// The report folder (`~` allowed).
    pub dir: String,
    /// Days and log times in local time (else UTC).
    pub local_time: bool,
    /// Daily counters kept (0 = forever).
    pub keep_days: u32,
    /// Log files kept, in days (0 = forever).
    pub keep_log_days: u32,
    /// Monthly counters kept (0 = forever).
    pub keep_months: u32,
    /// The event hook: a shell command (empty = off), and which events it
    /// gets (spec 20.6).
    pub hook: String,
    pub hook_errors: bool,
    pub hook_warnings: bool,
    pub hook_dsc: bool,
    pub hook_hfdl: bool,
    pub hook_health: bool,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            dir: "~/.local/state/ShellSDR".to_string(),
            local_time: true,
            keep_days: 400,
            keep_log_days: 90,
            keep_months: 0,
            hook: String::new(),
            hook_errors: true,
            hook_warnings: false,
            hook_dsc: true,
            hook_hfdl: true,
            hook_health: true,
        }
    }
}

impl ReportConfig {
    pub fn hook_filter(&self) -> hook::HookFilter {
        hook::HookFilter {
            errors: self.hook_errors,
            warnings: self.hook_warnings,
            dsc: self.hook_dsc,
            hfdl: self.hook_hfdl,
            health: self.hook_health,
        }
    }
}

/// Events of today kept in memory (the Log tab).
const MAX_TODAY: usize = 20_000;
/// Seconds between saves of the counters (and retries of a lost folder).
pub const SAVE_EVERY_S: i64 = 60;
/// A longer gap between two ticks (a suspend, a clock jump) is not counted.
const MAX_TICK_GAP_S: i64 = 120;

/// What the app is doing at a tick (for the time counters).
#[derive(Debug, Clone, PartialEq)]
pub struct TickInput {
    pub now: i64,
    pub offset_min: i32,
    pub source_running: bool,
    pub recording: bool,
    pub vfo_type: String,
    /// The station tuned (a bookmark's name, or `(manual) 7040 kHz`).
    pub station: String,
}

/// How the report goes to a new folder (spec 20.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relocate {
    /// Copy this profile's files there, check them, delete the old ones.
    Move,
    /// Leave the old folder alone; start counting in the new one (or carry
    /// on with this profile's report already there).
    Fresh,
}

/// The Report service.
#[derive(Debug)]
pub struct Report {
    pub config: ReportConfig,
    files: ReportFiles,
    doc: ReportDoc,
    /// Today's events (the day of `today_key`), oldest first.
    today: Vec<Event>,
    today_key: String,
    /// Log lines not written yet: (day, line).
    pending: Vec<(String, String)>,
    last_tick: Option<i64>,
    last_save: i64,
    dirty: bool,
    /// Why the folder cannot be written (cleared when it works again).
    error: Option<String>,
    /// The folder's failure was logged (once per outage).
    noted: bool,
    /// The folder could not be read at start: what is in memory is only
    /// what was counted since, to be merged into the file once it can be
    /// read.
    unloaded: bool,
    /// The file needs a newer build (or is malformed): never overwritten.
    read_only: bool,
    pub started: i64,
    hook: hook::HookRunner,
    /// HFDL planes seen and ground stations received (spec 27).
    seen: Seen,
}

impl Report {
    /// Open the profile's report in `dir` at `now`: count the start, note an
    /// unclean exit, read today's log back, prune what is too old.
    pub fn open(config: ReportConfig, files: ReportFiles, now: i64, offset_min: i32) -> Self {
        let fresh = ReportDoc {
            first_start: now,
            ..ReportDoc::default()
        };
        let (doc, error, read_only, unloaded) = match files.load() {
            Ok(Some(doc)) => {
                let newer = matches!(
                    crate::version::classify(doc.min_version.as_deref()),
                    crate::version::FileVersion::Newer(_)
                );
                (doc, None, newer, false)
            }
            Ok(None) => (fresh, None, false, false),
            // Malformed: never overwritten; count in memory.
            Err(e) if e.starts_with("malformed") => (fresh, Some(e), true, false),
            // The folder cannot be read (a NAS down): count in memory and
            // merge into the file when it can be read.
            Err(e) => (fresh, Some(e), false, true),
        };
        let clock = Clock {
            local: config.local_time,
            offset_min,
        };
        let today_key = clock.moment(now).day;
        let today = files.read_log(&today_key);
        let seen = Seen::open(&files, &clock.moment(now).month);
        let mut r = Self {
            config,
            files,
            doc,
            today,
            today_key,
            pending: Vec::new(),
            last_tick: Some(now),
            last_save: now,
            dirty: true,
            error: None,
            noted: false,
            unloaded,
            read_only,
            started: now,
            hook: hook::HookRunner::default(),
            seen,
        };
        if r.doc.running {
            let seen = if r.doc.last_seen > 0 {
                format!(" (last seen {})", clock.stamp(r.doc.last_seen))
            } else {
                String::new()
            };
            r.count(now, offset_min, keys::APP_UNCLEAN, 1);
            r.event(
                now,
                offset_min,
                Level::Warning,
                Category::App,
                format!("ShellSDR did not quit cleanly{seen}"),
                None,
            );
        }
        r.doc.running = true;
        r.count(now, offset_min, keys::APP_STARTS, 1);
        r.event(
            now,
            offset_min,
            Level::Info,
            Category::App,
            format!(
                "Started ShellSDR {}, profile {}",
                crate::version::VERSION,
                r.files.profile
            ),
            None,
        );
        if let Some(e) = error {
            if r.read_only {
                r.event(now, offset_min, Level::Error, Category::App, e, None);
            } else {
                r.error = Some(e);
            }
        }
        r.prune(now, offset_min);
        if r.error.is_none() && !r.unloaded {
            r.catch_up_day_report(now, offset_min);
        }
        r.save(now);
        r
    }

    pub fn clock(&self, offset_min: i32) -> Clock {
        Clock {
            local: self.config.local_time,
            offset_min,
        }
    }

    pub fn doc(&self) -> &ReportDoc {
        &self.doc
    }

    pub fn files(&self) -> &ReportFiles {
        &self.files
    }

    /// Today's events, oldest first.
    pub fn today(&self) -> &[Event] {
        &self.today
    }

    pub fn today_key(&self) -> &str {
        &self.today_key
    }

    /// Why the folder cannot be written, if it cannot.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// The newest warning or error of today.
    pub fn last_problem(&self) -> Option<&Event> {
        self.today.iter().rev().find(|e| e.level >= Level::Warning)
    }

    /// Log an event (written to the day's file at once when possible) and
    /// hand it to the hook when it is one the hook wants.
    pub fn event(
        &mut self,
        now: i64,
        offset_min: i32,
        level: Level,
        category: Category,
        text: String,
        file: Option<String>,
    ) {
        self.log(now, offset_min, level, category, text, file, true);
    }

    #[allow(clippy::too_many_arguments)]
    fn log(
        &mut self,
        now: i64,
        offset_min: i32,
        level: Level,
        category: Category,
        text: String,
        file: Option<String>,
        hook: bool,
    ) {
        let clock = self.clock(offset_min);
        let day = clock.moment(now).day;
        if day != self.today_key {
            self.today.clear();
            self.today_key = day.clone();
        }
        let e = Event {
            time: now,
            level,
            category,
            text,
            file,
        };
        self.pending.push((day, e.line(&clock)));
        if hook && !self.config.hook.trim().is_empty() && self.config.hook_filter().wants(&e) {
            let stamp = clock.stamp(e.time);
            self.hook
                .run(&self.config.hook, &self.files.profile, &e, &stamp);
        }
        self.today.push(e);
        if self.today.len() > MAX_TODAY {
            self.today.drain(..self.today.len() - MAX_TODAY);
        }
        match level {
            Level::Warning => self.count(now, offset_min, keys::WARNINGS, 1),
            Level::Error => self.count(now, offset_min, keys::ERRORS, 1),
            Level::Info => {}
        }
        if self.error.is_none() {
            self.flush_log();
            self.note_folder(now);
        }
    }

    /// Add `n` to counter `key` at `now`.
    pub fn count(&mut self, now: i64, offset_min: i32, key: &str, n: u64) {
        if n > 0 {
            let at = self.clock(offset_min).moment(now);
            self.doc.count(&at, key, n);
            self.dirty = true;
        }
    }

    /// Keep the day's and month's highest `v` of `key`.
    pub fn maximum(&mut self, now: i64, offset_min: i32, key: &str, v: u64) {
        let at = self.clock(offset_min).moment(now);
        self.doc.maximum(&at, key, v);
        self.dirty = true;
    }

    /// Add `n` to `station`'s counter `key` at `now`.
    pub fn count_station(&mut self, now: i64, offset_min: i32, station: &str, key: &str, n: u64) {
        if n > 0 {
            let at = self.clock(offset_min).moment(now);
            self.doc.count_station(&at, station, key, n);
            self.dirty = true;
        }
    }

    /// Every second or so: count the time since the last tick, save every
    /// minute (retrying a lost folder), prune at a new day.
    /// Things heard at `now` (spec 27): the day's and the month's
    /// distinct counts go to the counters as maxima (they only grow).
    pub fn seen(&mut self, now: i64, offset_min: i32, kind: SeenKind, ids: &[String]) {
        let at = self.clock(offset_min).moment(now);
        let mut new = false;
        for id in ids {
            new |= self.seen.mark(&self.files, &at, kind, id);
        }
        if new {
            let key = match kind {
                SeenKind::Aircraft => keys::HFDL_AIRCRAFT,
                SeenKind::Station => keys::HFDL_STATIONS,
            };
            let (day, month) = (
                self.seen.day_count(kind, &at.day),
                self.seen.month_count(kind),
            );
            self.doc.maximum_apart(&at, key, day, month);
            self.dirty = true;
        }
    }

    /// Everything counted: the months summed, with the distinct things
    /// heard over all months.
    pub fn lifetime(&self) -> Period {
        let mut p = self.doc.lifetime();
        for (key, kind) in [
            (keys::HFDL_AIRCRAFT, SeenKind::Aircraft),
            (keys::HFDL_STATIONS, SeenKind::Station),
        ] {
            let n = self.seen.lifetime_count(kind);
            if n > 0 {
                p.maxima.insert(key.to_string(), n);
            }
        }
        p
    }

    pub fn tick(&mut self, t: &TickInput) {
        let gap = self.last_tick.map_or(0, |l| t.now - l);
        self.last_tick = Some(t.now);
        if (1..=MAX_TICK_GAP_S).contains(&gap) {
            let n = gap as u64;
            let (now, off) = (t.now, t.offset_min);
            self.count(now, off, keys::TIME_APP, n);
            if t.source_running {
                self.count(now, off, keys::TIME_SOURCE, n);
                self.count(now, off, &format!("{}{}", keys::TIME_TYPE, t.vfo_type), n);
                self.count_station(now, off, &t.station, keys::STATION_TIME, n);
            }
            if t.recording {
                self.count(now, off, keys::TIME_REC, n);
            }
        }
        let (problems, held) = self.hook.take_problems();
        for p in problems {
            self.log(
                t.now,
                t.offset_min,
                Level::Warning,
                Category::App,
                p,
                None,
                false,
            );
        }
        if held > 0 {
            self.log(
                t.now,
                t.offset_min,
                Level::Warning,
                Category::App,
                format!(
                    "Hook: {held} events not sent (more than {} a minute)",
                    hook::HOOK_PER_MINUTE
                ),
                None,
                false,
            );
        }
        let day = self.clock(t.offset_min).moment(t.now).day;
        if day != self.today_key {
            // Midnight: the finished day as Markdown (spec 20.6).
            let done = std::mem::take(&mut self.today);
            let prev = std::mem::replace(&mut self.today_key, day);
            self.write_day_report(&prev, &done, t.offset_min);
            self.prune(t.now, t.offset_min);
        }
        if t.now - self.last_save >= SAVE_EVERY_S {
            self.save(t.now);
        }
    }

    /// Write `day`'s Markdown report (its events `events`); a folder that
    /// cannot be written just misses it.
    fn write_day_report(&mut self, day: &str, events: &[Event], offset_min: i32) {
        let Some(p) = self.doc.day(day) else {
            return;
        };
        let md = markdown::day_report(&self.files.profile, day, p, events, &self.clock(offset_min));
        let path = self.files.markdown_path(day);
        let _ = crate::bookmarks::model::atomic_write(&path, &md);
    }

    /// At a start: yesterday's report when it was not written (ShellSDR did
    /// not run at midnight).
    fn catch_up_day_report(&mut self, now: i64, offset_min: i32) {
        let today = self.clock(offset_min).moment(now).day;
        let Some(yesterday) = model::day_minus(&today, 1) else {
            return;
        };
        if self.doc.day(&yesterday).is_some() && !self.files.markdown_path(&yesterday).exists() {
            let events = self.files.read_log(&yesterday);
            self.write_day_report(&yesterday, &events, offset_min);
        }
    }

    /// Run the hook once with a test event (Settings), whatever it filters.
    pub fn test_hook(&mut self, now: i64, offset_min: i32) {
        let clock = self.clock(offset_min);
        let e = Event {
            time: now,
            level: Level::Info,
            category: Category::App,
            text: "ShellSDR hook test".to_string(),
            file: None,
        };
        let stamp = clock.stamp(now);
        self.hook
            .run(&self.config.hook, &self.files.profile, &e, &stamp);
        self.log(
            now,
            offset_min,
            Level::Info,
            Category::App,
            "Hook test sent".into(),
            None,
            false,
        );
    }

    /// Drop counters and logs older than the settings keep.
    fn prune(&mut self, now: i64, offset_min: i32) {
        let today = self.clock(offset_min).moment(now).day;
        self.doc
            .prune(&today, self.config.keep_days, self.config.keep_months);
        if self.config.keep_months > 0
            && self.error.is_none()
            && let Some(first) = model::month_minus(&today[..7], self.config.keep_months)
            && self.files.prune_seen(&first) > 0
        {
            self.seen.reopen(&self.files);
        }
        if self.config.keep_log_days > 0
            && let Some(first) = model::day_minus(&today, self.config.keep_log_days)
            && self.error.is_none()
        {
            self.files.prune_logs(&first);
        }
    }

    /// Write the pending log lines; keep them when the folder fails.
    fn flush_log(&mut self) {
        while let Some((day, _)) = self.pending.first() {
            let day = day.clone();
            let n = self.pending.iter().take_while(|(d, _)| *d == day).count();
            let lines: Vec<String> = self.pending[..n].iter().map(|(_, l)| l.clone()).collect();
            match self.files.append(&day, &lines) {
                Ok(()) => {
                    self.pending.drain(..n);
                }
                Err(e) => {
                    self.error = Some(e);
                    return;
                }
            }
        }
    }

    /// Write the counters and the pending log lines (a lost folder is
    /// retried; each outage is logged once, and its end).
    pub fn save(&mut self, now: i64) {
        self.last_save = now;
        self.error = None;
        if self.unloaded {
            // The folder can be read again: merge into what it holds.
            match self.files.load() {
                Ok(found) => {
                    if let Some(mut disk) = found {
                        disk.absorb(&self.doc);
                        disk.running = true;
                        self.doc = disk;
                    }
                    self.unloaded = false;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if self.error.is_none() {
            self.flush_log();
        }
        if self.error.is_none()
            && let Err(e) = self.seen.save(&self.files)
        {
            self.error = Some(e);
        }
        if self.error.is_none() && !self.unloaded && self.dirty && !self.read_only {
            self.doc.last_seen = now;
            match self.files.save(&self.doc) {
                Ok(()) => self.dirty = false,
                Err(e) => self.error = Some(e),
            }
        }
        self.note_folder(now);
    }

    /// Log the folder's outage once, and its end.
    fn note_folder(&mut self, now: i64) {
        match (self.error.clone(), self.noted) {
            (Some(e), false) => {
                self.noted = true;
                // Into memory and the backlog: the folder is what failed.
                let clock = self.clock(0);
                let ev = Event {
                    time: now,
                    level: Level::Error,
                    category: Category::App,
                    text: format!("Report folder not writable, counting in memory: {e}"),
                    file: None,
                };
                self.pending.push((self.today_key.clone(), ev.line(&clock)));
                self.today.push(ev);
                self.count(now, 0, keys::ERRORS, 1);
            }
            (None, true) => {
                self.noted = false;
                self.event(
                    now,
                    0,
                    Level::Info,
                    Category::App,
                    "Report folder writable again".to_string(),
                    None,
                );
            }
            _ => {}
        }
    }

    /// Go to another folder: move this profile's files there, or start
    /// fresh (spec 20.5). On a failed move nothing changes.
    pub fn relocate(
        &mut self,
        to: ReportFiles,
        how: Relocate,
        now: i64,
        offset_min: i32,
    ) -> Result<(), String> {
        if to.dir == self.files.dir {
            return Ok(());
        }
        let from = self.files.dir.display().to_string();
        let dest = to.dir.display().to_string();
        match how {
            Relocate::Move => {
                // Everything written first, so the copies are complete.
                self.dirty = true;
                self.save(now);
                if let Some(e) = &self.error {
                    return Err(format!("the old folder cannot be written: {e}"));
                }
                if !to.profile_files().is_empty() {
                    return Err(format!(
                        "{dest} already has reports of profile {}: choose Start fresh to carry on with them",
                        to.profile
                    ));
                }
                std::fs::create_dir_all(&to.dir).map_err(|e| format!("{dest}: {e}"))?;
                let files = self.files.profile_files();
                let mut copied = Vec::new();
                for f in &files {
                    let name = f.file_name().expect("a file");
                    let target = to.dir.join(name);
                    let ok =
                        std::fs::copy(f, &target).map_err(|e| format!("{}: {e}", target.display()));
                    let same = ok
                        .as_ref()
                        .is_ok_and(|n| std::fs::metadata(f).is_ok_and(|m| m.len() == *n));
                    if !same {
                        // Undo the copies: the old folder stays as it was.
                        for c in &copied {
                            let _ = std::fs::remove_file(c);
                        }
                        let _ = std::fs::remove_file(&target);
                        return Err(ok.err().unwrap_or_else(|| {
                            format!("{}: the copy is incomplete", target.display())
                        }));
                    }
                    copied.push(target);
                }
                for f in &files {
                    let _ = std::fs::remove_file(f);
                }
                self.files = to;
                self.seen.reopen(&self.files);
                self.event(
                    now,
                    offset_min,
                    Level::Info,
                    Category::App,
                    format!("Report moved from {from} to {dest} ({} files)", files.len()),
                    None,
                );
            }
            Relocate::Fresh => {
                self.save(now);
                self.files = to;
                let month = self.clock(offset_min).moment(now).month;
                self.seen = Seen::open(&self.files, &month);
                // Count from zero; a report of this profile already in the
                // new folder is carried on (merged at the next save).
                self.doc = ReportDoc {
                    first_start: now,
                    running: true,
                    ..ReportDoc::default()
                };
                self.unloaded = true;
                self.read_only = false;
                self.today = self.files.read_log(&self.today_key);
                self.pending.clear();
                self.error = None;
                self.noted = false;
                self.event(
                    now,
                    offset_min,
                    Level::Info,
                    Category::App,
                    format!("Report started in {dest} (the old reports stay in {from})"),
                    None,
                );
            }
        }
        self.dirty = true;
        self.save(now);
        Ok(())
    }

    /// New retention settings: applied at once.
    pub fn set_retention(
        &mut self,
        keep_days: u32,
        keep_log_days: u32,
        keep_months: u32,
        now: i64,
        offset_min: i32,
    ) {
        self.config.keep_days = keep_days;
        self.config.keep_log_days = keep_log_days;
        self.config.keep_months = keep_months;
        self.prune(now, offset_min);
        self.dirty = true;
    }

    /// A clean quit: the counters say so.
    pub fn close(&mut self, now: i64, offset_min: i32) {
        self.event(
            now,
            offset_min,
            Level::Info,
            Category::App,
            "Quit".to_string(),
            None,
        );
        self.doc.running = false;
        self.dirty = true;
        self.save(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_795_543_452; // 2026-11-24 18:04:12 UTC

    #[test]
    fn planes_seen_count_once_a_day_a_month_and_a_lifetime() {
        let dir = temp("seen");
        let files = || ReportFiles::new(&dir, "p");
        let mut r = Report::open(ReportConfig::default(), files(), T, 0);
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        r.seen(T, 0, SeenKind::Aircraft, &ids(&["AAA111", "BBB222"]));
        r.seen(T + 60, 0, SeenKind::Aircraft, &ids(&["AAA111"]));
        r.seen(T, 0, SeenKind::Station, &ids(&["7"]));
        let day = r.doc().day("2026-11-24").unwrap();
        assert_eq!(day.max_of(keys::HFDL_AIRCRAFT), Some(2));
        assert_eq!(day.max_of(keys::HFDL_STATIONS), Some(1));
        // the next day: AAA111 again is new for the day, not for the month
        let next = T + 86_400;
        r.seen(next, 0, SeenKind::Aircraft, &ids(&["AAA111", "CCC333"]));
        assert_eq!(
            r.doc()
                .day("2026-11-25")
                .unwrap()
                .max_of(keys::HFDL_AIRCRAFT),
            Some(2)
        );
        assert_eq!(
            r.doc()
                .month("2026-11")
                .unwrap()
                .max_of(keys::HFDL_AIRCRAFT),
            Some(3)
        );
        // the next month, then a restart: the lifetime is the union
        let dec = T + 8 * 86_400;
        r.seen(dec, 0, SeenKind::Aircraft, &ids(&["AAA111", "DDD444"]));
        assert_eq!(
            r.doc()
                .month("2026-12")
                .unwrap()
                .max_of(keys::HFDL_AIRCRAFT),
            Some(2)
        );
        assert_eq!(r.lifetime().max_of(keys::HFDL_AIRCRAFT), Some(4));
        r.close(dec, 0);
        let mut r = Report::open(ReportConfig::default(), files(), dec + 60, 0);
        assert_eq!(r.lifetime().max_of(keys::HFDL_AIRCRAFT), Some(4));
        r.seen(dec + 60, 0, SeenKind::Aircraft, &ids(&["DDD444"]));
        assert_eq!(
            r.doc()
                .day("2026-12-02")
                .unwrap()
                .max_of(keys::HFDL_AIRCRAFT),
            Some(2)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("shellsdr-rep-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn input(now: i64) -> TickInput {
        TickInput {
            now,
            offset_min: 120,
            source_running: true,
            recording: false,
            vfo_type: "DSC".into(),
            station: "DSC 2".into(),
        }
    }

    #[test]
    fn a_run_counts_its_start_time_and_events_and_a_clean_quit_is_remembered() {
        let d = temp("run");
        let files = ReportFiles::new(&d, "default");
        let mut r = Report::open(ReportConfig::default(), files.clone(), T, 120);
        for s in 1..=90 {
            r.tick(&input(T + s));
        }
        r.event(
            T + 90,
            120,
            Level::Warning,
            Category::Source,
            "lost".into(),
            None,
        );
        r.close(T + 91, 120);
        let doc = files.load().unwrap().unwrap();
        assert!(!doc.running);
        let day = doc.day("2026-11-24").unwrap();
        assert_eq!(day.get(keys::APP_STARTS), 1);
        assert_eq!(day.get(keys::TIME_APP), 90);
        assert_eq!(day.get(keys::TIME_SOURCE), 90);
        assert_eq!(day.get("time.type.DSC"), 90);
        assert_eq!(day.stations["DSC 2"][keys::STATION_TIME], 90);
        assert_eq!(day.get(keys::WARNINGS), 1);
        let log = files.read_log("2026-11-24");
        assert_eq!(log.len(), 3, "start, the warning, quit: {log:?}");
        assert!(log[0].text.starts_with("Started ShellSDR"));
        // The next start finds a clean quit and today's log in memory.
        let r2 = Report::open(ReportConfig::default(), files.clone(), T + 100, 120);
        assert_eq!(r2.today().len(), 4);
        assert_eq!(
            r2.doc().day("2026-11-24").unwrap().get(keys::APP_UNCLEAN),
            0
        );
        assert_eq!(r2.last_problem().unwrap().text, "lost");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn an_unclean_exit_is_found_at_the_next_start() {
        let d = temp("crash");
        let files = ReportFiles::new(&d, "default");
        let mut r = Report::open(ReportConfig::default(), files.clone(), T, 0);
        r.save(T + 60);
        drop(r); // no close: a crash
        let r = Report::open(ReportConfig::default(), files, T + 600, 0);
        assert_eq!(r.doc().day("2026-11-24").unwrap().get(keys::APP_UNCLEAN), 1);
        let p = r.last_problem().unwrap();
        assert!(
            p.text.contains("did not quit cleanly (last seen"),
            "{}",
            p.text
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_folder_that_cannot_be_written_keeps_counting_and_catches_up() {
        let d = temp("nas");
        // A file where the folder should be: nothing can be written there.
        std::fs::write(&d, "not a folder").unwrap();
        let files = ReportFiles::new(d.join("sub"), "default");
        let mut r = Report::open(ReportConfig::default(), files.clone(), T, 0);
        assert!(r.error().is_some(), "the folder fails");
        r.event(T + 1, 0, Level::Info, Category::User, "tuned".into(), None);
        r.tick(&input(T + 2));
        assert!(
            r.today()
                .iter()
                .any(|e| e.text.starts_with("Report folder not writable"))
        );
        // The NAS comes back.
        std::fs::remove_file(&d).unwrap();
        r.save(T + 61);
        assert!(r.error().is_none());
        let log = files.read_log("2026-11-24");
        assert!(
            log.iter().any(|e| e.text == "tuned"),
            "the backlog is written: {log:?}"
        );
        assert!(log.iter().any(|e| e.text == "Report folder writable again"));
        assert!(files.load().unwrap().is_some());
        assert!(
            log.iter().any(|e| e.text.starts_with("Started")),
            "the start too"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn gaps_are_not_counted_and_a_new_day_starts_an_empty_today() {
        let d = temp("gap");
        let files = ReportFiles::new(&d, "default");
        let cfg = ReportConfig {
            local_time: false,
            ..ReportConfig::default()
        };
        let mut r = Report::open(cfg, files, T, 0);
        r.tick(&input(T + 1));
        r.tick(&input(T + 3601)); // a suspend
        assert_eq!(r.doc().day("2026-11-24").unwrap().get(keys::TIME_APP), 1);
        r.tick(&input(T + 3602));
        assert_eq!(r.doc().day("2026-11-24").unwrap().get(keys::TIME_APP), 2);
        let next_day = T + 6 * 3600;
        r.tick(&input(next_day));
        assert_eq!(r.today_key(), "2026-11-25");
        assert!(r.today().is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn counts_made_while_the_folder_was_away_are_added_to_the_file() {
        let d = temp("merge");
        let files = ReportFiles::new(&d, "default");
        // An earlier run counted 5 messages.
        let mut r = Report::open(ReportConfig::default(), files.clone(), T, 0);
        r.count(T, 0, keys::DSC_MESSAGES, 5);
        r.close(T + 1, 0);
        // The folder is away at the next start (made unreadable).
        let hidden = d.with_extension("away");
        std::fs::rename(&d, &hidden).unwrap();
        std::fs::write(&d, "not a folder").unwrap();
        let mut r = Report::open(ReportConfig::default(), files.clone(), T + 10, 0);
        r.count(T + 11, 0, keys::DSC_MESSAGES, 2);
        r.save(T + 70);
        assert!(r.error().is_some());
        std::fs::remove_file(&d).unwrap();
        std::fs::rename(&hidden, &d).unwrap();
        r.save(T + 130);
        assert!(r.error().is_none());
        let day = files.load().unwrap().unwrap();
        let day = day.day("2026-11-24").unwrap();
        assert_eq!(day.get(keys::DSC_MESSAGES), 7, "merged, not overwritten");
        assert_eq!(day.get(keys::APP_STARTS), 2);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn moving_takes_this_profiles_files_and_fresh_leaves_them() {
        let d = temp("move");
        let a = ReportFiles::new(d.join("a"), "default");
        let other = ReportFiles::new(d.join("a"), "tuner2");
        other.append("2026-11-24", &["x".into()]).unwrap();
        let b = ReportFiles::new(d.join("b"), "default");
        let mut r = Report::open(ReportConfig::default(), a.clone(), T, 0);
        r.count(T, 0, keys::DSC_MESSAGES, 3);
        r.relocate(b.clone(), Relocate::Move, T + 5, 0).unwrap();
        assert!(a.profile_files().is_empty(), "the old files are gone");
        assert_eq!(other.profile_files().len(), 1, "another profile's stay");
        assert_eq!(
            b.load()
                .unwrap()
                .unwrap()
                .day("2026-11-24")
                .unwrap()
                .get(keys::DSC_MESSAGES),
            3
        );
        assert!(
            b.read_log("2026-11-24")
                .iter()
                .any(|e| e.text.starts_with("Report moved"))
        );
        // Moving back onto a folder that has reports of the profile: refused.
        a.save(&ReportDoc::default()).unwrap();
        assert!(r.relocate(a.clone(), Relocate::Move, T + 6, 0).is_err());
        assert_eq!(r.files().dir, d.join("b"));
        // Start fresh somewhere new: zero, the old folder untouched.
        let c = ReportFiles::new(d.join("c"), "default");
        r.relocate(c.clone(), Relocate::Fresh, T + 7, 0).unwrap();
        assert_eq!(
            r.doc()
                .day("2026-11-24")
                .map_or(0, |p| p.get(keys::DSC_MESSAGES)),
            0
        );
        assert!(b.load().unwrap().is_some(), "the old reports stay");
        assert!(c.load().unwrap().is_some());
        // Fresh onto a folder with this profile's report carries it on.
        r.count(T + 8, 0, keys::DSC_MESSAGES, 1);
        r.relocate(b.clone(), Relocate::Fresh, T + 9, 0).unwrap();
        r.count(T + 10, 0, keys::DSC_MESSAGES, 1);
        r.save(T + 70);
        let doc = b.load().unwrap().unwrap();
        assert_eq!(
            doc.day("2026-11-24").unwrap().get(keys::DSC_MESSAGES),
            4,
            "3 there + 1 here"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn midnight_writes_the_day_and_a_missed_one_is_written_at_the_next_start() {
        let d = temp("midnight");
        let files = ReportFiles::new(&d, "default");
        let cfg = ReportConfig {
            local_time: false,
            ..ReportConfig::default()
        };
        let mut r = Report::open(cfg.clone(), files.clone(), T, 0);
        r.count(T, 0, keys::DSC_MESSAGES, 7);
        r.tick(&input(T + 1));
        let next_day = T + 6 * 3600; // 2026-11-25 00:04
        r.tick(&input(next_day));
        let md = std::fs::read_to_string(files.markdown_path("2026-11-24")).unwrap();
        assert!(md.contains("| 7 | 0 |"), "{md}");
        assert!(
            md.contains("Started ShellSDR") || md.contains("## Warnings"),
            "{md}"
        );
        r.close(next_day + 1, 0);
        // A day nobody wrote at midnight is caught up at the next start.
        std::fs::remove_file(files.markdown_path("2026-11-24")).unwrap();
        let _r = Report::open(cfg, files.clone(), T + 86_400, 0);
        assert!(files.markdown_path("2026-11-24").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn hooked_events_reach_the_command_and_its_failures_do_not_loop() {
        let d = temp("hook");
        std::fs::create_dir_all(&d).unwrap();
        let out = d.join("hook.txt");
        let cfg = ReportConfig {
            hook: format!("echo \"$SHELLSDR_TEXT\" >> {}; exit 1", out.display()),
            ..ReportConfig::default()
        };
        let mut r = Report::open(cfg, ReportFiles::new(&d, "p"), T, 0);
        r.event(T, 0, Level::Info, Category::Rec, "not sent".into(), None);
        r.event(
            T,
            0,
            Level::Error,
            Category::Rec,
            "write error".into(),
            None,
        );
        let mut failed = false;
        for i in 0..50 {
            r.tick(&input(T + 1 + i));
            if r.today().iter().any(|e| e.text.starts_with("Hook failed")) {
                failed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(failed, "{:?}", r.today());
        let sent = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            sent, "write error\n",
            "only the error, and the failure is not sent"
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
