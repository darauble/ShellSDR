//! The per-instance schedule file (JSON, saved atomically on every change).
//!
//! Jobs belong to one instance (one tuner): two RSPduo instances with their
//! own `--config` must not both record the same job, so the schedule lives
//! next to the instance's TOML (`default.toml` -> `default.schedule.json`),
//! not in the shared bookmarks file. Unlike the TOML (saved on exit) it is
//! saved at once, so a crash or power cut never loses a job.
//!
//! 0.3.0 (spec 19.8): the jobs live in named lists, and only the active
//! list runs. The file becomes `<stem>.schedule.v3.json` (`min_version`
//! 0.3.0), migrated once from the 0.2.x `<stem>.schedule.v2.json` (its jobs
//! become the list "Default"), which is never written again, so a 0.2.x
//! build keeps working with its own file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::job::Job;

/// The `min_version` of the 0.3.0 schedule file (lists).
pub const SCHEDULE_MIN_VERSION: &str = "0.3.0";
/// The list the 0.2.x jobs are migrated into, and a new file's only list.
pub const DEFAULT_LIST: &str = "Default";

/// A named list of jobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScheduleList {
    pub name: String,
    pub jobs: Vec<Job>,
}

/// The persisted schedule (0.3.0): the switches, the lists, and which list
/// runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleDoc {
    pub min_version: Option<String>,
    /// Master switch: when off, no job fires.
    pub enabled: bool,
    /// Start the source for a job and stop it afterwards.
    pub auto_source: bool,
    /// Show and enter times in local time (stored in UTC either way).
    pub local_time: bool,
    /// The name of the list whose jobs run.
    pub active: String,
    pub lists: Vec<ScheduleList>,
}

impl Default for ScheduleDoc {
    fn default() -> Self {
        Self {
            min_version: Some(SCHEDULE_MIN_VERSION.to_string()),
            enabled: true,
            auto_source: true,
            local_time: false,
            active: DEFAULT_LIST.to_string(),
            lists: vec![ScheduleList {
                name: DEFAULT_LIST.to_string(),
                jobs: Vec::new(),
            }],
        }
    }
}

impl ScheduleDoc {
    /// A one-list document holding `jobs` (the shape of a migrated 0.2.x
    /// file).
    pub fn with_jobs(jobs: Vec<Job>) -> Self {
        let mut d = Self::default();
        d.lists[0].jobs = jobs;
        d
    }

    /// The 0.2.x schedule as lists: its jobs become "Default", the active
    /// list.
    pub fn from_v2(v2: ScheduleDocV2) -> Self {
        Self {
            enabled: v2.enabled,
            auto_source: v2.auto_source,
            local_time: v2.local_time,
            ..Self::with_jobs(v2.jobs)
        }
    }

    /// Load from `path`: defaults when missing, defaults plus a warning when
    /// unreadable or malformed.
    pub fn load_or_default(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(doc) => (doc, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("malformed schedule {}: {e}", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("cannot read schedule {}: {e}", path.display())),
            ),
        }
    }

    /// Save atomically (stamped with the lists' `min_version`).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut doc = self.clone();
        doc.min_version = Some(SCHEDULE_MIN_VERSION.to_string());
        let json = serde_json::to_string_pretty(&doc).expect("schedule serializes");
        crate::bookmarks::model::atomic_write(path, &json)
    }
}

/// The 0.2.x schedule (one job list): read to migrate, never written by
/// 0.3.0 (spec 19.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleDocV2 {
    /// The lowest ShellSDR version that can read the file (spec 13.9);
    /// missing in 0.1.x files.
    #[serde(default)] // missing = a 0.1.x file (not the struct default)
    pub min_version: Option<String>,
    /// Master switch: when off, no job fires.
    pub enabled: bool,
    /// Start the source for a job and stop it afterwards.
    pub auto_source: bool,
    /// Show and enter times in local time (stored in UTC either way).
    pub local_time: bool,
    pub jobs: Vec<Job>,
}

