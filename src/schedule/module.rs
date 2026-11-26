//! Schedule module state: the scheduler plus the left-panel view (pure).
//!
//! Panel field order (0.3.0, spec 19.5: one row each, then the job rows):
//! `[Manager]`, the scheduler's master [On]/[Off] switch, the jobs. ENTER on
//! a job opens the Manager page on it. The page has its own cursor
//! (`PageField`): the settings row (master switch, Auto start/stop, Local
//! time) and the job table; its function keys add, edit, copy, switch and
//! remove jobs. Jobs are kept ordered by their start time as shown (local
//! time when that is on), then end time, then the order they were added; the
//! selection follows its job when the order changes. Dialogs (the editor, the confirmation) are returned as a
//! `Request` for `AppState`, as in the Bookmarks module.

use super::job::Job;
use super::planner::Scheduler;
use super::store::{DEFAULT_LIST, ScheduleDoc, ScheduleList};

/// Job rows shown at once (the list scrolls).
pub const JOB_ROWS: usize = 6;

/// The panel's fields, in navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedField {
    /// `[Manager]`: open the module's page (spec 19.2).
    Manager,
    /// The master switch.
    Enabled,
    /// The list shown (`List:`).
    List,
    Items,
}

impl SchedField {
    pub const ALL: [SchedField; 4] = [
        SchedField::Manager,
        SchedField::Enabled,
        SchedField::List,
        SchedField::Items,
    ];

    fn step(self, dir: i32) -> Self {
        let n = Self::ALL.len() as i32;
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0) as i32;
        Self::ALL[(i + dir).rem_euclid(n) as usize]
    }
}

/// The Manager page's fields (spec 19.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageField {
    /// The settings row; `SchedSetting` says which one.
    Settings,
    /// The list rows (the cursor is the list shown).
    Lists,
    ListButtons,
    /// The job table.
    Table,
}

impl PageField {
    pub const ALL: [PageField; 4] = [
        PageField::Settings,
        PageField::Lists,
        PageField::ListButtons,
        PageField::Table,
    ];

    fn step(self, dir: i32) -> Self {
        let n = Self::ALL.len() as i32;
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0) as i32;
        Self::ALL[(i + dir).rem_euclid(n) as usize]
    }
}

/// The switches on the page's settings row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedSetting {
    Enabled,
    AutoSource,
    LocalTime,
}

impl SchedSetting {
    pub const ALL: [SchedSetting; 3] = [
        SchedSetting::Enabled,
        SchedSetting::AutoSource,
        SchedSetting::LocalTime,
    ];
}

/// An action that needs a dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedRequest {
    None,
    /// Open the module's page in the Work Area.
    OpenPage,
    Add,
    Edit(usize),
    /// A new job from job `idx` (the editor opens on the copy).
    Copy(usize),
    Remove(usize),
    /// Pick a list to move job `idx` of the list shown to.
    Move(usize),
    /// Pick the list to show (panel `List:`).
    PickList,
    /// Ask for a new list's name / the shown list's new name.
    NewList,
    RenameList,
    /// Confirm deleting the list shown.
    DeleteList,
    /// Make the list shown the active one (asks while a job records).
    Activate,
}

/// The page's list buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedListButton {
    New,
    Rename,
    Delete,
    Activate,
}

impl SchedListButton {
    pub const ALL: [SchedListButton; 4] = [
        SchedListButton::New,
        SchedListButton::Rename,
        SchedListButton::Delete,
        SchedListButton::Activate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SchedListButton::New => "[New]",
            SchedListButton::Rename => "[Rename]",
            SchedListButton::Delete => "[Delete]",
            SchedListButton::Activate => "[Activate]",
        }
    }
}

