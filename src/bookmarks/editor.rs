//! Bookmark editor form (pure).
//!
//! Step 7, schema-driven since 0.2.0 (spec 13.3, 13.7). The modal form behind
//! [Add] and [Edit], mirroring SDR++'s bookmark edit dialog (name, list,
//! frequency, UTC start/end, weekdays, geo info, notes) plus the VFO type and
//! that type's bookmark settings, whose rows are built from the type's
//! schema (Voice: Mode, Bandwidth, CW tone in CW, AM AGC in AM). It uses the
//! app's uniform field model: Up/Down (or TAB) move between fields, ENTER
//! edits a text field or activates a control, Left/Right act in the field,
//! Esc cancels an edit (or the whole form when no field is being edited). The
//! frequency uses the shared digit editor, so it edits exactly like the VFO
//! and center frequencies. `build()` validates and produces the `Bookmark` to
//! save; name uniqueness is checked by the manager on save. A bookmark of a
//! type this build lacks keeps its settings untouched (its Type is
//! read-only).

use serde_json::Value;

use crate::freq::FreqEntry;
use crate::ui::text_input::TextInput;
use crate::vfo::types::{Field, FieldKind, Settings};

use super::model::{Bookmark, DAY_LABELS, time_valid};

/// Upper bound accepted for a bookmark frequency (the digit editor's range).
const MAX_FREQ_HZ: u64 = 9_999_999_999;

/// The form's fields, in navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorField {
    Name,
    List,
    Frequency,
    /// The VFO type (read-only for a type this build lacks).
    Type,
    /// The i-th visible bookmark field of the type's schema.
    Setting(u8),
    Start,
    End,
    Days,
    Geo,
    Notes,
    Buttons,
}

impl EditorField {
    /// Voice's rows by position (Mode, Bandwidth, then CW tone / AM AGC).
    pub const MODE: EditorField = EditorField::Setting(0);
    pub const BANDWIDTH: EditorField = EditorField::Setting(1);
    pub const CW_TONE: EditorField = EditorField::Setting(2);

    /// Label of a fixed field (a setting's label comes from its schema).
    pub fn label(self) -> &'static str {
        match self {
            EditorField::Name => "Name",
            EditorField::List => "List",
            EditorField::Frequency => "Frequency",
            EditorField::Type => "Type",
            EditorField::Setting(_) => "",
            EditorField::Start => "Start UTC",
            EditorField::End => "End UTC",
            EditorField::Days => "Days",
            EditorField::Geo => "Geo info",
            EditorField::Notes => "Notes",
            EditorField::Buttons => "",
        }
    }
}

/// The form's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorButton {
    Apply,
    Cancel,
}

impl EditorButton {
    pub const ALL: [EditorButton; 2] = [EditorButton::Apply, EditorButton::Cancel];

    pub fn label(self) -> &'static str {
        match self {
            EditorButton::Apply => "[Apply]",
            EditorButton::Cancel => "[Cancel]",
        }
    }
}

/// What an input did to the form as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOutcome {
    /// Still open.
    Open,
    /// Apply was pressed: validate with `build()` and save.
    Apply,
    /// Closed without saving.
    Cancel,
}

/// The bookmark editor form.
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkEditor {
    /// The bookmark being edited as (list, index), or `None` for a new one.
    pub original: Option<(usize, usize)>,
    pub title: String,
    pub name: TextInput,
    pub list_names: Vec<String>,
    /// Target list (index into `list_names`).
    pub list: usize,
    pub freq: FreqEntry,
    /// The bookmark's type and its bookmark settings.
    pub vfo_type: String,
    pub settings: Settings,
    pub start: TextInput,
    pub end: TextInput,
    pub days: [bool; 7],
    pub day_cursor: usize,
    pub geo: TextInput,
    pub notes: TextInput,
    field: EditorField,
    editing: bool,
    button: EditorButton,
    /// Text of the field being edited when the edit began (Esc restores it).
    backup: Option<TextInput>,
    /// The text of a number/text setting while it is edited.
    setting_text: TextInput,
    /// Validation error from the last Apply, shown in the form.
    pub error: Option<String>,
}

/// Whether a setting of this kind is typed (as opposed to stepped).
fn is_typed(kind: &FieldKind) -> bool {
    matches!(
        kind,
        FieldKind::Text { .. }
            | FieldKind::Path
            | FieldKind::Integer { .. }
            | FieldKind::Number { .. }
            | FieldKind::FrequencyHz
    )
}