impl Default for ScheduleDocV2 {
    fn default() -> Self {
        Self {
            min_version: Some(crate::version::FILE_MIN_VERSION.to_string()),
            enabled: true,
            auto_source: true,
            local_time: false,
            jobs: Vec::new(),
        }
    }
}

fn config_stem(config_path: &Path) -> String {
    config_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "default".to_string())
}

/// The schedule file of the instance whose config is `config_path`
/// (0.3.0: `<stem>.schedule.v3.json`).
pub fn schedule_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(format!("{}.schedule.v3.json", config_stem(config_path)))
}

/// The 0.2.x schedule file (`<stem>.schedule.v2.json`): read once to
/// migrate, never written by 0.3.0.
pub fn v2_schedule_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(format!("{}.schedule.v2.json", config_stem(config_path)))
}

/// The 0.1.x schedule file (`<stem>.schedule.json`): read once to migrate,
/// never written by 0.2.0.
pub fn legacy_schedule_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(format!("{}.schedule.json", config_stem(config_path)))
}

/// A loaded schedule: the document, a warning, and whether it must not be
/// written (it needs a newer ShellSDR).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSchedule {
    pub doc: ScheduleDoc,
    pub warning: Option<String>,
    pub read_only: bool,
}

/// Load the 0.3.0 schedule at `path` (spec 19.8). When it does not exist,
/// the 0.2.x file `v2` (itself migrated from the 0.1.x `legacy` when need
/// be) is read, its jobs become the list "Default", and the result is
/// written to `path`. A file that needs a newer build loads read-only.
pub fn load(path: &Path, v2: Option<&Path>, legacy: Option<&Path>) -> LoadedSchedule {
    use crate::version::{FileVersion, classify, newer_warning};
    if !path.exists()
        && let Some(v2) = v2.filter(|v| v.exists() || legacy.is_some_and(|l| l.exists()))
    {
        let old = load_v2(v2, legacy);
        let doc = ScheduleDoc::from_v2(old.doc);
        if old.warning.is_some() {
            // An unreadable old file: start empty, keep it for the user.
            return LoadedSchedule {
                doc,
                warning: old.warning,
                read_only: false,
            };
        }
        let warning = doc
            .save(path)
            .err()
            .map(|e| format!("cannot write migrated schedule {}: {e}", path.display()));
        return LoadedSchedule {
            doc,
            warning,
            read_only: false,
        };
    }
    let (doc, warning) = ScheduleDoc::load_or_default(path);
    if warning.is_some() || !path.exists() {
        return LoadedSchedule {
            doc,
            warning,
            read_only: false,
        };
    }
    match classify(doc.min_version.as_deref()) {
        FileVersion::Legacy | FileVersion::Current => LoadedSchedule {
            doc,
            warning: None,
            read_only: false,
        },
        FileVersion::Newer(v) => LoadedSchedule {
            doc,
            warning: Some(newer_warning("schedule", path, &v)),
            read_only: true,
        },
    }
}

/// A loaded 0.2.x schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedScheduleV2 {
    pub doc: ScheduleDocV2,
    pub warning: Option<String>,
    pub read_only: bool,
}