/// The Schedule module.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleModule {
    /// The planner, holding the active list's jobs (only they run).
    pub scheduler: Scheduler,
    /// The lists in order; the active one's `jobs` are kept in
    /// `scheduler.jobs` (this entry's are empty meanwhile).
    lists: Vec<ScheduleList>,
    /// The list that runs.
    active: usize,
    /// The list shown and edited (panel and page); the cursor is in it.
    view: usize,
    /// Show and enter times in local time.
    pub local_time: bool,
    field: SchedField,
    page_field: PageField,
    setting: SchedSetting,
    list_button: SchedListButton,
    /// Selected job index.
    cursor: Option<usize>,
    /// First job row shown.
    scroll: usize,
    /// Settings or jobs changed by the user (to be saved).
    dirty: bool,
    /// Local time's offset from UTC in minutes (orders the jobs when local
    /// time is shown).
    utc_offset_min: i32,
    /// The scheduler was switched on or a list activated: the current job
    /// is to be restored (taken by the app at its next tick).
    resume: bool,
}

impl Default for ScheduleModule {
    fn default() -> Self {
        Self::from_doc(ScheduleDoc::default())
    }
}

impl ScheduleModule {
    pub fn from_doc(doc: ScheduleDoc) -> Self {
        let mut lists = doc.lists;
        if lists.is_empty() {
            lists.push(ScheduleList {
                name: DEFAULT_LIST.to_string(),
                jobs: Vec::new(),
            });
        }
        let active = lists.iter().position(|l| l.name == doc.active).unwrap_or(0);
        let jobs = std::mem::take(&mut lists[active].jobs);
        let mut scheduler = Scheduler::new(jobs, doc.auto_source);
        scheduler.enabled = doc.enabled;
        let mut m = Self {
            scheduler,
            lists,
            active,
            view: active,
            local_time: doc.local_time,
            field: SchedField::Enabled,
            page_field: PageField::Table,
            setting: SchedSetting::Enabled,
            list_button: SchedListButton::New,
            cursor: None,
            scroll: 0,
            dirty: false,
            utc_offset_min: 0,
            resume: false,
        };
        m.sort_all();
        m.cursor = (!m.jobs().is_empty()).then_some(0);
        m.clamp();
        m
    }

    // ── Lists (0.3.0, spec 19.8) ─────────────────────────────────────────

    pub fn list_names(&self) -> Vec<String> {
        self.lists.iter().map(|l| l.name.clone()).collect()
    }

    pub fn active_list(&self) -> usize {
        self.active
    }

    pub fn view_list(&self) -> usize {
        self.view
    }

    pub fn view_name(&self) -> &str {
        &self.lists[self.view].name
    }

    pub fn active_name(&self) -> &str {
        &self.lists[self.active].name
    }

    /// Whether the list shown is the one that runs (job states and the
    /// planner's indices apply only then).
    pub fn viewing_active(&self) -> bool {
        self.view == self.active
    }

    /// The number of jobs in list `i`.
    pub fn list_len(&self, i: usize) -> usize {
        if i == self.active {
            self.scheduler.jobs.len()
        } else {
            self.lists.get(i).map_or(0, |l| l.jobs.len())
        }
    }

    /// Show list `i` (the selection goes to its first job).
    pub fn select_list(&mut self, i: usize) {
        if i < self.lists.len() && i != self.view {
            self.view = i;
            self.cursor = (!self.jobs().is_empty()).then_some(0);
            self.scroll = 0;
            self.clamp();
        }
    }

    pub fn cycle_list(&mut self, dir: i32) {
        let n = self.lists.len() as i32;
        self.select_list((self.view as i32 + dir).rem_euclid(n) as usize);
    }

    /// Make the list shown the one that runs. A job of the old list that is
    /// recording ends (the planner no longer finds it).
    pub fn activate_view(&mut self) {
        if self.view == self.active {
            return;
        }
        let old = std::mem::take(&mut self.scheduler.jobs);
        self.lists[self.active].jobs = old;
        self.scheduler.jobs = std::mem::take(&mut self.lists[self.view].jobs);
        self.active = self.view;
        self.dirty = true;
        self.resume = self.scheduler.enabled;
    }

    /// Whether the current job is to be restored (once).
    pub fn take_resume(&mut self) -> bool {
        std::mem::take(&mut self.resume)
    }

    fn clean_list_name(&self, name: &str, except: Option<usize>) -> Result<String, String> {
        let n = name.trim();
        if n.is_empty() {
            return Err("A list needs a name".to_string());
        }
        if self
            .lists
            .iter()
            .enumerate()
            .any(|(i, l)| l.name == n && Some(i) != except)
        {
            return Err(format!("\"{n}\" already exists"));
        }
        Ok(n.to_string())
    }

