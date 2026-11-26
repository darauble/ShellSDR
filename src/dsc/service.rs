//! The DSC VFO's message service (0.2.1, spec 14.4, 14.5): what DSQrust's
//! session does for one channel, owned by the UI thread.
//!
//! - A decoded call becomes a record: parsed, enriched (stations, the
//!   user's location, the position cache), shown at once, and appended to
//!   `<dir>/YYYY-MM-DD.ndjson` by a writer thread (after its online lookup,
//!   when one runs, seed LKP-03).
//! - The view holds the current local day only: today's day file, followed
//!   as it grows (lines another ShellSDR instance appends show up too; this
//!   service's own lines are recognised and not shown twice), switched at
//!   local midnight.
//! - Alerts (seed 04-support 4.7) only for calls decoded here, never for
//!   lines read from the file (ALR-05).
//!
//! Time is passed in (`now`, UTC seconds) so it is testable without a clock.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};

use super::entries::{Entry, Store};
use super::message::Severity;
use super::presentation::{self, detail};
use super::record::{PosSource, Record, StationPos, TimeSource};
use super::stations::Stations;
use super::store::{dayfile, ndjson};
use super::time::{self, TimeZone};
use super::view::ListView;
use super::{alert, enrich, lookup};

/// The name used for saved message texts (seed EXP-01).
pub const PROJECT: &str = "shellsdr-dsc";
/// How long a lookup may take before it counts as failed.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);
/// The default aprs.fi endpoint (seed LKP-06).
pub const DEFAULT_ENDPOINT: &str =
    "https://api.aprs.fi/api/get?name={mmsi}&what=loc&apikey={apikey}&format=json";

/// How the detail pane sits next to the list (F3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailLayout {
    /// Beside the list on a wide view, below it on a tall one.
    #[default]
    Auto,
    /// Beside the list.
    Horizontal,
    /// Below the list.
    Vertical,
}

impl DetailLayout {
    pub fn label(self) -> &'static str {
        match self {
            DetailLayout::Auto => "Auto View",
            DetailLayout::Horizontal => "Horizontal",
            DetailLayout::Vertical => "Vertical",
        }
    }

    fn key(self) -> &'static str {
        match self {
            DetailLayout::Auto => "Auto",
            DetailLayout::Horizontal => "Horizontal",
            DetailLayout::Vertical => "Vertical",
        }
    }

    pub fn next(self) -> Self {
        match self {
            DetailLayout::Auto => DetailLayout::Horizontal,
            DetailLayout::Horizontal => DetailLayout::Vertical,
            DetailLayout::Vertical => DetailLayout::Auto,
        }
    }
}

/// The DSC settings set from the Work Area's Tools menu (spec 14.3), kept
/// in the VFO's `DSC` settings table next to the schema's `message_dir`.
#[derive(Debug, Clone, PartialEq)]
pub struct DscPrefs {
    pub location: Option<(f64, f64)>,
    pub lookup: bool,
    pub api_key: String,
    /// Empty = [`DEFAULT_ENDPOINT`].
    pub endpoint: String,
    pub alert_sound: bool,
    pub alert_urgency: bool,
    pub desktop_notify: bool,
    /// Where the last message text was saved (0.2.1 and 0.2.2; kept in the
    /// settings, unused since Save offers the message folder).
    pub save_dir: String,
    pub detail_layout: DetailLayout,
}

impl Default for DscPrefs {
    fn default() -> Self {
        Self {
            location: None,
            lookup: false,
            api_key: String::new(),
            endpoint: String::new(),
            alert_sound: true,
            alert_urgency: false,
            desktop_notify: true,
            save_dir: String::new(),
            detail_layout: DetailLayout::Auto,
        }
    }
}

type Settings = serde_json::Map<String, serde_json::Value>;

