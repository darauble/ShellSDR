//! Bookmark data model, UTC schedule logic and the JSON store (pure + file IO).
//!
//! Step 7. Modeled on SDR++'s Bookmark Manager plugin
//! (`misc_modules/bookmark_manager/src/main.cpp`): bookmarks live in named
//! lists, each list can be shown on or hidden from the strip above the
//! spectrum, and a bookmark carries an optional UTC broadcast window plus the
//! weekdays it airs, so the strip can tell on-air bookmarks from off-air ones.
//! Unlike SDR++ (frequency + bandwidth + mode only), a bookmark captures every
//! per-mode setting needed to reproduce the tuning (CW tone, AM AGC), per the
//! spec (section 9).
//!
//! The store is one JSON file shared by all ShellSDR instances
//! (`~/.config/ShellSDR/bookmarks.json`), separate from the per-instance TOML.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::vfo::model::{AmAgcMode, Mode, Vfo};

/// Weekday labels, index 0 = Sunday (C `tm_wday` order, as SDR++ stores them).
pub const DAY_LABELS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

/// Name of the list a fresh store starts with (SDR++'s default list).
pub const DEFAULT_LIST: &str = "General";

/// A UTC wall-clock instant at minute resolution: `hhmm` (e.g. 1430) and the
/// weekday (0 = Sunday).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UtcNow {
    pub hhmm: u16,
    pub weekday: u8,
}

impl UtcNow {
    /// From seconds since the Unix epoch (1970-01-01 was a Thursday).
    pub fn from_unix(secs: u64) -> Self {
        let days = secs / 86_400;
        let sod = secs % 86_400;
        let hhmm = (sod / 3600) * 100 + (sod % 3600) / 60;
        Self {
            hhmm: hhmm as u16,
            weekday: ((days + 4) % 7) as u8,
        }
    }

    /// The current UTC time from the system clock.
    pub fn now() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self::from_unix(secs)
    }

    /// "HH:MM" for display.
    pub fn label(&self) -> String {
        format!("{:02}:{:02}", self.hhmm / 100, self.hhmm % 100)
    }
}

/// The local wall-clock time as HHMM, in the system time zone (the C
/// library's `localtime_r`, so `TZ` and `/etc/localtime` are honored).
/// `None` if the conversion fails.
pub fn local_hhmm_now() -> Option<u16> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(now).ok()?;
    // SAFETY: `tm` is plain data that `localtime_r` fills; both pointers are
    // valid for the duration of the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some((tm.tm_hour * 100 + tm.tm_min) as u16)
}

/// "HH:MM" for an HHMM value.
pub fn hhmm_label(hhmm: u16) -> String {
    format!("{:02}:{:02}", hhmm / 100, hhmm % 100)
}

/// Whether `hhmm` is a valid 24-hour HHMM time (SDR++ `timeValid`).
pub fn time_valid(hhmm: u16) -> bool {
    hhmm / 100 <= 23 && hhmm % 100 <= 59
}

/// Write `contents` to `path` atomically: write a sibling temp file, then
/// rename it over the target, so a reader (another instance) never sees a
/// half-written file. Creates parent directories.
pub fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// The tuning fields of a 0.1.x bookmark (and job snapshot): read only to
/// migrate them into a Voice `settings` object, never written.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LegacyVoice {
    #[serde(skip_serializing)]
    pub mode: Option<Mode>,
    #[serde(skip_serializing)]
    pub bandwidth_hz: Option<f64>,
    #[serde(skip_serializing)]
    pub cw_tone_hz: Option<f64>,
    #[serde(skip_serializing)]
    pub am_agc: Option<AmAgcMode>,
}

/// Voice bookmark settings for `mode` with an optional bandwidth and CW
/// tone (defaults otherwise).
pub fn voice_part(
    mode: Mode,
    bandwidth_hz: Option<f64>,
    cw_tone_hz: Option<f64>,
) -> crate::vfo::types::Settings {
    LegacyVoice {
        mode: Some(mode),
        bandwidth_hz,
        cw_tone_hz,
        am_agc: None,
    }
    .to_voice_settings()
}