    /// A list name not yet used ("New List", "New List (1)"...).
    pub fn unique_list_name(&self, base: &str) -> String {
        crate::bookmarks::model::unique_name(base, |n| self.lists.iter().any(|l| l.name == n))
    }

    /// Add an empty list and show it.
    pub fn create_list(&mut self, name: &str) -> Result<(), String> {
        let name = self.clean_list_name(name, None)?;
        self.lists.push(ScheduleList {
            name,
            jobs: Vec::new(),
        });
        self.dirty = true;
        self.select_list(self.lists.len() - 1);
        Ok(())
    }

    pub fn rename_list(&mut self, name: &str) -> Result<(), String> {
        let name = self.clean_list_name(name, Some(self.view))?;
        self.lists[self.view].name = name;
        self.dirty = true;
        Ok(())
    }

    /// Delete the list shown; the active list cannot be deleted.
    pub fn delete_list(&mut self) -> Result<(), String> {
        if self.view == self.active {
            return Err("The active list cannot be deleted".to_string());
        }
        self.lists.remove(self.view);
        if self.active > self.view {
            self.active -= 1;
        }
        self.dirty = true;
        let v = self.view.min(self.lists.len() - 1);
        self.view = usize::MAX;
        self.select_list(v);
        Ok(())
    }

    /// Move job `idx` of the list shown into list `target` (a fresh id; it
    /// stays selected in its new list).
    pub fn move_job(&mut self, idx: usize, target: usize) {
        if target >= self.lists.len() || target == self.view || idx >= self.jobs().len() {
            return;
        }
        let mut job = self.jobs_mut().remove(idx);
        job.id = self.next_id();
        let dest = if target == self.active {
            &mut self.scheduler.jobs
        } else {
            &mut self.lists[target].jobs
        };
        dest.push(job.clone());
        self.dirty = true;
        self.view = target;
        self.sort_jobs();
        self.cursor = self.jobs().iter().position(|j| j.id == job.id);
        self.clamp();
    }

    /// An id not used by any job of any list.
    fn next_id(&self) -> u64 {
        self.lists
            .iter()
            .flat_map(|l| l.jobs.iter())
            .chain(self.scheduler.jobs.iter())
            .map(|j| j.id)
            .max()
            .map_or(1, |m| m + 1)
    }

    fn jobs_mut(&mut self) -> &mut Vec<Job> {
        if self.view == self.active {
            &mut self.scheduler.jobs
        } else {
            &mut self.lists[self.view].jobs
        }
    }

    /// Set local time's offset from UTC (called every tick); re-sorts the
    /// jobs when it changed (DST) and local time is shown.
    pub fn set_utc_offset(&mut self, offset_min: i32) {
        if offset_min != self.utc_offset_min {
            self.utc_offset_min = offset_min;
            self.sort_all();
        }
    }

    /// Sort every list (the selection stays on its job in the list shown).
    fn sort_all(&mut self) {
        let offset = self.shown_offset();
        for i in 0..self.lists.len() {
            if i != self.view {
                let jobs = if i == self.active {
                    &mut self.scheduler.jobs
                } else {
                    &mut self.lists[i].jobs
                };
                sort_by_shown_start(jobs, offset);
            }
        }
        self.sort_jobs();
    }

    /// The offset the job times are shown in (0 in UTC).
    fn shown_offset(&self) -> i32 {
        if self.local_time {
            self.utc_offset_min
        } else {
            0
        }
    }

    /// Order the jobs by their shown start, then end time (stable, so equal
    /// windows keep the order they were added in). The selection stays on
    /// its job.
    fn sort_jobs(&mut self) {
        let offset = self.shown_offset();
        let selected = self.selected_job().map(|j| j.id);
        sort_by_shown_start(self.jobs_mut(), offset);
        if let Some(id) = selected {
            self.cursor = self.jobs().iter().position(|j| j.id == id);
        }
        self.clamp();
    }

