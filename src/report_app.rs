//! The Report in the app (0.3.0, spec 20): opening it at start and closing
//! it at quit, the events and counts the app hands it, and a tick that
//! counts time and logs what changed since the last one (the source, the
//! recording, the VFO type, the scheduler switch, the active list).

use crate::app::{AppState, RunState};
use crate::report::model::{Category, Level, keys};
use crate::report::store::{ReportFiles, profile_of};
use crate::report::{Report, TickInput};

/// The Report page's tabs (spec 20.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReportTab {
    #[default]
    Today,
    Month,
    Lifetime,
    Log,
    Settings,
}

impl ReportTab {
    pub const ALL: [ReportTab; 5] = [
        ReportTab::Today,
        ReportTab::Month,
        ReportTab::Lifetime,
        ReportTab::Log,
        ReportTab::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ReportTab::Today => "Today",
            ReportTab::Month => "Month",
            ReportTab::Lifetime => "Lifetime",
            ReportTab::Log => "Log",
            ReportTab::Settings => "Settings",
        }
    }
}

/// The rows of the Settings tab (spec 20.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportSetting {
    Folder,
    LocalTime,
    KeepDays,
    KeepLogs,
    KeepMonths,
    /// The event hook's command, which events it gets, a test.
    Hook,
    HookErrors,
    HookWarnings,
    HookDsc,
    HookHfdl,
    HookHealth,
    HookTest,
}

impl ReportSetting {
    pub const ALL: [ReportSetting; 12] = [
        ReportSetting::Folder,
        ReportSetting::LocalTime,
        ReportSetting::KeepDays,
        ReportSetting::KeepLogs,
        ReportSetting::KeepMonths,
        ReportSetting::Hook,
        ReportSetting::HookErrors,
        ReportSetting::HookWarnings,
        ReportSetting::HookDsc,
        ReportSetting::HookHfdl,
        ReportSetting::HookHealth,
        ReportSetting::HookTest,
    ];

    /// A row Left/Right flips.
    fn is_switch(self) -> bool {
        matches!(
            self,
            ReportSetting::HookErrors
                | ReportSetting::HookWarnings
                | ReportSetting::HookDsc
                | ReportSetting::HookHfdl
                | ReportSetting::HookHealth
        )
    }

    /// Left/Right step of a number row.
    fn step(self) -> i64 {
        match self {
            ReportSetting::KeepDays => 30,
            ReportSetting::KeepLogs => 10,
            ReportSetting::KeepMonths => 12,
            _ => 0,
        }
    }
}

/// Which events the Log tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Show {
    #[default]
    All,
    /// Warnings and errors.
    Warnings,
    Errors,
    Category(Category),
}

impl Show {
    pub fn label(self) -> &'static str {
        match self {
            Show::All => "All",
            Show::Warnings => "Warnings",
            Show::Errors => "Errors",
            Show::Category(c) => c.label(),
        }
    }

    fn next(self) -> Self {
        use Category as C;
        let order = [
            Show::All,
            Show::Warnings,
            Show::Errors,
            Show::Category(C::App),
            Show::Category(C::Source),
            Show::Category(C::User),
            Show::Category(C::Sched),
            Show::Category(C::Rec),
            Show::Category(C::Dsc),
            Show::Category(C::Hfdl),
            Show::Category(C::Fax),
            Show::Category(C::Digital),
            Show::Category(C::Health),
        ];
        let i = order.iter().position(|s| *s == self).unwrap_or(0);
        order[(i + 1) % order.len()]
    }

    pub fn shows(self, e: &crate::report::model::Event) -> bool {
        match self {
            Show::All => true,
            Show::Warnings => e.level >= Level::Warning,
            Show::Errors => e.level == Level::Error,
            Show::Category(c) => e.category == c,
        }
    }
}

/// The Report page's state.
#[derive(Debug, Clone, Default)]
pub struct ReportUi {
    pub tab: ReportTab,
    /// Days (Today, Log) or months (Month) back from now.
    pub back: u32,
    pub scroll: usize,
    pub show: Show,
    /// The Settings tab's row.
    pub setting: usize,
    /// A new folder waiting for Move / Start fresh.
    pub pending_dir: Option<String>,
    /// An earlier day's log, read from its file: (day, events).
    pub log_cache: Option<(String, Vec<crate::report::model::Event>)>,
}

/// What the report saw at the last tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seen {
    pub started: bool,
    pub running: bool,
    pub recording: bool,
    pub vfo_type: String,
    pub sched_enabled: bool,
    pub active_list: String,
    /// The station tuned and since when (Unix s).
    pub station: String,
    pub station_since: i64,
    /// The scheduled job recording: id, name, start (Unix s).
    pub job: Option<(u64, String, i64)>,
    /// The newest finished fax picture counted.
    pub fax_done: u64,
    /// The Digital totals counted.
    pub digital: crate::digital::service::Totals,
    /// The source's running time in this run (s), and the last tick.
    pub source_secs: i64,
    pub last_tick: i64,
    /// Health (spec 20.6): the monitor, the minute being sampled, its
    /// highest DSP load, the driver's drop count already counted, the last
    /// clock check, the last disk reading (free bytes, days left).
    pub health: crate::report::health::HealthMonitor,
    pub minute_start: i64,
    pub dsp_minute: f32,
    pub overflows_seen: u64,
    pub clock_checked: i64,
    pub disk: Option<(u64, Option<f64>)>,
    /// Decoded output (calls, pictures, bytes) on the current station and
    /// during the current scheduled job; the station's warning was given.
    pub station_decoded: u64,
    pub station_warned: bool,
    pub job_decoded: u64,
    /// The last noise floor sample.
    pub noise_at: i64,
    /// Noise floor changes already logged: `day|station`.
    pub noise_warned: Vec<String>,
    /// The scheduler's and health events of the 7 days before today (for
    /// the jobs' last runs), and the day they were read on.
    pub history: Vec<crate::report::model::Event>,
    pub history_day: String,
}