/// Load the 0.2.x schedule at `path` (spec 13.9). When it does not exist and
/// the 0.1.x file `legacy` does, that one is read, migrated and written to
/// `path`.
pub fn load_v2(path: &Path, legacy: Option<&Path>) -> LoadedScheduleV2 {
    type LoadedSchedule = LoadedScheduleV2;
    use crate::version::{FileVersion, classify, newer_warning};
    if !path.exists()
        && let Some(legacy) = legacy.filter(|l| l.exists())
    {
        let (mut doc, warning) = ScheduleDocV2::load_or_default(legacy);
        if warning.is_none() {
            doc.migrate_legacy();
            let warning = doc
                .save(path)
                .err()
                .map(|e| format!("cannot write migrated schedule {}: {e}", path.display()));
            return LoadedSchedule {
                doc,
                warning,
                read_only: false,
            };
        }
        return LoadedSchedule {
            doc,
            warning,
            read_only: false,
        };
    }
    let (mut doc, warning) = ScheduleDocV2::load_or_default(path);
    if warning.is_some() || !path.exists() {
        return LoadedSchedule {
            doc,
            warning,
            read_only: false,
        };
    }
    match classify(doc.min_version.as_deref()) {
        FileVersion::Legacy => {
            doc.migrate_legacy();
            LoadedSchedule {
                doc,
                warning: None,
                read_only: false,
            }
        }
        FileVersion::Current => LoadedSchedule {
            doc,
            warning: None,
            read_only: false,
        },
        FileVersion::Newer(v) => LoadedSchedule {
            doc,
            warning: Some(newer_warning("schedule", path, &v)),
            read_only: true,
        },
    }
}

