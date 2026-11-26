//! Radio module state: the VFO list, the active VFO, and the frequency entry
//! bound to it (pure).
//!
//! Piece A of step 3. The Master VFO is always present at index 0. One VFO is
//! "active"; frequency edits and mode/bandwidth changes target it. Adding and
//! removing secondary VFOs arrives in step 4, so this step keeps a single
//! Master but the collection shape is already in place.
//
// NOTE(step3->4/5): some accessors (active_index, set_active, set_mode) are
// used by tests now and by multi-VFO selection (step 4) / bookmark apply
// (step 5). The allow is narrowed as those steps consume them.
#![allow(dead_code)]

use crate::freq::{CommitResult, FreqEntry};
use crate::vfo::model::{Mode, Vfo};

/// The editable fields in the Radio module, in navigation order. The type's
/// own rows are `Setting(i)`: the i-th visible field of its schema (spec
/// 13.3), so they change with the type and its settings (Voice: CW tone only
/// in CW, AM AGC only in AM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioField {
    Frequency,
    /// The VFO's type (`Voice`, ...).
    Type,
    /// The i-th visible field of the active type's schema.
    Setting(u8),
    Audio,
    /// The VFO's `[ ] Mute` checkbox (its audio output only).
    Mute,
    /// `[x] Native`: record the type's native output.
    RecNative,
    /// `[ ] Baseband`: record the IQ.
    RecBaseband,
    /// `[ ] Minimize baseband`.
    RecMinimize,
    /// Recording file-name template.
    RecName,
    /// Recording folder (global, shown with the VFO's recorder).
    RecDir,
    /// The [Record] / [Stop] button.
    Record,
}

impl RadioField {
    /// Voice's rows by position (Mode, Bandwidth, Snap, then CW tone in CW /
    /// AM AGC in AM).
    pub const MODE: RadioField = RadioField::Setting(0);
    pub const BANDWIDTH: RadioField = RadioField::Setting(1);
    pub const SNAP: RadioField = RadioField::Setting(2);
    pub const CW_TONE: RadioField = RadioField::Setting(3);
    pub const AM_AGC: RadioField = RadioField::Setting(3);
}

/// Clamp a CW pitch to the allowed range (250..3000 Hz).
fn clamp_cw_tone(hz: f64) -> f64 {
    use crate::vfo::model::{CW_TONE_MAX_HZ, CW_TONE_MIN_HZ};
    hz.clamp(CW_TONE_MIN_HZ, CW_TONE_MAX_HZ)
}

/// The Radio module: one or more VFOs with an active selection and the
/// frequency editor for the active VFO.
#[derive(Debug, Clone, PartialEq)]
pub struct RadioModule {
    vfos: Vec<Vfo>,
    active: usize,
    freq: FreqEntry,
    /// Which field the in-module cursor is on.
    field: RadioField,
    /// Whether the selected field is being edited.
    editing: bool,
    /// Available audio output options (system default is implicit at index 0
    /// and not stored here).
    audio_options: Vec<crate::engine::audio::OutputOption>,
}

impl RadioModule {
    /// Create with a Master VFO tuned to `master_hz`.
    pub fn new(master_hz: u64) -> Self {
        Self {
            vfos: vec![Vfo::master(master_hz)],
            active: 0,
            freq: FreqEntry::new(master_hz),
            field: RadioField::Frequency,
            editing: false,
            audio_options: Vec::new(),
        }
    }

    /// The fields visible for the active VFO, in navigation order. `CwTone`
    /// appears only in CW mode.
    pub fn visible_fields(&self) -> Vec<RadioField> {
        let mut f = vec![RadioField::Frequency, RadioField::Type];
        let n = self.setting_fields().len().min(u8::MAX as usize);
        f.extend((0..n).map(|i| RadioField::Setting(i as u8)));
        if self.has_audio() {
            f.extend([RadioField::Audio, RadioField::Mute]);
        }
        f.extend([
            RadioField::RecNative,
            RadioField::RecBaseband,
            RadioField::RecMinimize,
            RadioField::RecName,
            RadioField::RecDir,
            RadioField::Record,
        ]);
        f
    }

    /// Flip one recording choice (Native / Baseband / Minimize) of the
    /// active VFO. Locked while recording.
    pub fn toggle_rec(&mut self, f: RadioField) {
        if f == RadioField::RecNative && self.native_always_on() {
            return;
        }
        let v = self.active_mut();
        if v.recording {
            return;
        }
        match f {
            RadioField::RecNative => v.rec.native = !v.rec.native,
            RadioField::RecBaseband => v.rec.baseband = !v.rec.baseband,
            RadioField::RecMinimize => v.rec.minimize = !v.rec.minimize,
            _ => {}
        }
    }

    /// Whether `f` is a folder setting (edited in a prompt).
    pub fn is_path(&self, f: RadioField) -> bool {
        let RadioField::Setting(i) = f else {
            return false;
        };
        self.setting_field(i as usize)
            .is_some_and(|f| matches!(f.kind, crate::vfo::types::FieldKind::Path))
    }

    /// Whether the active type always records its native output (DSC).
    pub fn native_always_on(&self) -> bool {
        self.active()
            .type_def()
            .is_some_and(|t| t.native_always_on())
    }

    /// Whether `f` is one of the recording checkboxes.
    pub fn is_rec_check(f: RadioField) -> bool {
        matches!(
            f,
            RadioField::RecNative | RadioField::RecBaseband | RadioField::RecMinimize
        )
    }

