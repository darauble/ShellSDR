//! A scheduled recording (pure data).
//!
//! Every job is bookmark-driven (user decision): it names a bookmark (list +
//! name, since bookmarks live in the shared file and have no stable id),
//! keeps a snapshot of its tuning in case the bookmark is later renamed or
//! deleted, and carries its own window (copied from the bookmark's broadcast
//! schedule when chosen, then freely overridden), a repeat rule and the
//! action (record as Audio/Baseband, or only tune) and whether to mute the
//! VFO while it runs. The job's display name is the bookmark's name.

use serde::{Deserialize, Serialize};

use super::window::Window;
use crate::bookmarks::model::{Bookmark, BookmarkStore};
use crate::record::RecType;

/// Which bookmark a job records: its list and name in the shared store.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookmarkRef {
    pub list: String,
    pub name: String,
}

/// The tuning a job applies when it starts (from its bookmark): the
/// frequency, the VFO type and its bookmark settings (0.2.0, spec 13.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    pub frequency_hz: u64,
    /// Empty only in a 0.1.x snapshot before migration.
    #[serde(default)]
    pub vfo_type: String,
    #[serde(default)]
    pub settings: crate::vfo::types::Settings,
    /// 0.1.x snapshot fields (migration only).
    #[serde(flatten)]
    pub legacy: crate::bookmarks::model::LegacyVoice,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning::of(&Bookmark::default())
    }
}

impl Tuning {
    pub fn of(b: &Bookmark) -> Self {
        Self {
            frequency_hz: b.frequency_hz,
            vfo_type: b.vfo_type.clone(),
            settings: b.settings.clone(),
            legacy: Default::default(),
        }
    }

    /// Bring a 0.1.x snapshot to 0.2.0 (its tuning becomes Voice settings).
    pub fn migrate(&mut self) {
        if self.vfo_type.is_empty() {
            self.vfo_type = crate::vfo::types::VOICE.to_string();
            self.settings = self.legacy.to_voice_settings();
        }
        self.legacy = Default::default();
    }

    /// The tuning summary ("USB 2.4 kHz", `?DSC` for a type this build
    /// lacks).
    pub fn summary(&self) -> String {
        match crate::vfo::types::usable(&self.vfo_type) {
            Some(t) => t.summary(&self.settings),
            None => format!("?{}", self.vfo_type),
        }
    }

    /// The short mode label ("USB"; `?DSC` for a type this build lacks).
    pub fn mode_label(&self) -> String {
        match crate::vfo::types::usable(&self.vfo_type) {
            Some(t) => t.mode_label(&self.settings),
            None => format!("?{}", self.vfo_type),
        }
    }

    /// Whether this build can apply it.
    pub fn usable(&self) -> bool {
        crate::vfo::types::usable(&self.vfo_type).is_some()
    }
}

/// Whether a job repeats weekly or runs once and then switches itself off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Weekly,
    Once,
}

impl Repeat {
    pub fn label(self) -> &'static str {
        match self {
            Repeat::Weekly => "Weekly",
            Repeat::Once => "Once",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Repeat::Weekly => Repeat::Once,
            Repeat::Once => Repeat::Weekly,
        }
    }
}

/// What a job does when its window opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum JobAction {
    /// Tune and record (the job's Native / Baseband outputs).
    #[default]
    Record,
    /// Only tune, once, at the window's start (e.g. to listen at a set
    /// time); nothing is recorded and the end time is not used.
    Tune,
}

/// One scheduled job: a recording, or a tune-only job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    /// Stable id within the schedule file (identifies a running occurrence).
    pub id: u64,
    pub enabled: bool,
    pub bookmark: BookmarkRef,
    /// The bookmark's tuning when the job was last saved (fallback).
    pub tuning: Tuning,
    pub window: Window,
    pub repeat: Repeat,
    /// Record or only tune. Older files lack it: Record.
    pub action: JobAction,
    /// The outputs a Record job records (0.2.0, spec 13.8); Minimize comes
    /// from the VFO's own setting for the type.
    pub record_native: bool,
    pub record_baseband: bool,
    /// The 0.1.x recorder type, read only to migrate (never written).
    #[serde(skip_serializing)]
    pub rec_type: Option<RecType>,
    /// Mute the VFO's audio while the job runs (restored afterwards).
    pub mute: bool,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            id: 0,
            enabled: true,
            bookmark: BookmarkRef::default(),
            tuning: Tuning::default(),
            window: Window::default(),
            repeat: Repeat::Weekly,
            action: JobAction::Record,
            record_native: true,
            record_baseband: false,
            rec_type: None,
            mute: false,
        }
    }
}

impl Job {
    /// Whether the job may fire: switched on and its VFO type usable in
    /// this build (spec 13.8).
    pub fn can_fire(&self) -> bool {
        self.enabled && self.tuning.usable()
    }

    /// Whether the job records (as opposed to only tuning).
    pub fn records(&self) -> bool {
        self.action == JobAction::Record
    }

    /// When the job fires: its window, or for a "Tune only" job just the
    /// start minute (older files may hold any end there).
    pub fn fire_window(&self) -> Window {
        match self.action {
            JobAction::Record => self.window,
            JobAction::Tune => self.window.first_minute(),
        }
    }