impl ScheduleDocV2 {
    /// Load from `path`: defaults when missing, defaults plus a warning when
    /// unreadable or malformed.
    pub fn load_or_default(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(doc) => (doc, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("malformed schedule {}: {e}", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("cannot read schedule {}: {e}", path.display())),
            ),
        }
    }

    /// Bring a 0.1.x schedule to the current shape.
    pub fn migrate_legacy(&mut self) {
        self.min_version = Some(crate::version::FILE_MIN_VERSION.to_string());
        for j in &mut self.jobs {
            j.migrate_legacy();
        }
    }

    /// Save atomically (stamped with this build's `min_version`).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut doc = self.clone();
        doc.min_version = Some(crate::version::FILE_MIN_VERSION.to_string());
        let json = serde_json::to_string_pretty(&doc).expect("schedule serializes");
        crate::bookmarks::model::atomic_write(path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::window::Window;

    #[test]
    fn schedule_file_sits_next_to_the_instance_config() {
        assert_eq!(
            schedule_path(Path::new("/c/ShellSDR/default.toml")),
            PathBuf::from("/c/ShellSDR/default.schedule.v3.json")
        );
        assert_eq!(
            schedule_path(Path::new("/c/tuner2.toml")),
            PathBuf::from("/c/tuner2.schedule.v3.json")
        );
        assert_eq!(
            v2_schedule_path(Path::new("/c/tuner2.toml")),
            PathBuf::from("/c/tuner2.schedule.v2.json")
        );
        assert_eq!(
            legacy_schedule_path(Path::new("/c/tuner2.toml")),
            PathBuf::from("/c/tuner2.schedule.json")
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("shellsdr-sched-{}", std::process::id()));
        let path = dir.join("x.schedule.json");
        let doc = ScheduleDoc {
            local_time: true,
            ..ScheduleDoc::with_jobs(vec![Job {
                id: 3,
                window: Window::new(600, 630, [true; 7]),
                ..Job::default()
            }])
        };
        doc.save(&path).unwrap();
        assert_eq!(ScheduleDoc::load_or_default(&path), (doc, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_gives_defaults_and_malformed_warns() {
        let (doc, warn) = ScheduleDoc::load_or_default(Path::new("/nonexistent/s.json"));
        assert_eq!(doc, ScheduleDoc::default());
        assert!(warn.is_none());
        assert!(doc.auto_source, "auto start/stop on by default");
        let dir = std::env::temp_dir().join(format!("shellsdr-sched-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.json");
        std::fs::write(&p, "{not json").unwrap();
        let (doc, warn) = ScheduleDoc::load_or_default(&p);
        assert_eq!(doc, ScheduleDoc::default());
        assert!(warn.unwrap().contains("malformed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_schedule_is_migrated_once_and_a_newer_one_is_read_only() {
        let dir = std::env::temp_dir().join(format!("shellsdr-sched-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("p.schedule.json");
        let v2 = dir.join("p.schedule.v2.json");
        let old = r#"{"version":1,"enabled":false,"jobs":[]}"#;
        std::fs::write(&legacy, old).unwrap();
        let l = load_v2(&v2, Some(&legacy));
        assert!(l.warning.is_none() && !l.read_only);
        assert!(!l.doc.enabled);
        assert!(v2.exists());
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), old);
        std::fs::write(&v2, r#"{"min_version":"5.0.0","enabled":true}"#).unwrap();
        let l = load_v2(&v2, Some(&legacy));
        assert!(l.read_only);
        assert!(l.warning.unwrap().contains("read-only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_job_snapshot_becomes_voice_settings() {
        let dir = std::env::temp_dir().join(format!("shellsdr-sched-tun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("p.schedule.json");
        std::fs::write(
            &legacy,
            r#"{"version":1,"jobs":[{"id":1,"rec_type":"Baseband",
            "tuning":{"frequency_hz":518000,"mode":"Usb","bandwidth_hz":2400.0}}]}"#,
        )
        .unwrap();
        let l = load_v2(&dir.join("p.schedule.v2.json"), Some(&legacy));
        let j = &l.doc.jobs[0];
        assert_eq!(j.tuning.vfo_type, "Voice");
        assert_eq!(j.tuning.settings["mode"], "Usb");
        assert_eq!(j.tuning.settings["bandwidth_hz"], 2400.0);
        assert!(j.record_baseband && !j.record_native);
        let written = std::fs::read_to_string(dir.join("p.schedule.v2.json")).unwrap();
        assert!(written.contains("\"vfo_type\": \"Voice\""), "{written}");
        assert!(!written.contains("rec_type"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_v2_schedule_becomes_the_default_list_once_and_stays_for_0_2() {
        let dir = std::env::temp_dir().join(format!("shellsdr-sched-v3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let v2 = dir.join("p.schedule.v2.json");
        let v3 = dir.join("p.schedule.v3.json");
        let old = r#"{"min_version":"0.2.0","enabled":false,"auto_source":true,
            "local_time":true,"jobs":[{"id":7,"enabled":true}]}"#;
        std::fs::write(&v2, old).unwrap();
        let l = load(&v3, Some(&v2), None);
        assert!(l.warning.is_none() && !l.read_only, "{:?}", l.warning);
        assert!(
            !l.doc.enabled && l.doc.local_time,
            "the switches carry over"
        );
        assert_eq!(l.doc.active, DEFAULT_LIST);
        assert_eq!(l.doc.lists.len(), 1);
        assert_eq!(l.doc.lists[0].name, "Default");
        assert_eq!(l.doc.lists[0].jobs[0].id, 7);
        let written = std::fs::read_to_string(&v3).unwrap();
        assert!(written.contains("\"min_version\": \"0.3.0\""), "{written}");
        assert_eq!(
            std::fs::read_to_string(&v2).unwrap(),
            old,
            "0.2.x keeps its file"
        );
        // Once v3 exists the old file is not read again.
        std::fs::write(&v2, r#"{"jobs":[]}"#).unwrap();
        assert_eq!(load(&v3, Some(&v2), None).doc.lists[0].jobs.len(), 1);
        // A v3 file from a newer build is read-only.
        std::fs::write(&v3, r#"{"min_version":"9.0.0","lists":[]}"#).unwrap();
        assert!(load(&v3, Some(&v2), None).read_only);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_0_1_schedule_reaches_v3_through_v2() {
        let dir = std::env::temp_dir().join(format!("shellsdr-sched-v13-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("p.schedule.json");
        std::fs::write(&legacy, r#"{"version":1,"jobs":[{"id":2}]}"#).unwrap();
        let v3 = dir.join("p.schedule.v3.json");
        let l = load(&v3, Some(&dir.join("p.schedule.v2.json")), Some(&legacy));
        assert_eq!(l.doc.lists[0].jobs[0].id, 2);
        assert!(v3.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
