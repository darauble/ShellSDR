//! The scheduler state machine (pure, level-triggered).
//!
//! Every UI tick (`AppState::schedule_tick`) passes in the wall clock and the
//! two desired-state switches the app already has (source Running, VFO
//! recording), and gets back effects to apply to them. The engine and
//! recorder reconcilers in `ui::run` then make the hardware follow, exactly as
//! for a user's own clicks. Nothing is armed for later: each tick asks "which
//! occurrence is open now?", so a restart or crash inside a window resumes the
//! recording (in a new file), and a clock jump (NTP sync on a Pi with no RTC)
//! simply changes the answer.
//!
//! Rules (one VFO, Master, until Step 6 brings more):
//! - One job records at a time. Among open occurrences the earliest start
//!   wins (list order breaks ties); the others wait and start when it ends if
//!   their window is still open. Overlapping enabled jobs are flagged in the
//!   UI ([`Scheduler::conflicts`]).
//! - A "Tune only" job has no end: it tunes once, in its start minute, and
//!   holds nothing (no slot, no overlap, no source ownership), so a Record
//!   job may start in the same minute (after it). With "Auto start/stop" it
//!   starts a stopped source and leaves it running for the user. It never
//!   retunes a recording (a job's or the user's): then it is missed.
//! - Source ownership: with "Auto start/stop" on, a job that finds the source
//!   stopped starts it and the scheduler owns it; when no job runs any more it
//!   stops only a source it owns. A source the user started is never stopped.
//! - The user wins: stopping the recording or the source while a job runs
//!   skips that occurrence (until its window ends). A recording the user
//!   started by hand is never interrupted: a due job waits for it to end.
//! - An engine start failure (the source stops while the job still wants to
//!   record) is retried every [`RETRY_SECS`] while the window is open.
//! - A "Once" job switches itself off when its occurrence ends or is skipped.
//! - The master switch (`enabled`, the module's [On]/[Off]) keeps everything
//!   from firing, e.g. while the user listens. Switching it off during a job
//!   stops the recording and skips that occurrence; the source keeps running
//!   and becomes the user's (they are evidently at the radio).

use super::job::{Job, JobAction};
use super::window::clock_valid;

/// Seconds between attempts to restart a source that failed to start.
pub const RETRY_SECS: u64 = 10;

/// What the app reports to a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    /// Wall clock, seconds since the Unix epoch.
    pub now: u64,
    /// The source is (wanted) running.
    pub running: bool,
    /// The Master VFO's recording switch is on.
    pub recording: bool,
}

/// What a tick asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Start the source (set Running).
    StartSource,
    /// Stop the source (set Stopped).
    StopSource,
    /// Tune to the job's bookmark and switch recording on.
    StartJob(Job),
    /// The job ended: switch its recording off (a Record job) and undo
    /// its mute.
    StopJob(JobAction),
}

/// Why a due job is not recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// A recording the user started is in progress.
    ManualRecording,
    /// The source is stopped and Auto start/stop is off.
    SourceStopped,
    /// Another job holds the recorder.
    OtherJob,
    /// The source failed to start; retrying at this second.
    Retry(u64),
}

/// A job's state for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Switched off.
    Off,
    /// Its VFO type is not in this build (or unavailable): it never fires.
    Unavailable,
    /// The whole scheduler is switched off.
    SchedulerOff,
    /// The system clock is not set: nothing fires.
    ClockNotSet,
    /// Recording the occurrence that
    /// ends at this second.
    Active { until: u64 },
    /// Due (window open) but held back.
    Waiting { why: Wait, until: u64 },
    /// The user stopped this occurrence; it ends at this second.
    Skipped { until: u64 },
    /// Not due; the next occurrence starts at this second (None = never).
    Idle { next: Option<u64> },
}

/// The occurrence being recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Running {
    job_id: u64,
    occ: u64,
    /// Kept here so a deleted job can still be stopped correctly.
    action: JobAction,
}

