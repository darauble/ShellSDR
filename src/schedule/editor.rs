//! Schedule job editor form (pure).
//!
//! The modal form behind the Schedule module's [Add] and [Edit]. A job is
//! always bookmark-driven: the Bookmark field opens a picker. In a new job,
//! choosing a bookmark fills Start, End and Days from the bookmark's broadcast
//! schedule, which the user may then override; an edited job keeps its own. Times are shown and typed in UTC, or in
//! local time when the module's "Local time" box is checked (`offset_min`);
//! `build()` converts back, so jobs are always stored in UTC. Same uniform
//! field model as the bookmark editor: Up/Down (TAB) move, ENTER edits or
//! activates, Left/Right change a value, Esc reverts an edit then cancels.
//! The action is two radio rows, `( ) Tune only` and `( ) Record`, with
//! `[x] Native` / `[ ] Baseband` under Record (0.2.0, spec 13.8). With
//! "Tune only" the End, Mute and output fields are disabled (skipped, drawn
//! blank/dim): such a job only tunes, once, at its start.

use super::job::{BookmarkRef, Job, JobAction, Repeat, Tuning};
use super::window::Window;
use crate::bookmarks::editor::EditorButton;
use crate::bookmarks::model::{Bookmark, DAY_LABELS, time_valid};
use crate::ui::text_input::TextInput;

/// The form's fields, in navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedEditorField {
    Bookmark,
    Start,
    End,
    Days,
    Repeat,
    /// `( ) Tune only` (radio).
    TuneOnly,
    /// `( ) Record` (radio).
    Record,
    /// `[x] Native` under Record.
    RecNative,
    /// `[ ] Baseband` under Record.
    RecBaseband,
    /// Mute the VFO while the job runs.
    Mute,
    Buttons,
}

impl SchedEditorField {
    pub const ALL: [SchedEditorField; 11] = [
        SchedEditorField::Bookmark,
        SchedEditorField::Start,
        SchedEditorField::End,
        SchedEditorField::Days,
        SchedEditorField::Repeat,
        SchedEditorField::TuneOnly,
        SchedEditorField::Record,
        SchedEditorField::RecNative,
        SchedEditorField::RecBaseband,
        SchedEditorField::Mute,
        SchedEditorField::Buttons,
    ];

    /// Label; time labels name the clock in use.
    pub fn label(self, local: bool) -> &'static str {
        match self {
            SchedEditorField::Bookmark => "Bookmark",
            SchedEditorField::Start if local => "Start loc",
            SchedEditorField::Start => "Start UTC",
            SchedEditorField::End if local => "End local",
            SchedEditorField::End => "End UTC",
            SchedEditorField::Days => "Days",
            SchedEditorField::Repeat => "Repeat",
            SchedEditorField::TuneOnly => "Action",
            SchedEditorField::Record
            | SchedEditorField::RecNative
            | SchedEditorField::RecBaseband => "",
            SchedEditorField::Mute => "Mute",
            SchedEditorField::Buttons => "",
        }
    }

    fn is_text(self) -> bool {
        matches!(self, SchedEditorField::Start | SchedEditorField::End)
    }
}

/// What an input did to the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedEditorOutcome {
    Open,
    /// Validate with `build()` and save.
    Apply,
    Cancel,
    /// Open the bookmark picker.
    PickBookmark,
}

/// The job editor form.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleEditor {
    /// The edited job's index, or `None` when adding.
    pub original: Option<usize>,
    pub title: String,
    /// The chosen bookmark (required).
    pub bookmark: Option<BookmarkRef>,
    pub tuning: Tuning,
    pub start: TextInput,
    pub end: TextInput,
    /// Weekdays in the displayed clock (index 0 = Sunday).
    pub days: [bool; 7],
    pub day_cursor: usize,
    pub repeat: Repeat,
    pub action: JobAction,
    /// The outputs a Record job records (kept while Tune only is chosen).
    pub record_native: bool,
    pub record_baseband: bool,
    pub mute: bool,
    /// Displayed clock's offset from UTC in minutes (0 = UTC).
    pub offset_min: i32,
    /// Whether times are shown in local time.
    pub local: bool,
    /// Carried over from the edited job.
    enabled: bool,
    field: SchedEditorField,
    editing: bool,
    button: EditorButton,
    backup: Option<TextInput>,
    pub error: Option<String>,
}

