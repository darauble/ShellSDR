//! The scheduler wired into `AppState` (pure; the event loop calls in).
//!
//! `schedule_tick` runs the planner against the app's own desired-state
//! switches (source Running, Master recording) and applies its effects the
//! same way the user's clicks do; the existing engine and recorder
//! reconcilers in `ui::run` then make the hardware follow. A job tunes the
//! Master VFO to its bookmark: the source center and the VFO go to the
//! bookmark frequency, with its mode and per-mode settings (user decision).
//! A Record job then records; a Tune job stops there. A muting job mutes the
//! VFO while it runs and gives it its own mute back afterwards.

use std::path::PathBuf;

use crate::app::{AppState, PopupTarget, RunState};
use crate::freq::FreqEntry;
use crate::schedule::editor::{SchedEditorOutcome, ScheduleEditor};
use crate::schedule::job::{Job, JobAction};
use crate::schedule::module::{SchedRequest, ScheduleModule};
use crate::schedule::planner::{Effect, Inputs, JobState, Wait};
use crate::schedule::window::weekday_of_day;
use crate::ui::popup::ListPopup;

impl AppState {
    // ── Persistence ──────────────────────────────────────────────────────

    /// Load the instance's schedule file (startup). Returns a warning when it
    /// was unreadable or malformed (an empty schedule is used).
    pub fn load_schedule(&mut self, path: PathBuf) -> Option<String> {
        self.load_schedule_with_legacy(path, None, None)
    }

    /// As [`AppState::load_schedule`], migrating from the 0.2.x file `v2`
    /// (or the 0.1.x `legacy` behind it) when `path` does not exist yet
    /// (spec 13.9, 19.8). A schedule that needs a newer build is never saved.
    pub fn load_schedule_with_legacy(
        &mut self,
        path: PathBuf,
        v2: Option<PathBuf>,
        legacy: Option<PathBuf>,
    ) -> Option<String> {
        let l = crate::schedule::store::load(&path, v2.as_deref(), legacy.as_deref());
        self.schedule = ScheduleModule::from_doc(l.doc);
        self.schedule_read_only = l.read_only;
        if l.read_only {
            self.note_file_warning(l.warning.clone());
        }
        self.schedule_path = Some(path);
        l.warning
    }

    /// Save the schedule when it changed. Returns a warning on failure.
    pub fn sync_schedule(&mut self) -> Option<String> {
        if !self.schedule.take_dirty() || self.schedule_read_only {
            return None;
        }
        let path = self.schedule_path.as_ref()?;
        self.schedule
            .to_doc()
            .save(path)
            .err()
            .map(|e| format!("cannot save schedule {}: {e}", path.display()))
    }

    // ── Running the scheduler ────────────────────────────────────────────

    /// The app's switches as the planner sees them.
    pub fn sched_inputs(&self) -> Inputs {
        Inputs {
            now: self.now_secs,
            running: self.run == RunState::Running,
            recording: self.radio.active().recording,
        }
    }

    /// Switched on or a list activated: restore the current job. An open
    /// Record job starts at this tick (its skip is forgotten); otherwise the
    /// Tune job that fired last is applied again, starting the source with
    /// Auto start/stop.
    fn sched_resume(&mut self, now: u64) {
        use crate::report::model::{Category, Level};
        let Some((job, at)) = self.schedule.scheduler.resume(now) else {
            return;
        };
        // A Tune job never retunes a running recording (spec 9b).
        if self.radio.active().recording {
            return;
        }
        if self.schedule.scheduler.auto_source && self.run != RunState::Running {
            self.run = RunState::Running;
        }
        let when = self.sched_when(at);
        self.report_event(
            Level::Info,
            Category::Sched,
            format!("Resumed {} (Tune only, from {when})", job.name()),
            None,
        );
        self.start_job(&job);
    }