    /// Mute / unmute the active VFO's audio output.
    pub fn toggle_mute(&mut self) {
        let v = self.active_mut();
        v.muted = !v.muted;
    }

    /// Set the active VFO's mute (a scheduled job, and restoring after it).
    // NOTE(step 6): jobs run on Master; target it explicitly with more VFOs.
    pub fn set_muted(&mut self, muted: bool) {
        self.active_mut().muted = muted;
    }

    /// Switch the active VFO's recording on/off.
    pub fn toggle_recording(&mut self) {
        let v = self.active_mut();
        v.recording = !v.recording;
    }

    /// Start recording the outputs `native` / `baseband` (a scheduled job;
    /// Minimize stays the VFO's own). Returns the choice it replaced, so it
    /// can be restored when the job ends.
    pub fn start_recording_as(&mut self, native: bool, baseband: bool) -> crate::record::RecChoice {
        let v = self.active_mut();
        let prev = v.rec;
        v.rec.native = native;
        v.rec.baseband = baseband;
        v.recording = true;
        prev
    }

    /// Set the recording choice while not recording (restoring it after a
    /// job).
    pub fn set_rec_choice(&mut self, rec: crate::record::RecChoice) {
        let v = self.active_mut();
        if !v.recording {
            v.rec = rec;
        }
    }

    /// Switch recording off (Stop, or a failed start).
    pub fn stop_recording(&mut self) {
        self.active_mut().recording = false;
    }

    /// Set the active VFO's file-name template (blank = SDR++ default).
    pub fn set_rec_template(&mut self, t: &str) {
        let t = t.trim();
        self.active_mut().rec_template = if t.is_empty() {
            crate::record::naming::DEFAULT_TEMPLATE.to_string()
        } else {
            t.to_string()
        };
    }

    /// The field the cursor is on.
    pub fn field(&self) -> RadioField {
        self.field
    }

    /// Whether the selected field is being edited.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Move the field cursor to the next/previous visible field. No-op while
    /// editing (arrows then act within the field).
    pub fn field_next(&mut self) {
        if self.editing {
            return;
        }
        let fields = self.visible_fields();
        let i = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        self.field = fields[(i + 1) % fields.len()];
    }

    pub fn field_prev(&mut self) {
        if self.editing {
            return;
        }
        let fields = self.visible_fields();
        let n = fields.len();
        let i = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        self.field = fields[(i + n - 1) % n];
    }

    /// Put the field cursor on `field` if it is currently visible. Ignored
    /// while editing (the caller ends the edit first) unless it is the field
    /// already being edited.
    pub fn select_field(&mut self, field: RadioField) {
        if self.editing && field != self.field {
            return;
        }
        if self.visible_fields().contains(&field) {
            self.field = field;
        }
    }

    /// Enter edit mode on the selected field (idempotent). Mouse clicks use
    /// this instead of the ENTER toggle so a second click keeps editing.
    pub fn begin_edit(&mut self) {
        self.editing = true;
    }

    /// ENTER: toggle edit mode on the selected field. Leaving edit mode on the
    /// Frequency field commits the pending edit (clamped to `[lo, hi]`).
    pub fn toggle_edit(&mut self, lo: u64, hi: u64) -> Option<CommitResult> {
        if self.editing {
            self.editing = false;
            if self.field == RadioField::Frequency {
                return Some(self.commit_frequency(lo, hi));
            }
        } else {
            self.editing = true;
        }
        None
    }

    /// Esc: cancel editing. On the Frequency field, discards the pending edit.
    pub fn cancel_edit(&mut self) {
        if !self.editing {
            return;
        }
        self.editing = false;
        if self.field == RadioField::Frequency {
            self.freq.cancel();
        }
    }

    /// Left arrow while editing: acts within the selected field.
    pub fn edit_left(&mut self) {
        if !self.editing {
            return;
        }
        match self.field {
            RadioField::Frequency => self.freq.cursor_left(),
            RadioField::Type => self.cycle_type(-1),
            RadioField::Setting(i) => self.step_setting(i as usize, -1),
            f if Self::is_rec_check(f) => self.toggle_rec(f),
            RadioField::Mute => self.toggle_mute(),
            _ => {}
        }
    }

    /// Right arrow while editing.
    pub fn edit_right(&mut self) {
        if !self.editing {
            return;
        }
        match self.field {
            RadioField::Frequency => self.freq.cursor_right(),
            RadioField::Type => self.cycle_type(1),
            RadioField::Setting(i) => self.step_setting(i as usize, 1),
            f if Self::is_rec_check(f) => self.toggle_rec(f),
            RadioField::Mute => self.toggle_mute(),
            _ => {}
        }
    }

    /// Up arrow while editing (frequency digit value; other fields same as
    /// right for convenience).
    pub fn edit_up(&mut self) {
        if !self.editing {
            return;
        }
        match self.field {
            RadioField::Frequency => self.freq.increment(),
            _ => self.edit_right(),
        }
    }

    /// Down arrow while editing.
    pub fn edit_down(&mut self) {
        if !self.editing {
            return;
        }
        match self.field {
            RadioField::Frequency => self.freq.decrement(),
            _ => self.edit_left(),
        }
    }

    /// Type a digit while editing the Frequency field.
    pub fn edit_type_digit(&mut self, d: u8) {
        if self.editing && self.field == RadioField::Frequency {
            self.freq.type_digit(d);
        }
    }

    /// Zero digits to the right while editing the Frequency field.
    pub fn edit_zero_right(&mut self) {
        if self.editing && self.field == RadioField::Frequency {
            self.freq.zero_right();
        }
    }