impl LegacyVoice {
    /// The Voice settings these fields describe (defaults where missing).
    pub fn to_voice_settings(&self) -> crate::vfo::types::Settings {
        use crate::vfo::types::voice::VoiceSettings;
        let mut v = VoiceSettings::for_mode(self.mode.unwrap_or(Mode::Nfm));
        if let Some(b) = self.bandwidth_hz.filter(|b| *b > 0.0) {
            v.bandwidth_hz = b;
        }
        if let Some(t) = self.cw_tone_hz {
            v.cw_tone_hz = t;
        }
        if let Some(a) = self.am_agc {
            v.am_agc = a;
        }
        let mut s = v.clamped().to_settings();
        s.remove("snap_hz"); // not a bookmark setting
        s
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One bookmark (0.2.0, spec 13.7): its frequency and schedule, plus the VFO
/// type and that type's bookmark settings. `#[serde(default)]` so files
/// written by older/newer versions still load; a bookmark of a type this
/// build lacks keeps its settings untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub name: String,
    pub frequency_hz: u64,
    /// The VFO type (`Voice`); empty only in a 0.1.x document before
    /// migration.
    #[serde(default)]
    pub vfo_type: String,
    /// The type's bookmark settings (Voice: mode, bandwidth, CW tone, AM
    /// AGC).
    #[serde(default)]
    pub settings: crate::vfo::types::Settings,
    /// UTC start time HHMM. `start == end == 0` means "all day".
    pub start_utc: u16,
    /// UTC end time HHMM (exclusive).
    pub end_utc: u16,
    /// Weekdays it airs, index 0 = Sunday.
    pub days: [bool; 7],
    pub geoinfo: String,
    pub notes: String,
    /// 0.1.x tuning fields (migration only).
    #[serde(flatten)]
    pub legacy: LegacyVoice,
}

impl Default for Bookmark {
    fn default() -> Self {
        Self {
            name: String::new(),
            frequency_hz: 0,
            vfo_type: crate::vfo::types::VOICE.to_string(),
            settings: LegacyVoice::default().to_voice_settings(),
            start_utc: 0,
            end_utc: 0,
            days: [true; 7],
            geoinfo: String::new(),
            notes: String::new(),
            legacy: LegacyVoice::default(),
        }
    }
}

impl Bookmark {
    /// Capture the tuning of `vfo` (frequency, mode and every per-mode
    /// setting) as a new all-day, every-day bookmark named `name`.
    pub fn from_vfo(name: impl Into<String>, vfo: &Vfo) -> Self {
        let all = vfo.type_settings();
        let settings = match vfo.type_def() {
            Some(t) => t.schema(&all).bookmark_part(&all),
            None => all,
        };
        Self {
            name: name.into(),
            frequency_hz: vfo.frequency_hz,
            vfo_type: vfo.vfo_type.clone(),
            settings,
            ..Self::default()
        }
    }

    /// A Voice bookmark with `v`'s settings (tests and helpers).
    pub fn voice(
        name: impl Into<String>,
        frequency_hz: u64,
        v: crate::vfo::types::voice::VoiceSettings,
    ) -> Self {
        let legacy = LegacyVoice {
            mode: Some(v.mode),
            bandwidth_hz: Some(v.bandwidth_hz),
            cw_tone_hz: Some(v.cw_tone_hz),
            am_agc: Some(v.am_agc),
        };
        Self {
            name: name.into(),
            frequency_hz,
            settings: legacy.to_voice_settings(),
            ..Self::default()
        }
    }

    /// Bring a 0.1.x bookmark to 0.2.0: its tuning fields become Voice
    /// settings. No-op for a current one.
    pub fn migrate(&mut self) {
        if self.vfo_type.is_empty() {
            self.vfo_type = crate::vfo::types::VOICE.to_string();
            self.settings = self.legacy.to_voice_settings();
        }
        self.legacy = LegacyVoice::default();
    }

    /// The type, if it is usable in this build (None: shown, but inert).
    pub fn type_def(&self) -> Option<&'static dyn crate::vfo::types::VfoType> {
        crate::vfo::types::usable(&self.vfo_type)
    }