impl ScheduleEditor {
    /// A form for a new job. `offset_min` is the local clock's offset when
    /// `local` is set (ignored otherwise).
    pub fn new_add(local: bool, offset_min: i32) -> Self {
        let mut e = Self {
            original: None,
            title: "Add scheduled job".to_string(),
            bookmark: None,
            tuning: Tuning::default(),
            start: TextInput::digits("0000", 4),
            end: TextInput::digits("0000", 4),
            days: [true; 7],
            day_cursor: 0,
            repeat: Repeat::Weekly,
            action: JobAction::Record,
            record_native: true,
            record_baseband: false,
            mute: false,
            offset_min: if local { offset_min } else { 0 },
            local,
            enabled: true,
            field: SchedEditorField::Bookmark,
            editing: false,
            button: EditorButton::Apply,
            backup: None,
            error: None,
        };
        e.set_window(Window::default());
        e
    }

    /// A form over job `idx`.
    pub fn new_edit(idx: usize, job: &Job, local: bool, offset_min: i32) -> Self {
        let mut e = Self::new_add(local, offset_min);
        e.original = Some(idx);
        e.title = "Edit scheduled job".to_string();
        e.bookmark = Some(job.bookmark.clone());
        e.tuning = job.tuning.clone();
        e.repeat = job.repeat;
        e.action = job.action;
        e.record_native = job.record_native;
        e.record_baseband = job.record_baseband;
        e.mute = job.mute;
        e.enabled = job.enabled;
        e.set_window(job.window);
        e
    }

    /// Fill Start/End/Days from a UTC window, in the displayed clock.
    fn set_window(&mut self, utc: Window) {
        let w = utc.shifted(self.offset_min);
        self.start.set(format!("{:04}", w.start));
        self.end.set(format!("{:04}", w.end));
        self.days = w.days;
    }

    /// Choose `b` (in list `list`): its name and tuning fill the form. When
    /// adding, so does its schedule (which may then be overridden); when
    /// editing, the job keeps its times and days (the bookmark may be changed
    /// on purpose without the time).
    pub fn set_bookmark(&mut self, list: &str, b: &Bookmark) {
        self.bookmark = Some(BookmarkRef {
            list: list.to_string(),
            name: b.name.clone(),
        });
        self.tuning = Tuning::of(b);
        if self.native_locked() {
            self.record_native = true;
        }
        if self.original.is_none() {
            self.set_window(Window::new(b.start_utc, b.end_utc, b.days));
        }
        self.error = None;
    }

    pub fn field(&self) -> SchedEditorField {
        self.field
    }