    /// One scheduler step at `now` (Unix seconds).
    pub fn schedule_tick(&mut self, now: u64) {
        self.now_secs = now;
        if self.schedule.take_resume() {
            self.sched_resume(now);
        }
        let fx = self.schedule.scheduler.tick(self.sched_inputs());
        for e in fx {
            match e {
                Effect::StartSource => self.run = RunState::Running,
                Effect::StopSource => self.run = RunState::Stopped,
                Effect::StartJob(job) => {
                    use crate::report::model::{Category, Level, keys};
                    self.report_count(keys::SCHED_RUNS, 1);
                    let what = match job.action {
                        JobAction::Tune => "Tune only".to_string(),
                        JobAction::Record => {
                            let mut o = Vec::new();
                            if job.record_native {
                                o.push("Native");
                            }
                            if job.record_baseband {
                                o.push("Baseband");
                            }
                            format!("Record {}", o.join(" + "))
                        }
                    };
                    self.report_event(
                        Level::Info,
                        Category::Sched,
                        format!("Started {} ({what}, {})", job.name(), job.tuning.vfo_type),
                        None,
                    );
                    self.start_job(&job);
                }
                Effect::StopJob(action) => {
                    if action == JobAction::Record {
                        self.radio.stop_recording();
                    }
                }
            }
        }
        if self.schedule.scheduler.current_job().is_none() {
            // Once no job runs, give the VFO its own mute and recorder type
            // back.
            if let Some(m) = self.sched_prev_muted.take() {
                self.radio.set_muted(m);
            }
            if !self.radio.active().recording
                && let Some(t) = self.sched_prev_rec.take()
            {
                self.radio.set_rec_choice(t);
            }
            if !self.radio.active().recording
                && let Some(t) = self.sched_prev_type.take()
            {
                self.radio.set_type(&t);
            }
        }
    }

    /// Tune the Master VFO to the job's bookmark (center and VFO on its
    /// frequency, its mode and settings), apply the job's mute, and start
    /// recording for a Record job.
    // NOTE(step 6): with one VFO the active VFO is Master; target Master
    // explicitly once secondary VFOs exist.
    fn start_job(&mut self, job: &Job) {
        let t = job.resolve_tuning(&self.bookmarks.store);
        if self.global_editing {
            self.global_editing = false;
            self.center_freq.cancel();
        }
        // The bookmark's type for the run (spec 13.8): the VFO's own type is
        // restored when no job runs any more.
        if self.radio.active().vfo_type != t.vfo_type {
            self.sched_prev_type
                .get_or_insert(self.radio.active().vfo_type.clone());
        }
        if !self.radio.apply_type_settings(&t.vfo_type, &t.settings) {
            log::warn!("job {}: cannot apply type {}", job.name(), t.vfo_type);
        }
        let (lo, hi) = self.freq_band();
        let hz = t.frequency_hz.clamp(lo, hi);
        let old = self.center_hz();
        // The center follows unless it is locked to a playing file.
        let (clo, chi) = self.center_band();
        let center = hz.clamp(clo, chi);
        if center != old {
            self.center_freq = FreqEntry::new(center);
            self.radio.reproject_to_center(old, center);
        }
        self.radio.active_mut_freq(hz);
        // Back-to-back jobs: keep the mute and type from before the first.
        if job.mute {
            self.sched_prev_muted
                .get_or_insert(self.radio.active().muted);
            self.radio.set_muted(true);
        } else if let Some(m) = self.sched_prev_muted.take() {
            self.radio.set_muted(m);
        }
        match job.action {
            // A type that always records its native output (DSC) has nothing
            // more to record without the baseband: the job only holds the
            // tuning for its window (spec 14.7).
            JobAction::Record
                if !job.record_baseband
                    && crate::vfo::types::usable(&t.vfo_type)
                        .is_some_and(|ty| ty.native_always_on()) => {}
            JobAction::Record => {
                let prev = self
                    .radio
                    .start_recording_as(job.record_native, job.record_baseband);
                self.sched_prev_rec.get_or_insert(prev);
                self.rec_error = None;
            }
            JobAction::Tune => {
                // Tune only has no end (spec 14.7): its type is not restored.
                self.sched_prev_type = None;
                if !self.radio.active().recording
                    && let Some(t) = self.sched_prev_rec.take()
                {
                    self.radio.set_rec_choice(t);
                }
            }
        }
    }

    // ── Module requests and the editor ───────────────────────────────────