/// Scheduled jobs plus the scheduler's runtime state.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheduler {
    /// Master switch: when off, no job fires.
    pub enabled: bool,
    pub jobs: Vec<Job>,
    /// Start the source for a job and stop it afterwards.
    pub auto_source: bool,
    current: Option<Running>,
    owns_source: bool,
    /// Occurrences (job id, start) the user stopped.
    skipped: Vec<(u64, u64)>,
    /// When to retry a source that failed to start.
    retry_at: Option<u64>,
    /// Tune-only occurrences (job id, start) that fired.
    fired: Vec<(u64, u64)>,
    /// Jobs changed by the scheduler itself (a Once job switched off).
    dirty: bool,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            enabled: true,
            jobs: Vec::new(),
            auto_source: true,
            current: None,
            owns_source: false,
            skipped: Vec::new(),
            retry_at: None,
            fired: Vec::new(),
            dirty: false,
        }
    }
}

impl Scheduler {
    pub fn new(jobs: Vec<Job>, auto_source: bool) -> Self {
        Self {
            jobs,
            auto_source,
            ..Self::default()
        }
    }

    /// The id of the job being recorded, if any.
    pub fn current_job(&self) -> Option<u64> {
        self.current.map(|r| r.job_id)
    }

    /// Whether the scheduler started the running source.
    pub fn owns_source(&self) -> bool {
        self.owns_source
    }

    /// Whether the scheduler changed the jobs (to be saved). Clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// An id not used by any job.
    pub fn next_id(&self) -> u64 {
        self.jobs.iter().map(|j| j.id).max().map_or(1, |m| m + 1)
    }

    fn job(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    /// The open occurrence of an enabled Record job at `now` (Tune jobs
    /// hold no window: see [`Self::tune_due`]).
    fn open(&self, job: &Job, now: u64) -> Option<u64> {
        if !job.can_fire() || !job.records() {
            return None;
        }
        job.window.occurrence_at(now)
    }

    fn is_skipped(&self, job: &Job, occ: u64) -> bool {
        self.skipped.contains(&(job.id, occ))
    }

    /// The due job to start next: open, not skipped; earliest start first,
    /// then list order.
    fn due(&self, now: u64) -> Option<(usize, u64)> {
        self.jobs
            .iter()
            .enumerate()
            .filter_map(|(i, j)| self.open(j, now).map(|occ| (i, occ)))
            .filter(|&(i, occ)| !self.is_skipped(&self.jobs[i], occ))
            .min_by_key(|&(i, occ)| (occ, i))
    }

    /// Enabled Tune jobs in their start minute that have not fired: (index,
    /// occurrence start), in list order.
    fn tune_due(&self, now: u64) -> Vec<(usize, u64)> {
        self.jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.can_fire() && !j.records())
            .filter_map(|(i, j)| j.fire_window().occurrence_at(now).map(|o| (i, o)))
            .filter(|&(i, o)| !self.fired.contains(&(self.jobs[i].id, o)))
            .collect()
    }