    pub fn to_doc(&self) -> ScheduleDoc {
        let mut lists = self.lists.clone();
        lists[self.active].jobs = self.scheduler.jobs.clone();
        ScheduleDoc {
            enabled: self.scheduler.enabled,
            auto_source: self.scheduler.auto_source,
            local_time: self.local_time,
            active: self.lists[self.active].name.clone(),
            lists,
            ..ScheduleDoc::default()
        }
    }

    /// The jobs of the list shown.
    pub fn jobs(&self) -> &[Job] {
        if self.view == self.active {
            &self.scheduler.jobs
        } else {
            &self.lists[self.view].jobs
        }
    }

    pub fn field(&self) -> SchedField {
        self.field
    }

    pub fn page_field(&self) -> PageField {
        self.page_field
    }

    pub fn setting(&self) -> SchedSetting {
        self.setting
    }

    pub fn list_button(&self) -> SchedListButton {
        self.list_button
    }

    /// Press a list button on the page.
    pub fn press_list_button(&mut self, b: SchedListButton) -> SchedRequest {
        self.page_field = PageField::ListButtons;
        self.list_button = b;
        match b {
            SchedListButton::New => SchedRequest::NewList,
            SchedListButton::Rename => SchedRequest::RenameList,
            SchedListButton::Delete => SchedRequest::DeleteList,
            SchedListButton::Activate => SchedRequest::Activate,
        }
    }

    /// A click on the page's list row `i`.
    pub fn click_page_list(&mut self, i: usize) {
        self.page_field = PageField::Lists;
        self.select_list(i);
    }