    /// The Voice settings, for a Voice bookmark.
    pub fn voice_settings(&self) -> Option<crate::vfo::types::voice::VoiceSettings> {
        (self.vfo_type == crate::vfo::types::VOICE)
            .then(|| crate::vfo::types::voice::VoiceSettings::from_settings(&self.settings))
    }

    /// Its channel bandwidth (Hz; 0 for an unknown type).
    pub fn bandwidth(&self) -> f64 {
        self.type_def()
            .map_or(0.0, |t| t.bandwidth_hz(&self.settings))
    }

    /// The short tuning summary for lists ("USB 2800"), `?NAVTEX` plus the
    /// raw type name for a type this build lacks.
    pub fn summary(&self) -> String {
        match self.type_def() {
            Some(t) => t.summary(&self.settings),
            None => format!("?{}", self.vfo_type),
        }
    }

    /// Whether the bookmark has a broadcast window (not "all day").
    pub fn has_window(&self) -> bool {
        !(self.start_utc == 0 && self.end_utc == 0)
    }

    /// Whether the bookmark is on air at `now` (SDR++ `bookmarkOnline`):
    /// off on unchecked weekdays; all day when start == end == 0; otherwise
    /// inside `[start, end)`, wrapping past midnight when start > end; never
    /// when start == end != 0. (SDR++ treats the end minute as inclusive in
    /// the wrapping case only; here the end is exclusive in both cases.)
    pub fn is_online(&self, now: UtcNow) -> bool {
        if !self.days[now.weekday as usize % 7] {
            return false;
        }
        crate::schedule::window::time_in_window(self.start_utc, self.end_utc, now.hhmm)
    }

    /// Compact schedule text for the details line: "0530-0630", "all day"
    /// when unscheduled, with the airing days appended ("0530-0630
    /// MoTuWeThFr") only when not every day.
    pub fn schedule_label(&self) -> String {
        let window = if self.has_window() {
            format!("{:04}-{:04}", self.start_utc, self.end_utc)
        } else {
            "all day".to_string()
        };
        if self.days.iter().all(|d| *d) {
            window
        } else {
            let days: String = DAY_LABELS
                .iter()
                .zip(self.days)
                .filter(|(_, on)| *on)
                .map(|(l, _)| *l)
                .collect();
            format!("{window} {days}")
        }
    }
}

/// A named list of bookmarks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookmarkList {
    pub name: String,
    /// Whether this list's bookmarks are drawn on the strip.
    pub show_on_strip: bool,
    pub bookmarks: Vec<Bookmark>,
}

impl Default for BookmarkList {
    fn default() -> Self {
        Self::new(DEFAULT_LIST)
    }
}

impl BookmarkList {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            show_on_strip: true,
            bookmarks: Vec::new(),
        }
    }

    /// Index of the bookmark named `name`.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.bookmarks.iter().position(|b| b.name == name)
    }
}

/// All bookmark lists (the persisted JSON document).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookmarkStore {
    /// The lowest ShellSDR version that can read the file (spec 13.9);
    /// missing in 0.1.x files.
    #[serde(default)] // missing = a 0.1.x file (not the struct default)
    pub min_version: Option<String>,
    pub lists: Vec<BookmarkList>,
}

impl Default for BookmarkStore {
    fn default() -> Self {
        Self {
            min_version: Some(crate::version::FILE_MIN_VERSION.to_string()),
            lists: vec![BookmarkList::new(DEFAULT_LIST)],
        }
    }
}

/// `base` if `taken` rejects it, else `base (1)`, `base (2)`, ... (SDR++'s
/// "New Bookmark (n)" naming).
pub fn unique_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (1..)
        .map(|i| format!("{base} ({i})"))
        .find(|n| !taken(n))
        .expect("an unused name exists")
}

impl BookmarkStore {
    /// Index of the list named `name`.
    pub fn list_index(&self, name: &str) -> Option<usize> {
        self.lists.iter().position(|l| l.name == name)
    }

    /// A list name not yet used, derived from `base`.
    pub fn unique_list_name(&self, base: &str) -> String {
        unique_name(base, |n| self.list_index(n).is_some())
    }