/// The raw text of a typed setting's value (no unit).
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n
            .as_f64()
            .map(crate::vfo::types::schema::fmt_num)
            .unwrap_or_else(|| n.to_string()),
        other => other.to_string(),
    }
}

impl BookmarkEditor {
    /// A form over `bm`, saving into list `list` (an index into
    /// `list_names`). `original` is the edited bookmark's position, or `None`
    /// when adding.
    pub fn new(
        bm: &Bookmark,
        original: Option<(usize, usize)>,
        list: usize,
        list_names: Vec<String>,
    ) -> Self {
        Self {
            original,
            title: if original.is_some() {
                "Edit bookmark".to_string()
            } else {
                "Add bookmark".to_string()
            },
            name: TextInput::new(bm.name.clone()),
            list: list.min(list_names.len().saturating_sub(1)),
            list_names,
            freq: FreqEntry::new(bm.frequency_hz),
            vfo_type: bm.vfo_type.clone(),
            settings: bm.settings.clone(),
            start: TextInput::digits(format!("{:04}", bm.start_utc), 4),
            end: TextInput::digits(format!("{:04}", bm.end_utc), 4),
            days: bm.days,
            day_cursor: 0,
            geo: TextInput::new(bm.geoinfo.clone()),
            notes: TextInput::new(bm.notes.clone()),
            field: EditorField::Name,
            editing: false,
            button: EditorButton::Apply,
            backup: None,
            setting_text: TextInput::new(String::new()),
            error: None,
        }
    }

    pub fn field(&self) -> EditorField {
        self.field
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn button(&self) -> EditorButton {
        self.button
    }

    // ── The type and its schema rows ─────────────────────────────────────

    /// The type, if usable in this build (None: its settings are kept as
    /// they are and cannot be edited).
    pub fn type_def(&self) -> Option<&'static dyn crate::vfo::types::VfoType> {
        crate::vfo::types::usable(&self.vfo_type)
    }

    /// The Type row's text: the id, `?NAVTEX` for a type this build lacks.
    pub fn type_label(&self) -> String {
        match self.type_def() {
            Some(_) => self.vfo_type.clone(),
            None => format!("?{}", self.vfo_type),
        }
    }