impl DscPrefs {
    /// From the VFO's settings table (missing or bad keys = defaults).
    pub fn from_settings(s: &Settings) -> Self {
        let d = Self::default();
        let b = |k: &str, def: bool| s.get(k).and_then(|v| v.as_bool()).unwrap_or(def);
        let t = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        Self {
            location: super::geo::parse_location(&t("location")),
            lookup: b("lookup", d.lookup),
            api_key: t("api_key"),
            endpoint: t("endpoint"),
            alert_sound: b("alert_sound", d.alert_sound),
            alert_urgency: b("alert_urgency", d.alert_urgency),
            desktop_notify: b("desktop_notify", d.desktop_notify),
            save_dir: t("save_dir"),
            detail_layout: match t("detail_layout").as_str() {
                "Horizontal" => DetailLayout::Horizontal,
                "Vertical" => DetailLayout::Vertical,
                _ => DetailLayout::Auto,
            },
        }
    }

    /// Into the VFO's settings table (other keys kept).
    pub fn write_to(&self, s: &mut Settings) {
        use serde_json::Value;
        let loc = self
            .location
            .map(|(a, b)| format!("{a}, {b}"))
            .unwrap_or_default();
        s.insert("location".into(), Value::from(loc));
        s.insert("lookup".into(), Value::from(self.lookup));
        s.insert("api_key".into(), Value::from(self.api_key.clone()));
        s.insert("endpoint".into(), Value::from(self.endpoint.clone()));
        s.insert("alert_sound".into(), Value::from(self.alert_sound));
        s.insert("alert_urgency".into(), Value::from(self.alert_urgency));
        s.insert("desktop_notify".into(), Value::from(self.desktop_notify));
        s.insert("save_dir".into(), Value::from(self.save_dir.clone()));
        s.insert(
            "detail_layout".into(),
            Value::from(self.detail_layout.key()),
        );
    }

    /// The endpoint in use.
    pub fn endpoint(&self) -> &str {
        if self.endpoint.trim().is_empty() {
            DEFAULT_ENDPOINT
        } else {
            self.endpoint.trim()
        }
    }
}

/// The active alert (seed ALR-02).
#[derive(Debug, Clone, PartialEq)]
pub struct AlertInfo {
    pub seq: u64,
    pub summary: String,
    pub body: String,
    pub severity: Severity,
}

/// A decoded call as the service receives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reception {
    pub symbols: Vec<u8>,
    /// UTC seconds of its phasing.
    pub utc: i64,
    pub time_source: TimeSource,
    /// The channel label (`8414.5 kHz`).
    pub label: String,
    /// The baseband file it was played from, if any.
    pub file: Option<String>,
}

enum WriteJob {
    Record(PathBuf, Box<Record>),
    Barrier(Sender<()>),
}

/// The result of a lookup worker.
struct LookupDone {
    seq: u64,
    found: Vec<(String, lookup::Fix)>,
    failure: Option<String>,
}

/// A record waiting for its lookup before it is written.
struct Pending {
    record: Record,
    dir: PathBuf,
}

/// Side effects the UI performs (the terminal bell, a desktop notification
/// already went out from here).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Effects {
    /// Ring the bell (a new alert with the sound on).
    pub bell: bool,
}

/// The service.
/// What the Report needs of an accepted call.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportedCall {
    pub severity: Severity,
    pub garbled: bool,
    /// The caller: MMSI and name when known.
    pub from: String,
    /// The channel it came in on.
    pub channel: String,
}

pub struct DscService {
    /// Calls accepted since the Report last took them (spec 20.3).
    reported: Vec<ReportedCall>,
    dir: PathBuf,
    tz: TimeZone,
    prefs: DscPrefs,
    stations: Stations,
    store: Store,
    view: ListView,
    next_seq: u64,
    /// The local date in view (`YYYY-MM-DD`).
    day: String,
    /// Bytes of today's file already read.
    offset: u64,
    /// Lines this service wrote (or is about to) that the file follower has
    /// not met yet, with their counts.
    own_lines: HashMap<String, usize>,
    writer: Sender<WriteJob>,
    write_errors: Receiver<String>,
    lookups_tx: Sender<LookupDone>,
    lookups_rx: Receiver<LookupDone>,
    pending: HashMap<u64, Pending>,
    key_told: bool,
    alert: Option<AlertInfo>,
    notices: VecDeque<String>,
    effects: Effects,
    /// Desktop notifications (replaceable in tests).
    notifier: fn(&str, &str, bool),
}

