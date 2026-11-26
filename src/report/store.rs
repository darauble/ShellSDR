//! The Report's files in its folder (spec 20.5): only the folder is
//! configured; the names are fixed and start with the profile:
//! `<profile>_report.v1.json` (counters), `<profile>_log_YYYY-MM-DD.log`
//! (one event per line), `<profile>_report_YYYY-MM-DD.md` (the midnight
//! report).

use std::io::Write;
use std::path::{Path, PathBuf};

use super::model::{Event, ReportDoc};

/// The profile's report files in one folder.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportFiles {
    pub dir: PathBuf,
    pub profile: String,
}

impl ReportFiles {
    pub fn new(dir: impl Into<PathBuf>, profile: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            profile: profile.into(),
        }
    }

    pub fn report_path(&self) -> PathBuf {
        self.dir.join(format!("{}_report.v1.json", self.profile))
    }

    pub fn log_path(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{}_log_{day}.log", self.profile))
    }

    pub fn markdown_path(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{}_report_{day}.md", self.profile))
    }

    /// The counters: `Ok(None)` when there is no file yet.
    pub fn load(&self) -> Result<Option<ReportDoc>, String> {
        let p = self.report_path();
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| format!("malformed report {}: {e}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", p.display())),
        }
    }

    /// Write the counters atomically (creating the folder).
    pub fn save(&self, doc: &ReportDoc) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let p = self.report_path();
        let json = serde_json::to_string_pretty(doc).expect("report serializes");
        crate::bookmarks::model::atomic_write(&p, &json)
            .map_err(|e| format!("{}: {e}", p.display()))
    }

    /// Append `lines` to the log of `day`.
    pub fn append(&self, day: &str, lines: &[String]) -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let p = self.log_path(day);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        let mut text = lines.join("\n");
        text.push('\n');
        f.write_all(text.as_bytes())
            .map_err(|e| format!("{}: {e}", p.display()))
    }

    /// The events of `day`'s log (unreadable lines skipped).
    pub fn read_log(&self, day: &str) -> Vec<Event> {
        std::fs::read_to_string(self.log_path(day))
            .map(|s| s.lines().filter_map(Event::parse).collect())
            .unwrap_or_default()
    }

    /// The days that have a log, oldest first.
    pub fn log_days(&self) -> Vec<String> {
        let prefix = format!("{}_log_", self.profile);
        let mut days: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                let day = n.strip_prefix(&prefix)?.strip_suffix(".log")?.to_string();
                (day.len() == 10).then_some(day)
            })
            .collect();
        days.sort();
        days
    }

    /// A month's things heard (`seen`): `YYYY-MM`.
    pub fn seen_path(&self, month: &str) -> PathBuf {
        self.dir.join(format!("{}_seen_{month}.json", self.profile))
    }

    /// The months with a `seen` file, oldest first.
    pub fn seen_months(&self) -> Vec<String> {
        let prefix = format!("{}_seen_", self.profile);
        let mut v: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                let m = n.strip_prefix(&prefix)?.strip_suffix(".json")?;
                (m.len() == 7).then(|| m.to_string())
            })
            .collect();
        v.sort();
        v
    }

    /// A month's things heard: `Ok(None)` when there is no file.
    pub fn load_seen(&self, month: &str) -> Result<Option<super::seen::SeenMonth>, String> {
        let p = self.seen_path(month);
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| format!("malformed {}: {e}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", p.display())),
        }
    }

    pub fn save_seen(&self, month: &str, m: &super::seen::SeenMonth) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let p = self.seen_path(month);
        let json = serde_json::to_string(m).expect("seen serializes");
        crate::bookmarks::model::atomic_write(&p, &json)
            .map_err(|e| format!("{}: {e}", p.display()))
    }

    /// Delete the `seen` months before `first` (`YYYY-MM`); returns how many.
    pub fn prune_seen(&self, first: &str) -> usize {
        self.seen_months()
            .into_iter()
            .filter(|m| m.as_str() < first)
            .filter(|m| std::fs::remove_file(self.seen_path(m)).is_ok())
            .count()
    }

    /// Delete the logs from before `first` (`YYYY-MM-DD`); returns how many.
    pub fn prune_logs(&self, first: &str) -> usize {
        self.log_days()
            .into_iter()
            .filter(|d| d.as_str() < first)
            .filter(|d| std::fs::remove_file(self.log_path(d)).is_ok())
            .count()
    }

    /// Every file of this profile in the folder (for moving it elsewhere).
    pub fn profile_files(&self) -> Vec<PathBuf> {
        let prefix = format!("{}_", self.profile);
        let mut v: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with(&prefix)
                        && (n.ends_with(".log") || n.ends_with(".md") || n.ends_with(".json"))
                })
            })
            .collect();
        v.sort();
        v
    }
}

/// The profile name of a config file: its stem (`default`, `tuner2`).
pub fn profile_of(config_path: &Path) -> String {
    config_path.file_stem().map_or_else(
        || "default".to_string(),
        |s| s.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::{Category, Clock, Level};

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shellsdr-report-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn names_carry_the_profile_and_two_profiles_share_a_folder() {
        let a = ReportFiles::new("/nas/sdr", "default");
        let b = ReportFiles::new("/nas/sdr", "tuner2");
        assert_eq!(
            a.report_path(),
            PathBuf::from("/nas/sdr/default_report.v1.json")
        );
        assert_eq!(
            b.log_path("2026-11-24"),
            PathBuf::from("/nas/sdr/tuner2_log_2026-11-24.log")
        );
        assert_eq!(
            a.markdown_path("2026-11-24"),
            PathBuf::from("/nas/sdr/default_report_2026-11-24.md")
        );
        assert_eq!(profile_of(Path::new("/c/ShellSDR/tuner2.toml")), "tuner2");
    }

    #[test]
    fn logs_append_read_back_and_old_ones_are_pruned() {
        let d = temp("logs");
        let f = ReportFiles::new(&d, "p");
        let other = ReportFiles::new(&d, "q");
        let clock = Clock {
            local: false,
            offset_min: 0,
        };
        let e = Event {
            time: 1_795_543_452,
            level: Level::Error,
            category: Category::Rec,
            text: "write error".into(),
            file: Some("/r/x.wav".into()),
        };
        f.append("2026-11-24", &[e.line(&clock)]).unwrap();
        f.append("2026-11-24", &[e.line(&clock)]).unwrap();
        f.append("2026-08-01", &[e.line(&clock)]).unwrap();
        other.append("2026-01-01", &[e.line(&clock)]).unwrap();
        assert_eq!(f.read_log("2026-11-24"), vec![e.clone(), e]);
        assert_eq!(f.log_days(), ["2026-08-01", "2026-11-24"]);
        assert_eq!(f.prune_logs("2026-09-01"), 1);
        assert_eq!(f.log_days(), ["2026-11-24"]);
        assert_eq!(
            other.log_days(),
            ["2026-01-01"],
            "another profile's logs stay"
        );
        f.save(&ReportDoc::default()).unwrap();
        assert_eq!(f.load().unwrap(), Some(ReportDoc::default()));
        assert_eq!(f.profile_files().len(), 2, "its log and its report");
        std::fs::remove_dir_all(d).unwrap();
    }
}