    /// Carry out a request from the Schedule module.
    pub fn schedule_request(&mut self, req: SchedRequest) {
        match req {
            SchedRequest::None => {}
            SchedRequest::OpenPage => self.open_page(crate::ui::page::WorkOwner::Schedule),
            SchedRequest::Add => {
                self.sched_editor = Some(ScheduleEditor::new_add(
                    self.schedule.local_time,
                    self.utc_offset_min,
                ));
                // A bookmark is required: offer the picker at once.
                self.open_sched_bookmark_picker();
            }
            SchedRequest::Edit(i) => {
                if let Some(job) = self.schedule.jobs().get(i) {
                    self.sched_editor = Some(ScheduleEditor::new_edit(
                        i,
                        job,
                        self.schedule.local_time,
                        self.utc_offset_min,
                    ));
                }
            }
            SchedRequest::Move(i) => {
                if let Some(job) = self.schedule.jobs().get(i) {
                    let title = format!("Move \"{}\" to list", job.name());
                    let cur = self.schedule.view_list();
                    self.popup = Some(ListPopup::new(title, self.schedule.list_names(), cur));
                    self.popup_target = Some(PopupTarget::MoveJob(i));
                }
            }
            SchedRequest::PickList => {
                let cur = self.schedule.view_list();
                self.popup = Some(ListPopup::new(
                    "Schedule list",
                    self.schedule.list_names(),
                    cur,
                ));
                self.popup_target = Some(PopupTarget::ScheduleList);
            }
            SchedRequest::NewList => {
                let name = self.schedule.unique_list_name("New List");
                self.prompt = Some(crate::ui::prompt::TextPrompt::new(
                    "New schedule list",
                    name,
                ));
                self.prompt_target = Some(crate::app::PromptTarget::NewSchedList);
            }
            SchedRequest::RenameList => {
                let name = self.schedule.view_name().to_string();
                self.prompt = Some(crate::ui::prompt::TextPrompt::new(
                    "Rename schedule list",
                    name,
                ));
                self.prompt_target = Some(crate::app::PromptTarget::RenameSchedList);
            }
            SchedRequest::DeleteList => {
                if self.schedule.viewing_active() {
                    self.show_notice("The active list cannot be deleted".to_string());
                } else {
                    let title = format!(
                        "Delete schedule list \"{}\" ({} jobs)?",
                        self.schedule.view_name(),
                        self.schedule.jobs().len()
                    );
                    self.popup = Some(ListPopup::new(
                        title,
                        vec!["Cancel".to_string(), "Delete".to_string()],
                        0,
                    ));
                    self.popup_target = Some(PopupTarget::ConfirmDeleteSchedList);
                }
            }
            SchedRequest::Activate => {
                if self.schedule.viewing_active() {
                } else if let Some(id) = self.schedule.scheduler.current_job() {
                    // A job records now: switching ends it.
                    let name = self
                        .schedule
                        .scheduler
                        .jobs
                        .iter()
                        .find(|j| j.id == id)
                        .map_or(String::new(), |j| j.name().to_string());
                    let title = format!(
                        "Activate \"{}\"? \"{name}\" stops recording",
                        self.schedule.view_name()
                    );
                    self.popup = Some(ListPopup::new(
                        title,
                        vec!["Cancel".to_string(), "Activate".to_string()],
                        0,
                    ));
                    self.popup_target = Some(PopupTarget::ConfirmActivate);
                } else {
                    self.schedule.activate_view();
                }
            }
            SchedRequest::Copy(i) => {
                // The editor on a copy: Apply adds it as a new job.
                if let Some(job) = self.schedule.jobs().get(i) {
                    let mut ed = ScheduleEditor::new_edit(
                        i,
                        job,
                        self.schedule.local_time,
                        self.utc_offset_min,
                    );
                    ed.original = None;
                    ed.title = "Copy scheduled job".to_string();
                    self.sched_editor = Some(ed);
                }
            }
            SchedRequest::Remove(i) => {
                if let Some(job) = self.schedule.jobs().get(i) {
                    let title = format!("Remove scheduled \"{}\"?", job.name());
                    self.popup = Some(ListPopup::new(
                        title,
                        vec!["Cancel".to_string(), "Remove".to_string()],
                        0,
                    ));
                    self.popup_target = Some(PopupTarget::ConfirmRemoveJob(i));
                }
            }
        }
    }