    /// Whether the job's type always records its native output (DSC), so
    /// its Native check is on and locked (spec 14.7).
    pub fn native_locked(&self) -> bool {
        crate::vfo::types::lookup(&self.tuning.vfo_type).is_some_and(|t| t.native_always_on())
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn button(&self) -> EditorButton {
        self.button
    }

    pub fn text(&self, f: SchedEditorField) -> Option<&TextInput> {
        match f {
            SchedEditorField::Start => Some(&self.start),
            SchedEditorField::End => Some(&self.end),
            _ => None,
        }
    }

    fn text_mut(&mut self, f: SchedEditorField) -> Option<&mut TextInput> {
        match f {
            SchedEditorField::Start => Some(&mut self.start),
            SchedEditorField::End => Some(&mut self.end),
            _ => None,
        }
    }

    pub fn day_labels(&self) -> [(&'static str, bool); 7] {
        std::array::from_fn(|i| (DAY_LABELS[i], self.days[i]))
    }

    // ── Navigation ───────────────────────────────────────────────────────

    /// Whether field `f` can be used: a "Tune only" job has no end (it
    /// only tunes at its start) and so nothing to mute.
    pub fn is_enabled(&self, f: SchedEditorField) -> bool {
        !(self.action == JobAction::Tune
            && matches!(
                f,
                SchedEditorField::End
                    | SchedEditorField::Mute
                    | SchedEditorField::RecNative
                    | SchedEditorField::RecBaseband
            ))
    }

    /// Move to the next usable field in direction `dir`.
    fn step_field(&mut self, dir: i32) {
        let all = SchedEditorField::ALL;
        let n = all.len() as i32;
        let mut i = all.iter().position(|f| *f == self.field).unwrap_or(0) as i32;
        loop {
            i = (i + dir).rem_euclid(n);
            if self.is_enabled(all[i as usize]) {
                break;
            }
        }
        self.field = all[i as usize];
    }

    fn finish_edit(&mut self) {
        self.editing = false;
        self.backup = None;
    }

    /// Select `f` (a click), committing an edit of another field. A
    /// disabled field is not selected.
    pub fn select_field(&mut self, f: SchedEditorField) {
        if !self.is_enabled(f) {
            return;
        }
        if f != self.field {
            self.finish_edit();
        }
        self.field = f;
    }

    pub fn up(&mut self) {
        self.finish_edit();
        self.step_field(-1);
    }

    pub fn down(&mut self) {
        self.finish_edit();
        self.step_field(1);
    }

    pub fn tab(&mut self, forward: bool) {
        self.finish_edit();
        self.step_field(if forward { 1 } else { -1 });
    }

    /// Left/Right: move the text cursor while editing, else change the value.
    pub fn horizontal(&mut self, dir: i32) {
        if self.editing {
            let f = self.field;
            if let Some(t) = self.text_mut(f) {
                if dir < 0 { t.left() } else { t.right() }
            }
            return;
        }
        match self.field {
            SchedEditorField::Days => {
                self.day_cursor = (self.day_cursor as i32 + dir).rem_euclid(7) as usize
            }
            SchedEditorField::Repeat => self.repeat = self.repeat.toggled(),
            f @ (SchedEditorField::TuneOnly
            | SchedEditorField::Record
            | SchedEditorField::RecNative
            | SchedEditorField::RecBaseband
            | SchedEditorField::Mute) => self.activate(f),
            SchedEditorField::Buttons => {
                self.button = if self.button == EditorButton::Apply {
                    EditorButton::Cancel
                } else {
                    EditorButton::Apply
                }
            }
            _ => {}
        }
    }

    /// Choose a radio row or flip a checkbox.
    pub fn activate(&mut self, f: SchedEditorField) {
        if !self.is_enabled(f) {
            return;
        }
        match f {
            SchedEditorField::TuneOnly => self.action = JobAction::Tune,
            SchedEditorField::Record => self.action = JobAction::Record,
            SchedEditorField::RecNative if self.native_locked() => self.record_native = true,
            SchedEditorField::RecNative => self.record_native = !self.record_native,
            SchedEditorField::RecBaseband => self.record_baseband = !self.record_baseband,
            SchedEditorField::Mute => self.mute = !self.mute,
            _ => {}
        }
    }

    /// The text of a radio / checkbox row.
    pub fn choice_label(&self, f: SchedEditorField) -> String {
        let radio = |on: bool| if on { "(•)" } else { "( )" };
        let check = |on: bool| if on { "[x]" } else { "[ ]" };
        match f {
            SchedEditorField::TuneOnly => {
                format!("{} Tune only", radio(self.action == JobAction::Tune))
            }
            SchedEditorField::Record => {
                format!("{} Record", radio(self.action == JobAction::Record))
            }
            SchedEditorField::RecNative if self.native_locked() => "    [x] Native (always)".into(),
            SchedEditorField::RecNative => format!("    {} Native", check(self.record_native)),
            SchedEditorField::RecBaseband => {
                format!("    {} Baseband", check(self.record_baseband))
            }
            SchedEditorField::Mute => format!("{} while it runs", check(self.mute)),
            _ => String::new(),
        }
    }

    pub fn toggle_day(&mut self, i: usize) {
        if i < 7 {
            self.finish_edit();
            self.field = SchedEditorField::Days;
            self.day_cursor = i;
            self.days[i] = !self.days[i];
        }
    }

    /// ENTER.
    pub fn enter(&mut self) -> SchedEditorOutcome {
        if self.editing {
            self.finish_edit();
            self.step_field(1);
            return SchedEditorOutcome::Open;
        }
        match self.field {
            SchedEditorField::Bookmark => return SchedEditorOutcome::PickBookmark,
            SchedEditorField::Days => self.days[self.day_cursor] = !self.days[self.day_cursor],
            SchedEditorField::Repeat => self.repeat = self.repeat.toggled(),
            f @ (SchedEditorField::TuneOnly
            | SchedEditorField::Record
            | SchedEditorField::RecNative
            | SchedEditorField::RecBaseband
            | SchedEditorField::Mute) => self.activate(f),
            SchedEditorField::Buttons => return self.press(self.button),
            f => self.begin_text_edit(f),
        }
        SchedEditorOutcome::Open
    }

    pub fn begin_text_edit(&mut self, f: SchedEditorField) {
        if !self.is_enabled(f) {
            return;
        }
        self.select_field(f);
        if let Some(t) = self.text_mut(f) {
            t.select_all();
            let snapshot = t.clone();
            self.backup = Some(snapshot);
            self.editing = true;
        }
    }

    pub fn press(&mut self, b: EditorButton) -> SchedEditorOutcome {
        self.finish_edit();
        self.field = SchedEditorField::Buttons;
        self.button = b;
        match b {
            EditorButton::Apply => SchedEditorOutcome::Apply,
            EditorButton::Cancel => SchedEditorOutcome::Cancel,
        }
    }

    /// Esc: revert the field being edited, or cancel the form.
    pub fn escape(&mut self) -> SchedEditorOutcome {
        if !self.editing {
            return SchedEditorOutcome::Cancel;
        }
        self.editing = false;
        if let Some(b) = self.backup.take() {
            let f = self.field;
            if let Some(t) = self.text_mut(f) {
                *t = b;
            }
        }
        SchedEditorOutcome::Open
    }

    // ── Typing ───────────────────────────────────────────────────────────

    /// A printable character: typed into a time ("type to edit" on a
    /// selected time field); Space toggles the weekday under the cursor.
    pub fn type_char(&mut self, c: char) {
        if !self.editing {
            match self.field {
                f if f.is_text() => self.begin_text_edit(f),
                SchedEditorField::Days if c == ' ' => {
                    self.days[self.day_cursor] = !self.days[self.day_cursor];
                    return;
                }
                _ => return,
            }
        }
        let f = self.field;
        if let Some(t) = self.text_mut(f) {
            t.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        let f = self.field;
        if self.editing
            && let Some(t) = self.text_mut(f)
        {
            t.backspace();
        }
    }

    pub fn delete(&mut self) {
        let f = self.field;
        if self.editing
            && let Some(t) = self.text_mut(f)
        {
            t.delete();
        }
    }

    pub fn home(&mut self) {
        let f = self.field;
        if self.editing
            && let Some(t) = self.text_mut(f)
        {
            t.home();
        }
    }

    pub fn end_key(&mut self) {
        let f = self.field;
        if self.editing
            && let Some(t) = self.text_mut(f)
        {
            t.end();
        }
    }

    // ── Result ───────────────────────────────────────────────────────────

    /// Validate and build the job (id 0: the module assigns or keeps it),
    /// with its window converted back to UTC.
    pub fn build(&self) -> Result<Job, String> {
        let bookmark = self.bookmark.clone().ok_or("Choose a bookmark")?;
        let time = |t: &TextInput, what: &str| -> Result<u16, String> {
            t.text()
                .parse::<u16>()
                .ok()
                .filter(|v| time_valid(*v))
                .ok_or(format!("{what} must be a time HHMM (0000..2359)"))
        };
        let start = time(&self.start, "Start")?;
        let tune = self.action == JobAction::Tune;
        // A "Tune only" job has no end: it is kept as its start minute.
        let end = if tune {
            Window::new(start, start, self.days).first_minute().end
        } else {
            time(&self.end, "End")?
        };
        if start == end && start != 0 {
            return Err("Start and end must differ (0000-0000 = all day)".into());
        }
        if !self.days.iter().any(|d| *d) {
            return Err("Check at least one day".into());
        }
        let record_native = self.record_native || self.native_locked();
        if !tune && !record_native && !self.record_baseband {
            return Err("Choose Native or Baseband to record".into());
        }
        let shown = Window::new(start, end, self.days);
        Ok(Job {
            id: 0,
            enabled: self.enabled,
            bookmark,
            tuning: self.tuning.clone(),
            window: shown.shifted(-self.offset_min),
            repeat: self.repeat,
            action: self.action,
            record_native,
            record_baseband: self.record_baseband,
            rec_type: None,
            mute: self.mute && !tune,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfo::model::Mode;

    fn navtex() -> Bookmark {
        Bookmark {
            name: "NAVTEX".into(),
            frequency_hz: 518_000,
            start_utc: 2300,
            end_utc: 2330,
            days: [false, true, false, false, false, false, false], // Monday
            settings: crate::bookmarks::model::voice_part(Mode::Usb, None, None),
            ..Bookmark::default()
        }
    }

    fn type_str(e: &mut ScheduleEditor, s: &str) {
        for c in s.chars() {
            e.type_char(c);
        }
    }

    #[test]
    fn a_bookmark_is_required() {
        let e = ScheduleEditor::new_add(false, 0);
        assert_eq!(e.build().unwrap_err(), "Choose a bookmark");
        assert_eq!(e.clone().enter(), SchedEditorOutcome::PickBookmark);
    }

    #[test]
    fn choosing_a_bookmark_fills_the_schedule_and_tuning() {
        let mut e = ScheduleEditor::new_add(false, 0);
        e.set_bookmark("Ships", &navtex());
        assert_eq!((e.start.text(), e.end.text()), ("2300", "2330"));
        assert_eq!(e.days, navtex().days);
        let j = e.build().unwrap();
        assert_eq!(j.name(), "NAVTEX");
        assert_eq!(j.bookmark.list, "Ships");
        assert_eq!(j.tuning.frequency_hz, 518_000);
        assert_eq!(j.window, Window::new(2300, 2330, navtex().days));
        assert!(j.enabled);
    }

    #[test]
    fn changing_the_bookmark_of_an_edited_job_keeps_its_times_and_days() {
        let mut add = ScheduleEditor::new_add(false, 0);
        add.set_bookmark("Ships", &navtex());
        let mut job = add.build().unwrap();
        job.window = Window::new(600, 630, [false, true, false, false, false, false, false]);
        let mut e = ScheduleEditor::new_edit(0, &job, false, 0);
        let other = Bookmark {
            name: "Unscheduled".into(),
            frequency_hz: 2_187_500,
            ..Bookmark::default()
        };
        e.set_bookmark("General", &other);
        assert_eq!(
            (e.start.text(), e.end.text()),
            ("0600", "0630"),
            "times kept"
        );
        assert_eq!(e.days, job.window.days, "days kept");
        let j = e.build().unwrap();
        assert_eq!(j.name(), "Unscheduled");
        assert_eq!(
            j.tuning.frequency_hz, 2_187_500,
            "the tuning follows the bookmark"
        );
        assert_eq!(j.window, job.window);
    }

    #[test]
    fn the_bookmark_schedule_can_be_overridden() {
        let mut e = ScheduleEditor::new_add(false, 0);
        e.set_bookmark("Ships", &navtex());
        e.select_field(SchedEditorField::Start);
        type_str(&mut e, "0600"); // type-to-edit replaces the selection
        e.enter();
        assert_eq!(e.field(), SchedEditorField::End);
        type_str(&mut e, "0630");
        e.enter();
        e.toggle_day(3); // add Wednesday
        let j = e.build().unwrap();
        assert_eq!(j.window.start, 600);
        assert_eq!(j.window.end, 630);
        assert!(j.window.days[1] && j.window.days[3]);
    }

    #[test]
    fn local_time_is_shown_and_typed_but_stored_in_utc() {
        // UTC+2: the bookmark's Monday 23:00 UTC shows as Tuesday 01:00.
        let mut e = ScheduleEditor::new_add(true, 120);
        e.set_bookmark("Ships", &navtex());
        assert_eq!((e.start.text(), e.end.text()), ("0100", "0130"));
        assert!(e.days[2] && !e.days[1], "Tuesday locally");
        assert_eq!(SchedEditorField::Start.label(true), "Start loc");
        let j = e.build().unwrap();
        assert_eq!(j.window, Window::new(2300, 2330, navtex().days));
        // Editing shows the stored UTC job in local time again.
        let back = ScheduleEditor::new_edit(0, &j, true, 120);
        assert_eq!(back.start.text(), "0100");
        assert_eq!(back.build().unwrap().window, j.window);
    }

    #[test]
    fn validation_errors() {
        let mut e = ScheduleEditor::new_add(false, 0);
        e.set_bookmark("Ships", &navtex());
        e.start.set("2460");
        assert!(e.build().unwrap_err().starts_with("Start must be"));
        e.start.set("0600");
        e.end.set("0600");
        assert!(
            e.build()
                .unwrap_err()
                .starts_with("Start and end must differ")
        );
        e.start.set("0000");
        e.end.set("0000");
        assert!(e.build().is_ok(), "0000-0000 = all day");
        e.days = [false; 7];
        assert_eq!(e.build().unwrap_err(), "Check at least one day");
    }

    #[test]
    fn action_is_tune_only_or_record_with_native_and_baseband_checks() {
        let mut e = ScheduleEditor::new_add(false, 0);
        e.set_bookmark("Ships", &navtex());
        assert_eq!(e.choice_label(SchedEditorField::TuneOnly), "( ) Tune only");
        assert_eq!(e.choice_label(SchedEditorField::Record), "(•) Record");
        assert_eq!(
            e.choice_label(SchedEditorField::RecNative),
            "    [x] Native"
        );
        assert_eq!(
            e.choice_label(SchedEditorField::RecBaseband),
            "    [ ] Baseband"
        );
        e.select_field(SchedEditorField::RecBaseband);
        e.enter();
        let j = e.build().unwrap();
        assert!(j.record_native && j.record_baseband, "both");
        e.horizontal(1);
        e.select_field(SchedEditorField::RecNative);
        e.enter();
        assert_eq!(
            e.build().unwrap_err(),
            "Choose Native or Baseband to record"
        );
        e.select_field(SchedEditorField::TuneOnly);
        e.enter();
        let j = e.build().unwrap();
        assert_eq!(j.action, JobAction::Tune, "Tune needs no output");
        e.select_field(SchedEditorField::Repeat);
        e.enter();
        assert_eq!(e.repeat, Repeat::Once);
        let back = ScheduleEditor::new_edit(0, &j, false, 0);
        assert_eq!(back.action, JobAction::Tune);
    }

    #[test]
    fn mute_is_a_checkbox_and_round_trips_through_edit() {
        let mut e = ScheduleEditor::new_add(false, 0);
        e.set_bookmark("Ships", &navtex());
        e.select_field(SchedEditorField::Mute);
        assert!(!e.mute);
        e.enter();
        assert!(e.mute);
        e.horizontal(1);
        assert!(!e.mute, "Left/Right toggle it too");
        e.enter();
        let j = e.build().unwrap();
        assert!(j.mute);
        let back = ScheduleEditor::new_edit(0, &j, false, 0);
        assert!(back.mute);
    }

    #[test]
    fn tune_only_has_no_end_no_outputs_and_no_mute() {
        let mut e = ScheduleEditor::new_add(false, 0);
        e.set_bookmark("Ships", &navtex()); // 2300-2330
        e.mute = true;
        e.select_field(SchedEditorField::TuneOnly);
        e.enter();
        for f in [
            SchedEditorField::End,
            SchedEditorField::Mute,
            SchedEditorField::RecNative,
            SchedEditorField::RecBaseband,
        ] {
            assert!(!e.is_enabled(f), "{f:?}");
        }
        // Navigation skips the disabled fields; a click does nothing.
        e.select_field(SchedEditorField::Start);
        e.down();
        assert_eq!(e.field(), SchedEditorField::Days, "End skipped");
        e.select_field(SchedEditorField::Record);
        e.down();
        assert_eq!(
            e.field(),
            SchedEditorField::Buttons,
            "outputs and Mute skipped"
        );
        e.up();
        assert_eq!(e.field(), SchedEditorField::Record);
        e.select_field(SchedEditorField::End);
        assert_eq!(e.field(), SchedEditorField::Record, "not selectable");
        // Start == End and an invalid End are fine: End is not used.
        e.end.set("");
        let j = e.build().unwrap();
        assert_eq!(j.action, JobAction::Tune);
        assert_eq!(j.window.start, 2300);
        assert_eq!(j.window.end, 2301, "stored as its start minute");
        assert!(!j.mute, "nothing to mute");
        // Back to recording: End is required again.
        e.select_field(SchedEditorField::Record);
        e.enter();
        assert!(e.is_enabled(SchedEditorField::End));
        assert!(e.build().unwrap_err().starts_with("End must be"));
    }

    #[test]
    fn tune_only_in_local_time_keeps_its_start() {
        let job = Job {
            action: JobAction::Tune,
            bookmark: BookmarkRef {
                list: "Ships".into(),
                name: "NAVTEX".into(),
            },
            window: Window::new(2330, 2331, [true; 7]),
            ..Job::default()
        };
        let e = ScheduleEditor::new_edit(0, &job, true, 60);
        assert_eq!(e.start.text(), "0030");
        let back = e.build().unwrap();
        assert_eq!((back.window.start, back.window.end), (2330, 2331));
        assert_eq!(back.window.days, job.window.days);
    }

    #[test]
    fn edit_keeps_the_jobs_switch_and_esc_reverts_then_cancels() {
        let job = Job {
            enabled: false,
            bookmark: BookmarkRef {
                list: "Ships".into(),
                name: "NAVTEX".into(),
            },
            window: Window::new(600, 630, [true; 7]),
            ..Job::default()
        };
        let mut e = ScheduleEditor::new_edit(0, &job, false, 0);
        assert!(!e.build().unwrap().enabled, "an Off job stays off");
        e.select_field(SchedEditorField::Start);
        type_str(&mut e, "07");
        assert_eq!(e.escape(), SchedEditorOutcome::Open);
        assert_eq!(e.start.text(), "0600", "reverted");
        assert_eq!(e.escape(), SchedEditorOutcome::Cancel);
    }

    #[test]
    fn a_dsc_bookmark_locks_native_on() {
        let mut e = ScheduleEditor::new_add(false, 0);
        e.record_native = false;
        let mut b = crate::bookmarks::model::Bookmark {
            name: "DSC 8".into(),
            frequency_hz: 8_414_500,
            ..Default::default()
        };
        b.vfo_type = crate::vfo::types::DSC.into();
        e.set_bookmark("L", &b);
        assert!(e.native_locked() && e.record_native);
        e.activate(SchedEditorField::Record);
        e.activate(SchedEditorField::RecNative);
        assert!(e.record_native, "cannot be switched off");
        assert_eq!(
            e.choice_label(SchedEditorField::RecNative),
            "    [x] Native (always)"
        );
    }
}