    // ── The VFO type and its schema-driven settings (spec 13.3) ─────────

    /// Whether the active type plays audio (the Audio and Mute rows are
    /// shown only then).
    pub fn has_audio(&self) -> bool {
        self.active().type_def().is_none_or(|t| t.plays_audio())
    }

    /// The active type's visible schema fields (none for a type this build
    /// lacks).
    pub fn setting_fields(&self) -> Vec<crate::vfo::types::Field> {
        let v = self.active();
        let Some(t) = v.type_def() else {
            return Vec::new();
        };
        let s = v.type_settings();
        t.schema(&s)
            .visible_fields(&s, false)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The i-th visible schema field.
    pub fn setting_field(&self, i: usize) -> Option<crate::vfo::types::Field> {
        self.setting_fields().into_iter().nth(i)
    }

    /// The i-th field and its current value.
    pub fn setting_value(&self, i: usize) -> Option<(crate::vfo::types::Field, serde_json::Value)> {
        let f = self.setting_field(i)?;
        let v = f.value(&self.active().type_settings()).clone();
        Some((f, v))
    }

    /// Set the i-th field to `value`, then let the type apply its rules.
    pub fn set_setting(&mut self, i: usize, value: serde_json::Value) {
        let Some(f) = self.setting_field(i) else {
            return;
        };
        let v = self.active_mut();
        let mut s = v.type_settings();
        s.insert(f.key.clone(), value);
        if let Some(t) = v.type_def() {
            t.normalize(&mut s, Some(&f.key));
        }
        v.set_type_settings(&s);
        self.clamp_field();
    }

    /// Replace the active VFO's type settings (normalized by its type).
    pub fn set_active_type_settings(&mut self, settings: &crate::vfo::types::Settings) {
        self.active_mut().set_type_settings(settings);
    }

    /// Set the i-th field from typed text (checked by its schema).
    pub fn set_setting_text(&mut self, i: usize, text: &str) -> Result<(), String> {
        let f = self.setting_field(i).ok_or("no such setting")?;
        let v = f.parse(text)?;
        self.set_setting(i, v);
        Ok(())
    }

    /// Step the i-th field by `dir` (choices cycle, numbers step, bools flip).
    pub fn step_setting(&mut self, i: usize, dir: i32) {
        if let Some((f, v)) = self.setting_value(i) {
            self.set_setting(i, f.step(&v, dir));
        }
    }

    /// Whether `f` is a list-like row (Type or a choice setting): Left/Right
    /// change it without edit mode and ENTER opens a picker.
    pub fn is_choice(&self, f: RadioField) -> bool {
        match f {
            RadioField::Type => true,
            RadioField::Setting(i) => self
                .setting_field(i as usize)
                .is_some_and(|f| matches!(f.kind, crate::vfo::types::FieldKind::Choice { .. })),
            _ => false,
        }
    }

    /// Whether `f` is a checkbox setting (toggled at once).
    pub fn is_toggle(&self, f: RadioField) -> bool {
        matches!(f, RadioField::Setting(i)
            if self.setting_field(i as usize)
                .is_some_and(|f| matches!(f.kind, crate::vfo::types::FieldKind::Bool)))
    }

    /// The ids the Type row offers (the selectable types; the current one
    /// first if this build lacks it).
    pub fn type_options(&self) -> Vec<String> {
        let mut ids: Vec<String> = crate::vfo::types::selectable_ids()
            .into_iter()
            .map(str::to_string)
            .collect();
        let cur = &self.active().vfo_type;
        if !ids.contains(cur) {
            ids.insert(0, cur.clone());
        }
        ids
    }

    /// The Type row's text: the id, `?NAVTEX` when this build lacks it.
    pub fn type_label(&self) -> String {
        let v = self.active();
        match crate::vfo::types::lookup(&v.vfo_type) {
            Some(t) if t.availability() == crate::vfo::types::Availability::Ok => {
                v.vfo_type.clone()
            }
            Some(_) => format!("{} (unavailable)", v.vfo_type),
            None => format!("?{}", v.vfo_type),
        }
    }

    /// Switch the active VFO to type `id` (only selectable types; locked
    /// while recording, since the outputs depend on it).
    pub fn set_type(&mut self, id: &str) {
        if self.active().recording || crate::vfo::types::usable(id).is_none() {
            return;
        }
        self.active_mut().switch_type(id);
        self.clamp_field();
    }

    /// Change a list-like or checkbox row by `dir` without edit mode
    /// (Left/Right, the wheel).
    pub fn edit_toggle_dir(&mut self, f: RadioField, dir: i32) {
        match f {
            RadioField::Type => self.cycle_type(dir),
            RadioField::Setting(i) => self.step_setting(i as usize, dir),
            _ => {}
        }
    }

    /// ENTER / a click on a checkbox setting flips it.
    pub fn edit_toggle(&mut self, f: RadioField) {
        self.edit_toggle_dir(f, 1);
    }

    /// Step the Type row through the selectable types.
    pub fn cycle_type(&mut self, dir: i32) {
        let opts = self.type_options();
        let n = opts.len() as i32;
        let i = opts
            .iter()
            .position(|o| *o == self.active().vfo_type)
            .unwrap_or(0) as i32;
        let next = opts[((i + dir) % n + n) as usize % opts.len()].clone();
        self.set_type(&next);
    }

    /// All VFOs, in order (Master first).
    pub fn vfos(&self) -> &[Vfo] {
        &self.vfos
    }

    /// Index of the active VFO.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The active VFO.
    pub fn active(&self) -> &Vfo {
        &self.vfos[self.active]
    }

    fn active_mut(&mut self) -> &mut Vfo {
        &mut self.vfos[self.active]
    }

    /// The frequency editor bound to the active VFO.
    pub fn freq(&self) -> &FreqEntry {
        &self.freq
    }

    pub fn freq_mut(&mut self) -> &mut FreqEntry {
        &mut self.freq
    }

    /// Set the active VFO by index (clamped).
    pub fn set_active(&mut self, idx: usize) {
        if idx < self.vfos.len() {
            self.active = idx;
            // Rebind the editor to the newly active VFO's frequency.
            self.freq = FreqEntry::new(self.vfos[idx].frequency_hz);
        }
    }

    /// Cycle the active demod mode forward, applying mode defaults.
    pub fn next_mode(&mut self) {
        let m = self.active().mode.next();
        self.active_mut().set_mode(m);
        self.clamp_field();
    }

    /// Cycle the active demod mode backward, applying mode defaults.
    pub fn prev_mode(&mut self) {
        let m = self.active().mode.prev();
        self.active_mut().set_mode(m);
        self.clamp_field();
    }

    /// Set the active VFO's mode directly.
    pub fn set_mode(&mut self, mode: Mode) {
        self.active_mut().set_mode(mode);
        self.clamp_field();
    }

    // ── Audio output device selection (per-VFO) ──────────────────────────

    /// Set the list of available audio output options (from enumeration). The
    /// system default is implicit at option index 0.
    pub fn set_audio_options(&mut self, options: Vec<crate::engine::audio::OutputOption>) {
        self.audio_options = options;
    }

    /// The option labels for the audio picker: "Default" (index 0) followed by
    /// each option's friendly label, in order.
    pub fn audio_option_labels(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(self.audio_options.len() + 1);
        v.push("Default".to_string());
        v.extend(self.audio_options.iter().map(|o| o.label.clone()));
        v
    }

    /// The option index currently selected for the active VFO (0 = default,
    /// else the matching option's position + 1). A persisted target no longer
    /// present resolves to 0 (default) for display while the stored value is
    /// preserved.
    pub fn selected_audio_index(&self) -> usize {
        let stored = &self.active().audio_device;
        match stored {
            None => 0,
            Some(s) => self
                .audio_options
                .iter()
                .position(|o| o.target.to_stored().as_deref() == Some(s.as_str()))
                .map(|i| i + 1)
                .unwrap_or(0),
        }
    }

    /// Apply an audio option index to the active VFO: 0 clears to the system
    /// default, otherwise selects the option at `index - 1` (clamped),
    /// storing its canonical target string.
    pub fn apply_audio_index(&mut self, index: usize) {
        let stored = if index == 0 {
            None
        } else {
            self.audio_options
                .get(index - 1)
                .and_then(|o| o.target.to_stored())
        };
        self.active_mut().audio_device = stored;
    }

    /// Set the active VFO's audio device by its canonical stored string
    /// (`None` = default). Used by tests and config restore.
    pub fn set_active_audio(&mut self, stored: Option<String>) {
        self.active_mut().audio_device = stored;
    }

    /// Quick-cycle the active VFO's audio device by `dir` (+1/-1) across the
    /// option list (Default + options), wrapping.
    pub fn cycle_audio(&mut self, dir: i32) {
        let n = self.audio_options.len() + 1; // +1 for Default
        let cur = self.selected_audio_index() as i32;
        let next = (((cur + dir) % n as i32 + n as i32) % n as i32) as usize;
        self.apply_audio_index(next);
    }

    /// Ensure the field cursor points at a currently-visible field (e.g. after
    /// leaving CW mode hides the CwTone field).
    fn clamp_field(&mut self) {
        let fields = self.visible_fields();
        if !fields.contains(&self.field) {
            self.field = RadioField::Frequency;
            self.editing = false;
        }
    }

    /// Replace the active (Master) VFO from its profile entry and rebind the
    /// frequency editor.
    pub fn set_active_from_entry(&mut self, name: &str, e: &crate::config::VfoEntry) {
        self.vfos[self.active] = Vfo::from_entry(name, e);
        self.freq = FreqEntry::new(self.vfos[self.active].frequency_hz);
        self.clamp_field();
    }

    /// Apply a bookmark's demod settings to the active VFO: the mode (with
    /// its defaults), then the saved bandwidth (floored at the mode minimum),
    /// CW tone (clamped like the CW field) and AM AGC. Any edit in progress
    /// is abandoned.
    pub fn apply_settings(
        &mut self,
        mode: Mode,
        bandwidth_hz: f64,
        cw_tone_hz: f64,
        am_agc: crate::vfo::model::AmAgcMode,
    ) {
        self.editing = false;
        self.freq.cancel();
        let v = self.active_mut();
        v.set_mode(mode);
        if bandwidth_hz > 0.0 {
            v.bandwidth_hz = bandwidth_hz.max(mode.min_bandwidth());
        }
        v.cw_tone_hz = clamp_cw_tone(cw_tone_hz);
        v.am_agc = am_agc;
        self.clamp_field();
    }

    /// Apply a type and (some of) its settings to the active VFO: a
    /// bookmark, a scheduled job, a minimized file (spec 13.7). The type is
    /// switched first; each given setting that differs is then set in schema
    /// order with the type's rules (Voice: a new mode resets bandwidth and
    /// snap before the bookmark's bandwidth is applied); settings not given
    /// (snap) keep the VFO's. Returns false, changing nothing, when the type
    /// is not usable in this build or the VFO records as another type. Any
    /// edit in progress is abandoned.
    pub fn apply_type_settings(
        &mut self,
        vfo_type: &str,
        settings: &crate::vfo::types::Settings,
    ) -> bool {
        let Some(t) = crate::vfo::types::usable(vfo_type) else {
            return false;
        };
        if self.active().recording && self.active().vfo_type != vfo_type {
            return false;
        }
        self.editing = false;
        self.freq.cancel();
        let v = self.active_mut();
        v.switch_type(vfo_type);
        let mut s = v.type_settings();
        for f in t.schema(settings).fields {
            if let Some(val) = settings.get(&f.key)
                && s.get(&f.key) != Some(val)
            {
                s.insert(f.key.clone(), val.clone());
                t.normalize(&mut s, Some(&f.key));
            }
        }
        for (k, val) in settings {
            s.entry(k.clone()).or_insert_with(|| val.clone());
        }
        t.normalize(&mut s, None);
        v.set_type_settings(&s);
        self.clamp_field();
        true
    }

    /// Set the active VFO's absolute frequency and rebind the editor.
    pub fn active_mut_freq(&mut self, hz: u64) {
        self.active_mut().frequency_hz = hz;
        self.freq = FreqEntry::new(hz);
    }

    /// Commit the pending frequency edit to the active VFO, clamping to the
    /// band `[lo, hi]`. Returns the commit result (incl. out-of-bounds flag).
    pub fn commit_frequency(&mut self, lo: u64, hi: u64) -> CommitResult {
        let r = self.freq.commit(lo, hi);
        self.active_mut().frequency_hz = r.hz;
        r
    }

    /// Move every VFO so its offset from center is preserved when the source
    /// center changes from `old_center` to `new_center`. A VFO at offset 0
    /// stays centered (moves to `new_center`). Rebinds the active VFO's
    /// frequency editor to its new frequency.
    pub fn reproject_to_center(&mut self, old_center: u64, new_center: u64) {
        let delta = new_center as i64 - old_center as i64;
        for v in &mut self.vfos {
            let nf = v.frequency_hz as i64 + delta;
            v.frequency_hz = nf.max(0) as u64;
        }
        // Keep the editor in sync with the (possibly moved) active VFO, unless
        // the user is mid-edit on it.
        if !self.freq.is_dirty() {
            self.freq = FreqEntry::new(self.vfos[self.active].frequency_hz);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn recorder_fields_follow_audio_in_order() {
        let r = RadioModule::new(100_000_000);
        let f = r.visible_fields();
        let n = f.len();
        assert_eq!(
            &f[n - 8..],
            &[
                RadioField::Audio,
                RadioField::Mute,
                RadioField::RecNative,
                RadioField::RecBaseband,
                RadioField::RecMinimize,
                RadioField::RecName,
                RadioField::RecDir,
                RadioField::Record
            ]
        );
    }

    #[test]
    fn recording_choices_toggle_and_lock_while_recording() {
        let mut r = RadioModule::new(100_000_000);
        let rec = r.active().rec;
        assert!(
            rec.native && !rec.baseband && !rec.minimize,
            "default Native"
        );
        r.toggle_rec(RadioField::RecBaseband);
        r.toggle_rec(RadioField::RecMinimize);
        assert!(r.active().rec.baseband && r.active().rec.minimize);
        r.toggle_recording();
        r.toggle_rec(RadioField::RecNative);
        assert!(r.active().rec.native, "locked while recording");
        r.stop_recording();
        r.toggle_rec(RadioField::RecNative);
        assert!(!r.active().rec.native);
    }

    #[test]
    fn recording_choices_are_kept_per_type() {
        use crate::vfo::types::test_type;
        let mut r = RadioModule::new(100_000_000);
        r.toggle_rec(RadioField::RecBaseband);
        r.set_type(test_type::ID);
        assert_eq!(r.active().rec, crate::record::RecChoice::default());
        r.toggle_rec(RadioField::RecMinimize);
        r.set_type("Voice");
        assert!(r.active().rec.baseband && !r.active().rec.minimize);
        r.set_type(test_type::ID);
        assert!(r.active().rec.minimize && !r.active().rec.baseband);
    }

    #[test]
    fn blank_template_falls_back_to_the_sdrpp_default() {
        let mut r = RadioModule::new(100_000_000);
        r.set_rec_template("  $n_$t  ");
        assert_eq!(r.active().rec_template, "$n_$t");
        r.set_rec_template("");
        assert_eq!(
            r.active().rec_template,
            crate::record::naming::DEFAULT_TEMPLATE
        );
    }

    use super::*;

    /// A cpal-device audio option for tests.
    fn cpal_opt(name: &str) -> crate::engine::audio::OutputOption {
        crate::engine::audio::OutputOption {
            label: name.to_string(),
            target: crate::engine::audio_target::AudioTarget::CpalDevice(name.to_string()),
        }
    }

    #[test]
    fn starts_with_one_master_vfo_active() {
        let r = RadioModule::new(100_000_000);
        assert_eq!(r.vfos().len(), 1);
        assert!(r.active().is_master());
        assert_eq!(r.active_index(), 0);
        assert_eq!(r.freq().committed_hz(), 100_000_000);
    }

    #[test]
    fn audio_option_labels_prepend_default() {
        let mut r = RadioModule::new(100_000_000);
        r.set_audio_options(vec![cpal_opt("Speakers"), cpal_opt("USB Out")]);
        assert_eq!(
            r.audio_option_labels(),
            vec!["Default", "Speakers", "USB Out"],
            "index 0 is the system default (None)"
        );
    }

    #[test]
    fn selected_audio_index_maps_to_device_name() {
        let mut r = RadioModule::new(100_000_000);
        r.set_audio_options(vec![cpal_opt("Speakers"), cpal_opt("USB Out")]);
        assert_eq!(r.selected_audio_index(), 0);
        r.set_active_audio(Some("USB Out".to_string()));
        assert_eq!(r.active().audio_device.as_deref(), Some("USB Out"));
        assert_eq!(r.selected_audio_index(), 2);
    }

    #[test]
    fn apply_audio_index_sets_default_for_zero_and_named_otherwise() {
        let mut r = RadioModule::new(100_000_000);
        r.set_audio_options(vec![cpal_opt("Speakers"), cpal_opt("USB Out")]);
        r.apply_audio_index(1);
        assert_eq!(r.active().audio_device.as_deref(), Some("Speakers"));
        r.apply_audio_index(0);
        assert_eq!(r.active().audio_device, None, "index 0 clears to default");
    }

    #[test]
    fn apply_audio_index_stores_pipewire_canonical_string() {
        use crate::engine::audio::OutputOption;
        use crate::engine::audio_target::AudioTarget;
        let mut r = RadioModule::new(100_000_000);
        r.set_audio_options(vec![OutputOption {
            label: "VirtualSink (PipeWire)".into(),
            target: AudioTarget::PipeWireSink("VirtualSink".into()),
        }]);
        r.apply_audio_index(1);
        assert_eq!(
            r.active().audio_device.as_deref(),
            Some("pw:VirtualSink"),
            "PipeWire sink stores with the pw: prefix"
        );
        assert_eq!(
            r.selected_audio_index(),
            1,
            "and resolves back to its index"
        );
    }

    #[test]
    fn mute_is_a_field_after_audio_and_toggles_only_the_active_vfo() {
        let mut r = RadioModule::new(100_000_000);
        let f = r.visible_fields();
        let a = f.iter().position(|x| *x == RadioField::Audio).unwrap();
        assert_eq!(f[a + 1], RadioField::Mute);
        r.toggle_mute();
        assert!(r.active().muted);
        r.select_field(RadioField::Mute);
        r.begin_edit();
        r.edit_left();
        assert!(!r.active().muted, "Left/Right toggle it while editing");
        r.set_muted(true);
        assert!(r.active().muted);
        assert!(!r.active().recording, "mute leaves the recorder alone");
    }

    #[test]
    fn cycle_audio_wraps_through_default_and_named() {
        let mut r = RadioModule::new(100_000_000);
        r.set_audio_options(vec![cpal_opt("Speakers")]);
        assert_eq!(r.selected_audio_index(), 0);
        r.cycle_audio(1);
        assert_eq!(r.active().audio_device.as_deref(), Some("Speakers"));
        r.cycle_audio(1);
        assert_eq!(r.active().audio_device, None, "wraps back to Default");
        r.cycle_audio(-1);
        assert_eq!(r.active().audio_device.as_deref(), Some("Speakers"));
    }

    #[test]
    fn next_prev_mode_cycles_and_applies_defaults() {
        let mut r = RadioModule::new(100_000_000);
        assert_eq!(r.active().mode, Mode::Nfm);
        r.next_mode();
        assert_eq!(r.active().mode, Mode::Wfm);
        assert_eq!(r.active().bandwidth_hz, Mode::Wfm.default_bandwidth());
        r.prev_mode();
        assert_eq!(r.active().mode, Mode::Nfm);
    }

    #[test]
    fn commit_frequency_updates_active_vfo() {
        let mut r = RadioModule::new(100_000_000);
        // Edit via the frequency entry, then commit.
        r.freq_mut().increment(); // +1 kHz at default cursor
        let res = r.commit_frequency(0, 2_000_000_000);
        assert_eq!(res.hz, 100_001_000);
        assert_eq!(r.active().frequency_hz, 100_001_000);
        assert!(!res.out_of_bounds);
    }

    #[test]
    fn apply_settings_sets_mode_bandwidth_tone_and_agc() {
        use crate::vfo::model::AmAgcMode;
        let mut r = RadioModule::new(100_000_000);
        r.apply_settings(Mode::Cw, 300.0, 650.0, AmAgcMode::Off);
        let v = r.active();
        assert_eq!(v.mode, Mode::Cw);
        assert_eq!(v.bandwidth_hz, 300.0);
        assert_eq!(v.cw_tone_hz, 650.0);
        assert_eq!(
            v.snap_hz,
            Mode::Cw.default_snap_hz(),
            "mode defaults applied"
        );
        r.apply_settings(Mode::Am, 0.0, 800.0, AmAgcMode::Carrier);
        assert_eq!(
            r.active().bandwidth_hz,
            10_000.0,
            "0 keeps the mode default"
        );
        assert_eq!(r.active().am_agc, AmAgcMode::Carrier);
    }

    #[test]
    fn commit_out_of_bounds_clamps_and_flags() {
        let mut r = RadioModule::new(100_000_000);
        r.freq_mut().cancel();
        // Force the editor above the band.
        for _ in 0..3 {
            r.freq_mut().increment();
        }
        let res = r.commit_frequency(0, 100_000_500);
        assert!(res.out_of_bounds);
        assert_eq!(res.hz, 100_000_500);
        assert_eq!(r.active().frequency_hz, 100_000_500);
    }

    #[test]
    fn set_active_rebinds_frequency_editor() {
        // With a single VFO, set_active(0) rebinds to its frequency.
        let mut r = RadioModule::new(145_000_000);
        r.freq_mut().increment();
        assert!(r.freq().is_dirty());
        r.set_active(0);
        assert!(!r.freq().is_dirty(), "rebinding clears the pending edit");
        assert_eq!(r.freq().committed_hz(), 145_000_000);
    }

    #[test]
    fn field_cursor_cycles_visible_fields() {
        let mut r = RadioModule::new(100_000_000);
        assert_eq!(r.field(), RadioField::Frequency);
        r.field_next();
        assert_eq!(r.field(), RadioField::Type);
        r.field_next();
        assert_eq!(r.field(), RadioField::MODE);
        r.field_next();
        assert_eq!(r.field(), RadioField::BANDWIDTH);
        r.field_next();
        assert_eq!(r.field(), RadioField::SNAP);
        r.field_next();
        // NFM: no CwTone, so next is Audio, Mute, then the recorder.
        assert_eq!(r.field(), RadioField::Audio);
        r.field_next();
        assert_eq!(r.field(), RadioField::Mute);
        for _ in 0..6 {
            r.field_next();
        }
        assert_eq!(r.field(), RadioField::Record);
        r.field_next();
        assert_eq!(r.field(), RadioField::Frequency, "wraps");
    }

    #[test]
    fn cw_tone_steps_up_to_3000_hz_and_clamps() {
        use crate::vfo::model::AmAgcMode;
        let mut r = RadioModule::new(100_000_000);
        r.apply_settings(Mode::Cw, 200.0, 2_995.0, AmAgcMode::Off);
        assert_eq!(r.active().cw_tone_hz, 2_995.0, "above SDR++'s 1250 Hz");
        r.step_setting(3, 1);
        assert_eq!(r.active().cw_tone_hz, 3_000.0, "clamped at the top");
        r.apply_settings(Mode::Cw, 200.0, 5_000.0, AmAgcMode::Off);
        assert_eq!(r.active().cw_tone_hz, 3_000.0);
        r.apply_settings(Mode::Cw, 200.0, 100.0, AmAgcMode::Off);
        assert_eq!(r.active().cw_tone_hz, 250.0, "floor unchanged");
    }

    #[test]
    fn cw_tone_field_only_present_in_cw() {
        let mut r = RadioModule::new(100_000_000);
        assert!(!r.visible_fields().contains(&RadioField::CW_TONE));
        r.set_mode(Mode::Cw);
        assert!(r.visible_fields().contains(&RadioField::CW_TONE));
    }

    #[test]
    fn enter_toggles_edit_and_commits_frequency_on_exit() {
        let mut r = RadioModule::new(100_000_000);
        assert!(!r.is_editing());
        // Enter edit on Frequency.
        assert!(r.toggle_edit(0, 2_000_000_000).is_none());
        assert!(r.is_editing());
        // Edit a digit; stays pending.
        r.edit_up();
        assert!(r.freq().is_dirty());
        assert_eq!(r.active().frequency_hz, 100_000_000, "not retuned yet");
        // Enter again: leaves edit mode and commits.
        let res = r.toggle_edit(0, 2_000_000_000).expect("commit on exit");
        assert!(!r.is_editing());
        assert_eq!(res.hz, r.active().frequency_hz);
        assert_ne!(r.active().frequency_hz, 100_000_000, "committed");
    }

    #[test]
    fn field_nav_suppressed_while_editing() {
        let mut r = RadioModule::new(100_000_000);
        r.toggle_edit(0, 2_000_000_000); // editing Frequency
        r.field_next();
        assert_eq!(
            r.field(),
            RadioField::Frequency,
            "cannot change field while editing"
        );
    }

    #[test]
    fn esc_cancels_frequency_edit() {
        let mut r = RadioModule::new(100_000_000);
        r.toggle_edit(0, 2_000_000_000);
        r.edit_up();
        r.cancel_edit();
        assert!(!r.is_editing());
        assert!(!r.freq().is_dirty());
    }

    #[test]
    fn editing_mode_field_cycles_demod() {
        let mut r = RadioModule::new(100_000_000);
        r.field_next(); // Type
        r.field_next(); // Mode
        assert_eq!(r.field(), RadioField::MODE);
        r.toggle_edit(0, 2_000_000_000);
        assert_eq!(r.active().mode, Mode::Nfm);
        r.edit_right();
        assert_eq!(r.active().mode, Mode::Wfm);
        r.edit_left();
        assert_eq!(r.active().mode, Mode::Nfm);
    }

    #[test]
    fn cw_bandwidth_steps_by_100_hz_down_to_100_hz() {
        let mut r = RadioModule::new(7_000_000);
        r.set_mode(Mode::Cw);
        r.select_field(RadioField::BANDWIDTH);
        assert_eq!(r.field(), RadioField::BANDWIDTH);
        assert_eq!(r.active().bandwidth_hz, 200.0);
        r.toggle_edit(0, 2_000_000_000);
        r.edit_right();
        assert_eq!(r.active().bandwidth_hz, 300.0);
        r.edit_left();
        r.edit_left();
        assert_eq!(r.active().bandwidth_hz, 100.0);
        r.edit_left();
        assert_eq!(r.active().bandwidth_hz, 100.0, "floor is 100 Hz");
    }

    #[test]
    fn editing_bandwidth_and_snap_steps() {
        let mut r = RadioModule::new(100_000_000);
        // Go to Bandwidth (Frequency, Type, Mode, Bandwidth).
        r.field_next();
        r.field_next();
        r.field_next();
        assert_eq!(r.field(), RadioField::BANDWIDTH);
        let bw0 = r.active().bandwidth_hz;
        r.toggle_edit(0, 2_000_000_000);
        r.edit_right();
        assert!(r.active().bandwidth_hz > bw0);
        r.toggle_edit(0, 2_000_000_000); // leave edit (not frequency -> no commit)

        // Snap.
        r.field_next();
        assert_eq!(r.field(), RadioField::SNAP);
        r.toggle_edit(0, 2_000_000_000);
        r.edit_right();
        assert!(r.active().snap_hz > 0);
    }

    #[test]
    fn reproject_preserves_vfo_offset_from_center() {
        // Master at center 100 MHz, offset 0. Move center to 101 MHz; the VFO
        // (offset 0) follows to 101 MHz.
        let mut r = RadioModule::new(100_000_000);
        r.reproject_to_center(100_000_000, 101_000_000);
        assert_eq!(r.active().frequency_hz, 101_000_000);
        assert_eq!(r.freq().committed_hz(), 101_000_000, "editor rebinds");
    }

    #[test]
    fn reproject_keeps_nonzero_offset() {
        // A VFO 200 kHz above center keeps that +200 kHz offset after a move.
        let mut r = RadioModule::new(100_000_000);
        // Put the active VFO at +200 kHz by committing an edited frequency.
        r.active_mut().frequency_hz = 100_200_000;
        r.freq = FreqEntry::new(100_200_000);
        r.reproject_to_center(100_000_000, 100_500_000);
        // Offset preserved: 100_500_000 + 200_000.
        assert_eq!(r.active().frequency_hz, 100_700_000);
    }

    #[test]
    fn reproject_does_not_clobber_a_pending_edit() {
        let mut r = RadioModule::new(100_000_000);
        r.freq_mut().increment(); // pending edit
        assert!(r.freq().is_dirty());
        r.reproject_to_center(100_000_000, 101_000_000);
        // VFO frequency shifts, but the dirty editor is left alone.
        assert!(r.freq().is_dirty(), "mid-edit frequency editor preserved");
    }

    #[test]
    fn type_row_switches_types_and_rebuilds_the_setting_rows() {
        use crate::vfo::types::test_type;
        let mut r = RadioModule::new(100_000_000);
        assert_eq!(r.type_label(), "Voice");
        // FAX is listed only where libfax.so.1 loads
        let opts = r.type_options();
        assert_eq!(opts[..3], ["Voice", test_type::ID, "DSC"]);
        assert_eq!(opts[opts.len() - 2..], ["Digital", "HFDL"]);
        assert!(
            opts[3..opts.len() - 2].iter().all(|o| o == "FAX"),
            "{opts:?}"
        );
        assert!(r.is_choice(RadioField::Type));
        assert!(r.is_choice(RadioField::MODE), "Mode is a choice");
        assert!(!r.is_choice(RadioField::BANDWIDTH));
        r.select_field(RadioField::SNAP);
        r.cycle_type(1);
        assert_eq!(r.active().vfo_type, test_type::ID);
        let labels: Vec<String> = r.setting_fields().iter().map(|f| f.label.clone()).collect();
        assert_eq!(labels, ["Speed", "Width"]);
        assert!(!r.has_audio(), "a data type has no Audio / Mute rows");
        assert!(!r.visible_fields().contains(&RadioField::Audio));
        assert_eq!(r.field(), RadioField::Frequency, "Snap row is gone");
        r.step_setting(0, 1);
        assert_eq!(r.active().mode_label(), "Fast");
        r.cycle_type(-1);
        assert_eq!(r.active().vfo_type, "Voice");
        assert!(r.has_audio());
    }

    #[test]
    fn type_is_locked_while_recording_and_unknown_types_show_a_question_mark() {
        let mut r = RadioModule::new(100_000_000);
        r.toggle_recording();
        r.cycle_type(1);
        assert_eq!(r.active().vfo_type, "Voice", "locked while recording");
        r.stop_recording();
        r.active_mut().vfo_type = "NAVTEX".into();
        assert_eq!(r.type_label(), "?NAVTEX");
        assert!(r.setting_fields().is_empty());
        assert_eq!(r.type_options()[0], "NAVTEX");
        r.set_type("NAVTEX");
        r.cycle_type(1);
        assert_eq!(r.active().vfo_type, "Voice", "can leave an unknown type");
    }

    #[test]
    fn applying_bookmark_settings_resets_by_mode_then_sets_the_rest() {
        use serde_json::json;
        let mut r = RadioModule::new(100_000_000);
        r.active_mut().snap_hz = 12_345;
        let s = json!({"mode": "Usb", "bandwidth_hz": 2400.0, "cw_tone_hz": 700.0})
            .as_object()
            .unwrap()
            .clone();
        assert!(r.apply_type_settings("Voice", &s));
        let v = r.active();
        assert_eq!(
            (v.mode, v.bandwidth_hz, v.cw_tone_hz),
            (Mode::Usb, 2400.0, 700.0)
        );
        assert_eq!(
            v.snap_hz, 100,
            "a new mode resets the snap (not in bookmarks)"
        );
        assert!(
            !r.apply_type_settings("NAVTEX", &s),
            "unknown type: nothing changes"
        );
        assert!(r.apply_type_settings(crate::vfo::types::test_type::ID, &Default::default()));
        assert_eq!(r.active().vfo_type, "Test");
    }
}