    /// Open the bookmark picker for the schedule editor: every bookmark of
    /// every list, "List: Name (kHz)", the editor's current one (or the
    /// bookmark selected in the Bookmarks module) highlighted.
    pub fn open_sched_bookmark_picker(&mut self) {
        let Some(ed) = &mut self.sched_editor else {
            return;
        };
        let store = &self.bookmarks.store;
        let mut pick = Vec::new();
        let mut items = Vec::new();
        for (l, list) in store.lists.iter().enumerate() {
            for (i, b) in list.bookmarks.iter().enumerate() {
                pick.push((l, i));
                items.push(format!(
                    "{}: {} ({} kHz)",
                    list.name,
                    b.name,
                    crate::ui::bookmark_view::khz_label(b.frequency_hz)
                ));
            }
        }
        if items.is_empty() {
            ed.error = Some("No bookmarks: add one in Bookmarks first".to_string());
            return;
        }
        let current = match &ed.bookmark {
            Some(r) => pick.iter().position(|&(l, i)| {
                store.lists[l].name == r.list && store.lists[l].bookmarks[i].name == r.name
            }),
            None => match (self.bookmarks.list_index(), self.bookmarks.selected()) {
                (Some(l), Some(i)) => pick.iter().position(|p| *p == (l, i)),
                _ => None,
            },
        };
        self.popup = Some(ListPopup::new(
            "Which bookmark?",
            items,
            current.unwrap_or(0),
        ));
        self.popup_target = Some(PopupTarget::ScheduleBookmark);
        self.sched_pick = pick;
    }

    /// The picker confirmed entry `idx`: fill the editor from that bookmark.
    pub(crate) fn sched_pick_confirm(&mut self, idx: usize) {
        let Some(&(l, i)) = self.sched_pick.get(idx) else {
            return;
        };
        let store = &self.bookmarks.store;
        if let (Some(ed), Some(list)) = (&mut self.sched_editor, store.lists.get(l))
            && let Some(b) = list.bookmarks.get(i)
        {
            // A bookmark of a type this build lacks cannot be scheduled
            // (spec 13.7): it would never fire.
            if b.type_def().is_none() {
                ed.error = Some(format!(
                    "{} is a {} bookmark: not in this build",
                    b.name, b.vfo_type
                ));
                return;
            }
            ed.set_bookmark(&list.name, b);
        }
    }

    /// Act on an editor outcome.
    pub fn sched_editor_outcome(&mut self, outcome: SchedEditorOutcome) {
        match outcome {
            SchedEditorOutcome::Open => {}
            SchedEditorOutcome::Cancel => self.sched_editor = None,
            SchedEditorOutcome::PickBookmark => self.open_sched_bookmark_picker(),
            SchedEditorOutcome::Apply => {
                let Some(ed) = &mut self.sched_editor else {
                    return;
                };
                match ed.build() {
                    Ok(job) => {
                        let original = ed.original;
                        self.schedule.save_job(original, job);
                        self.sched_editor = None;
                    }
                    Err(e) => ed.error = Some(e),
                }
            }
        }
    }

    // ── Display text ─────────────────────────────────────────────────────

    /// "Tue 06:00 UTC" (or local) for a Unix second, in the module's clock.
    pub fn sched_when(&self, secs: u64) -> String {
        let (secs, zone) = if self.schedule.local_time {
            (
                (secs as i64 + i64::from(self.utc_offset_min) * 60).max(0) as u64,
                "local",
            )
        } else {
            (secs, "UTC")
        };
        let day = crate::bookmarks::model::DAY_LABELS[weekday_of_day(secs / 86_400)];
        let sod = secs % 86_400;
        format!("{day} {:02}:{:02} {zone}", sod / 3600, sod / 60 % 60)
    }