/// One run of a scheduled job, from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRun {
    pub start: i64,
    /// None while it runs (or when the log has no end).
    pub end: Option<i64>,
    pub skipped: bool,
    /// Ended without decoding anything.
    pub silent: bool,
}

/// The runs of job `name` in `events` (oldest first): `Started <name> (`
/// opens one, `Ended <name> (`, `<name> stopped before its end` and
/// `<name>: nothing decoded` close or mark it.
pub fn job_runs(events: &[&crate::report::model::Event], name: &str) -> Vec<JobRun> {
    let started = format!("Started {name} (");
    let ended = format!("Ended {name} (");
    let stopped = format!("{name} stopped before its end");
    let silent = format!("{name}: nothing decoded");
    let mut runs: Vec<JobRun> = Vec::new();
    for e in events {
        if e.category == Category::Sched && e.text.starts_with(&started) {
            runs.push(JobRun {
                start: e.time,
                end: None,
                skipped: false,
                silent: false,
            });
        } else if let Some(r) = runs.last_mut() {
            if e.category == Category::Sched && e.text.starts_with(&ended) && r.end.is_none() {
                r.end = Some(e.time);
            } else if e.category == Category::Sched
                && e.text.starts_with(&stopped)
                && r.end.is_none()
            {
                r.end = Some(e.time);
                r.skipped = true;
            } else if e.category == Category::Health && e.text.starts_with(&silent) {
                r.silent = true;
            }
        }
    }
    runs
}

/// The VFO types that decode something (their silence is worth a warning).
fn decodes(vfo_type: &str) -> bool {
    matches!(vfo_type, "DSC" | "HFDL" | "FAX" | "Digital")
}

/// The median of a spectrum frame, dB.
fn median_db(frame: &[f32]) -> Option<f32> {
    if frame.is_empty() {
        return None;
    }
    let mut v: Vec<f32> = frame.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    let mid = v.len() / 2;
    v.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    Some(v[mid])
}