    /// An occurrence is over: a Once job switches itself off.
    fn finish(&mut self, job_id: u64) {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == job_id)
            && j.repeat == super::job::Repeat::Once
            && j.enabled
        {
            j.enabled = false;
            self.dirty = true;
        }
    }

    /// The scheduler was switched on, or its list activated (0.3.0): the
    /// user wants the current job back. Skipped occurrences are forgotten
    /// (an open Record job starts again at the next tick, in a new file)
    /// and, when no Record job is open, the Tune job that fired last
    /// (within a week; the last in list order on a tie) is returned with
    /// its start, to be applied again.
    pub fn resume(&mut self, now: u64) -> Option<(Job, u64)> {
        self.skipped.clear();
        self.retry_at = None;
        if !self.enabled || !clock_valid(now) || self.due(now).is_some() {
            return None;
        }
        let last = self
            .jobs
            .iter()
            .filter(|j| j.can_fire() && !j.records())
            .filter_map(|j| j.fire_window().last_start_at_or_before(now).map(|s| (j, s)))
            .max_by_key(|(_, s)| *s)
            .map(|(j, s)| (j.clone(), s))?;
        // Its own minute must not fire it a second time.
        self.fired.push((last.0.id, last.1));
        Some(last)
    }

    /// One scheduler step. See the module docs for the rules.
    pub fn tick(&mut self, inp: Inputs) -> Vec<Effect> {
        let Inputs {
            now,
            mut running,
            mut recording,
        } = inp;
        let mut fx = Vec::new();
        if !clock_valid(now) {
            return fx;
        }
        // Forget skipped occurrences whose window is over.
        let jobs = &self.jobs;
        self.skipped.retain(|&(id, occ)| {
            jobs.iter()
                .find(|j| j.id == id)
                .is_some_and(|j| now < j.window.end_of(occ))
        });
        self.fired.retain(|&(_, occ)| now < occ + 60);

        if !self.enabled {
            // Master switch off: stop a running job (skipping its
            // occurrence) and leave the source to the user.
            if let Some(r) = self.current.take() {
                fx.push(Effect::StopJob(r.action));
                self.skipped.push((r.job_id, r.occ));
                self.retry_at = None;
                self.finish(r.job_id);
            }
            self.owns_source = false;
            return fx;
        }

        // The running occurrence: ended, stopped by the user, or failing.
        if let Some(r) = self.current {
            let still_open = self.job(r.job_id).and_then(|j| self.open(j, now)) == Some(r.occ);
            // Only Record jobs run: stopped by their recording ending.
            let user_stopped = !recording;
            if !still_open {
                fx.push(Effect::StopJob(r.action));
                // The job's own recording ends now: it is not a manual one.
                recording = false;
                self.current = None;
                self.retry_at = None;
                self.finish(r.job_id);
            } else if user_stopped {
                // The user pressed Stop (recording or source), or the
                // recording could not be written: skip this occurrence.
                self.skipped.push((r.job_id, r.occ));
                self.current = None;
                self.retry_at = None;
                self.finish(r.job_id);
            } else if !running {
                // Still recording but the source is down: it failed to start.
                if self.owns_source {
                    match self.retry_at {
                        None => self.retry_at = Some(now + RETRY_SECS),
                        Some(t) if now >= t => {
                            fx.push(Effect::StartSource);
                            self.retry_at = None;
                        }
                        Some(_) => {}
                    }
                }
            } else {
                self.retry_at = None;
            }
        }

        if self.current.is_none() {
            if !running {
                // A source stopped by the user (or never started) is not ours.
                self.owns_source = false;
            }
            // Tune jobs first, so a Record job of the same minute follows.
            let tunes = if recording {
                Vec::new()
            } else {
                self.tune_due(now)
            };
            for (n, &(i, occ)) in tunes.iter().enumerate() {
                let job = self.jobs[i].clone();
                self.fired.push((job.id, occ));
                // Several in one minute: only the last one's tuning would
                // stay, so only that one tunes.
                if n + 1 == tunes.len() {
                    if !running && self.auto_source {
                        fx.push(Effect::StartSource);
                        running = true;
                    }
                    // Tuned in: the source is the user's from now on.
                    self.owns_source = false;
                    fx.push(Effect::StartJob(job.clone()));
                }
                self.finish(job.id);
            }
            if let Some((i, occ)) = self.due(now) {
                let can_start = !recording && (running || self.auto_source);
                if can_start {
                    if !running {
                        fx.push(Effect::StartSource);
                        self.owns_source = true;
                    }
                    let job = self.jobs[i].clone();
                    self.current = Some(Running {
                        job_id: job.id,
                        occ,
                        action: job.action,
                    });
                    fx.push(Effect::StartJob(job));
                }
            }
        }

        // Release a source the scheduler started once no job needs it. A
        // recording the user started on it makes it the user's.
        if self.current.is_none() && self.owns_source && running {
            self.owns_source = false;
            if !recording {
                fx.push(Effect::StopSource);
            }
        }
        fx
    }

    /// The state of job `idx` at `now` for display, given the app's switches.
    pub fn job_state(&self, idx: usize, inp: Inputs) -> JobState {
        let Some(job) = self.jobs.get(idx) else {
            return JobState::Idle { next: None };
        };
        if !job.enabled {
            return JobState::Off;
        }
        if !job.can_fire() {
            return JobState::Unavailable;
        }
        if !self.enabled {
            return JobState::SchedulerOff;
        }
        if !clock_valid(inp.now) {
            return JobState::ClockNotSet;
        }
        if !job.records() {
            return JobState::Idle {
                next: job.fire_window().next_start_after(inp.now),
            };
        }
        let Some(occ) = self.open(job, inp.now) else {
            return JobState::Idle {
                next: job.window.next_start_after(inp.now),
            };
        };
        let until = job.window.end_of(occ);
        if self
            .current
            .is_some_and(|r| r.job_id == job.id && r.occ == occ)
        {
            if !inp.running
                && let Some(t) = self.retry_at
            {
                return JobState::Waiting {
                    why: Wait::Retry(t),
                    until,
                };
            }
            return JobState::Active { until };
        }
        if self.is_skipped(job, occ) {
            return JobState::Skipped { until };
        }
        let why = if self.current.is_some() || self.due(inp.now).map(|d| d.0) != Some(idx) {
            Wait::OtherJob
        } else if inp.recording {
            Wait::ManualRecording
        } else {
            Wait::SourceStopped
        };
        JobState::Waiting { why, until }
    }

    /// Whether enabled Record job `idx` overlaps another enabled Record job
    /// in time (only one runs at a time, so one of them would wait). Tune
    /// jobs hold no time.
    pub fn conflicts(&self, idx: usize) -> bool {
        let active = |j: &&Job| j.can_fire() && j.records();
        let Some(a) = self.jobs.get(idx).filter(active) else {
            return false;
        };
        self.jobs
            .iter()
            .enumerate()
            .any(|(i, b)| i != idx && active(&b) && a.window.overlaps(&b.window))
    }

    /// The next job start strictly after `now` among enabled jobs: (index,
    /// start second).
    pub fn next_start(&self, now: u64) -> Option<(usize, u64)> {
        self.jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.can_fire())
            .filter_map(|(i, j)| j.fire_window().next_start_after(now).map(|t| (i, t)))
            .min_by_key(|&(i, t)| (t, i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::job::{BookmarkRef, Repeat};
    use crate::schedule::window::Window;

    /// 2026-11-09 (a Monday) 00:00 UTC.
    const MON: u64 = 1_794_182_400;
    const H: u64 = 3_600;
    const M: u64 = 60;

    fn job(id: u64, start: u16, end: u16) -> Job {
        Job {
            id,
            bookmark: BookmarkRef {
                list: "General".into(),
                name: format!("job{id}"),
            },
            window: Window::new(start, end, [true; 7]),
            ..Job::default()
        }
    }

    /// Drives a scheduler against a model of the app's two switches, applying
    /// effects the way `AppState` does.
    struct Sim {
        s: Scheduler,
        running: bool,
        recording: bool,
        started: Vec<u64>,
    }

    impl Sim {
        fn new(jobs: Vec<Job>, auto: bool) -> Self {
            Self {
                s: Scheduler::new(jobs, auto),
                running: false,
                recording: false,
                started: Vec::new(),
            }
        }

        fn tick(&mut self, now: u64) -> Vec<Effect> {
            let fx = self.s.tick(Inputs {
                now,
                running: self.running,
                recording: self.recording,
            });
            for e in &fx {
                match e {
                    Effect::StartSource => self.running = true,
                    Effect::StopSource => self.running = false,
                    Effect::StartJob(j) => {
                        if j.records() {
                            self.recording = true;
                        }
                        self.started.push(j.id);
                    }
                    Effect::StopJob(a) => {
                        if *a == JobAction::Record {
                            self.recording = false;
                        }
                    }
                }
            }
            fx
        }

        fn state(&self, idx: usize, now: u64) -> JobState {
            self.s.job_state(
                idx,
                Inputs {
                    now,
                    running: self.running,
                    recording: self.recording,
                },
            )
        }
    }

    #[test]
    fn auto_source_starts_records_and_stops_around_the_window() {
        let mut sim = Sim::new(vec![job(1, 600, 630)], true);
        assert!(
            sim.tick(MON + 5 * H).is_empty(),
            "nothing before the window"
        );
        let fx = sim.tick(MON + 6 * H);
        assert_eq!(fx[0], Effect::StartSource);
        assert!(matches!(&fx[1], Effect::StartJob(j) if j.id == 1));
        assert!(sim.s.owns_source());
        assert!(sim.tick(MON + 6 * H + 10 * M).is_empty(), "steady state");
        assert_eq!(
            sim.state(0, MON + 6 * H + 10 * M),
            JobState::Active {
                until: MON + 6 * H + 30 * M
            }
        );
        let fx = sim.tick(MON + 6 * H + 30 * M);
        assert_eq!(
            fx,
            vec![Effect::StopJob(JobAction::Record), Effect::StopSource]
        );
        assert!(!sim.running && !sim.recording);
    }

    #[test]
    fn without_auto_source_a_due_job_waits_for_the_user_to_start() {
        let mut sim = Sim::new(vec![job(1, 600, 630)], false);
        assert!(sim.tick(MON + 6 * H).is_empty());
        assert!(matches!(
            sim.state(0, MON + 6 * H),
            JobState::Waiting {
                why: Wait::SourceStopped,
                ..
            }
        ));
        sim.running = true; // the user presses Start
        let fx = sim.tick(MON + 6 * H + M);
        assert!(matches!(&fx[..], [Effect::StartJob(_)]));
        assert!(!sim.s.owns_source());
        let fx = sim.tick(MON + 6 * H + 30 * M);
        assert_eq!(
            fx,
            vec![Effect::StopJob(JobAction::Record)],
            "the user's source keeps running"
        );
        assert!(sim.running);
    }

    #[test]
    fn a_source_the_user_started_is_never_stopped() {
        let mut sim = Sim::new(vec![job(1, 600, 630)], true);
        sim.running = true;
        let fx = sim.tick(MON + 6 * H);
        assert!(matches!(&fx[..], [Effect::StartJob(_)]), "no StartSource");
        let fx = sim.tick(MON + 6 * H + 30 * M);
        assert_eq!(fx, vec![Effect::StopJob(JobAction::Record)]);
        assert!(sim.running);
    }

    #[test]
    fn starting_mid_window_records_the_rest_of_it() {
        let mut sim = Sim::new(vec![job(1, 600, 700)], true);
        let fx = sim.tick(MON + 6 * H + 40 * M);
        assert!(fx.iter().any(|e| matches!(e, Effect::StartJob(_))));
    }

    #[test]
    fn user_stop_skips_the_occurrence_until_the_next_one() {
        let mut sim = Sim::new(vec![job(1, 600, 630)], true);
        sim.tick(MON + 6 * H);
        // The user presses Stop on the source (which also ends recording).
        sim.running = false;
        sim.recording = false;
        assert!(sim.tick(MON + 6 * H + M).is_empty(), "not restarted");
        assert!(matches!(
            sim.state(0, MON + 6 * H + 2 * M),
            JobState::Skipped { .. }
        ));
        assert!(!sim.s.owns_source());
        // Next day it runs again.
        let fx = sim.tick(MON + 30 * H);
        assert!(fx.iter().any(|e| matches!(e, Effect::StartJob(_))));
    }

    #[test]
    fn user_stopping_only_the_recording_releases_an_owned_source() {
        let mut sim = Sim::new(vec![job(1, 600, 630)], true);
        sim.tick(MON + 6 * H);
        sim.recording = false; // [Stop] on the recorder
        let fx = sim.tick(MON + 6 * H + M);
        assert_eq!(fx, vec![Effect::StopSource]);
    }

    #[test]
    fn a_failed_source_start_is_retried_with_a_backoff() {
        let mut sim = Sim::new(vec![job(1, 600, 700)], true);
        sim.tick(MON + 6 * H);
        sim.running = false; // engine start failed; recording stays wanted
        assert!(sim.tick(MON + 6 * H + 1).is_empty());
        assert!(matches!(
            sim.state(0, MON + 6 * H + 2),
            JobState::Waiting {
                why: Wait::Retry(_),
                ..
            }
        ));
        assert!(sim.tick(MON + 6 * H + 5).is_empty(), "backing off");
        assert_eq!(
            sim.tick(MON + 6 * H + 1 + RETRY_SECS),
            vec![Effect::StartSource]
        );
    }

    #[test]
    fn a_manual_recording_is_not_interrupted() {
        let mut sim = Sim::new(vec![job(1, 600, 630)], true);
        sim.running = true;
        sim.recording = true; // the user is recording by hand
        assert!(sim.tick(MON + 6 * H).is_empty());
        assert!(matches!(
            sim.state(0, MON + 6 * H),
            JobState::Waiting {
                why: Wait::ManualRecording,
                ..
            }
        ));
        sim.recording = false; // the user stops
        let fx = sim.tick(MON + 6 * H + 5 * M);
        assert!(matches!(&fx[..], [Effect::StartJob(_)]));
    }

    #[test]
    fn overlapping_jobs_run_one_after_the_other() {
        // Job 2 starts first; job 1 waits and runs when job 2 ends.
        let mut sim = Sim::new(vec![job(1, 615, 700), job(2, 600, 630)], true);
        assert!(sim.s.conflicts(0) && sim.s.conflicts(1));
        sim.tick(MON + 6 * H + 20 * M);
        assert_eq!(sim.started, vec![2], "earliest start wins");
        assert!(matches!(
            sim.state(0, MON + 6 * H + 20 * M),
            JobState::Waiting {
                why: Wait::OtherJob,
                ..
            }
        ));
        let fx = sim.tick(MON + 6 * H + 30 * M);
        assert_eq!(fx[0], Effect::StopJob(JobAction::Record));
        assert!(matches!(&fx[1], Effect::StartJob(j) if j.id == 1));
        assert!(
            !fx.contains(&Effect::StopSource),
            "back to back: the source stays on"
        );
        assert!(sim.s.owns_source());
    }

    #[test]
    fn once_job_switches_itself_off_after_its_occurrence() {
        let mut j = job(1, 600, 630);
        j.repeat = Repeat::Once;
        let mut sim = Sim::new(vec![j], true);
        sim.tick(MON + 6 * H);
        assert!(!sim.s.take_dirty());
        sim.tick(MON + 6 * H + 30 * M);
        assert!(!sim.s.jobs[0].enabled);
        assert!(sim.s.take_dirty(), "to be saved");
        assert!(sim.tick(MON + 30 * H).is_empty(), "not again tomorrow");
    }

    #[test]
    fn disabling_or_deleting_a_running_job_stops_it() {
        let mut sim = Sim::new(vec![job(1, 600, 700)], true);
        sim.tick(MON + 6 * H);
        sim.s.jobs[0].enabled = false;
        let fx = sim.tick(MON + 6 * H + M);
        assert_eq!(
            fx,
            vec![Effect::StopJob(JobAction::Record), Effect::StopSource]
        );
        let mut sim = Sim::new(vec![job(1, 600, 700)], true);
        sim.tick(MON + 6 * H);
        sim.s.jobs.clear();
        assert_eq!(
            sim.tick(MON + 6 * H + M)[0],
            Effect::StopJob(JobAction::Record)
        );
    }

    #[test]
    fn nothing_fires_while_the_clock_is_not_set() {
        let mut sim = Sim::new(vec![job(1, 0, 0)], true);
        assert!(sim.tick(1_000).is_empty(), "1970: Pi before NTP sync");
        assert_eq!(sim.state(0, 1_000), JobState::ClockNotSet);
    }

    #[test]
    fn idle_job_reports_its_next_start_and_next_start_spans_jobs() {
        let s = Scheduler::new(vec![job(1, 900, 930), job(2, 700, 730)], true);
        let now = MON + 6 * H;
        let inp = Inputs {
            now,
            running: false,
            recording: false,
        };
        assert_eq!(
            s.job_state(0, inp),
            JobState::Idle {
                next: Some(MON + 9 * H)
            }
        );
        assert_eq!(s.next_start(now), Some((1, MON + 7 * H)));
        assert!(!s.conflicts(0));
    }

    #[test]
    fn switched_off_scheduler_fires_nothing() {
        let mut sim = Sim::new(vec![job(1, 600, 630)], true);
        sim.s.enabled = false;
        assert!(sim.tick(MON + 6 * H).is_empty());
        assert_eq!(sim.state(0, MON + 6 * H), JobState::SchedulerOff);
        sim.s.enabled = true;
        assert!(
            sim.tick(MON + 6 * H + M)
                .iter()
                .any(|e| matches!(e, Effect::StartJob(_))),
            "switched back on inside the window: it starts"
        );
    }

    #[test]
    fn switching_off_mid_job_stops_it_and_leaves_the_source_to_the_user() {
        let mut sim = Sim::new(vec![job(1, 600, 700)], true);
        sim.tick(MON + 6 * H);
        assert!(sim.s.owns_source());
        sim.s.enabled = false;
        assert_eq!(
            sim.tick(MON + 6 * H + M),
            vec![Effect::StopJob(JobAction::Record)]
        );
        assert!(sim.running, "the user is listening: the source stays on");
        assert!(!sim.s.owns_source());
        sim.s.enabled = true;
        assert!(
            sim.tick(MON + 6 * H + 2 * M).is_empty(),
            "the interrupted occurrence is skipped"
        );
    }

    fn tune(id: u64, start: u16) -> Job {
        Job {
            action: JobAction::Tune,
            // Older files may hold any end: it is ignored.
            ..job(id, start, start + 30)
        }
    }

    #[test]
    fn a_tune_job_tunes_once_at_its_start_and_holds_nothing() {
        let mut sim = Sim::new(vec![tune(1, 600)], true);
        assert!(sim.tick(MON + 6 * H - 1).is_empty());
        let fx = sim.tick(MON + 6 * H + 5);
        assert_eq!(fx[0], Effect::StartSource);
        assert!(matches!(&fx[1], Effect::StartJob(j) if j.id == 1));
        assert_eq!(fx.len(), 2, "the source is left running for listening");
        assert!(!sim.recording);
        assert!(!sim.s.owns_source(), "the source is the user's");
        assert!(sim.s.current_job().is_none(), "holds no slot");
        assert!(sim.tick(MON + 6 * H + 30).is_empty(), "fires once");
        assert_eq!(
            sim.state(0, MON + 6 * H + 30),
            JobState::Idle {
                next: Some(MON + 30 * H)
            }
        );
        assert!(sim.tick(MON + 6 * H + 31 * M).is_empty(), "no end");
        assert!(sim.running);
        assert_eq!(sim.s.next_start(MON + 6 * H), Some((0, MON + 30 * H)));
    }

    #[test]
    fn a_tune_job_without_auto_source_only_tunes() {
        let mut sim = Sim::new(vec![tune(1, 600)], false);
        let fx = sim.tick(MON + 6 * H);
        assert!(matches!(&fx[..], [Effect::StartJob(_)]));
        assert!(!sim.running);
    }

    #[test]
    fn a_tune_job_missed_in_its_minute_does_not_fire_later() {
        let mut sim = Sim::new(vec![tune(1, 600)], true);
        assert!(sim.tick(MON + 6 * H + M).is_empty());
    }

    #[test]
    fn a_record_job_can_start_in_the_tune_jobs_minute() {
        let mut sim = Sim::new(vec![job(2, 600, 630), tune(1, 600)], true);
        let fx = sim.tick(MON + 6 * H);
        assert_eq!(
            sim.started,
            vec![1, 2],
            "the tune first, then the recording"
        );
        assert_eq!(fx[0], Effect::StartSource);
        assert_eq!(fx.iter().filter(|e| **e == Effect::StartSource).count(), 1);
        assert!(sim.recording);
        assert!(!sim.s.conflicts(0) && !sim.s.conflicts(1), "no overlap");
        let fx = sim.tick(MON + 6 * H + 30 * M);
        assert_eq!(
            fx,
            vec![Effect::StopJob(JobAction::Record)],
            "the tuned-in source stays"
        );
    }

    #[test]
    fn a_tune_job_spares_a_running_recording() {
        let mut sim = Sim::new(vec![job(2, 550, 630), tune(1, 600)], true);
        sim.tick(MON + 5 * H + 50 * M);
        sim.tick(MON + 6 * H);
        assert_eq!(sim.started, vec![2], "a running job is not retuned");
        let mut sim = Sim::new(vec![tune(1, 600)], true);
        sim.running = true;
        sim.recording = true;
        assert!(sim.tick(MON + 6 * H).is_empty(), "nor a manual recording");
    }

    #[test]
    fn a_once_tune_job_switches_itself_off_after_firing() {
        let mut sim = Sim::new(
            vec![Job {
                repeat: Repeat::Once,
                ..tune(1, 600)
            }],
            true,
        );
        sim.tick(MON + 6 * H);
        assert!(!sim.s.jobs[0].enabled);
        assert!(sim.s.take_dirty());
    }

    #[test]
    fn next_id_is_unused() {
        assert_eq!(Scheduler::default().next_id(), 1);
        let s = Scheduler::new(vec![job(4, 0, 0), job(2, 0, 0)], true);
        assert_eq!(s.next_id(), 5);
    }

    #[test]
    fn switching_on_restarts_an_open_recording_even_one_the_user_stopped() {
        let mut sim = Sim::new(vec![job(1, 600, 700)], true);
        sim.tick(MON + 6 * H);
        assert_eq!(sim.started, [1]);
        // Off: the recording stops, its occurrence is skipped.
        sim.s.enabled = false;
        sim.tick(MON + 6 * H + 10 * M);
        assert!(!sim.recording);
        sim.s.enabled = true;
        sim.tick(MON + 6 * H + 11 * M);
        assert_eq!(sim.started, [1], "without resume it stays skipped");
        assert_eq!(
            sim.s.resume(MON + 6 * H + 12 * M),
            None,
            "no Tune job to apply"
        );
        sim.tick(MON + 6 * H + 12 * M);
        assert_eq!(sim.started, [1, 1], "resumed in a new file");
        // A recording the user stopped is restarted too.
        sim.recording = false;
        sim.tick(MON + 6 * H + 13 * M);
        sim.s.resume(MON + 6 * H + 14 * M);
        sim.tick(MON + 6 * H + 14 * M);
        assert_eq!(sim.started, [1, 1, 1]);
    }

    #[test]
    fn switching_on_applies_the_tune_job_that_fired_last() {
        let mut s = Scheduler::new(vec![tune(1, 600), tune(2, 1600), tune(3, 2300)], true);
        // 17:00: DSC 2 at 16:00 is the current one.
        let (j, at) = s.resume(MON + 17 * H).unwrap();
        assert_eq!((j.id, at), (2, MON + 16 * H));
        // 02:00 on Monday: Sunday's 23:00.
        let (j, at) = s.resume(MON + 2 * H).unwrap();
        assert_eq!((j.id, at), (3, MON - H));
        // An open Record job wins: it is what runs now.
        let mut s = Scheduler::new(vec![tune(1, 600), job(2, 1630, 1730)], true);
        assert_eq!(s.resume(MON + 17 * H), None);
        // Switched off: nothing.
        s.enabled = false;
        assert_eq!(s.resume(MON + 7 * H), None);
    }
}