    pub fn move_request(&self) -> SchedRequest {
        self.cursor.map_or(SchedRequest::None, SchedRequest::Move)
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn selected_job(&self) -> Option<&Job> {
        self.cursor.and_then(|i| self.jobs().get(i))
    }

    /// Whether anything needs saving (user edits or the scheduler switching
    /// a Once job off). Clears the flags.
    pub fn take_dirty(&mut self) -> bool {
        let sched = self.scheduler.take_dirty();
        std::mem::take(&mut self.dirty) || sched
    }

    fn clamp(&mut self) {
        let n = self.jobs().len();
        self.cursor = match (n, self.cursor) {
            (0, _) => None,
            (_, None) => Some(0),
            (_, Some(c)) => Some(c.min(n - 1)),
        };
        if let Some(c) = self.cursor {
            if c < self.scroll {
                self.scroll = c;
            } else if c >= self.scroll + JOB_ROWS {
                self.scroll = c + 1 - JOB_ROWS;
            }
        }
        self.scroll = self.scroll.min(n.saturating_sub(JOB_ROWS));
    }

    // ── Navigation ───────────────────────────────────────────────────────

    pub fn select_field(&mut self, f: SchedField) {
        self.field = f;
    }

    /// Put the page's cursor on setting `s`.
    pub fn select_setting(&mut self, s: SchedSetting) {
        self.page_field = PageField::Settings;
        self.setting = s;
    }

    /// A click on the page's job `idx` (an index into `jobs()`).
    pub fn click_page_row(&mut self, idx: usize) {
        if idx < self.jobs().len() {
            self.page_field = PageField::Table;
            self.cursor = Some(idx);
            self.clamp();
        }
    }

    /// A click on job row `pos` (index into the visible window).
    pub fn click_row(&mut self, pos: usize) {
        let i = self.scroll + pos;
        if i < self.jobs().len() {
            self.field = SchedField::Items;
            self.cursor = Some(i);
            self.clamp();
        }
    }

    pub fn up(&mut self) {
        if self.field == SchedField::Items
            && let Some(c) = self.cursor
            && c > 0
        {
            self.cursor = Some(c - 1);
            self.clamp();
            return;
        }
        self.field = self.field.step(-1);
        if self.field == SchedField::Items {
            self.cursor = self.jobs().len().checked_sub(1);
            self.clamp();
        }
    }

    pub fn down(&mut self) {
        if self.field == SchedField::Items
            && let Some(c) = self.cursor
            && c + 1 < self.jobs().len()
        {
            self.cursor = Some(c + 1);
            self.clamp();
            return;
        }
        self.field = self.field.step(1);
        if self.field == SchedField::Items {
            self.cursor = (!self.jobs().is_empty()).then_some(0);
            self.clamp();
        }
    }

    pub fn first(&mut self) {
        if self.field == SchedField::Items && !self.jobs().is_empty() {
            self.cursor = Some(0);
            self.clamp();
        }
    }

    pub fn last(&mut self) {
        if self.field == SchedField::Items {
            self.cursor = self.jobs().len().checked_sub(1);
            self.clamp();
        }
    }

    /// Scroll the job window (wheel) without moving the selection.
    pub fn scroll_view(&mut self, delta: i32) {
        let max = self.jobs().len().saturating_sub(JOB_ROWS) as i32;
        self.scroll = (self.scroll as i32 + delta).clamp(0, max) as usize;
    }

    /// Left/Right: flip a checkbox, or move between buttons.
    pub fn horizontal(&mut self, dir: i32) {
        match self.field {
            SchedField::Enabled => {
                self.enter();
            }
            SchedField::List | SchedField::Items => self.cycle_list(dir),
            SchedField::Manager => {}
        }
    }

    /// ENTER on the focused panel field: `[Manager]` and a job open the
    /// page (on that job), the master switch flips.
    pub fn enter(&mut self) -> SchedRequest {
        match self.field {
            SchedField::Manager => SchedRequest::OpenPage,
            SchedField::Enabled => {
                self.flip(SchedSetting::Enabled);
                SchedRequest::None
            }
            SchedField::List => SchedRequest::PickList,
            SchedField::Items => {
                self.page_field = PageField::Table;
                SchedRequest::OpenPage
            }
        }
    }

    /// Flip one of the switches.
    pub fn flip(&mut self, s: SchedSetting) {
        match s {
            SchedSetting::Enabled => {
                self.scheduler.enabled = !self.scheduler.enabled;
                self.resume = self.scheduler.enabled;
            }
            SchedSetting::AutoSource => self.scheduler.auto_source = !self.scheduler.auto_source,
            SchedSetting::LocalTime => {
                self.local_time = !self.local_time;
                self.sort_all();
            }
        }
        self.dirty = true;
    }

    // ── The Manager page (spec 19.5) ─────────────────────────────────────

    /// Up on the page: the previous job or list, else the previous field
    /// (wrapping; into the table at its last job).
    pub fn page_up(&mut self) {
        match (self.page_field, self.cursor) {
            (PageField::Table, Some(c)) if c > 0 => {
                self.cursor = Some(c - 1);
                return self.clamp();
            }
            (PageField::Lists, _) if self.view > 0 => return self.select_list(self.view - 1),
            _ => {}
        }
        self.page_field = self.page_field.step(-1);
        match self.page_field {
            PageField::Table => {
                self.cursor = self.jobs().len().checked_sub(1);
                self.clamp();
            }
            PageField::Lists => self.select_list(self.lists.len() - 1),
            _ => {}
        }
    }

    /// Down on the page: the next job or list, else the next field
    /// (wrapping).
    pub fn page_down(&mut self) {
        match (self.page_field, self.cursor) {
            (PageField::Table, Some(c)) if c + 1 < self.jobs().len() => {
                self.cursor = Some(c + 1);
                return self.clamp();
            }
            (PageField::Lists, _) if self.view + 1 < self.lists.len() => {
                return self.select_list(self.view + 1);
            }
            _ => {}
        }
        self.page_field = self.page_field.step(1);
        match self.page_field {
            PageField::Table => {
                self.cursor = (!self.jobs().is_empty()).then_some(0);
                self.clamp();
            }
            PageField::Lists => self.select_list(0),
            _ => {}
        }
    }

    /// Left/Right on the settings row move between its switches.
    pub fn page_horizontal(&mut self, dir: i32) {
        match self.page_field {
            PageField::Settings => {
                let all = SchedSetting::ALL;
                let i = all.iter().position(|s| *s == self.setting).unwrap_or(0) as i32;
                self.setting = all[(i + dir).rem_euclid(all.len() as i32) as usize];
            }
            PageField::ListButtons => {
                let all = SchedListButton::ALL;
                let i = all.iter().position(|b| *b == self.list_button).unwrap_or(0) as i32;
                self.list_button = all[(i + dir).rem_euclid(all.len() as i32) as usize];
            }
            PageField::Lists | PageField::Table => self.cycle_list(dir),
        }
    }

    /// ENTER on the page: flip the switch, or edit the job.
    pub fn page_enter(&mut self) -> SchedRequest {
        match self.page_field {
            PageField::Settings => {
                self.flip(self.setting);
                SchedRequest::None
            }
            PageField::Lists => {
                self.page_field = PageField::Table;
                self.cursor = (!self.jobs().is_empty()).then_some(0);
                self.clamp();
                SchedRequest::None
            }
            PageField::ListButtons => self.press_list_button(self.list_button),
            PageField::Table => self.edit_request(),
        }
    }

    pub fn edit_request(&self) -> SchedRequest {
        self.cursor.map_or(SchedRequest::None, SchedRequest::Edit)
    }

    pub fn copy_request(&self) -> SchedRequest {
        self.cursor.map_or(SchedRequest::None, SchedRequest::Copy)
    }

    /// The page's Delete / F8.
    pub fn remove_request(&self) -> SchedRequest {
        self.cursor.map_or(SchedRequest::None, SchedRequest::Remove)
    }

    /// Switch the selected job off or on (F5).
    pub fn toggle_job(&mut self) {
        if let Some(i) = self.cursor {
            let j = &mut self.jobs_mut()[i];
            j.enabled = !j.enabled;
            self.dirty = true;
        }
    }

    // ── Editing jobs ─────────────────────────────────────────────────────

    /// Add `job` (a fresh id is assigned) or replace job `original`, keeping
    /// its id. Selects it.
    pub fn save_job(&mut self, original: Option<usize>, mut job: Job) {
        match original.filter(|i| *i < self.jobs().len()) {
            Some(i) => {
                job.id = self.jobs()[i].id;
                self.jobs_mut()[i] = job;
                self.cursor = Some(i);
            }
            None => {
                job.id = self.next_id();
                self.jobs_mut().push(job);
                self.cursor = Some(self.jobs().len() - 1);
            }
        }
        self.field = SchedField::Items;
        self.dirty = true;
        self.sort_jobs();
    }

    pub fn remove_job(&mut self, idx: usize) {
        if idx < self.jobs().len() {
            self.jobs_mut().remove(idx);
            self.dirty = true;
            self.clamp();
        }
    }
}

/// Order `jobs` by their shown start, then end time (stable, so equal
/// windows keep the order they were added in).
fn sort_by_shown_start(jobs: &mut [Job], offset: i32) {
    jobs.sort_by_key(|j| {
        let w = j.window.shifted(offset);
        (w.start, w.end)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::job::BookmarkRef;

    fn job(name: &str) -> Job {
        Job {
            bookmark: BookmarkRef {
                list: "General".into(),
                name: name.into(),
            },
            ..Job::default()
        }
    }

    fn module_with(n: usize) -> ScheduleModule {
        let mut m = ScheduleModule::default();
        for i in 0..n {
            m.save_job(None, job(&format!("j{i}")));
        }
        m.take_dirty();
        m
    }

    #[test]
    fn fields_cycle_and_job_rows_scroll() {
        assert_eq!(
            ScheduleModule::default().field(),
            SchedField::Enabled,
            "master switch first"
        );
        let mut m = module_with(8);
        m.select_field(SchedField::Enabled);
        m.down();
        assert_eq!(m.field(), SchedField::List);
        m.down();
        assert_eq!((m.field(), m.cursor()), (SchedField::Items, Some(0)));
        for _ in 0..7 {
            m.down();
        }
        assert_eq!(m.cursor(), Some(7));
        assert_eq!(m.scroll(), 8 - JOB_ROWS, "the window follows");
        m.down();
        assert_eq!(m.field(), SchedField::Manager, "wraps");
        m.up();
        assert_eq!((m.field(), m.cursor()), (SchedField::Items, Some(7)));
    }

    #[test]
    fn checkboxes_flip_and_mark_dirty() {
        let mut m = module_with(0);
        assert!(m.scheduler.enabled, "on by default");
        m.enter();
        assert!(!m.scheduler.enabled, "[On]/[Off] flips the master switch");
        assert!(!m.to_doc().enabled, "and it is saved");
        assert!(m.take_dirty());
        m.select_setting(SchedSetting::AutoSource);
        assert!(m.scheduler.auto_source);
        m.page_enter();
        assert!(!m.scheduler.auto_source);
        assert!(m.take_dirty());
        m.page_horizontal(1);
        assert_eq!(m.setting(), SchedSetting::LocalTime);
        m.page_enter();
        assert!(m.local_time);
        assert!(m.to_doc().local_time);
    }

    #[test]
    fn page_keys_request_dialogs_and_toggle_switches_a_job() {
        let mut m = module_with(2);
        m.click_page_row(1);
        assert_eq!(m.page_field(), PageField::Table);
        assert_eq!(m.edit_request(), SchedRequest::Edit(1));
        assert_eq!(m.copy_request(), SchedRequest::Copy(1));
        assert_eq!(m.remove_request(), SchedRequest::Remove(1));
        assert_eq!(
            m.page_enter(),
            SchedRequest::Edit(1),
            "ENTER on a row edits"
        );
        m.toggle_job();
        assert!(!m.jobs()[1].enabled);
        assert!(m.take_dirty());
        // the panel: ENTER on a job opens the page on it
        m.select_field(SchedField::Items);
        assert_eq!(m.enter(), SchedRequest::OpenPage);
        m.select_field(SchedField::Manager);
        assert_eq!(m.enter(), SchedRequest::OpenPage);
    }

    #[test]
    fn the_page_walks_settings_and_jobs() {
        let mut m = module_with(3);
        m.click_page_row(0);
        m.page_up();
        assert_eq!(
            m.page_field(),
            PageField::ListButtons,
            "up from the first job"
        );
        m.page_up();
        assert_eq!(m.page_field(), PageField::Lists);
        m.page_up();
        assert_eq!(m.page_field(), PageField::Settings);
        m.page_up();
        assert_eq!(
            (m.page_field(), m.cursor()),
            (PageField::Table, Some(2)),
            "wraps"
        );
        m.page_down();
        assert_eq!(m.page_field(), PageField::Settings, "past the last job");
    }

    #[test]
    fn edit_keeps_the_id_and_add_assigns_a_new_one() {
        let mut m = module_with(2);
        let id1 = m.jobs()[1].id;
        m.save_job(Some(1), job("renamed"));
        assert_eq!(m.jobs()[1].id, id1);
        assert_eq!(m.jobs()[1].name(), "renamed");
        m.save_job(None, job("new"));
        assert_eq!(m.jobs().len(), 3);
        assert!(m.jobs()[2].id > id1);
        assert_eq!(m.cursor(), Some(2), "new job selected");
    }

    #[test]
    fn removing_keeps_a_valid_selection() {
        let mut m = module_with(2);
        m.click_row(1);
        m.remove_job(1);
        assert_eq!(m.cursor(), Some(0));
        m.remove_job(0);
        assert_eq!(m.cursor(), None);
        assert!(m.take_dirty());
    }

    fn timed(name: &str, start: u16, end: u16) -> Job {
        Job {
            window: crate::schedule::window::Window::new(start, end, [true; 7]),
            ..job(name)
        }
    }

    fn names(m: &ScheduleModule) -> Vec<String> {
        m.jobs().iter().map(|j| j.name().to_string()).collect()
    }

    #[test]
    fn jobs_are_ordered_by_start_time_and_the_selection_follows() {
        let mut m = ScheduleModule::default();
        m.save_job(None, timed("late", 1650, 1651));
        m.save_job(None, timed("early", 724, 745));
        assert_eq!(names(&m), ["early", "late"]);
        assert_eq!(
            m.selected_job().unwrap().name(),
            "early",
            "the new job stays selected"
        );
        m.save_job(None, timed("mid", 1500, 1501));
        assert_eq!(names(&m), ["early", "mid", "late"]);
        assert_eq!(m.cursor(), Some(1));
        // Editing the start moves the job; it stays selected.
        m.save_job(Some(0), timed("early", 1700, 1710));
        assert_eq!(names(&m), ["mid", "late", "early"]);
        assert_eq!(m.selected_job().unwrap().name(), "early");
        // Same start: by end, then the order they were added.
        m.save_job(None, timed("mid2", 1500, 1501));
        m.save_job(None, timed("mid0", 1500, 1502));
        assert_eq!(&names(&m)[..3], ["mid", "mid2", "mid0"]);
    }

    #[test]
    fn loading_sorts_and_local_time_orders_by_the_shown_time() {
        let doc = ScheduleDoc::with_jobs(vec![timed("b", 2300, 2330), timed("a", 100, 130)]);
        let mut m = ScheduleModule::from_doc(doc);
        assert_eq!(names(&m), ["a", "b"], "sorted on load");
        assert!(!m.take_dirty(), "sorting alone is not a change");
        // UTC+3: b shows 0200, a shows 0400.
        m.set_utc_offset(180);
        assert_eq!(names(&m), ["a", "b"], "UTC shown: offset ignored");
        m.flip(SchedSetting::LocalTime);
        assert_eq!(names(&m), ["b", "a"], "local time shown");
        m.set_utc_offset(0);
        assert_eq!(names(&m), ["a", "b"], "offset change (DST) re-sorts");
    }

    #[test]
    fn doc_round_trip() {
        let m = module_with(2);
        let back = ScheduleModule::from_doc(m.to_doc());
        assert_eq!(back.jobs(), m.jobs());
        assert_eq!(back.local_time, m.local_time);
    }

    #[test]
    fn only_the_active_list_runs_and_lists_are_managed() {
        let mut m = module_with(2);
        assert_eq!(m.list_names(), ["Default"]);
        assert!(m.viewing_active());
        m.create_list("Fax").unwrap();
        assert!(m.create_list(" fax ").is_ok(), "names are case sensitive");
        assert!(m.create_list("Fax").is_err(), "duplicate");
        assert!(m.create_list("  ").is_err());
        // "Fax" was created and shown; the planner still has Default's jobs.
        m.select_list(1);
        assert_eq!(m.view_name(), "Fax");
        assert!(!m.viewing_active());
        assert!(m.jobs().is_empty());
        assert_eq!(m.scheduler.jobs.len(), 2, "the active list runs");
        m.save_job(None, job("fax1"));
        assert_eq!(
            m.scheduler.jobs.len(),
            2,
            "a job of another list does not run"
        );
        assert_eq!(m.list_len(1), 1);
        // Activate Fax: its job runs, Default's wait.
        m.activate_view();
        assert_eq!(m.active_name(), "Fax");
        assert_eq!(m.scheduler.jobs.len(), 1);
        assert_eq!(m.scheduler.jobs[0].name(), "fax1");
        assert_eq!(m.list_len(0), 2);
        assert!(m.take_dirty());
        // The active list cannot be deleted; another can.
        assert!(m.delete_list().is_err());
        m.select_list(2);
        m.delete_list().unwrap();
        assert_eq!(m.list_names(), ["Default", "Fax"]);
        assert_eq!(m.active_name(), "Fax");
        // Ids stay unique across lists; the file keeps every list.
        let ids: Vec<u64> = m
            .to_doc()
            .lists
            .iter()
            .flat_map(|l| l.jobs.iter().map(|j| j.id))
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len(), "{ids:?}");
        let doc = m.to_doc();
        assert_eq!(doc.active, "Fax");
        let back = ScheduleModule::from_doc(doc);
        assert_eq!(back.active_name(), "Fax");
        assert_eq!(back.scheduler.jobs.len(), 1);
        assert_eq!(back.list_len(0), 2);
    }

    #[test]
    fn a_job_moves_to_another_list_and_stays_selected() {
        let mut m = module_with(2);
        m.create_list("Fax").unwrap();
        m.select_list(0);
        m.click_page_row(1);
        let name = m.jobs()[1].name().to_string();
        m.move_job(1, 1);
        assert_eq!(m.view_name(), "Fax", "the view follows the job");
        assert_eq!(m.selected_job().unwrap().name(), name);
        assert_eq!(m.list_len(0), 1);
        assert_eq!(m.scheduler.jobs.len(), 1, "it left the running list");
        m.rename_list("Weather fax").unwrap();
        assert_eq!(m.list_names(), ["Default", "Weather fax"]);
    }
}