    /// The time label for `w` (the job's window in the shown clock):
    /// "0600-0630", "all day", or only "0600" for a "Tune only" job.
    pub fn time_label(&self, w: &Window) -> String {
        match self.action {
            JobAction::Record => w.time_label(),
            JobAction::Tune => format!("{:04}", w.start),
        }
    }

    /// The job list's action letters: `T` tune only, `N` native, `B`
    /// baseband, `NB` both.
    pub fn kind_label(&self) -> &'static str {
        match (self.action, self.record_native, self.record_baseband) {
            (JobAction::Tune, _, _) => "T",
            (JobAction::Record, true, true) => "NB",
            (JobAction::Record, false, true) => "B",
            (JobAction::Record, _, _) => "N",
        }
    }

    /// Bring a 0.1.x job to the current shape: its recorder type becomes
    /// the Native / Baseband outputs.
    pub fn migrate_legacy(&mut self) {
        self.tuning.migrate();
        if let Some(t) = self.rec_type.take() {
            self.record_native = t == RecType::Audio;
            self.record_baseband = t == RecType::Baseband;
        }
    }

    /// The job's name (its bookmark's name).
    pub fn name(&self) -> &str {
        &self.bookmark.name
    }

    /// The job's bookmark in `store`, if it still exists.
    pub fn find_bookmark<'a>(&self, store: &'a BookmarkStore) -> Option<&'a Bookmark> {
        store
            .lists
            .iter()
            .find(|l| l.name == self.bookmark.list)
            .and_then(|l| l.bookmarks.iter().find(|b| b.name == self.bookmark.name))
    }

    /// The tuning to apply: the live bookmark's, else the saved snapshot.
    pub fn resolve_tuning(&self, store: &BookmarkStore) -> Tuning {
        self.find_bookmark(store)
            .map(Tuning::of)
            .unwrap_or_else(|| self.tuning.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bookmarks::model::BookmarkList;

    fn store_with(b: Bookmark) -> BookmarkStore {
        let mut l = BookmarkList::new("Ships");
        l.bookmarks.push(b);
        BookmarkStore {
            lists: vec![BookmarkList::new("General"), l],
            ..BookmarkStore::default()
        }
    }

    fn navtex() -> Bookmark {
        Bookmark {
            name: "NAVTEX".into(),
            frequency_hz: 518_000,
            settings: crate::bookmarks::model::voice_part(
                crate::vfo::model::Mode::Usb,
                Some(2_400.0),
                None,
            ),
            ..Bookmark::default()
        }
    }

    fn job() -> Job {
        Job {
            bookmark: BookmarkRef {
                list: "Ships".into(),
                name: "NAVTEX".into(),
            },
            tuning: Tuning::of(&navtex()),
            ..Job::default()
        }
    }

    #[test]
    fn live_bookmark_tuning_wins_over_the_snapshot() {
        let mut b = navtex();
        b.frequency_hz = 490_000;
        let t = job().resolve_tuning(&store_with(b));
        assert_eq!(t.frequency_hz, 490_000, "bookmark edited since");
    }

    #[test]
    fn missing_bookmark_falls_back_to_the_snapshot() {
        let j = job();
        let store = BookmarkStore::default();
        assert!(j.find_bookmark(&store).is_none());
        assert_eq!(j.resolve_tuning(&store).frequency_hz, 518_000);
    }

    #[test]
    fn same_name_in_another_list_is_not_the_bookmark() {
        let mut store = store_with(navtex());
        store.lists[0].bookmarks.push(Bookmark {
            name: "NAVTEX".into(),
            frequency_hz: 1,
            ..Bookmark::default()
        });
        assert_eq!(job().resolve_tuning(&store).frequency_hz, 518_000);
    }

    #[test]
    fn job_round_trips_through_json_and_old_files_load() {
        let j = job();
        let s = serde_json::to_string(&j).unwrap();
        assert_eq!(serde_json::from_str::<Job>(&s).unwrap(), j);
        let partial: Job = serde_json::from_str(r#"{"id":7}"#).unwrap();
        assert_eq!(partial.id, 7);
        assert!(partial.enabled, "missing fields take defaults");
        assert_eq!(partial.action, JobAction::Record, "old jobs record");
        assert!(!partial.mute, "old jobs do not mute");
        let tune = Job {
            action: JobAction::Tune,
            mute: true,
            ..job()
        };
        let s = serde_json::to_string(&tune).unwrap();
        assert_eq!(serde_json::from_str::<Job>(&s).unwrap(), tune);
    }

    #[test]
    fn kind_label_names_the_action_and_outputs() {
        let mut j = job();
        assert_eq!(j.kind_label(), "N");
        j.record_baseband = true;
        assert_eq!(j.kind_label(), "NB");
        j.record_native = false;
        assert_eq!(j.kind_label(), "B");
        j.action = JobAction::Tune;
        assert_eq!(j.kind_label(), "T");
        assert!(!j.records());
    }

    #[test]
    fn a_legacy_job_type_becomes_the_outputs() {
        let mut j: Job = serde_json::from_str(r#"{"id":1,"rec_type":"Baseband"}"#).unwrap();
        j.migrate_legacy();
        assert!(j.record_baseband && !j.record_native);
        let text = serde_json::to_string(&j).unwrap();
        assert!(!text.contains("rec_type"), "never written");
        assert!(text.contains("\"record_baseband\":true"));
    }
}