fn desktop_notify(summary: &str, body: &str, critical: bool) {
    let _ = super::notify::desktop(summary, body, critical);
}

impl DscService {
    /// The service for `dir`, viewing the local day of `now`.
    pub fn new(dir: PathBuf, tz: TimeZone, prefs: DscPrefs, now: i64) -> Self {
        let (writer, jobs) = unbounded::<WriteJob>();
        let (err_tx, write_errors) = unbounded::<String>();
        let _ = std::thread::Builder::new()
            .name("dsc-writer".into())
            .spawn(move || {
                for job in jobs {
                    match job {
                        WriteJob::Record(dir, r) => {
                            if let Err(e) = dayfile::append(&dir, &r) {
                                let _ = err_tx.send(format!("cannot write the message: {e}"));
                            }
                        }
                        WriteJob::Barrier(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        let (lookups_tx, lookups_rx) = unbounded();
        let mut s = Self {
            stations: Stations::load_dir(&dir),
            dir,
            tz,
            prefs,
            store: Store::default(),
            view: ListView::new(),
            next_seq: 1,
            day: String::new(),
            offset: 0,
            own_lines: HashMap::new(),
            writer,
            write_errors,
            lookups_tx,
            lookups_rx,
            pending: HashMap::new(),
            key_told: false,
            reported: Vec::new(),
            alert: None,
            notices: VecDeque::new(),
            effects: Effects::default(),
            notifier: desktop_notify,
        };
        s.open_day(now);
        s
    }

    /// Replace the desktop notifier (tests).
    pub fn set_notifier(&mut self, f: fn(&str, &str, bool)) {
        self.notifier = f;
    }

    /// The message folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The local date in view.
    pub fn day(&self) -> &str {
        &self.day
    }

    /// Today's day file.
    pub fn day_file(&self) -> PathBuf {
        self.dir.join(format!("{}.ndjson", self.day))
    }

    pub fn prefs(&self) -> &DscPrefs {
        &self.prefs
    }

    /// New Tools settings (the VFO's table changed).
    pub fn set_prefs(&mut self, prefs: DscPrefs) {
        self.prefs = prefs;
    }

    /// Switch to another message folder: reload the stations and the view.
    pub fn set_dir(&mut self, dir: PathBuf, now: i64) {
        if dir == self.dir {
            return;
        }
        self.flush();
        self.stations = Stations::load_dir(&dir);
        self.dir = dir;
        self.own_lines.clear();
        self.day.clear();
        self.open_day(now);
    }

    /// Reload the station files (Tools menu).
    pub fn reload_stations(&mut self) {
        self.stations = Stations::load_dir(&self.dir);
        self.notice(format!(
            "stations reloaded from {}",
            self.dir.join("stations").display()
        ));
    }

    fn local_date(&self, utc: i64) -> String {
        time::local_date(utc, time::offset_at(&self.tz, utc))
    }

    /// View the local day of `now` from its start.
    fn open_day(&mut self, now: i64) {
        self.day = self.local_date(now);
        self.offset = 0;
        self.store.replace(Vec::new(), vec![self.day_file()]);
        self.follow_file();
    }

    /// The periodic work (about once a second): midnight, the day file
    /// grown by others, finished lookups, writer errors.
    pub fn tick(&mut self, now: i64) {
        if self.local_date(now) != self.day {
            self.flush();
            self.own_lines.clear();
            self.open_day(now);
        } else {
            self.follow_file();
        }
        while let Ok(done) = self.lookups_rx.try_recv() {
            self.finish_lookup(done, now);
        }
        while let Ok(e) = self.write_errors.try_recv() {
            self.notice(e);
        }
    }

    /// Read what was appended to today's file since the last read: whole
    /// lines only (a line being written is read next time).
    fn follow_file(&mut self) {
        let path = self.day_file();
        let Ok(mut f) = std::fs::File::open(&path) else {
            return;
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            // Replaced or truncated: read it again from the start.
            self.offset = 0;
            self.store.replace(Vec::new(), vec![path.clone()]);
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        }
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else {
            return;
        };
        self.offset += end as u64 + 1;
        let mut added = false;
        for line in bytes[..end].split(|&b| b == b'\n') {
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Some(n) = self.own_lines.get_mut(line) {
                *n -= 1;
                if *n == 0 {
                    self.own_lines.remove(line);
                }
                continue;
            }
            if let Some(r) = ndjson::from_line(line) {
                self.add_entry(r, path.clone());
                added = true;
            }
        }
        if added {
            self.view.update(self.store.snapshot());
        }
    }

    fn add_entry(&mut self, record: Record, file: PathBuf) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.store.add(Arc::new(Entry { seq, record, file }));
        seq
    }

    /// A call decoded by this VFO (seed 02-architecture "Data flow").
    /// The calls accepted since the last call (for the Report).
    pub fn take_reported(&mut self) -> Vec<ReportedCall> {
        std::mem::take(&mut self.reported)
    }

    pub fn accept(&mut self, rx: Reception, now: i64) {
        let offset = time::offset_at(&self.tz, rx.utc);
        let mut r = Record::new(rx.symbols, rx.utc, offset, rx.time_source);
        r.source_label = Some(rx.label);
        r.file = rx.file;
        // Cache a received position, then enrich (ENR-04, ENR-05).
        if let Some((id, pos)) = enrich::observed_position(&r)
            && let Err(e) = self.stations.observe(&id, None, pos)
        {
            self.notice(format!("cannot write the position cache: {e}"));
        }
        enrich::enrich(&mut r, &self.stations, self.prefs.location);
        self.reported.push(ReportedCall {
            severity: r.message.severity,
            garbled: r.message.garbled_count > 0,
            from: match (&r.message.from, &r.from_st) {
                (Some(id), Some(st)) => match &st.name {
                    Some(n) => format!("{} {n}", id.digits),
                    None => id.digits.clone(),
                },
                (Some(id), None) => id.digits.clone(),
                (None, _) => "unknown".to_string(),
            },
            channel: r.source_label.clone().unwrap_or_default(),
        });
        let targets = if self.prefs.lookup && now - r.utc <= 3600 {
            lookup_targets(&r, &self.stations)
        } else {
            Vec::new()
        };
        let file = dayfile::path_for(&self.dir, &r);
        let today = self.local_date(r.utc) == self.day;
        let seq = if today {
            let seq = self.add_entry(r.clone(), file.clone());
            self.view.update(self.store.snapshot());
            seq
        } else {
            self.notice(format!(
                "a message of {} was written to {} (only today is shown)",
                self.local_date(r.utc),
                file.display()
            ));
            0
        };

        // Write now, or after the lookup (LKP-03).
        let mut write_now = true;
        if !targets.is_empty() {
            if self.prefs.api_key.trim().is_empty() {
                if !self.key_told {
                    self.key_told = true;
                    self.notice(
                        "online lookup is on, but no API key is set: no position is looked up"
                            .into(),
                    );
                }
            } else if seq != 0 {
                write_now = false;
                self.pending.insert(
                    seq,
                    Pending {
                        record: r.clone(),
                        dir: self.dir.clone(),
                    },
                );
                let tx = self.lookups_tx.clone();
                let (endpoint, key) = (
                    self.prefs.endpoint().to_string(),
                    self.prefs.api_key.clone(),
                );
                let spawned = std::thread::Builder::new()
                    .name("dsc-lookup".into())
                    .spawn(move || run_lookup(tx, seq, targets, endpoint, key));
                if spawned.is_err() {
                    self.pending.remove(&seq);
                    write_now = true;
                }
            }
        }
        if write_now {
            self.write(r.clone());
        }

        // The alert, from the parsed fields only (ALR-01).
        if seq != 0 && alert::decide(&r.message, self.prefs.alert_urgency) {
            let (summary, body) = presentation::alert_text(&r);
            let severity = r.message.severity;
            if self.prefs.desktop_notify {
                (self.notifier)(&summary, &body, severity == Severity::Distress);
            }
            self.effects.bell |= self.prefs.alert_sound;
            self.alert = Some(AlertInfo {
                seq,
                summary,
                body,
                severity,
            });
        }
    }

    /// Queue a record for its day file, remembering its line so the file
    /// follower does not show it twice.
    fn write(&mut self, r: Record) {
        if dayfile::path_for(&self.dir, &r) == self.day_file() {
            *self.own_lines.entry(ndjson::to_line(&r)).or_insert(0) += 1;
        }
        let _ = self
            .writer
            .send(WriteJob::Record(self.dir.clone(), Box::new(r)));
    }

    fn finish_lookup(&mut self, done: LookupDone, now: i64) {
        let Some(mut p) = self.pending.remove(&done.seq) else {
            return;
        };
        for (id, fix) in &done.found {
            let pos = StationPos {
                lat: fix.lat,
                lon: fix.lon,
                source: PosSource::AprsFi,
                time: Some(fix.time.unwrap_or(now)),
            };
            let _ = self.stations.observe(id, fix.name.as_deref(), pos);
        }
        let rx = p.record.rx;
        enrich::enrich(&mut p.record, &self.stations, rx);
        if !done.found.is_empty() && self.store.update(done.seq, p.record.clone()) {
            self.view.update(self.store.snapshot());
        }
        let r = p.record;
        let dir = std::mem::replace(&mut self.dir, p.dir);
        self.write(r);
        self.dir = dir;
        if let Some(f) = done.failure {
            self.notice(f);
        }
    }

    /// Wait until every queued record is on disk (or failed).
    pub fn flush(&self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.writer.send(WriteJob::Barrier(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }

    /// Quitting: records still waiting for a lookup are written without its
    /// result (LKP-05), then everything is flushed.
    pub fn shutdown(&mut self) {
        let pending: Vec<Pending> = self.pending.drain().map(|(_, p)| p).collect();
        for p in pending {
            let dir = std::mem::replace(&mut self.dir, p.dir);
            self.write(p.record);
            self.dir = dir;
        }
        self.flush();
    }

    /// Whether a lookup is running (`API_REQ...`).
    pub fn lookup_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Messages in view (before search and filter).
    pub fn count(&self) -> usize {
        self.store.snapshot().entries.len()
    }

    pub fn view(&self) -> &ListView {
        &self.view
    }

    pub fn view_mut(&mut self) -> &mut ListView {
        &mut self.view
    }

    pub fn alert(&self) -> Option<&AlertInfo> {
        self.alert.as_ref()
    }

    pub fn acknowledge_alert(&mut self) {
        self.alert = None;
    }

    fn notice(&mut self, text: String) {
        self.notices.push_back(text);
        while self.notices.len() > 8 {
            self.notices.pop_front();
        }
    }

    pub fn take_notice(&mut self) -> Option<String> {
        self.notices.pop_front()
    }

    /// Effects to perform since the last call.
    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut self.effects)
    }

    /// The detail fields of the selected message (seed VIEW-05).
    pub fn detail_fields(&self) -> Option<Vec<(String, String)>> {
        let e = self.view.selected()?;
        Some(detail::fields(
            &e.record,
            &detail::Current {
                location: self.prefs.location,
                stations: &self.stations,
            },
        ))
    }

    /// The selected message's text, as saved and copied (EXP-01/02).
    pub fn detail_text(&self) -> Option<String> {
        self.detail_fields().map(|f| detail::text(&f))
    }

    /// Where the selected message is saved by default: the message folder
    /// (by default the DSC recordings folder) and the EXP-01 name.
    pub fn default_save_path(&self) -> Option<PathBuf> {
        let e = self.view.selected()?;
        Some(self.dir.join(detail::save_file_name(PROJECT, &e.record)))
    }

    /// Save the selected message's text to `path`.
    pub fn save_selected(&self, path: &Path) -> Result<(), String> {
        let text = self.detail_text().ok_or("no message selected")?;
        if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

impl std::fmt::Debug for DscService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DscService")
            .field("dir", &self.dir)
            .field("day", &self.day)
            .field("count", &self.count())
            .finish_non_exhaustive()
    }
}

impl Drop for DscService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The identities to look up (LKP-02): from, to and distress, not coast
/// stations or groups, without a fresh position.
fn lookup_targets(r: &Record, stations: &Stations) -> Vec<String> {
    let m = &r.message;
    let mut out: Vec<String> = Vec::new();
    for id in [&m.from, &m.to, &m.distress_mmsi].into_iter().flatten() {
        let d = &id.digits;
        if d.len() == 9 && !d.starts_with('0') && !stations.is_fresh(d, r.utc) && !out.contains(d) {
            out.push(d.clone());
        }
    }
    out
}

fn run_lookup(
    tx: Sender<LookupDone>,
    seq: u64,
    targets: Vec<String>,
    endpoint: String,
    key: String,
) {
    let mut found = Vec::new();
    let mut failure = None;
    for id in targets {
        match lookup::fetch(&endpoint, &key, &id, LOOKUP_TIMEOUT) {
            Ok(fix) => found.push((id, fix)),
            Err(e) => {
                failure.get_or_insert(format!("{id}: {e}"));
            }
        }
    }
    let _ = tx.send(LookupDone {
        seq,
        found,
        failure,
    });
}

/// Copy `text` to the clipboard with the first of `wl-copy`, `xclip`,
/// `xsel` found (seed EXP-02). Returns the tool used.
pub fn copy_to_clipboard(text: &str) -> Result<&'static str, String> {
    use std::io::Write;
    let tools: [(&str, &[&str]); 3] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (tool, args) in tools {
        let child = std::process::Command::new(tool)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            if let Some(mut stdin) = c.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = c.wait();
            return Ok(tool);
        }
    }
    Err("no clipboard tool (wl-copy, xclip or xsel)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::enrich::samples;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 2026-10-20 12:00:00 UTC (15:00 in Vilnius).
    const NOON: i64 = 1_792_497_600;

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "shellsdr-dsc-{name}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Vilnius").unwrap()
    }

    fn quiet(_: &str, _: &str, _: bool) {}

    fn service(dir: &Dir, prefs: DscPrefs, now: i64) -> DscService {
        let mut s = DscService::new(dir.0.clone(), tz(), prefs, now);
        s.set_notifier(quiet);
        s
    }

    fn rx(symbols: &[u8], utc: i64) -> Reception {
        Reception {
            symbols: symbols.to_vec(),
            utc,
            time_source: TimeSource::Wallclock,
            label: "8414.5 kHz".into(),
            file: None,
        }
    }

    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    #[test]
    fn a_call_is_shown_at_once_and_appended_to_todays_file() {
        let d = Dir::new("accept");
        let mut s = service(&d, DscPrefs::default(), NOON);
        assert_eq!(s.day(), "2026-10-20");
        s.accept(rx(&samples::SAFETY, NOON - 5), NOON);
        assert_eq!(s.count(), 1);
        assert_eq!(s.view().len(), 1);
        let e = s.view().selected().unwrap().clone();
        assert_eq!(e.record.source_label.as_deref(), Some("8414.5 kHz"));
        assert_eq!(e.record.offset, 3 * 3600, "Vilnius summer time");
        s.flush();
        let l = lines(&d.0.join("2026-10-20.ndjson"));
        assert_eq!(l.len(), 1);
        assert_eq!(ndjson::from_line(&l[0]).unwrap().symbols, samples::SAFETY);
    }

    #[test]
    fn own_lines_are_not_shown_twice_but_another_writers_are() {
        let d = Dir::new("follow");
        let mut s = service(&d, DscPrefs::default(), NOON);
        s.accept(rx(&samples::SAFETY, NOON - 5), NOON);
        s.flush();
        s.tick(NOON + 1);
        assert_eq!(s.count(), 1, "own line recognised");
        // Another instance appends a call to the same day file.
        let mut other = Record::new(
            samples::SAFETY.to_vec(),
            NOON + 2,
            10800,
            TimeSource::Wallclock,
        );
        other.source_label = Some("12577.0 kHz".into());
        dayfile::append(&d.0, &other).unwrap();
        s.tick(NOON + 3);
        assert_eq!(s.count(), 2);
        assert!(s.alert().is_none(), "file lines never alert");
    }

    #[test]
    fn todays_file_is_loaded_at_start_and_a_torn_line_waits() {
        let d = Dir::new("start");
        let r = Record::new(
            samples::SAFETY.to_vec(),
            NOON - 60,
            10800,
            TimeSource::Wallclock,
        );
        dayfile::append(&d.0, &r).unwrap();
        // Yesterday's file is not shown.
        let old = Record::new(
            samples::SAFETY.to_vec(),
            NOON - 86_400,
            10800,
            TimeSource::Wallclock,
        );
        dayfile::append(&d.0, &old).unwrap();
        let path = d.0.join("2026-10-20.ndjson");
        let mut torn = ndjson::to_line(&r);
        let half = torn.split_off(torn.len() / 2);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(torn.as_bytes()).unwrap();
        }
        let mut s = service(&d, DscPrefs::default(), NOON);
        assert_eq!(s.count(), 1);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(format!("{half}\n").as_bytes()).unwrap();
        }
        s.tick(NOON + 1);
        assert_eq!(s.count(), 2, "the line completed");
    }

    #[test]
    fn midnight_switches_to_the_new_empty_day() {
        let d = Dir::new("midnight");
        let mut s = service(&d, DscPrefs::default(), NOON);
        s.accept(rx(&samples::SAFETY, NOON), NOON);
        // 2026-10-20 21:00 UTC = 00:00 on the 21st in Vilnius.
        let midnight = NOON + 9 * 3600;
        s.tick(midnight - 1);
        assert_eq!(s.count(), 1);
        s.tick(midnight);
        assert_eq!(s.day(), "2026-10-21");
        assert_eq!(s.count(), 0);
        s.accept(rx(&samples::SAFETY, midnight + 5), midnight + 5);
        s.flush();
        assert_eq!(lines(&d.0.join("2026-10-21.ndjson")).len(), 1);
        assert_eq!(lines(&d.0.join("2026-10-20.ndjson")).len(), 1);
    }

    #[test]
    fn a_call_of_another_day_is_written_there_and_not_shown() {
        let d = Dir::new("otherday");
        let mut s = service(&d, DscPrefs::default(), NOON);
        let mut r = rx(&samples::SAFETY, NOON - 3 * 86_400);
        r.time_source = TimeSource::Filename;
        r.file = Some("x.wav".into());
        s.accept(r, NOON);
        assert_eq!(s.count(), 0);
        assert!(s.take_notice().unwrap().contains("2026-10-17"));
        s.flush();
        assert_eq!(lines(&d.0.join("2026-10-17.ndjson")).len(), 1);
    }

    #[test]
    fn distress_alerts_rings_and_acknowledges() {
        let d = Dir::new("alert");
        let mut s = service(&d, DscPrefs::default(), NOON);
        s.accept(rx(&samples::DISTRESS, NOON), NOON);
        let a = s.alert().cloned().expect("alert");
        assert_eq!(a.severity, Severity::Distress);
        assert!(a.summary.contains("DISTRESS"));
        assert!(s.take_effects().bell);
        assert!(!s.take_effects().bell, "once");
        s.acknowledge_alert();
        assert!(s.alert().is_none());
        s.set_prefs(DscPrefs {
            alert_sound: false,
            ..DscPrefs::default()
        });
        s.accept(rx(&samples::DISTRESS, NOON + 1), NOON + 1);
        assert!(s.alert().is_some() && !s.take_effects().bell);
    }

    #[test]
    fn lookup_without_a_key_tells_once_and_writes_at_once() {
        let d = Dir::new("nokey");
        let prefs = DscPrefs {
            lookup: true,
            ..DscPrefs::default()
        };
        let mut s = service(&d, prefs, NOON);
        s.accept(rx(&samples::SAFETY, NOON), NOON);
        s.accept(rx(&samples::SAFETY, NOON + 1), NOON + 1);
        assert!(s.take_notice().unwrap().contains("no API key"));
        assert!(s.take_notice().is_none());
        assert!(!s.lookup_pending());
        s.flush();
        assert_eq!(lines(&s.day_file()).len(), 2);
    }

    #[test]
    fn a_failed_lookup_writes_the_record_and_tells_why() {
        let d = Dir::new("lookupfail");
        let prefs = DscPrefs {
            lookup: true,
            api_key: "k".into(),
            // Nothing listens there: the lookup fails fast.
            endpoint: "http://127.0.0.1:9/{mmsi}?{apikey}".into(),
            ..DscPrefs::default()
        };
        let mut s = service(&d, prefs, NOON);
        s.accept(rx(&samples::SAFETY, NOON), NOON);
        assert!(s.lookup_pending());
        let t0 = std::time::Instant::now();
        while s.lookup_pending() && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
            s.tick(NOON + 1);
        }
        assert!(!s.lookup_pending());
        assert!(s.take_notice().is_some_and(|n| n.contains(':')));
        s.flush();
        assert_eq!(lines(&s.day_file()).len(), 1);
        s.tick(NOON + 2);
        assert_eq!(s.count(), 1, "not shown twice");
    }

    #[test]
    fn shutdown_writes_records_still_waiting_for_a_lookup() {
        let d = Dir::new("shutdown");
        let prefs = DscPrefs {
            lookup: true,
            api_key: "k".into(),
            // A black-hole address: the lookup would wait for its timeout.
            endpoint: "http://10.255.255.1/{mmsi}?{apikey}".into(),
            ..DscPrefs::default()
        };
        let mut s = service(&d, prefs, NOON);
        s.accept(rx(&samples::SAFETY, NOON), NOON);
        assert!(s.lookup_pending());
        s.shutdown();
        assert_eq!(lines(&s.day_file()).len(), 1);
    }

    #[test]
    fn prefs_round_trip_through_the_settings_table() {
        let p = DscPrefs {
            location: Some((54.687, 25.28)),
            lookup: true,
            api_key: "abc".into(),
            endpoint: String::new(),
            alert_sound: false,
            alert_urgency: true,
            desktop_notify: false,
            save_dir: "/tmp".into(),
            detail_layout: DetailLayout::Vertical,
        };
        let mut s = Settings::new();
        s.insert("message_dir".into(), "x".into());
        p.write_to(&mut s);
        assert_eq!(DscPrefs::from_settings(&s), p);
        assert_eq!(s["message_dir"], "x", "other keys kept");
        assert_eq!(
            DscPrefs::from_settings(&Settings::new()),
            DscPrefs::default()
        );
        assert_eq!(p.endpoint(), DEFAULT_ENDPOINT);
    }

    #[test]
    fn save_writes_the_detail_text_offering_the_message_folder() {
        let d = Dir::new("save");
        let mut s = service(&d, DscPrefs::default(), NOON);
        assert!(s.default_save_path().is_none());
        s.accept(rx(&samples::SAFETY, NOON), NOON);
        let p = s.default_save_path().unwrap();
        assert!(p.starts_with(&d.0));
        assert!(
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("shellsdr-dsc_2026-10-20T15-00-00")
        );
        let target = d.0.join("out").join("m.txt");
        s.save_selected(&target).unwrap();
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("Format"), "{text}");
        assert!(
            s.default_save_path().unwrap().starts_with(&d.0),
            "the next Save offers the message folder again"
        );
    }

    #[test]
    fn a_new_folder_reloads_the_view() {
        let a = Dir::new("dir-a");
        let b = Dir::new("dir-b");
        let r = Record::new(
            samples::SAFETY.to_vec(),
            NOON - 60,
            10800,
            TimeSource::Wallclock,
        );
        dayfile::append(&b.0, &r).unwrap();
        dayfile::append(&b.0, &r).unwrap();
        let mut s = service(&a, DscPrefs::default(), NOON);
        assert_eq!(s.count(), 0);
        s.set_dir(b.0.clone(), NOON);
        assert_eq!(s.count(), 2);
    }
}