    /// The type's visible bookmark fields.
    pub fn setting_fields(&self) -> Vec<Field> {
        let Some(t) = self.type_def() else {
            return Vec::new();
        };
        t.schema(&self.settings)
            .visible_fields(&self.settings, true)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The i-th setting field.
    pub fn setting_field(&self, i: usize) -> Option<Field> {
        self.setting_fields().into_iter().nth(i)
    }

    /// A setting row's label.
    pub fn label(&self, f: EditorField) -> String {
        match f {
            EditorField::Setting(i) => self
                .setting_field(i as usize)
                .map(|f| f.label)
                .unwrap_or_default(),
            f => f.label().to_string(),
        }
    }

    /// A setting row's value as shown (the typed text while it is edited).
    pub fn setting_display(&self, i: usize) -> String {
        let Some(f) = self.setting_field(i) else {
            return String::new();
        };
        f.display(f.value(&self.settings))
    }

    /// The text input of a setting being typed.
    pub fn setting_input(&self) -> &TextInput {
        &self.setting_text
    }

    /// Set setting `key`, then let the type apply its rules (Voice: a new
    /// mode resets the bandwidth to its default).
    fn set_setting(&mut self, key: &str, v: Value) {
        self.settings.insert(key.to_string(), v);
        if let Some(t) = self.type_def() {
            t.normalize(&mut self.settings, Some(key));
            // Keep only the bookmark part (normalize fills every key).
            let part = t.schema(&self.settings).bookmark_part(&self.settings);
            self.settings = part;
        }
        self.clamp_field();
    }

    /// Step a choice / checkbox setting.
    fn step_setting(&mut self, i: usize, dir: i32) {
        if let Some(f) = self.setting_field(i)
            && !is_typed(&f.kind)
        {
            let v = f.step(f.value(&self.settings), dir);
            self.set_setting(&f.key, v);
        }
    }

    /// Step the Type through the available types; the type rows are rebuilt
    /// with the new type's defaults. A type this build lacks is not changed.
    pub fn cycle_type(&mut self, dir: i32) {
        if self.type_def().is_none() {
            return;
        }
        let ids = crate::vfo::types::selectable_ids();
        let n = ids.len() as i32;
        let i = ids.iter().position(|t| *t == self.vfo_type).unwrap_or(0) as i32;
        let next = ids[((i + dir) % n + n) as usize % ids.len()];
        if next != self.vfo_type
            && let Some(t) = crate::vfo::types::usable(next)
        {
            self.vfo_type = next.to_string();
            let d = t.defaults();
            self.settings = t.schema(&d).bookmark_part(&d);
        }
        self.clamp_field();
    }

    /// Whether field `f` is typed with a cursor.
    pub fn is_text_field(&self, f: EditorField) -> bool {
        match f {
            EditorField::Name
            | EditorField::Start
            | EditorField::End
            | EditorField::Geo
            | EditorField::Notes => true,
            EditorField::Setting(i) => self
                .setting_field(i as usize)
                .is_some_and(|f| is_typed(&f.kind)),
            _ => false,
        }
    }

    /// Whether a text field is being edited.
    pub fn is_text_editing(&self) -> bool {
        self.editing && self.is_text_field(self.field)
    }

    /// The fields shown for the current type and settings, in order.
    pub fn visible_fields(&self) -> Vec<EditorField> {
        use EditorField as F;
        let mut v = vec![F::Name, F::List, F::Frequency, F::Type];
        let n = self.setting_fields().len().min(u8::MAX as usize);
        v.extend((0..n).map(|i| F::Setting(i as u8)));
        v.extend([F::Start, F::End, F::Days, F::Geo, F::Notes, F::Buttons]);
        v
    }

    /// Keep the cursor on a visible field (a type or mode change may hide
    /// the row it was on).
    fn clamp_field(&mut self) {
        if !self.visible_fields().contains(&self.field) {
            self.field = EditorField::Type;
            self.editing = false;
        }
    }

    /// The text input behind a text field.
    pub fn text(&self, f: EditorField) -> Option<&TextInput> {
        match f {
            EditorField::Name => Some(&self.name),
            EditorField::Start => Some(&self.start),
            EditorField::End => Some(&self.end),
            EditorField::Geo => Some(&self.geo),
            EditorField::Notes => Some(&self.notes),
            EditorField::Setting(_) if self.is_text_field(f) => Some(&self.setting_text),
            _ => None,
        }
    }

    fn text_mut(&mut self, f: EditorField) -> Option<&mut TextInput> {
        match f {
            EditorField::Name => Some(&mut self.name),
            EditorField::Start => Some(&mut self.start),
            EditorField::End => Some(&mut self.end),
            EditorField::Geo => Some(&mut self.geo),
            EditorField::Notes => Some(&mut self.notes),
            EditorField::Setting(_) if self.is_text_field(f) => Some(&mut self.setting_text),
            _ => None,
        }
    }

    // ── Field navigation ─────────────────────────────────────────────────

    fn step_field(&mut self, dir: i32) {
        let f = self.visible_fields();
        let n = f.len() as i32;
        let i = f.iter().position(|x| *x == self.field).unwrap_or(0) as i32;
        self.field = f[((i + dir) % n + n) as usize % f.len()];
    }

    /// Finish an in-progress edit, keeping (commit) its value. A typed
    /// setting is parsed and checked by its schema field; a bad value is
    /// reverted with the reason shown.
    fn finish_edit(&mut self) {
        if !self.editing {
            return;
        }
        self.editing = false;
        self.backup = None;
        match self.field {
            EditorField::Frequency => {
                self.freq.commit(0, MAX_FREQ_HZ);
            }
            EditorField::Setting(i) => {
                if let Some(f) = self.setting_field(i as usize) {
                    match f.parse(self.setting_text.text()) {
                        Ok(v) => {
                            self.error = None;
                            self.set_setting(&f.key, v);
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            }
            _ => {}
        }
    }

    /// Select `field` (a click), committing any other field's edit.
    pub fn select_field(&mut self, field: EditorField) {
        if field != self.field {
            self.finish_edit();
        }
        if self.visible_fields().contains(&field) {
            self.field = field;
        }
    }

    /// Up: previous digit value while editing the frequency, else the
    /// previous field (an edit in progress is committed).
    pub fn up(&mut self) {
        if self.editing && self.field == EditorField::Frequency {
            self.freq.increment();
            return;
        }
        self.finish_edit();
        self.step_field(-1);
    }

    pub fn down(&mut self) {
        if self.editing && self.field == EditorField::Frequency {
            self.freq.decrement();
            return;
        }
        self.finish_edit();
        self.step_field(1);
    }

    /// TAB / Shift+TAB: next/previous field, committing any edit.
    pub fn tab(&mut self, forward: bool) {
        self.finish_edit();
        self.step_field(if forward { 1 } else { -1 });
    }

    /// Left/Right (`dir` = -1/+1): move the text or digit cursor while
    /// editing, otherwise change the field's value (list, type, a choice
    /// setting, day cursor, button).
    pub fn horizontal(&mut self, dir: i32) {
        if self.editing {
            match self.field {
                EditorField::Frequency if dir < 0 => self.freq.cursor_left(),
                EditorField::Frequency => self.freq.cursor_right(),
                f => {
                    if let Some(t) = self.text_mut(f) {
                        if dir < 0 { t.left() } else { t.right() }
                    }
                }
            }
            return;
        }
        match self.field {
            EditorField::List => self.cycle_list(dir),
            EditorField::Type => self.cycle_type(dir),
            EditorField::Setting(i) => self.step_setting(i as usize, dir),
            EditorField::Days => {
                self.day_cursor = (self.day_cursor as i32 + dir).rem_euclid(7) as usize
            }
            EditorField::Buttons => {
                self.button = if self.button == EditorButton::Apply {
                    EditorButton::Cancel
                } else {
                    EditorButton::Apply
                }
            }
            _ => {}
        }
    }

    /// Change a stepped field by `dir` (the mouse wheel).
    pub fn wheel(&mut self, dir: i32) {
        match self.field {
            EditorField::List => self.cycle_list(dir),
            EditorField::Type => self.cycle_type(dir),
            EditorField::Setting(i) => self.step_setting(i as usize, dir),
            _ => {}
        }
    }

    pub fn cycle_list(&mut self, dir: i32) {
        let n = self.list_names.len() as i32;
        if n > 0 {
            self.list = (self.list as i32 + dir).rem_euclid(n) as usize;
        }
    }

    /// Toggle weekday `i` (0 = Sunday).
    pub fn toggle_day(&mut self, i: usize) {
        if i < 7 {
            self.finish_edit();
            self.field = EditorField::Days;
            self.day_cursor = i;
            self.days[i] = !self.days[i];
        }
    }

    /// ENTER.
    pub fn enter(&mut self) -> EditorOutcome {
        if self.editing {
            // Commit the field and move on to the next one.
            self.finish_edit();
            if self.error.is_none() {
                self.step_field(1);
            }
            return EditorOutcome::Open;
        }
        match self.field {
            EditorField::List => self.cycle_list(1),
            EditorField::Type => self.cycle_type(1),
            EditorField::Days => self.days[self.day_cursor] = !self.days[self.day_cursor],
            EditorField::Buttons => return self.press(self.button),
            EditorField::Frequency => self.editing = true,
            EditorField::Setting(i) if !self.is_text_field(self.field) => {
                self.step_setting(i as usize, 1)
            }
            f => self.begin_text_edit(f),
        }
        EditorOutcome::Open
    }

    /// Begin editing the frequency with the digit cursor on `pos`.
    pub fn edit_digit(&mut self, pos: usize) {
        self.select_field(EditorField::Frequency);
        self.editing = true;
        self.freq.set_cursor(pos);
    }

    /// Begin editing text field `f` (its text selected).
    pub fn begin_text_edit(&mut self, f: EditorField) {
        self.select_field(f);
        if let EditorField::Setting(i) = f
            && let Some(field) = self.setting_field(i as usize)
        {
            let digits_only = matches!(
                field.kind,
                FieldKind::Integer { .. } | FieldKind::FrequencyHz
            );
            let text = value_text(field.value(&self.settings));
            self.setting_text = if digits_only {
                TextInput::digits(text, 12)
            } else {
                TextInput::new(text)
            };
        }
        if let Some(t) = self.text_mut(f) {
            t.select_all();
            let snapshot = t.clone();
            self.backup = Some(snapshot);
            self.editing = true;
        }
    }

    /// Press a button.
    pub fn press(&mut self, b: EditorButton) -> EditorOutcome {
        self.finish_edit();
        self.field = EditorField::Buttons;
        self.button = b;
        match b {
            EditorButton::Apply => EditorOutcome::Apply,
            EditorButton::Cancel => EditorOutcome::Cancel,
        }
    }

    /// Esc: revert the field being edited, or cancel the form.
    pub fn escape(&mut self) -> EditorOutcome {
        if !self.editing {
            return EditorOutcome::Cancel;
        }
        self.editing = false;
        if self.field == EditorField::Frequency {
            self.freq.cancel();
        } else if let Some(b) = self.backup.take() {
            let f = self.field;
            if let Some(t) = self.text_mut(f) {
                *t = b;
            }
        }
        EditorOutcome::Open
    }

    // ── Typing ───────────────────────────────────────────────────────────

    /// A printable character. While editing it goes into the field; on a
    /// selected text field it starts editing (replacing the selected text,
    /// "type to edit"); a digit on the frequency starts editing it at the
    /// digit cursor; Space toggles the weekday under the day cursor.
    pub fn type_char(&mut self, c: char) {
        if !self.editing {
            match self.field {
                f if self.is_text_field(f) => self.begin_text_edit(f),
                EditorField::Frequency if c.is_ascii_digit() => self.editing = true,
                EditorField::Days if c == ' ' => {
                    self.days[self.day_cursor] = !self.days[self.day_cursor];
                    return;
                }
                _ => return,
            }
        }
        if self.field == EditorField::Frequency {
            if let Some(d) = c.to_digit(10) {
                self.freq.type_digit(d as u8);
            } else if c == 'z' {
                self.freq.zero_right();
            }
            return;
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

    pub fn end(&mut self) {
        let f = self.field;
        if self.editing
            && let Some(t) = self.text_mut(f)
        {
            t.end();
        }
    }

    // ── Result ───────────────────────────────────────────────────────────

    /// Validate the form and build (target list, bookmark).
    pub fn build(&self) -> Result<(usize, Bookmark), String> {
        if self.name.text().trim().is_empty() {
            return Err("Name must not be empty".into());
        }
        if self.list_names.is_empty() {
            return Err("Create a list first".into());
        }
        let freq = self.freq.display_hz();
        if freq == 0 {
            return Err("Frequency must not be 0".into());
        }
        // The type's own rules (a type this build lacks is kept as is).
        if let Some(t) = self.type_def() {
            t.schema(&self.settings).validate(&self.settings)?;
        }
        let time = |t: &TextInput, what: &str| -> Result<u16, String> {
            t.text()
                .parse::<u16>()
                .ok()
                .filter(|v| t.len() == 4 && time_valid(*v))
                .ok_or(format!("{what} must be HHMM (0000..2359)"))
        };
        let start_utc = time(&self.start, "Start")?;
        let end_utc = time(&self.end, "End")?;
        Ok((
            self.list,
            Bookmark {
                name: self.name.text().trim().to_string(),
                frequency_hz: freq,
                vfo_type: self.vfo_type.clone(),
                settings: self.settings.clone(),
                start_utc,
                end_utc,
                days: self.days,
                geoinfo: self.geo.text().to_string(),
                notes: self.notes.text().to_string(),
                legacy: Default::default(),
            },
        ))
    }

    /// The weekday labels with their state, for rendering.
    pub fn day_labels(&self) -> [(&'static str, bool); 7] {
        std::array::from_fn(|i| (DAY_LABELS[i], self.days[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::vfo::model::Mode;
    use crate::vfo::types::voice::VoiceSettings;
    use serde_json::json;

    fn sample() -> Bookmark {
        let mut v = VoiceSettings::for_mode(Mode::Usb);
        v.bandwidth_hz = 2_300.0;
        Bookmark::voice("Rostov", 8_939_000, v)
    }

    fn editor() -> BookmarkEditor {
        BookmarkEditor::new(
            &sample(),
            Some((0, 1)),
            0,
            vec!["VOLMET".into(), "Ships".into()],
        )
    }

    fn type_str(e: &mut BookmarkEditor, s: &str) {
        for c in s.chars() {
            e.type_char(c);
        }
    }

    #[test]
    fn unchanged_form_builds_the_same_bookmark() {
        let e = editor();
        assert_eq!(e.title, "Edit bookmark");
        assert_eq!(e.build().unwrap(), (0, sample()));
    }

    #[test]
    fn text_field_edit_then_enter_commits_and_moves_on() {
        let mut e = editor();
        assert_eq!(e.field(), EditorField::Name);
        e.enter();
        assert!(e.is_text_editing());
        e.end(); // keep the text, append
        type_str(&mut e, " Volmet");
        e.enter();
        assert!(!e.is_editing());
        assert_eq!(e.field(), EditorField::List);
        assert_eq!(e.build().unwrap().1.name, "Rostov Volmet");
    }

    #[test]
    fn esc_reverts_a_field_then_cancels_the_form() {
        let mut e = editor();
        e.enter();
        e.backspace();
        e.backspace();
        assert_eq!(e.escape(), EditorOutcome::Open);
        assert_eq!(e.name.text(), "Rostov", "edit reverted");
        assert_eq!(e.escape(), EditorOutcome::Cancel, "second Esc closes");
    }

    #[test]
    fn cw_tone_accepts_250_to_3000_hz() {
        let with_tone = |t: f64| {
            let mut v = VoiceSettings::for_mode(Mode::Cw);
            v.cw_tone_hz = t;
            let mut bm = Bookmark::voice("CW", 7_030_000, v);
            bm.settings.insert("cw_tone_hz".into(), json!(t)); // unclamped
            BookmarkEditor::new(&bm, Some((0, 1)), 0, vec!["VOLMET".into()]).build()
        };
        assert!(with_tone(250.0).is_ok());
        assert!(with_tone(3000.0).is_ok(), "user request: up to 3000 Hz");
        assert!(with_tone(3010.0).unwrap_err().starts_with("CW tone"));
        assert!(with_tone(240.0).is_err());
    }

    #[test]
    fn mode_change_resets_bandwidth_and_shows_cw_tone() {
        let mut e = editor();
        assert_eq!(e.label(EditorField::MODE), "Mode");
        assert_eq!(e.setting_display(0), "USB");
        e.select_field(EditorField::MODE);
        // USB -> LSB -> CW
        e.horizontal(1);
        e.horizontal(1);
        assert_eq!(e.settings["mode"], "Cw");
        assert_eq!(e.setting_display(1), "200 Hz", "CW default bandwidth");
        assert!(e.visible_fields().contains(&EditorField::CW_TONE));
        assert_eq!(e.label(EditorField::CW_TONE), "CW tone");
        e.horizontal(1);
        assert!(!e.visible_fields().contains(&EditorField::CW_TONE));
        assert!(
            !e.settings.contains_key("snap_hz"),
            "only bookmark settings"
        );
    }

    #[test]
    fn a_typed_setting_is_checked_by_its_schema() {
        let mut e = editor();
        e.begin_text_edit(EditorField::BANDWIDTH);
        assert_eq!(e.setting_input().text(), "2300");
        type_str(&mut e, "100");
        e.enter();
        assert!(
            e.error.as_ref().unwrap().starts_with("Bandwidth"),
            "{:?}",
            e.error
        );
        assert_eq!(e.settings["bandwidth_hz"], 2_300.0, "kept");
        assert_eq!(e.field(), EditorField::BANDWIDTH, "stays for a retry");
        e.begin_text_edit(EditorField::BANDWIDTH);
        type_str(&mut e, "2700");
        e.enter();
        assert!(e.error.is_none());
        assert_eq!(e.build().unwrap().1.settings["bandwidth_hz"], 2_700.0);
    }

    #[test]
    fn switching_the_type_rebuilds_the_rows_with_defaults() {
        let mut e = editor();
        e.select_field(EditorField::Type);
        e.enter();
        assert_eq!(e.vfo_type, "Test");
        let labels: Vec<String> = e
            .visible_fields()
            .into_iter()
            .filter(|f| matches!(f, EditorField::Setting(_)))
            .map(|f| e.label(f))
            .collect();
        assert_eq!(labels, ["Speed", "Width"]);
        let b = e.build().unwrap().1;
        assert_eq!(b.vfo_type, "Test");
        assert_eq!(b.settings["speed"], "Slow");
        e.horizontal(-1);
        assert_eq!(e.vfo_type, "Voice");
        assert_eq!(e.settings["mode"], "Nfm", "Voice defaults");
    }

    #[test]
    fn an_unknown_type_is_read_only_and_kept_verbatim() {
        let bm = Bookmark {
            name: "NAVTEX 518".into(),
            frequency_hz: 156_525_000,
            vfo_type: "NAVTEX".into(),
            settings: json!({"channel": 70}).as_object().unwrap().clone(),
            ..Bookmark::default()
        };
        let mut e = BookmarkEditor::new(&bm, Some((0, 0)), 0, vec!["Ships".into()]);
        assert_eq!(e.type_label(), "?NAVTEX");
        assert!(
            !e.visible_fields()
                .iter()
                .any(|f| matches!(f, EditorField::Setting(_)))
        );
        e.select_field(EditorField::Type);
        e.enter();
        assert_eq!(e.vfo_type, "NAVTEX", "cannot be changed");
        e.select_field(EditorField::Name);
        e.begin_text_edit(EditorField::Name);
        type_str(&mut e, "Ch 70");
        e.enter();
        let (_, b) = e.build().unwrap();
        assert_eq!(b.name, "Ch 70");
        assert_eq!(b.settings, bm.settings);
    }

    #[test]
    fn frequency_edits_with_digits_and_commits_on_leave() {
        let mut e = editor();
        e.edit_digit(3); // kHz digit
        e.up();
        e.up();
        assert_eq!(e.freq.display_hz(), 8_941_000);
        e.tab(true);
        assert!(!e.is_editing());
        assert_eq!(e.build().unwrap().1.frequency_hz, 8_941_000);
    }

    #[test]
    fn days_toggle_by_cursor_and_click() {
        let mut e = editor();
        e.select_field(EditorField::Days);
        e.horizontal(-1); // Saturday
        e.enter();
        assert!(!e.days[6]);
        e.toggle_day(0);
        assert!(!e.days[0]);
        assert_eq!(
            e.build().unwrap().1.days,
            [false, true, true, true, true, true, false]
        );
    }

    #[test]
    fn invalid_times_and_bandwidth_are_rejected() {
        let mut e = editor();
        e.begin_text_edit(EditorField::Start);
        e.start.set("2460");
        assert!(e.build().unwrap_err().contains("Start"));
        e.start.set("530"); // three digits
        assert!(e.build().is_err());
        e.start.set("0530");
        e.end.set("0630");
        assert_eq!(e.build().unwrap().1.start_utc, 530);
    }

    #[test]
    fn typing_into_a_full_time_field_replaces_it() {
        let mut e = editor();
        e.begin_text_edit(EditorField::Start);
        type_str(&mut e, "0530");
        e.enter();
        assert_eq!(e.start.text(), "0530");
        assert_eq!(e.field(), EditorField::End);
    }

    #[test]
    fn typing_on_a_selected_field_starts_editing_it() {
        let mut e = editor();
        e.select_field(EditorField::Start);
        type_str(&mut e, "0530");
        assert!(e.is_text_editing());
        e.enter();
        type_str(&mut e, "0630"); // now on End, not editing yet
        e.enter();
        let b = e.build().unwrap().1;
        assert_eq!((b.start_utc, b.end_utc), (530, 630));
        e.select_field(EditorField::Frequency);
        type_str(&mut e, "1");
        assert!(e.is_editing(), "a digit starts the frequency edit");
        e.select_field(EditorField::Days);
        e.type_char(' ');
        assert!(!e.days[0], "Space toggles the day under the cursor");
        e.select_field(EditorField::MODE);
        e.type_char('x');
        assert!(!e.is_editing(), "letters do nothing on a cycle field");
    }

    #[test]
    fn digits_only_fields_ignore_letters() {
        let mut e = editor();
        e.begin_text_edit(EditorField::Start);
        e.start.clear();
        type_str(&mut e, "12ab3456");
        assert_eq!(e.start.text(), "1234");
    }

    #[test]
    fn empty_name_is_rejected() {
        let mut e = editor();
        e.name.set("   ");
        assert!(e.build().is_err());
    }

    #[test]
    fn list_cycles_and_buttons_finish_the_form() {
        let mut e = editor();
        e.select_field(EditorField::List);
        e.enter();
        assert_eq!(e.build().unwrap().0, 1, "moved to Ships");
        e.select_field(EditorField::Buttons);
        assert_eq!(e.enter(), EditorOutcome::Apply);
        e.horizontal(1);
        assert_eq!(e.enter(), EditorOutcome::Cancel);
    }

    #[test]
    fn navigation_wraps_and_commits_edits() {
        let mut e = editor();
        e.up();
        assert_eq!(e.field(), EditorField::Buttons, "wraps to the buttons");
        e.down();
        assert_eq!(e.field(), EditorField::Name);
        e.enter();
        e.end();
        type_str(&mut e, "!");
        e.down();
        assert!(!e.is_editing());
        assert_eq!(e.name.text(), "Rostov!", "moving away keeps the edit");
    }
}