    /// Parse from JSON (0.1.x bookmarks are migrated on the way in).
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let mut store: Self = serde_json::from_str(s)?;
        for b in store.lists.iter_mut().flat_map(|l| l.bookmarks.iter_mut()) {
            b.migrate();
        }
        Ok(store)
    }

    /// Serialize to pretty JSON (always stamped with this build's
    /// `min_version`).
    pub fn to_json(&self) -> String {
        let mut doc = self.clone();
        doc.min_version = Some(crate::version::FILE_MIN_VERSION.to_string());
        serde_json::to_string_pretty(&doc).expect("bookmark store serializes")
    }

    /// Bring a 0.1.x store to the current shape.
    pub fn migrate_legacy(&mut self) {
        self.min_version = Some(crate::version::FILE_MIN_VERSION.to_string());
    }

    /// Load from `path`: the default store when the file does not exist, the
    /// default plus a warning when it is unreadable or malformed.
    pub fn load_or_default(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(s) => match Self::from_json(&s) {
                Ok(store) => (store, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("malformed bookmarks {}: {e}", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("cannot read bookmarks {}: {e}", path.display())),
            ),
        }
    }

    /// Save to `path` atomically (write a sibling temp file, then rename), so
    /// another instance never reads a half-written file. Creates parent dirs.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        atomic_write(path, &self.to_json())
    }
}

/// The shared bookmarks file plus the modification time we last saw, so an
/// instance can notice another instance's save and reload (several ShellSDR
/// instances share one bookmarks file).
#[derive(Debug, Clone, PartialEq)]
pub struct StoreFile {
    pub path: PathBuf,
    /// The 0.1.x file to migrate from when `path` does not exist yet.
    pub legacy: Option<PathBuf>,
    /// The file needs a newer ShellSDR: never written (spec 13.9).
    pub read_only: bool,
    seen: Option<SystemTime>,
}