    /// The module's summary line: what records now, else what is next.
    pub fn sched_summary(&self) -> String {
        let s = &self.schedule.scheduler;
        if !s.enabled {
            return "Scheduler is off".to_string();
        }
        if !crate::schedule::window::clock_valid(self.now_secs) {
            return "Clock not set: waiting".to_string();
        }
        if let Some(id) = s.current_job()
            && let Some(j) = s.jobs.iter().find(|j| j.id == id)
        {
            return format!("REC {}", j.name());
        }
        match s.next_start(self.now_secs) {
            Some((i, t)) => format!("Next {} {}", self.sched_when(t), s.jobs[i].name()),
            None => "No jobs due".to_string(),
        }
    }

    /// Status of the selected job for its details line.
    pub fn sched_job_status(&self, idx: usize) -> String {
        if !self.schedule.viewing_active() {
            return match self.schedule.jobs().get(idx) {
                Some(j) if !j.enabled => "Off".to_string(),
                _ => format!("List not active (active: {})", self.schedule.active_name()),
            };
        }
        let inp = self.sched_inputs();
        match self.schedule.scheduler.job_state(idx, inp) {
            JobState::Off => "Off".to_string(),
            JobState::Unavailable => match self.schedule.jobs().get(idx) {
                Some(j) => format!("Never: {} not in this build", j.tuning.vfo_type),
                None => "Never".to_string(),
            },
            JobState::SchedulerOff => "Scheduler is off".to_string(),
            JobState::ClockNotSet => "Clock not set: waiting".to_string(),
            JobState::Active { until } => format!("REC until {}", self.sched_when(until)),
            JobState::Skipped { until } => format!("Skipped to {}", self.sched_when(until)),
            JobState::Waiting { why, .. } => match why {
                Wait::ManualRecording => "Waits: manual recording".to_string(),
                Wait::SourceStopped => "Waits: source stopped".to_string(),
                Wait::OtherJob => "Waits: another job".to_string(),
                Wait::Retry(t) => {
                    format!("Source failed: retry {}s", t.saturating_sub(self.now_secs))
                }
            },
            JobState::Idle { next: Some(t) } => format!("Next {}", self.sched_when(t)),
            JobState::Idle { next: None } => "Never (no day checked)".to_string(),
        }
    }

    /// The selected job's schedule and tuning: "0600-0630 MoWe USB 518".
    pub fn sched_job_details(&self, idx: usize) -> String {
        let Some(job) = self.schedule.jobs().get(idx) else {
            return String::new();
        };
        let w = if self.schedule.local_time {
            job.window.shifted(self.utc_offset_min)
        } else {
            job.window
        };
        let t = job.resolve_tuning(&self.bookmarks.store);
        let missing = if job.find_bookmark(&self.bookmarks.store).is_none() {
            " (bookmark gone)"
        } else {
            ""
        };
        format!(
            "{} {} {} {}{missing}",
            job.time_label(&w),
            w.days_label(),
            t.mode_label(),
            crate::ui::bookmark_view::khz_label(t.frequency_hz)
        )
        .replace("  ", " ")
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{AppState, PopupTarget, RunState};
    use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
    use crate::record::RecType;
    use crate::schedule::editor::SchedEditorOutcome;
    use crate::schedule::job::JobAction;
    use crate::schedule::module::SchedRequest;
    use crate::vfo::model::Mode;

    /// 2026-11-09 (a Monday) 00:00 UTC.
    const MON: u64 = 1_794_182_400;
    const H: u64 = 3_600;

    fn navtex() -> Bookmark {
        Bookmark {
            name: "NAVTEX".into(),
            frequency_hz: 518_000,
            start_utc: 600,
            end_utc: 630,
            settings: crate::bookmarks::model::voice_part(Mode::Usb, Some(2_400.0), None),
            ..Bookmark::default()
        }
    }

    fn state_with_bookmark() -> AppState {
        let mut s = AppState::default();
        let mut l = BookmarkList::new("Ships");
        l.bookmarks.push(navtex());
        s.bookmarks.replace_store(BookmarkStore {
            lists: vec![l],
            ..BookmarkStore::default()
        });
        s
    }

    /// Add a job for NAVTEX through the UI path: [Add], pick, [Apply].
    fn add_navtex_job(s: &mut AppState, rec: RecType) {
        s.schedule_request(SchedRequest::Add);
        assert_eq!(s.popup_target, Some(PopupTarget::ScheduleBookmark));
        s.popup_confirm();
        let ed = s.sched_editor.as_mut().unwrap();
        ed.record_native = rec == RecType::Audio;
        ed.record_baseband = rec == RecType::Baseband;
        s.sched_editor_outcome(SchedEditorOutcome::Apply);
        assert!(s.sched_editor.is_none(), "saved and closed");
    }

    #[test]
    fn add_opens_the_bookmark_picker_and_saves_a_bookmark_job() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        let j = &s.schedule.jobs()[0];
        assert_eq!(j.name(), "NAVTEX");
        assert_eq!((j.window.start, j.window.end), (600, 630));
        assert!(s.schedule.take_dirty(), "to be saved");
    }