fn severity_name(s: crate::dsc::message::Severity) -> &'static str {
    use crate::dsc::message::Severity;
    match s {
        Severity::Distress => "distress",
        Severity::Urgency => "urgency",
        Severity::Safety => "safety",
        Severity::Routine => "routine",
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl AppState {
    /// Open the profile's report (startup), in the configured folder.
    pub fn open_report(&mut self, config_path: &std::path::Path) {
        let now = now_unix();
        let offset = crate::schedule::window::local_offset_min(now.max(0) as u64);
        let dir = crate::app::expand_home(&self.report_config.dir);
        let files = ReportFiles::new(dir, profile_of(config_path));
        self.report = Some(Report::open(self.report_config.clone(), files, now, offset));
    }

    /// A clean quit.
    pub fn close_report(&mut self) {
        let now = now_unix();
        let offset = self.utc_offset_min;
        if let Some(r) = &mut self.report {
            r.close(now, offset);
        }
    }

    /// Log an event (nothing without an open report).
    pub fn report_event(
        &mut self,
        level: Level,
        category: Category,
        text: impl Into<String>,
        file: Option<String>,
    ) {
        let (now, offset) = (now_unix(), self.utc_offset_min);
        if let Some(r) = &mut self.report {
            r.event(now, offset, level, category, text.into(), file);
        }
    }

    /// Count `n` of `key`.
    pub fn report_count(&mut self, key: &str, n: u64) {
        let (now, offset) = (now_unix(), self.utc_offset_min);
        if let Some(r) = &mut self.report {
            r.count(now, offset, key, n);
        }
    }

    /// Count decoded output: from a file playback under `file.` (spec
    /// 20.3), live also to the station tuned.
    pub fn report_output(&mut self, key: &str, n: u64) {
        let file = self.source.kind == crate::source::model::SourceKind::File;
        let (now, offset) = (now_unix(), self.utc_offset_min);
        let station = self.report_station();
        if let Some(r) = &mut self.report {
            if file {
                r.count(now, offset, &format!("{}{key}", keys::FILE), n);
            } else {
                r.count(now, offset, key, n);
                r.count_station(now, offset, &station, key, n);
            }
        }
    }

    /// The station the active VFO is on: the name of a bookmark with its
    /// frequency and type, else `(manual) 7040 kHz`.
    pub fn report_station(&self) -> String {
        let v = self.radio.active();
        self.bookmarks
            .store
            .lists
            .iter()
            .flat_map(|l| l.bookmarks.iter())
            .find(|b| b.frequency_hz == v.frequency_hz && b.vfo_type == v.vfo_type)
            .map_or_else(
                || {
                    format!(
                        "(manual) {} kHz",
                        crate::ui::bookmark_view::khz_label(v.frequency_hz)
                    )
                },
                |b| b.name.clone(),
            )
    }

    /// How long the active VFO has been on its station (type and
    /// frequency), in seconds.
    pub fn report_station_secs(&self) -> i64 {
        (now_unix() - self.report_seen.station_since).max(0)
    }

    /// A scheduled recording that ended since the last tick (its start is
    /// logged by the scheduler glue): ended at its window's end, or earlier
    /// (stopped by the user, the master switch, a list switch: skipped).
    fn report_jobs(&mut self, now: i64) {
        let s = &self.schedule.scheduler;
        let cur = s.current_job().and_then(|id| {
            s.jobs
                .iter()
                .find(|j| j.id == id)
                .map(|j| (id, j.name().to_string()))
        });
        let prev = self.report_seen.job.clone();
        match (&prev, &cur) {
            (Some((pid, name, start)), c) if c.as_ref().map(|c| c.0) != Some(*pid) => {
                let mins = (now - start).max(0) / 60;
                let early = s
                    .jobs
                    .iter()
                    .find(|j| j.id == *pid)
                    .is_some_and(|j| j.window.occurrence_at(now.max(0) as u64).is_some());
                let ty = s
                    .jobs
                    .iter()
                    .find(|j| j.id == *pid)
                    .map(|j| j.tuning.vfo_type.clone());
                if early {
                    self.report_count(keys::SCHED_SKIPPED, 1);
                    self.report_event(
                        Level::Warning,
                        Category::Sched,
                        format!("{name} stopped before its end ({mins} min): skipped"),
                        None,
                    );
                } else {
                    self.report_event(
                        Level::Info,
                        Category::Sched,
                        format!("Ended {name} ({mins} min)"),
                        None,
                    );
                }
                if ty.as_deref().is_some_and(decodes)
                    && self.report_seen.job_decoded == 0
                    && mins > 0
                {
                    self.report_event(
                        Level::Warning,
                        Category::Health,
                        format!("{name}: nothing decoded in {mins} min"),
                        None,
                    );
                }
            }
            _ => {}
        }
        self.report_seen.job = match (prev, cur) {
            (Some(p), Some(c)) if p.0 == c.0 => Some(p),
            (_, Some((id, name))) => {
                self.report_seen.job_decoded = 0;
                Some((id, name, now))
            }
            (_, None) => None,
        };
    }

    /// Decoded output since the last tick: DSC calls (distress and urgency
    /// logged one by one), HFDL messages (alerts logged one by one),
    /// finished fax pictures, Digital text, pictures and RSIDs.
    fn report_decoders(&mut self) {
        let hfdl = self
            .hfdl
            .as_mut()
            .map(|s| s.take_reported())
            .unwrap_or_default();
        let total: u64 = hfdl.by_severity.iter().sum();
        if total > 0 {
            self.note_decoded(total);
            self.report_output(keys::HFDL_MESSAGES, total);
            use crate::hfdl::msg::Severity as S;
            for s in [S::Link, S::Routine, S::Caution, S::Emergency] {
                self.report_output(
                    &format!("{}{}", keys::HFDL_SEVERITY, s.name()),
                    hfdl.by_severity[s as usize],
                );
            }
        }
        self.report_output(keys::HFDL_BAD_FRAMES, hfdl.bad_frames);
        // distinct things heard: live only (a replay is not today's)
        if self.source.kind != crate::source::model::SourceKind::File
            && let Some(r) = &mut self.report
        {
            let (now, off) = (now_unix(), self.utc_offset_min);
            let ac: Vec<String> = hfdl.aircraft.into_iter().collect();
            let gs: Vec<String> = hfdl.stations.iter().map(u8::to_string).collect();
            r.seen(now, off, crate::report::seen::SeenKind::Aircraft, &ac);
            r.seen(now, off, crate::report::seen::SeenKind::Station, &gs);
        }
        for (kind, text) in hfdl.alerts {
            let level = if kind == crate::hfdl::msg::AlertKind::Watch {
                Level::Info
            } else {
                Level::Warning
            };
            self.report_event(level, Category::Hfdl, text, None);
        }
        let calls = self
            .dsc
            .as_mut()
            .map(|d| d.take_reported())
            .unwrap_or_default();
        for c in calls {
            let sev = severity_name(c.severity);
            self.note_decoded(1);
            self.report_output(keys::DSC_MESSAGES, 1);
            self.report_output(&format!("{}{sev}", keys::DSC_SEVERITY), 1);
            if c.garbled {
                self.report_output(keys::DSC_GARBLED, 1);
            }
            if matches!(sev, "distress" | "urgency") {
                self.report_event(
                    Level::Warning,
                    Category::Dsc,
                    format!("{} from {} on {}", sev.to_uppercase(), c.from, c.channel),
                    None,
                );
            }
        }
        // Fax: pictures closed since the last tick (ids restart with a new
        // decoder).
        let done: Vec<(u64, i32)> = self
            .fax
            .as_ref()
            .map(|f| {
                f.pictures()
                    .iter()
                    .filter(|p| p.open == 0)
                    .map(|p| (p.id, p.lines - p.first_line))
                    .collect()
            })
            .unwrap_or_default();
        if done.iter().map(|d| d.0).max().unwrap_or(0) < self.report_seen.fax_done {
            self.report_seen.fax_done = 0;
        }
        for (id, lines) in done {
            if id > self.report_seen.fax_done {
                self.report_seen.fax_done = id;
                self.note_decoded(1);
                self.report_output(keys::FAX_PICTURES, 1);
                self.report_output(keys::FAX_LINES, lines.max(0) as u64);
                let station = self.report_station();
                let file = self
                    .fax
                    .as_ref()
                    .and_then(|f| f.recorded_file(id))
                    .map(|p| p.display().to_string());
                self.report_event(
                    Level::Info,
                    Category::Fax,
                    format!("Picture done: {lines} lines, {station}"),
                    file,
                );
            }
        }
        // Digital: the service's totals grow; a new service starts at 0.
        let t = self
            .digital
            .as_ref()
            .map(|d| d.totals())
            .unwrap_or_default();
        let p = self.report_seen.digital;
        let p = if t.text_bytes < p.text_bytes || t.pictures < p.pictures {
            Default::default()
        } else {
            p
        };
        self.note_decoded((t.text_bytes - p.text_bytes) + (t.pictures - p.pictures));
        self.report_output(keys::DIGITAL_BYTES, t.text_bytes - p.text_bytes);
        self.report_output(keys::DIGITAL_RSID, t.rsid - p.rsid);
        self.report_output(keys::DIGITAL_SWITCHES, t.switches - p.switches);
        let files = self
            .digital
            .as_mut()
            .map(|d| d.take_new_files())
            .unwrap_or_default();
        if t.pictures > p.pictures {
            self.report_output(keys::DIGITAL_PICTURES, t.pictures - p.pictures);
            let station = self.report_station();
            let n = (t.pictures - p.pictures) as usize;
            for i in 0..n {
                let file = files.get(i).map(|f| f.display().to_string());
                self.report_event(
                    Level::Info,
                    Category::Digital,
                    format!("Picture received, {station}"),
                    file,
                );
            }
        }
        self.report_seen.digital = t;
    }

    /// Once a day: the scheduler's and health events of the last week from
    /// the log files (the jobs' last runs).
    fn report_history(&mut self) {
        let Some(r) = &self.report else {
            return;
        };
        let today = r.today_key().to_string();
        if self.report_seen.history_day == today {
            return;
        }
        let mut h = Vec::new();
        for n in (1..=7).rev() {
            if let Some(d) = crate::report::model::day_minus(&today, n) {
                h.extend(
                    r.files()
                        .read_log(&d)
                        .into_iter()
                        .filter(|e| matches!(e.category, Category::Sched | Category::Health)),
                );
            }
        }
        self.report_seen.history = h;
        self.report_seen.history_day = today;
    }

    /// The last runs of job `name` (newest first, at most `n`).
    pub fn sched_last_runs(&self, name: &str, n: usize) -> Vec<JobRun> {
        let today: Vec<&crate::report::model::Event> = self
            .report
            .as_ref()
            .map(|r| r.today().iter().collect())
            .unwrap_or_default();
        let all: Vec<&crate::report::model::Event> =
            self.report_seen.history.iter().chain(today).collect();
        let mut runs = job_runs(&all, name);
        runs.reverse();
        runs.truncate(n);
        runs
    }

    /// Every tick: the DSP load's peak and a noise sample every 10 s; once a
    /// minute: the health sample (spec 20.3) and its events.
    fn report_health(&mut self, now: i64, running: bool) {
        use crate::report::health::{self, Sample};
        let seen = &mut self.report_seen;
        if running {
            seen.dsp_minute = seen.dsp_minute.max(self.dsp_load);
        }
        if running
            && now - seen.noise_at >= 10
            && let Some(db) = median_db(&self.spectrum)
        {
            seen.noise_at = now;
            let station = seen.station.clone();
            let tenths = ((f64::from(db) + 200.0) * 10.0).max(0.0) as u64;
            let (off, nowi) = (self.utc_offset_min, now);
            if let Some(r) = &mut self.report {
                r.count_station(nowi, off, &station, keys::STATION_NOISE_SUM, tenths);
                r.count_station(nowi, off, &station, keys::STATION_NOISE_N, 1);
            }
        }
        let seen = &mut self.report_seen;
        if seen.minute_start == 0 {
            seen.minute_start = now;
            seen.overflows_seen = self.dsp_overflows;
        }
        if now - seen.minute_start < 60 {
            return;
        }
        seen.minute_start = now;
        // The engine's count restarts with it.
        if self.dsp_overflows < seen.overflows_seen {
            seen.overflows_seen = 0;
        }
        let drops = self.dsp_overflows - seen.overflows_seen;
        seen.overflows_seen = self.dsp_overflows;
        let dsp = running.then_some(seen.dsp_minute);
        seen.dsp_minute = 0.0;
        // The clock at the first sample, then hourly.
        let check_clock = seen.clock_checked == 0 || now - seen.clock_checked >= 3600;
        if check_clock {
            seen.clock_checked = now;
        }
        let dir = std::path::PathBuf::from(crate::app::expand_home(self.active_rec_dir()));
        let free = health::disk_free(&dir);
        // Days left at today's recording rate.
        let days = self.report.as_ref().and_then(|r| {
            let offset = r.clock(self.utc_offset_min).offset();
            let since_midnight = (now + i64::from(offset) * 60).rem_euclid(86_400).max(60);
            let today = r.doc().day(r.today_key())?.get(keys::REC_BYTES);
            let per_day = today * 86_400 / since_midnight as u64;
            health::days_left(free?, per_day)
        });
        self.report_seen.disk = free.map(|f| (f, days));
        let sample = Sample {
            dsp,
            drops,
            temp_c: health::cpu_temp_c(),
            throttled: health::throttled(),
            disk_free: free,
            days_left: days,
            clock_synced: if check_clock {
                health::clock_synced()
            } else {
                None
            },
        };
        // The station's noise floor against its week (spec 20.6).
        if let Some(r) = &self.report {
            let day = r.today_key().to_string();
            let station = self.report_seen.station.clone();
            let key = format!("{day}|{station}");
            if !self.report_seen.noise_warned.contains(&key)
                && let Some(change) = r.doc().noise_change(&day, &station)
            {
                self.report_seen.noise_warned.push(key);
                let way = if change > 0.0 { "up" } else { "down" };
                self.report_event(
                    Level::Warning,
                    Category::Health,
                    format!(
                        "{station}: noise floor {way} {:.0} dB against its last week (antenna, interference?)",
                        change.abs()
                    ),
                    None,
                );
            }
        }
        let events = self.report_seen.health.evaluate(&sample);
        for (level, text) in events {
            self.report_event(level, Category::Health, text, None);
        }
        let (off, mem) = (self.utc_offset_min, health::rss_kb());
        if let Some(r) = &mut self.report {
            if let Some(d) = dsp {
                r.maximum(now, off, keys::HEALTH_DSP, (d * 1000.0) as u64);
            }
            if let Some(t) = sample.temp_c {
                r.maximum(now, off, keys::HEALTH_TEMP, (t * 10.0).max(0.0) as u64);
            }
            if let Some(m) = mem {
                r.maximum(now, off, keys::HEALTH_MEM, m);
            }
            r.count(now, off, keys::HEALTH_DROPS, drops);
        }
    }

    /// A decoding station tuned for an hour without output (spec 20.6).
    fn report_silence(&mut self, now: i64, running: bool) {
        let s = &self.report_seen;
        if !running
            || s.station_warned
            || s.station_decoded > 0
            || !decodes(&s.vfo_type)
            || now - s.station_since < 3600
        {
            return;
        }
        let station = s.station.clone();
        self.report_seen.station_warned = true;
        self.report_event(
            Level::Warning,
            Category::Health,
            format!("{station}: nothing decoded in an hour"),
            None,
        );
    }

    /// Live decoded output (for the silence warnings).
    fn note_decoded(&mut self, n: u64) {
        if self.source.kind != crate::source::model::SourceKind::File {
            self.report_seen.station_decoded += n;
            self.report_seen.job_decoded += n;
        }
    }

    /// A finished recording output (counted, logged with its size).
    pub fn report_recording(&mut self, file: &str, seconds: f64, bytes: u64, dropped: u64) {
        self.report_count(keys::REC_FILES, 1);
        self.report_count(keys::REC_BYTES, bytes);
        let mins = (seconds / 60.0).round();
        if dropped > 0 {
            self.report_event(
                Level::Warning,
                Category::Rec,
                format!(
                    "Closed {file}: {mins} min, {}, {dropped} frames dropped",
                    mb(bytes)
                ),
                Some(file.to_string()),
            );
        } else {
            self.report_event(
                Level::Info,
                Category::Rec,
                format!("Closed {file}: {mins} min, {}", mb(bytes)),
                Some(file.to_string()),
            );
        }
    }

    // ── The Report page ──────────────────────────────────────────────────

    /// The day shown on Today and Log (`back` days before today).
    pub fn report_day(&self) -> Option<String> {
        let r = self.report.as_ref()?;
        crate::report::model::day_minus(r.today_key(), self.report_ui.back)
    }

    /// The month shown on Month.
    pub fn report_month(&self) -> Option<String> {
        let r = self.report.as_ref()?;
        crate::report::model::month_minus(&r.today_key()[..7], self.report_ui.back)
    }

    pub fn report_select_tab(&mut self, t: ReportTab) {
        if self.report_ui.tab != t {
            self.report_ui.tab = t;
            self.report_ui.back = 0;
            self.report_ui.scroll = 0;
            self.report_load_log();
        }
    }

    /// Older (-1) or newer (+1) day or month.
    pub fn report_step(&mut self, dir: i32) {
        let ui = &mut self.report_ui;
        ui.back = if dir < 0 {
            ui.back.saturating_add(1)
        } else {
            ui.back.saturating_sub(1)
        };
        ui.scroll = 0;
        self.report_load_log();
    }

    /// The Log tab on an earlier day reads its file once.
    fn report_load_log(&mut self) {
        if !matches!(self.report_ui.tab, ReportTab::Log | ReportTab::Today)
            || self.report_ui.back == 0
        {
            self.report_ui.log_cache = None;
            return;
        }
        let (Some(r), Some(day)) = (&self.report, self.report_day()) else {
            return;
        };
        if self.report_ui.log_cache.as_ref().map(|c| &c.0) != Some(&day) {
            let events = r.files().read_log(&day);
            self.report_ui.log_cache = Some((day, events));
        }
    }

    /// The events of the day shown, oldest first.
    pub fn report_day_events(&self) -> &[crate::report::model::Event] {
        match (&self.report_ui.log_cache, &self.report) {
            (Some((_, ev)), _) if self.report_ui.back > 0 => ev,
            (_, Some(r)) if self.report_ui.back == 0 => r.today(),
            _ => &[],
        }
    }

    /// The events the Log tab shows (the filter applied), oldest first.
    pub fn report_log(&self) -> Vec<&crate::report::model::Event> {
        let show = self.report_ui.show;
        self.report_day_events()
            .iter()
            .filter(|e| show.shows(e))
            .collect()
    }

    /// The Report page's keys: Left/Right older/newer, Up/Down/PgUp/PgDn
    /// scroll; true when taken.
    pub fn report_page_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        if self.report_ui.tab == ReportTab::Settings {
            let n = ReportSetting::ALL.len();
            let row = ReportSetting::ALL[self.report_ui.setting.min(n - 1)];
            match action {
                Action::Up => self.report_ui.setting = (self.report_ui.setting + n - 1) % n,
                Action::Down => self.report_ui.setting = (self.report_ui.setting + 1) % n,
                Action::Enter => self.report_setting_enter(row),
                Action::Left => self.report_setting_step(row, -1),
                Action::Right => self.report_setting_step(row, 1),
                _ => return false,
            }
            return true;
        }
        let ui = &mut self.report_ui;
        match action {
            Action::Left => self.report_step(-1),
            Action::Right => self.report_step(1),
            Action::Up => ui.scroll = ui.scroll.saturating_sub(1),
            Action::Down => ui.scroll += 1,
            Action::PageUp => ui.scroll = ui.scroll.saturating_sub(20),
            Action::PageDown => ui.scroll += 20,
            Action::Home => ui.scroll = 0,
            Action::End => ui.scroll = 100_000,
            _ => return false,
        }
        true
    }

    /// The retention numbers (days, log days, months).
    fn report_keep(&self) -> [u32; 3] {
        let c = &self.report_config;
        [c.keep_days, c.keep_log_days, c.keep_months]
    }

    /// Set the retention (0 = forever) and prune at once.
    pub fn report_set_keep(&mut self, keep: [u32; 3]) {
        self.report_config.keep_days = keep[0];
        self.report_config.keep_log_days = keep[1];
        self.report_config.keep_months = keep[2];
        let (now, offset) = (now_unix(), self.utc_offset_min);
        if let Some(r) = &mut self.report {
            r.set_retention(keep[0], keep[1], keep[2], now, offset);
        }
    }

    fn report_keep_index(row: ReportSetting) -> Option<usize> {
        match row {
            ReportSetting::KeepDays => Some(0),
            ReportSetting::KeepLogs => Some(1),
            ReportSetting::KeepMonths => Some(2),
            _ => None,
        }
    }

    /// ENTER (or a click) on a Settings row.
    pub fn report_setting_enter(&mut self, row: ReportSetting) {
        use crate::app::PromptTarget;
        use crate::ui::prompt::TextPrompt;
        match row {
            ReportSetting::Folder => {
                self.prompt = Some(TextPrompt::new(
                    "Report folder",
                    self.report_config.dir.clone(),
                ));
                self.prompt_target = Some(PromptTarget::ReportDir);
            }
            ReportSetting::LocalTime => self.report_page_fkey(7),
            ReportSetting::Hook => {
                self.prompt = Some(TextPrompt::new(
                    "Hook command (sh -c; empty = off)",
                    self.report_config.hook.clone(),
                ));
                self.prompt_target = Some(PromptTarget::ReportHook);
            }
            ReportSetting::HookErrors
            | ReportSetting::HookWarnings
            | ReportSetting::HookDsc
            | ReportSetting::HookHfdl
            | ReportSetting::HookHealth => {
                let c = &mut self.report_config;
                let b = match row {
                    ReportSetting::HookErrors => &mut c.hook_errors,
                    ReportSetting::HookWarnings => &mut c.hook_warnings,
                    ReportSetting::HookDsc => &mut c.hook_dsc,
                    ReportSetting::HookHfdl => &mut c.hook_hfdl,
                    _ => &mut c.hook_health,
                };
                *b = !*b;
                self.report_apply_hook();
            }
            ReportSetting::HookTest => {
                if self.report_config.hook.trim().is_empty() {
                    self.show_notice("Set the hook command first".to_string());
                } else {
                    let (now, offset) = (now_unix(), self.utc_offset_min);
                    if let Some(r) = &mut self.report {
                        r.test_hook(now, offset);
                    }
                }
            }
            keep => {
                let i = Self::report_keep_index(keep).expect("a number row");
                let title = match keep {
                    ReportSetting::KeepDays => "Keep daily counters (days, 0 = forever)",
                    ReportSetting::KeepLogs => "Keep logs (days, 0 = forever)",
                    _ => "Keep monthly counters (months, 0 = forever)",
                };
                self.prompt = Some(TextPrompt::new(title, self.report_keep()[i].to_string()));
                self.prompt_target = Some(PromptTarget::ReportKeep(i as u8));
            }
        }
    }

    /// Left/Right on a number row; on a switch it flips.
    fn report_setting_step(&mut self, row: ReportSetting, dir: i64) {
        match Self::report_keep_index(row) {
            Some(i) => {
                let mut keep = self.report_keep();
                keep[i] = (i64::from(keep[i]) + dir * row.step()).clamp(0, 100_000) as u32;
                self.report_set_keep(keep);
            }
            None if row == ReportSetting::LocalTime || row.is_switch() => {
                self.report_setting_enter(row)
            }
            None => {}
        }
    }

    /// Hand the hook settings to the open report.
    pub fn report_apply_hook(&mut self) {
        let c = self.report_config.clone();
        if let Some(r) = &mut self.report {
            r.config.hook = c.hook;
            r.config.hook_errors = c.hook_errors;
            r.config.hook_warnings = c.hook_warnings;
            r.config.hook_dsc = c.hook_dsc;
            r.config.hook_hfdl = c.hook_hfdl;
            r.config.hook_health = c.hook_health;
        }
    }

    /// The folder prompt confirmed: a new folder asks Move / Start fresh.
    pub fn report_dir_entered(&mut self, dir: &str) -> Result<(), String> {
        let d = dir.trim();
        if d.is_empty() {
            return Err("The folder must not be empty".to_string());
        }
        if d == self.report_config.dir {
            return Ok(());
        }
        self.report_ui.pending_dir = Some(d.to_string());
        self.popup = Some(crate::ui::popup::ListPopup::new(
            "Move the existing reports and logs to the new folder?",
            vec![
                "Cancel".to_string(),
                "Move".to_string(),
                "Start fresh".to_string(),
            ],
            0,
        ));
        self.popup_target = Some(crate::app::PopupTarget::ReportRelocate);
        Ok(())
    }

    /// Move (item 1) or Start fresh (item 2) into the pending folder.
    pub fn report_relocate(&mut self, item: usize) {
        let Some(dir) = self.report_ui.pending_dir.take() else {
            return;
        };
        let how = match item {
            1 => crate::report::Relocate::Move,
            2 => crate::report::Relocate::Fresh,
            _ => return,
        };
        let (now, offset) = (now_unix(), self.utc_offset_min);
        let result = match &mut self.report {
            Some(r) => {
                let to = ReportFiles::new(crate::app::expand_home(&dir), r.files().profile.clone());
                r.relocate(to, how, now, offset)
            }
            None => Ok(()),
        };
        match result {
            Ok(()) => {
                self.report_config.dir = dir;
                if let Some(r) = &mut self.report {
                    r.config.dir = self.report_config.dir.clone();
                }
            }
            Err(e) => self.show_notice(format!("Report folder not changed: {e}")),
        }
    }

    /// The Report page's function keys (`report_page::FKEYS`).
    pub fn report_page_fkey(&mut self, n: u8) {
        match n {
            1 => self.toggle_help(),
            3 => {
                let all = ReportTab::ALL;
                let i = all
                    .iter()
                    .position(|t| *t == self.report_ui.tab)
                    .unwrap_or(0);
                self.report_select_tab(all[(i + 1) % all.len()]);
            }
            4 => self.report_step(-1),
            5 => self.report_step(1),
            6 => {
                self.report_ui.show = self.report_ui.show.next();
                self.report_ui.scroll = 0;
            }
            7 => {
                // Local time or UTC: days and times from now on (spec 20.5).
                self.report_config.local_time = !self.report_config.local_time;
                let on = self.report_config.local_time;
                if let Some(r) = &mut self.report {
                    r.config.local_time = on;
                }
            }
            _ => {}
        }
    }

    /// Once a second: log what changed, count the time.
    pub fn report_tick(&mut self) {
        if self.report.is_none() {
            return;
        }
        let now = now_unix();
        let running = self.run == RunState::Running;
        let recording = self.radio.active().recording;
        let vfo_type = self.radio.active().vfo_type.clone();
        let station = self.report_station();
        let sched_enabled = self.schedule.scheduler.enabled;
        let active_list = self.schedule.active_name().to_string();
        let prev = std::mem::take(&mut self.report_seen);
        let mut seen = Seen {
            started: true,
            running,
            recording,
            vfo_type: vfo_type.clone(),
            sched_enabled,
            active_list: active_list.clone(),
            station: station.clone(),
            station_since: prev.station_since,
            job: prev.job.clone(),
            fax_done: prev.fax_done,
            digital: prev.digital,
            source_secs: prev.source_secs,
            last_tick: now,
            ..prev.clone()
        };
        if prev.started && running {
            let gap = now - prev.last_tick;
            if (1..=120).contains(&gap) {
                seen.source_secs += gap;
            }
        }
        if !prev.started || prev.station != station || prev.vfo_type != vfo_type {
            seen.station_since = now;
            seen.station_decoded = 0;
            seen.station_warned = false;
        }
        if prev.started {
            if prev.vfo_type != vfo_type {
                let name = self.radio.active().name.clone();
                self.report_event(
                    Level::Info,
                    Category::User,
                    format!("VFO {name}: type {vfo_type}"),
                    None,
                );
            }
            if prev.sched_enabled != sched_enabled {
                let on = if sched_enabled { "on" } else { "off" };
                self.report_event(
                    Level::Info,
                    Category::Sched,
                    format!("Scheduler switched {on}"),
                    None,
                );
            }
            if prev.active_list != active_list {
                self.report_event(
                    Level::Info,
                    Category::Sched,
                    format!("Active schedule list: {active_list}"),
                    None,
                );
            }
            if prev.recording != recording {
                let by = if self.schedule.scheduler.current_job().is_some() {
                    "schedule"
                } else {
                    "user"
                };
                let text = if recording {
                    format!("Recording started ({by}) on {station}")
                } else {
                    "Recording stopped".to_string()
                };
                self.report_event(Level::Info, Category::Rec, text, None);
            }
        }
        self.report_seen = seen;
        self.report_history();
        self.report_jobs(now);
        self.report_decoders();
        self.report_health(now, running);
        self.report_silence(now, running);
        let input = TickInput {
            now,
            offset_min: self.utc_offset_min,
            source_running: running,
            recording,
            vfo_type,
            station,
        };
        if let Some(r) = &mut self.report {
            r.tick(&input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::ReportConfig;

    fn state_with_report(name: &str) -> (AppState, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("shellsdr-rapp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut s = AppState::with_mock_source();
        s.report_config = ReportConfig {
            dir: d.to_string_lossy().into_owned(),
            ..ReportConfig::default()
        };
        s.open_report(std::path::Path::new("/c/ShellSDR/tuner2.toml"));
        (s, d)
    }

    #[test]
    fn changes_between_ticks_are_logged_and_files_carry_the_profile() {
        let (mut s, d) = state_with_report("tick");
        s.report_tick();
        s.schedule
            .flip(crate::schedule::module::SchedSetting::Enabled);
        s.report_tick();
        let r = s.report.as_ref().unwrap();
        let texts: Vec<&str> = r.today().iter().map(|e| e.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.starts_with("Started ShellSDR")),
            "{texts:?}"
        );
        assert!(texts.contains(&"Scheduler switched off"), "{texts:?}");
        assert!(d.join(format!("tuner2_log_{}.log", r.today_key())).exists());
        s.close_report();
        assert!(d.join("tuner2_report.v1.json").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn decoded_output_of_a_file_is_counted_apart() {
        let (mut s, d) = state_with_report("file");
        s.report_output(keys::DSC_MESSAGES, 2);
        s.source.kind = crate::source::model::SourceKind::File;
        s.report_output(keys::DSC_MESSAGES, 5);
        let r = s.report.as_ref().unwrap();
        let day = r.doc().day(r.today_key()).unwrap();
        assert_eq!(day.get(keys::DSC_MESSAGES), 2);
        assert_eq!(day.get("file.dsc.messages"), 5);
        let station = s.report_station();
        assert!(station.starts_with("(manual) "), "{station}");
        assert_eq!(day.stations[&station][keys::DSC_MESSAGES], 2);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_new_folder_asks_and_move_takes_the_files_there() {
        let (mut s, d) = state_with_report("relocate");
        s.report_ui.tab = ReportTab::Settings;
        s.report_setting_enter(ReportSetting::Folder);
        assert!(s.prompt.is_some(), "the folder prompt");
        let new = d.join("nas");
        s.prompt.as_mut().unwrap().input.set(new.to_string_lossy());
        s.prompt_confirm();
        assert!(s.prompt.is_none());
        let p = s.popup.as_ref().expect("Move / Start fresh");
        assert_eq!(p.items, ["Cancel", "Move", "Start fresh"]);
        s.popup_down(); // Move
        s.popup_confirm();
        assert_eq!(s.report_config.dir, new.to_string_lossy());
        assert!(new.join("tuner2_report.v1.json").exists());
        assert!(
            !d.join("tuner2_report.v1.json").exists(),
            "moved, not copied"
        );
        // Retention: Left/Right step, 0 is forever.
        s.report_ui.setting = 2;
        s.report_page_key(crate::ui::run::Action::Left);
        assert_eq!(s.report_config.keep_days, 370);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn runs_are_paired_from_the_log() {
        use crate::report::model::Event;
        let ev = |t: i64, c: Category, text: &str| Event {
            time: t,
            level: Level::Info,
            category: c,
            text: text.into(),
            file: None,
        };
        let log = [
            ev(100, Category::Sched, "Started DDH3 (Record Native, FAX)"),
            ev(200, Category::Sched, "Ended DDH3 (20 min)"),
            ev(250, Category::Health, "DDH3: nothing decoded in 20 min"),
            ev(300, Category::Sched, "Started DDH3 (Record Native, FAX)"),
            ev(
                350,
                Category::Sched,
                "DDH3 stopped before its end (1 min): skipped",
            ),
            ev(360, Category::Sched, "Started DDH3x (Record Native, FAX)"),
            ev(400, Category::Sched, "Started DDH3 (Record Native, FAX)"),
        ];
        let refs: Vec<&Event> = log.iter().collect();
        let runs = job_runs(&refs, "DDH3");
        assert_eq!(runs.len(), 3, "another job's lines are not this job's");
        assert_eq!(
            runs[0],
            JobRun {
                start: 100,
                end: Some(200),
                skipped: false,
                silent: true
            }
        );
        assert_eq!(
            runs[1],
            JobRun {
                start: 300,
                end: Some(350),
                skipped: true,
                silent: false
            }
        );
        assert_eq!(runs[2].end, None, "running");
    }
}