impl StoreFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            legacy: None,
            read_only: false,
            seen: None,
        }
    }

    /// As [`StoreFile::new`], migrating from the 0.1.x file `legacy` the
    /// first time `path` is missing.
    pub fn with_legacy(path: impl Into<PathBuf>, legacy: impl Into<PathBuf>) -> Self {
        Self {
            legacy: Some(legacy.into()),
            ..Self::new(path)
        }
    }

    fn mtime(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
    }

    /// Load the store (default when missing), remembering the file's mtime.
    /// When the file is missing and the 0.1.x file exists, that one is read,
    /// migrated and written to `path` (the old file is left untouched). A
    /// file that needs a newer build makes this store read-only.
    pub fn load(&mut self) -> (BookmarkStore, Option<String>) {
        use crate::version::{FileVersion, classify, newer_warning};
        if !self.path.exists()
            && let Some(legacy) = self.legacy.clone().filter(|l| l.exists())
        {
            let (mut store, warn) = BookmarkStore::load_or_default(&legacy);
            if warn.is_some() {
                self.seen = self.mtime();
                return (store, warn);
            }
            store.migrate_legacy();
            let warn = store.save(&self.path).err().map(|e| {
                format!(
                    "cannot write migrated bookmarks {}: {e}",
                    self.path.display()
                )
            });
            self.seen = self.mtime();
            return (store, warn);
        }
        self.seen = self.mtime();
        let (mut store, warn) = BookmarkStore::load_or_default(&self.path);
        if warn.is_some() {
            return (store, warn);
        }
        match classify(store.min_version.as_deref()) {
            FileVersion::Legacy => {
                store.migrate_legacy();
                (store, None)
            }
            FileVersion::Current => {
                self.read_only = false;
                (store, None)
            }
            FileVersion::Newer(v) => {
                self.read_only = true;
                (store, Some(newer_warning("bookmarks", &self.path, &v)))
            }
        }
    }

    /// Save the store, remembering the new mtime (our own save is not a
    /// change "on disk" to reload). A read-only store is not written.
    pub fn save(&mut self, store: &BookmarkStore) -> std::io::Result<()> {
        if self.read_only {
            return Ok(());
        }
        store.save(&self.path)?;
        self.seen = self.mtime();
        Ok(())
    }

    /// Whether the file was written by someone else since we last loaded or
    /// saved it.
    pub fn changed_on_disk(&self) -> bool {
        match self.mtime() {
            Some(m) => Some(m) != self.seen,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bm(start: u16, end: u16) -> Bookmark {
        Bookmark {
            start_utc: start,
            end_utc: end,
            ..Bookmark::default()
        }
    }

    fn at(hhmm: u16) -> UtcNow {
        UtcNow { hhmm, weekday: 2 }
    }

    #[test]
    fn local_time_matches_the_system_clock() {
        // Independent source of truth: the `date` command's local HH:MM,
        // read before and after (the minute may tick in between).
        let date = || {
            let o = std::process::Command::new("date")
                .arg("+%H%M")
                .output()
                .ok()?;
            String::from_utf8(o.stdout).ok()?.trim().parse::<u16>().ok()
        };
        let (Some(before), Some(after)) = (date(), date()) else {
            return; // no `date` available: nothing to compare against
        };
        let got = local_hhmm_now().expect("local time");
        let after = date().unwrap_or(after);
        assert!(got == before || got == after, "{got} vs {before}/{after}");
        assert_eq!(hhmm_label(905), "09:05");
    }

    #[test]
    fn utc_from_unix_epoch_and_a_known_instant() {
        // 1970-01-01 00:00 UTC was a Thursday.
        assert_eq!(
            UtcNow::from_unix(0),
            UtcNow {
                hhmm: 0,
                weekday: 4
            }
        );
        // 1_700_000_000 = Tue 2023-11-14 22:13:20 UTC.
        assert_eq!(
            UtcNow::from_unix(1_700_000_000),
            UtcNow {
                hhmm: 2213,
                weekday: 2
            }
        );
        assert_eq!(UtcNow::from_unix(1_700_000_000).label(), "22:13");
    }

    #[test]
    fn time_validity_matches_hhmm_rules() {
        assert!(time_valid(0));
        assert!(time_valid(2359));
        assert!(time_valid(530));
        assert!(!time_valid(2400));
        assert!(!time_valid(1260));
    }

    #[test]
    fn all_day_bookmark_is_always_online() {
        assert!(bm(0, 0).is_online(at(0)));
        assert!(bm(0, 0).is_online(at(1234)));
    }

    #[test]
    fn daytime_window_is_start_inclusive_end_exclusive() {
        // All India Radio 0530-0630.
        let b = bm(530, 630);
        assert!(!b.is_online(at(529)));
        assert!(b.is_online(at(530)));
        assert!(b.is_online(at(629)));
        assert!(!b.is_online(at(630)));
    }

    #[test]
    fn window_wraps_past_midnight() {
        let b = bm(2200, 200);
        assert!(b.is_online(at(2200)));
        assert!(b.is_online(at(2359)));
        assert!(b.is_online(at(0)));
        assert!(b.is_online(at(159)));
        assert!(!b.is_online(at(200)));
        assert!(!b.is_online(at(1200)));
    }

    #[test]
    fn equal_nonzero_start_and_end_is_never_online() {
        assert!(!bm(1200, 1200).is_online(at(1200)));
    }

    #[test]
    fn unchecked_weekday_is_offline() {
        let mut b = bm(0, 0);
        b.days[2] = false; // Tuesday
        assert!(!b.is_online(at(1000)), "Tuesday unchecked");
        assert!(b.is_online(UtcNow {
            hhmm: 1000,
            weekday: 3
        }));
    }

    #[test]
    fn schedule_label_shows_window_and_restricted_days() {
        assert_eq!(bm(0, 0).schedule_label(), "all day");
        assert_eq!(bm(530, 630).schedule_label(), "0530-0630");
        let mut b = bm(530, 630);
        b.days = [false, true, true, true, true, true, false];
        assert_eq!(b.schedule_label(), "0530-0630 MoTuWeThFr");
    }

    #[test]
    fn from_vfo_captures_mode_settings() {
        let mut v = Vfo::new("Master", 7_030_000, Mode::Cw);
        v.bandwidth_hz = 300.0;
        v.cw_tone_hz = 650.0;
        v.snap_hz = 50;
        let b = Bookmark::from_vfo("CW net", &v);
        assert_eq!(b.frequency_hz, 7_030_000);
        assert_eq!(b.vfo_type, "Voice");
        let vs = b.voice_settings().unwrap();
        assert_eq!(
            (vs.mode, vs.bandwidth_hz, vs.cw_tone_hz),
            (Mode::Cw, 300.0, 650.0)
        );
        assert!(
            !b.settings.contains_key("snap_hz"),
            "snap is not a bookmark setting"
        );
        assert_eq!(b.summary(), "CW 300 Hz 650Hz");
        assert!(!b.has_window());
        assert_eq!(b.days, [true; 7]);
    }

    #[test]
    fn unique_name_appends_a_counter() {
        let taken = ["New Bookmark", "New Bookmark (1)"];
        assert_eq!(unique_name("Other", |n| taken.contains(&n)), "Other");
        assert_eq!(
            unique_name("New Bookmark", |n| taken.contains(&n)),
            "New Bookmark (2)"
        );
    }

    #[test]
    fn default_store_has_one_general_list() {
        let s = BookmarkStore::default();
        assert_eq!(s.lists.len(), 1);
        assert_eq!(s.lists[0].name, "General");
        assert!(s.lists[0].show_on_strip);
    }

    #[test]
    fn store_json_round_trips() {
        let mut s = BookmarkStore::default();
        let mut v = crate::vfo::types::voice::VoiceSettings::for_mode(Mode::Usb);
        v.bandwidth_hz = 2_300.0;
        s.lists[0].bookmarks.push(Bookmark {
            notes: "H+00/H+30".into(),
            ..Bookmark::voice("Rostov Volmet", 8_939_000, v)
        });
        // A bookmark of a type this build lacks survives untouched.
        s.lists[0].bookmarks.push(Bookmark {
            name: "NAVTEX 518".into(),
            frequency_hz: 156_525_000,
            vfo_type: "NAVTEX".into(),
            settings: serde_json::json!({"channel": 70, "format": "VHF"})
                .as_object()
                .unwrap()
                .clone(),
            ..Bookmark::default()
        });
        let mut hidden = BookmarkList::new("Español");
        hidden.show_on_strip = false;
        s.lists.push(hidden);
        let back = BookmarkStore::from_json(&s.to_json()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let s = BookmarkStore::from_json(
            r#"{"lists":[{"name":"FM","bookmarks":[{"name":"LRT","frequency_hz":102600000,"mode":"Wfm"}]}]}"#,
        )
        .unwrap();
        let b = &s.lists[0].bookmarks[0];
        assert!(s.lists[0].show_on_strip, "missing flag defaults to shown");
        assert_eq!(b.days, [true; 7]);
        assert_eq!(b.vfo_type, "Voice", "migrated");
        assert_eq!(b.settings["mode"], "Wfm");
        assert_eq!(b.settings["bandwidth_hz"], 150_000.0, "the mode's default");
        assert_eq!(s.min_version, None, "no min_version: a 0.1.x document");
        let json = s.to_json();
        assert!(
            !json.contains("\"mode\": \"Wfm\",\n      \"bandwidth"),
            "no legacy keys at the top"
        );
        assert!(json.contains("\"vfo_type\": \"Voice\""));
    }

    #[test]
    fn store_file_notices_another_writer_but_not_its_own_save() {
        let dir = std::env::temp_dir().join(format!("shellsdr-sf-{}", std::process::id()));
        let path = dir.join("bookmarks.json");
        let mut a = StoreFile::new(&path);
        let mut b = StoreFile::new(&path);
        let (store, _) = a.load();
        assert!(!a.changed_on_disk(), "missing file is not a change");
        a.save(&store).unwrap();
        assert!(!a.changed_on_disk(), "own save is not a change");
        let (_, _) = b.load();
        // Make the other writer's mtime differ even on coarse filesystems.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut other = store.clone();
        other.lists.push(BookmarkList::new("From B"));
        b.save(&other).unwrap();
        assert!(a.changed_on_disk(), "A sees B's save");
        let (reloaded, _) = a.load();
        assert_eq!(reloaded, other);
        assert!(!a.changed_on_disk());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_via_disk_and_missing_file_is_default() {
        let dir = std::env::temp_dir().join(format!("shellsdr-bm-{}", std::process::id()));
        let path = dir.join("sub").join("bookmarks.json");
        let (missing, warn) = BookmarkStore::load_or_default(&path);
        assert_eq!(missing, BookmarkStore::default());
        assert!(warn.is_none(), "a missing file is not an error");

        let mut s = BookmarkStore::default();
        s.lists.push(BookmarkList::new("Laivyba"));
        s.save(&path).unwrap();
        let (loaded, warn) = BookmarkStore::load_or_default(&path);
        assert!(warn.is_none());
        assert_eq!(loaded, s);

        std::fs::write(&path, "{ not json").unwrap();
        let (bad, warn) = BookmarkStore::load_or_default(&path);
        assert_eq!(bad, BookmarkStore::default());
        assert!(warn.is_some(), "malformed file warns");
        let _ = std::fs::remove_dir_all(&dir);
    }
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shellsdr-bm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_legacy_bookmarks_file_is_migrated_into_the_v2_file_once() {
        let dir = temp_dir("migrate");
        let legacy = dir.join("bookmarks.json");
        let v2 = dir.join("bookmarks.v2.json");
        let old = r#"{"version":1,"lists":[{"name":"FM","bookmarks":[{"name":"LRT","frequency_hz":102600000,"mode":"Wfm"}]}]}"#;
        std::fs::write(&legacy, old).unwrap();
        let mut f = StoreFile::with_legacy(&v2, &legacy);
        let (store, warn) = f.load();
        assert!(warn.is_none(), "{warn:?}");
        assert_eq!(store.lists[0].bookmarks[0].name, "LRT");
        assert!(v2.exists(), "the migrated store is written to the v2 file");
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            old,
            "old file untouched"
        );
        assert!(
            std::fs::read_to_string(&v2)
                .unwrap()
                .contains("\"min_version\": \"0.2.0\"")
        );
        // Later loads use the v2 file only.
        std::fs::write(&legacy, "{not json").unwrap();
        let (again, warn) = StoreFile::with_legacy(&v2, &legacy).load();
        assert!(warn.is_none());
        assert_eq!(again.lists[0].name, "FM");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bookmarks_file_from_a_newer_build_is_never_written() {
        let dir = temp_dir("newer");
        let v2 = dir.join("bookmarks.v2.json");
        let newer = r#"{"min_version":"9.0.0","lists":[{"name":"X","future":1}]}"#;
        std::fs::write(&v2, newer).unwrap();
        let mut f = StoreFile::new(&v2);
        let (store, warn) = f.load();
        assert!(f.read_only);
        assert!(warn.unwrap().contains("read-only"));
        assert_eq!(store.lists[0].name, "X");
        f.save(&store).unwrap();
        assert_eq!(std::fs::read_to_string(&v2).unwrap(), newer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_bookmark_keeps_all_its_tuning_as_voice_settings() {
        let s = BookmarkStore::from_json(
            r#"{"version":1,"lists":[{"name":"X","bookmarks":[{"name":"CW","frequency_hz":7030000,
            "mode":"Cw","bandwidth_hz":300.0,"cw_tone_hz":650.0,"am_agc":"Carrier"}]}]}"#,
        )
        .unwrap();
        let b = &s.lists[0].bookmarks[0];
        let v = b.voice_settings().unwrap();
        assert_eq!(
            (v.mode, v.bandwidth_hz, v.cw_tone_hz, v.am_agc),
            (Mode::Cw, 300.0, 650.0, AmAgcMode::Carrier)
        );
        assert!(b.legacy.is_empty(), "legacy fields cleared");
        let text = serde_json::to_string(b).unwrap();
        assert!(
            !text.contains("cw_tone_hz\":650.0,\"am_agc\":\"Carrier\",\"start"),
            "{text}"
        );
        assert!(text.contains("\"settings\""));
    }

    #[test]
    fn an_unknown_type_is_inert_but_kept() {
        let b = Bookmark {
            vfo_type: "NAVTEX".into(),
            ..Bookmark::default()
        };
        assert!(b.type_def().is_none());
        assert_eq!(b.summary(), "?NAVTEX");
        assert_eq!(b.bandwidth(), 0.0);
        assert!(b.voice_settings().is_none());
    }
}