    #[test]
    fn add_without_bookmarks_explains_why() {
        let mut s = AppState::default();
        s.bookmarks.replace_store(BookmarkStore::default());
        s.schedule_request(SchedRequest::Add);
        assert!(s.popup.is_none());
        let err = s.sched_editor.as_ref().unwrap().error.clone().unwrap();
        assert!(err.contains("add one in Bookmarks"));
    }

    #[test]
    fn a_job_tunes_master_to_the_bookmark_records_and_cleans_up() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Baseband);
        s.schedule_tick(MON + 6 * H);
        assert_eq!(s.run, RunState::Running, "auto start");
        assert_eq!(s.center_hz(), 518_000, "main frequency = bookmark");
        let v = s.radio.active();
        assert_eq!(v.frequency_hz, 518_000);
        assert_eq!(v.mode, Mode::Usb);
        assert_eq!(v.bandwidth_hz, 2_400.0);
        assert!(v.recording);
        assert!(v.rec.baseband && !v.rec.native, "the job's outputs");
        assert_eq!(s.sched_summary(), "REC NAVTEX");
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert_eq!(s.run, RunState::Stopped, "auto stop");
        assert!(!s.radio.active().recording);
        let rec = s.radio.active().rec;
        assert!(rec.native && !rec.baseband, "the VFO's choice restored");
    }

    #[test]
    fn switching_the_scheduler_on_tunes_back_to_the_current_tune_job() {
        use crate::schedule::module::SchedSetting;
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].action = JobAction::Tune;
        s.schedule_tick(MON + 6 * H);
        assert_eq!(s.radio.active().frequency_hz, 518_000);
        // Off for casual listening, tuned elsewhere and stopped.
        s.schedule.flip(SchedSetting::Enabled);
        s.schedule_tick(MON + 7 * H);
        s.radio.active_mut_freq(7_040_000);
        s.run = RunState::Stopped;
        // On again: back on NAVTEX, the source started (Auto).
        s.schedule.flip(SchedSetting::Enabled);
        s.schedule_tick(MON + 8 * H);
        assert_eq!(s.radio.active().frequency_hz, 518_000);
        assert_eq!(s.run, RunState::Running);
        // Only once: retuning by hand afterwards is left alone.
        s.radio.active_mut_freq(7_040_000);
        s.schedule_tick(MON + 8 * H + 60);
        assert_eq!(s.radio.active().frequency_hz, 7_040_000);
    }

    #[test]
    fn activating_a_list_resumes_its_open_recording() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.create_list("Other").unwrap(); // shown, not active
        s.schedule.select_list(1);
        s.schedule.activate_view(); // Other runs: nothing in it
        s.schedule_tick(MON + 6 * H + 60);
        assert!(!s.radio.active().recording);
        s.schedule.select_list(0);
        s.schedule.activate_view();
        s.schedule_tick(MON + 6 * H + 120);
        assert!(s.radio.active().recording, "NAVTEX 0600-0630 records");
    }

    #[test]
    fn a_tune_only_job_tunes_once_and_leaves_the_source_running() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].action = JobAction::Tune;
        s.schedule_tick(MON + 6 * H);
        assert_eq!(s.run, RunState::Running, "auto start");
        assert_eq!(s.radio.active().frequency_hz, 518_000);
        assert_eq!(s.radio.active().mode, Mode::Usb);
        assert!(!s.radio.active().recording, "tune only");
        assert_eq!(s.sched_summary(), "Next Tu 06:00 UTC NAVTEX");
        assert_eq!(s.sched_job_status(0), "Next Tu 06:00 UTC");
        assert!(s.sched_job_details(0).starts_with("0600 "), "no end");
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert_eq!(s.run, RunState::Running, "left on for listening");
    }

    #[test]
    fn a_manual_recording_during_a_tune_job_is_kept_with_its_source() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].action = JobAction::Tune;
        s.schedule_tick(MON + 6 * H);
        s.toggle_recording(); // the user records what they hear
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert!(s.radio.active().recording, "the user's recording goes on");
        assert_eq!(s.run, RunState::Running, "and keeps the source");
    }

    #[test]
    fn a_source_stopped_after_a_tune_job_is_not_restarted() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].action = JobAction::Tune;
        s.schedule_tick(MON + 6 * H);
        s.toggle_run(); // Stop
        s.schedule_tick(MON + 6 * H + 30);
        assert_eq!(s.run, RunState::Stopped, "fired once");
    }

    #[test]
    fn a_muting_job_mutes_the_vfo_and_restores_it() {
        let mut s = state_with_bookmark();
        s.volume = 0.5;
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].mute = true;
        s.schedule_tick(MON + 6 * H);
        assert!(s.radio.active().muted);
        assert!(s.radio.active().recording, "it still records");
        assert_eq!(s.effective_volume(), 0.0);
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert!(!s.radio.active().muted, "unmuted again");
        assert!((s.effective_volume() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_muting_job_keeps_a_vfo_the_user_had_muted_muted() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].mute = true;
        s.radio.set_muted(true);
        s.schedule_tick(MON + 6 * H);
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert!(s.radio.active().muted, "the user's own mute is restored");
    }

    #[test]
    fn a_job_without_mute_leaves_the_vfo_mute_alone() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.radio.set_muted(true);
        s.schedule_tick(MON + 6 * H);
        assert!(s.radio.active().muted);
        s.radio.set_muted(false); // the user unmutes during the job
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert!(!s.radio.active().muted, "not touched at the end either");
    }

    #[test]
    fn user_stop_skips_the_job_and_the_master_switch_blocks_it() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule_tick(MON + 6 * H);
        s.toggle_run(); // the user presses Stop
        s.schedule_tick(MON + 6 * H + 60);
        assert_eq!(s.run, RunState::Stopped);
        assert!(s.sched_job_status(0).starts_with("Skipped to Mo 06:30"));
        // Next day, with the scheduler switched off: nothing fires.
        s.schedule.scheduler.enabled = false;
        s.schedule_tick(MON + 30 * H);
        assert_eq!(s.run, RunState::Stopped);
        assert_eq!(s.sched_summary(), "Scheduler is off");
    }

    #[test]
    fn local_time_display_uses_the_offset() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.now_secs = MON;
        assert_eq!(s.sched_summary(), "Next Mo 06:00 UTC NAVTEX");
        s.schedule.local_time = true;
        s.utc_offset_min = 120;
        assert_eq!(s.sched_summary(), "Next Mo 08:00 local NAVTEX");
        assert!(s.sched_job_details(0).starts_with("0800-0830 USB 518"));
    }

    #[test]
    fn remove_asks_first() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule_request(SchedRequest::Remove(0));
        assert_eq!(s.popup_target, Some(PopupTarget::ConfirmRemoveJob(0)));
        s.popup_confirm(); // Cancel is the default
        assert_eq!(s.schedule.jobs().len(), 1);
        s.schedule_request(SchedRequest::Remove(0));
        s.popup_down();
        s.popup_confirm();
        assert!(s.schedule.jobs().is_empty());
    }

    /// A state whose NAVTEX bookmark has VFO type `vfo_type`.
    fn state_with_typed_bookmark(vfo_type: &str) -> AppState {
        let mut s = state_with_bookmark();
        let b = &mut s.bookmarks.store.lists[0].bookmarks[0];
        b.vfo_type = vfo_type.to_string();
        b.settings = serde_json::json!({"speed": "Fast"})
            .as_object()
            .unwrap()
            .clone();
        s
    }

    #[test]
    fn a_job_switches_the_vfo_type_for_its_run_and_restores_it() {
        let mut s = state_with_typed_bookmark(crate::vfo::types::test_type::ID);
        s.radio.set_mode(Mode::Am);
        add_navtex_job(&mut s, RecType::Audio);
        // Make it a Tune-free Record job the test type can run: no native
        // recording for a data type, so record the baseband.
        s.schedule.scheduler.jobs[0].record_native = false;
        s.schedule.scheduler.jobs[0].record_baseband = true;
        s.schedule_tick(MON + 6 * H);
        let v = s.radio.active();
        assert_eq!(v.vfo_type, "Test", "switched for the run");
        assert_eq!(v.mode_label(), "Fast");
        assert!(v.recording);
        s.schedule_tick(MON + 6 * H + 30 * 60);
        let v = s.radio.active();
        assert_eq!(v.vfo_type, "Voice", "restored");
        assert_eq!(v.mode, Mode::Am, "with its settings");
    }

    #[test]
    fn a_job_of_an_unknown_type_never_fires_and_says_why() {
        let mut s = state_with_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].tuning.vfo_type = "NAVTEX".into();
        s.bookmarks.store.lists[0].bookmarks[0].vfo_type = "NAVTEX".into();
        s.schedule_tick(MON + 6 * H);
        assert!(!s.radio.active().recording);
        assert_eq!(s.run, RunState::Stopped, "no auto start either");
        assert_eq!(s.sched_job_status(0), "Never: NAVTEX not in this build");
    }

    #[test]
    fn an_unknown_type_bookmark_cannot_be_picked_for_a_job() {
        let mut s = state_with_typed_bookmark("NAVTEX");
        s.schedule_request(SchedRequest::Add);
        s.popup_confirm();
        let ed = s.sched_editor.as_ref().unwrap();
        assert!(ed.bookmark.is_none());
        assert!(ed.error.as_ref().unwrap().contains("not in this build"));
    }

    fn state_with_dsc_bookmark() -> AppState {
        let mut s = state_with_bookmark();
        let b = &mut s.bookmarks.store.lists[0].bookmarks[0];
        b.vfo_type = crate::vfo::types::DSC.to_string();
        b.settings = Default::default();
        s
    }

    #[test]
    fn a_tune_only_job_switches_the_type_for_good() {
        let mut s = state_with_dsc_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        s.schedule.scheduler.jobs[0].action = JobAction::Tune;
        s.schedule_tick(MON + 6 * H);
        assert_eq!(s.radio.active().vfo_type, "DSC");
        // Spec 14.7: it stays until another job or the user changes it.
        s.schedule_tick(MON + 6 * H + 60);
        s.schedule_tick(MON + 8 * H);
        assert_eq!(s.radio.active().vfo_type, "DSC");
        assert!(!s.radio.active().recording);
    }

    #[test]
    fn a_dsc_record_job_without_baseband_decodes_for_its_window_only() {
        let mut s = state_with_dsc_bookmark();
        add_navtex_job(&mut s, RecType::Audio);
        let j = &s.schedule.jobs()[0];
        assert!(j.record_native && !j.record_baseband);
        s.schedule_tick(MON + 6 * H);
        let v = s.radio.active();
        assert_eq!(v.vfo_type, "DSC", "switched for the window");
        assert!(
            !v.recording,
            "the messages are always recorded: no file to write"
        );
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert_eq!(s.radio.active().vfo_type, "Voice", "restored at the end");
    }

    #[test]
    fn a_dsc_record_job_with_baseband_records_it() {
        let mut s = state_with_dsc_bookmark();
        add_navtex_job(&mut s, RecType::Baseband);
        s.schedule_tick(MON + 6 * H);
        let v = s.radio.active();
        assert_eq!(v.vfo_type, "DSC");
        assert!(v.recording && v.rec.baseband && v.rec.native);
        s.schedule_tick(MON + 6 * H + 30 * 60);
        assert_eq!(s.radio.active().vfo_type, "Voice");
    }
}
