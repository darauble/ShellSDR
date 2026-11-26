//! Terminal runtime: event loop that drives the UI.
//!
//! This is the thin I/O edge. It translates crossterm key events into the
//! pure `NavKey` / `AppState` operations and redraws via `render`. All logic
//! lives in the pure modules; this file is intentionally not unit-tested
//! (it owns the terminal), which is why key translation is factored out into
//! the testable `translate_key` function.

use std::io;
use std::time::Duration;

use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{DisableMouseCapture, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::style::ResetColor;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::AppState;
use crate::ui::nav::NavKey;

/// A UI action decoded from an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nav(NavKey),
    ToggleSpectrum,
    ToggleWaterfall,
    ToggleTuneMode,
    ToggleRun,
    /// `m`: global mute on/off.
    ToggleMute,
    /// Ctrl+Shift+Right / Left: the left panel wider / narrower.
    PanelWider,
    PanelNarrower,
    /// `?`: open/close the key-binding help popup.
    Help,
    CycleTheme,
    /// `w`: switch the Work Area tab (Spectrum / VFO).
    SwitchTab,
    /// `l`: Work Area layout Tabs / Split.
    ToggleLayout,
    /// Scroll the left panel a page up/down.
    PageUp,
    PageDown,
    /// Arrow keys (context-dependent: field nav vs in-field edit).
    Left,
    Right,
    Up,
    Down,
    /// Zero digits to the right (Radio: while editing frequency).
    ZeroRight,
    /// ENTER: enter/leave edit mode (and commit frequency on leave).
    Enter,
    /// Esc: cancel edit.
    Escape,
    /// Type a decimal digit (Radio: while editing frequency).
    TypeDigit(u8),
    /// Toggle spectrum dB auto-range.
    ToggleDbAuto,
    /// Adjust the dB window ceiling (max) down/up.
    DbMaxDown,
    DbMaxUp,
    /// Adjust the dB window floor (min) down/up.
    DbMinDown,
    DbMinUp,
    /// `/`: focus the Bookmarks module and type into its find filter.
    Find,
    /// A printable character: text while a text field captures the
    /// keyboard, otherwise a key of the focused Work Area view (`d`, `f`...).
    Char(char),
    /// A function key F1..F12: only for the Work Area views (spec 14.6).
    FKey(u8),
    Backspace,
    Delete,
    Home,
    End,
    Quit,
}

/// Translate a key event into an `Action`, if it maps to one. Pure and
/// testable; the event loop applies the result to `AppState`.
pub fn translate_key(key: KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) => Some(Action::Quit),
        (KeyCode::Right, m) if m.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
            Some(Action::PanelWider)
        }
        (KeyCode::Left, m) if m.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
            Some(Action::PanelNarrower)
        }
        (KeyCode::Tab, _) => Some(Action::Nav(NavKey::Tab)),
        (KeyCode::BackTab, _) => Some(Action::Nav(NavKey::BackTab)),
        (KeyCode::Char('+'), _) => Some(Action::Nav(NavKey::Expand)),
        (KeyCode::Char('-'), _) => Some(Action::Nav(NavKey::Collapse)),
        (KeyCode::Char(' '), _) => Some(Action::ToggleRun),
        (KeyCode::Char('c'), m) if !m.contains(KeyModifiers::CONTROL) => {
            Some(Action::ToggleTuneMode)
        }
        (KeyCode::Char('['), _) => Some(Action::ToggleSpectrum),
        (KeyCode::Char(']'), _) => Some(Action::ToggleWaterfall),
        (KeyCode::Char('m'), _) => Some(Action::ToggleMute),
        (KeyCode::Char('?'), _) => Some(Action::Help),
        (KeyCode::Char('t'), _) => Some(Action::CycleTheme),
        (KeyCode::Char('w'), _) => Some(Action::SwitchTab),
        (KeyCode::Char('l'), _) => Some(Action::ToggleLayout),
        (KeyCode::Char('a'), _) => Some(Action::ToggleDbAuto),
        (KeyCode::Char(','), _) => Some(Action::DbMaxDown),
        (KeyCode::Char('.'), _) => Some(Action::DbMaxUp),
        (KeyCode::Char('<'), _) => Some(Action::DbMinDown),
        (KeyCode::Char('>'), _) => Some(Action::DbMinUp),
        (KeyCode::PageUp, _) => Some(Action::PageUp),
        (KeyCode::PageDown, _) => Some(Action::PageDown),
        (KeyCode::Char('z'), _) => Some(Action::ZeroRight),
        (KeyCode::Char(d @ '0'..='9'), _) => Some(Action::TypeDigit(d as u8 - b'0')),
        (KeyCode::Left, _) => Some(Action::Left),
        (KeyCode::Right, _) => Some(Action::Right),
        (KeyCode::Up, _) => Some(Action::Up),
        (KeyCode::Down, _) => Some(Action::Down),
        (KeyCode::Enter, _) => Some(Action::Enter),
        (KeyCode::Esc, _) => Some(Action::Escape),
        (KeyCode::Char('/'), _) => Some(Action::Find),
        (KeyCode::Backspace, _) => Some(Action::Backspace),
        (KeyCode::Delete, _) => Some(Action::Delete),
        (KeyCode::Home, _) => Some(Action::Home),
        (KeyCode::End, _) => Some(Action::End),
        (KeyCode::F(n), _) => Some(Action::FKey(n)),
        (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            Some(Action::Char(c))
        }
        _ => None,
    }
}

/// Translate a key while a text field captures the keyboard
/// (`AppState::text_capture`): printable characters are text, not
/// accelerators (so typing "q" or "t" into a name neither quits nor changes
/// the theme); editing and navigation keys keep their meaning.
pub fn translate_text_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(Action::Char(c))
        }
        KeyCode::Char(_) => None,
        _ => translate_key(key),
    }
}

/// Keys for the open file dialog (modal). The list: Up/Down, PgUp/PgDn,
/// Home/End move; Enter or Right opens; Left or Backspace goes up; a letter
/// jumps to the next name starting with it; `~` goes home. Tab moves between
/// the list and the button row (Left/Right select, Enter presses). Esc closes.
fn apply_file_browser(state: &mut AppState, action: Action) {
    use crate::ui::file_browser::{BrowserFocus, Outcome};
    use crate::ui::nav::NavKey;
    let Some(b) = &mut state.file_browser else {
        return;
    };
    let page = b.page as i64;
    let outcome = match (b.focus, action) {
        (_, Action::Escape) => Outcome::Cancel,
        (_, Action::Nav(NavKey::Tab | NavKey::BackTab)) => {
            b.toggle_focus();
            Outcome::Stay
        }
        (_, Action::Quit) => {
            state.request_quit();
            return;
        }
        (BrowserFocus::Buttons, Action::Left) => {
            b.step_button(-1);
            Outcome::Stay
        }
        (BrowserFocus::Buttons, Action::Right) => {
            b.step_button(1);
            Outcome::Stay
        }
        (BrowserFocus::Buttons, Action::Enter) => {
            let btn = b.button;
            b.press(btn)
        }
        (BrowserFocus::Buttons, Action::Up | Action::Down) => {
            b.toggle_focus();
            Outcome::Stay
        }
        (BrowserFocus::List, Action::Up) => {
            b.move_by(-1);
            Outcome::Stay
        }
        (BrowserFocus::List, Action::Down) => {
            b.move_by(1);
            Outcome::Stay
        }
        (BrowserFocus::List, Action::PageUp) => {
            b.move_by(-page);
            Outcome::Stay
        }
        (BrowserFocus::List, Action::PageDown) => {
            b.move_by(page);
            Outcome::Stay
        }
        (BrowserFocus::List, Action::Home) => {
            b.first();
            Outcome::Stay
        }
        (BrowserFocus::List, Action::End) => {
            b.last();
            Outcome::Stay
        }
        (BrowserFocus::List, Action::Enter) => b.activate(),
        (BrowserFocus::List, Action::Right) => {
            // Right opens folders only; files need Enter.
            if b.selected().map(|e| e.kind) != Some(crate::ui::file_browser::EntryKind::File) {
                b.activate()
            } else {
                Outcome::Stay
            }
        }
        (BrowserFocus::List, Action::Left | Action::Backspace) => {
            b.up();
            Outcome::Stay
        }
        (BrowserFocus::List, Action::Char('~')) => {
            b.home();
            Outcome::Stay
        }
        (BrowserFocus::List, Action::Char(c)) if !c.is_whitespace() => {
            b.jump_to(c);
            Outcome::Stay
        }
        _ => Outcome::Stay,
    };
    state.file_browser_outcome(outcome);
}

/// Keys for an open text prompt.
fn apply_prompt(state: &mut AppState, action: Action) {
    let Some(p) = &mut state.prompt else { return };
    let t = &mut p.input;
    match action {
        Action::Char(c) => {
            t.insert(c);
        }
        Action::Backspace => {
            t.backspace();
        }
        Action::Delete => {
            t.delete();
        }
        Action::Left => t.left(),
        Action::Right => t.right(),
        Action::Home => t.home(),
        Action::End => t.end(),
        Action::Enter => return state.prompt_confirm(),
        Action::Escape => return state.prompt_cancel(),
        _ => return,
    }
    p.error = None;
}

/// Keys for an open bookmark editor (modal: nothing leaks to the app).
fn apply_editor(state: &mut AppState, action: Action) {
    use crate::bookmarks::editor::EditorOutcome;
    let Some(ed) = &mut state.bm_editor else {
        return;
    };
    let outcome = match action {
        Action::Up => {
            ed.up();
            EditorOutcome::Open
        }
        Action::Down => {
            ed.down();
            EditorOutcome::Open
        }
        Action::Left => {
            ed.horizontal(-1);
            EditorOutcome::Open
        }
        Action::Right => {
            ed.horizontal(1);
            EditorOutcome::Open
        }
        Action::Nav(NavKey::Tab) => {
            ed.tab(true);
            EditorOutcome::Open
        }
        Action::Nav(NavKey::BackTab) => {
            ed.tab(false);
            EditorOutcome::Open
        }
        Action::Enter => ed.enter(),
        Action::Escape => ed.escape(),
        Action::Char(c) => {
            ed.type_char(c);
            EditorOutcome::Open
        }
        Action::TypeDigit(d) => {
            ed.type_char(char::from(b'0' + d));
            EditorOutcome::Open
        }
        Action::ZeroRight => {
            ed.type_char('z');
            EditorOutcome::Open
        }
        Action::Backspace => {
            ed.backspace();
            EditorOutcome::Open
        }
        Action::Delete => {
            ed.delete();
            EditorOutcome::Open
        }
        Action::Home => {
            ed.home();
            EditorOutcome::Open
        }
        Action::End => {
            ed.end();
            EditorOutcome::Open
        }
        _ => EditorOutcome::Open,
    };
    state.editor_outcome(outcome);
}

/// Keys for an open schedule editor (modal: nothing leaks to the app).
fn apply_sched_editor(state: &mut AppState, action: Action) {
    use crate::schedule::editor::SchedEditorOutcome as O;
    let Some(ed) = &mut state.sched_editor else {
        return;
    };
    let outcome = match action {
        Action::Up => {
            ed.up();
            O::Open
        }
        Action::Down => {
            ed.down();
            O::Open
        }
        Action::Left => {
            ed.horizontal(-1);
            O::Open
        }
        Action::Right => {
            ed.horizontal(1);
            O::Open
        }
        Action::Nav(NavKey::Tab) => {
            ed.tab(true);
            O::Open
        }
        Action::Nav(NavKey::BackTab) => {
            ed.tab(false);
            O::Open
        }
        Action::Enter => ed.enter(),
        Action::Escape => ed.escape(),
        Action::Char(c) => {
            ed.type_char(c);
            O::Open
        }
        Action::TypeDigit(d) => {
            ed.type_char(char::from(b'0' + d));
            O::Open
        }
        Action::Backspace => {
            ed.backspace();
            O::Open
        }
        Action::Delete => {
            ed.delete();
            O::Open
        }
        Action::Home => {
            ed.home();
            O::Open
        }
        Action::End => {
            ed.end_key();
            O::Open
        }
        _ => O::Open,
    };
    state.sched_editor_outcome(outcome);
}

/// Keys for the focused Schedule module.
fn apply_schedule(state: &mut AppState, action: Action) {
    let m = &mut state.schedule;
    match action {
        Action::Up => m.up(),
        Action::Down => m.down(),
        Action::Left => m.horizontal(-1),
        Action::Right => m.horizontal(1),
        Action::Home => m.first(),
        Action::End => m.last(),
        Action::Enter => {
            let req = m.enter();
            state.schedule_request(req);
        }
        _ => {}
    }
}

/// Keys while typing into the bookmark find filter.
fn apply_find(state: &mut AppState, action: Action) {
    let bm = &mut state.bookmarks;
    match action {
        Action::Char(c) => bm.find_char(c),
        Action::Backspace => bm.find_backspace(),
        Action::Delete => bm.find_delete(),
        Action::Left => bm.filter.left(),
        Action::Right => bm.filter.right(),
        Action::Home => bm.filter.home(),
        Action::End => bm.filter.end(),
        Action::Enter | Action::Down | Action::Up => bm.end_find(true),
        Action::Escape => bm.end_find(false),
        Action::Nav(k) => {
            bm.end_find(true);
            state.on_nav(k);
        }
        Action::Quit => state.request_quit(),
        _ => {}
    }
}

/// Keys for the focused Bookmarks module.
fn apply_bookmarks(state: &mut AppState, action: Action) {
    use crate::bookmarks::manager::BookmarkField;
    let bm = &mut state.bookmarks;
    match action {
        Action::Up => bm.up(),
        Action::Down => bm.down(),
        Action::Left => bm.horizontal(-1),
        Action::Right => bm.horizontal(1),
        Action::Home => bm.first(),
        Action::End => bm.last(),
        Action::PageUp | Action::PageDown if bm.field() == BookmarkField::Items => {
            bm.page(action == Action::PageDown)
        }
        Action::PageUp => state.page_left(false),
        Action::PageDown => state.page_left(true),
        Action::Enter => {
            let req = bm.enter();
            state.bookmark_request(req);
        }
        Action::Delete => {
            let req = bm.remove_request();
            state.bookmark_request(req);
        }
        _ => {}
    }
}

/// Keys for the open help popup (modal: nothing leaks to the app).
fn apply_help(state: &mut AppState, action: Action) {
    let Some(h) = &mut state.help else { return };
    match action {
        Action::Up => h.scroll_by(-1),
        Action::Down => h.scroll_by(1),
        Action::PageUp => h.page(false),
        Action::PageDown => h.page(true),
        Action::Home => h.home(),
        Action::End => h.end(),
        Action::Help | Action::Escape | Action::Enter => state.help = None,
        Action::Quit => state.request_quit(),
        _ => {}
    }
}

/// Apply an action to the state.
pub fn apply_action(state: &mut AppState, action: Action) {
    use crate::ui::nav::Focus;
    let band = state.freq_band();

    // The help popup sits above everything and captures the keyboard.
    if state.help.is_some() {
        return apply_help(state, action);
    }

    // Modal dialogs capture the keyboard, topmost first.
    if !state.popup_open() {
        if state.prompt.is_some() {
            return apply_prompt(state, action);
        }
        if state.file_browser.is_some() {
            return apply_file_browser(state, action);
        }
        if state.bm_editor.is_some() {
            return apply_editor(state, action);
        }
        if state.sched_editor.is_some() {
            return apply_sched_editor(state, action);
        }
        if state.bookmarks.is_filter_editing() {
            return apply_find(state, action);
        }
    }
    if state.nav.focus == Focus::Bookmarks
        && matches!(
            action,
            Action::Up
                | Action::Down
                | Action::Left
                | Action::Right
                | Action::Enter
                | Action::Home
                | Action::End
                | Action::Delete
                | Action::PageUp
                | Action::PageDown
        )
        && !state.popup_open()
    {
        return apply_bookmarks(state, action);
    }

    // The Report module's only control: [Report] (spec 20.1).
    if state.nav.focus == Focus::Report && action == Action::Enter && !state.popup_open() {
        return state.open_page(crate::ui::page::WorkOwner::Report);
    }

    if state.nav.focus == Focus::Schedule
        && matches!(
            action,
            Action::Up
                | Action::Down
                | Action::Left
                | Action::Right
                | Action::Enter
                | Action::Home
                | Action::End
                | Action::Delete
        )
        && !state.popup_open()
    {
        return apply_schedule(state, action);
    }

    // The DSC view (spec 14.6): its open F9 menu is modal, the function
    // keys are its own, and when focused it takes its keys first.
    if !state.popup_open() {
        if state.dsc_ui.menu.is_some() && state.dsc_view_shown() {
            state.dsc_menu_key(action);
            return;
        }
        if state.hfdl_ui.menu.is_some() && state.hfdl_view_shown() {
            state.hfdl_menu_key(action);
            return;
        }
        if state.fax_ui.menu.is_some() && state.fax_view_shown() {
            state.fax_menu_key(action);
            return;
        }
        if state.digital_ui.menu.is_some() && state.digital_view_shown() {
            state.digital_menu_key(action);
            return;
        }
        // A module's page (spec 19): its F-keys, and its keys when focused.
        if state.page_shown() {
            if let Action::FKey(n) = action {
                state.page_fkey(n);
                return;
            }
            if state.nav.focus == Focus::WorkArea && state.page_key(action) {
                return;
            }
        }
        if let Action::FKey(n) = action {
            state.dsc_fkey(n);
            state.hfdl_fkey(n);
            state.fax_fkey(n);
            state.digital_fkey(n);
            return;
        }
        if state.nav.focus == Focus::WorkArea && state.dsc_view_shown() && state.dsc_key(action) {
            return;
        }
        if state.nav.focus == Focus::WorkArea && state.hfdl_view_shown() && state.hfdl_key(action) {
            return;
        }
        if state.nav.focus == Focus::WorkArea && state.fax_view_shown() && state.fax_key(action) {
            return;
        }
        if state.nav.focus == Focus::WorkArea
            && state.digital_view_shown()
            && state.digital_key(action)
        {
            return;
        }
    }

    // When a modal selection popup is open, it captures navigation keys.
    if state.popup_open() {
        match action {
            Action::Up => state.popup_up(),
            Action::Down => state.popup_down(),
            Action::Enter => state.popup_confirm(),
            Action::Escape => state.popup_cancel(),
            Action::Quit => state.request_quit(),
            // Swallow everything else so it does not leak to the background.
            _ => {}
        }
        return;
    }

    match action {
        Action::Nav(k) => state.on_nav(k),
        Action::ToggleSpectrum => state.toggle_spectrum(),
        Action::ToggleWaterfall => state.toggle_waterfall(),
        Action::ToggleTuneMode => state.toggle_tune_mode(),
        Action::ToggleRun => state.toggle_run(),
        Action::ToggleMute => state.toggle_mute(),
        Action::PanelWider => state.resize_left_panel(true),
        Action::PanelNarrower => state.resize_left_panel(false),
        Action::Help => state.toggle_help(),
        Action::CycleTheme => state.cycle_theme(),
        Action::SwitchTab => state.switch_work_tab(),
        Action::ToggleLayout => state.toggle_work_layout(),
        Action::PageUp => state.page_left(false),
        Action::PageDown => state.page_left(true),
        Action::Left => match state.nav.focus {
            Focus::Global => state.global_left(),
            Focus::Source => state.source_adjust(-1),
            Focus::Radio => {
                // On the Mode field, Left/Right cycles the mode directly even
                // without entering edit mode (quick change); Enter opens the
                // popup for a full list.
                let f = state.radio.field();
                if (state.radio.is_choice(f) || state.radio.is_toggle(f))
                    && !state.radio.is_editing()
                {
                    state.radio.edit_toggle_dir(f, -1);
                } else if state.radio.field() == crate::vfo::radio::RadioField::Audio
                    && !state.radio.is_editing()
                {
                    state.radio.cycle_audio(-1);
                } else if crate::vfo::radio::RadioModule::is_rec_check(state.radio.field()) {
                    state.radio.toggle_rec(state.radio.field());
                } else if state.radio.field() == crate::vfo::radio::RadioField::Mute {
                    state.radio.toggle_mute();
                } else {
                    state.radio.edit_left();
                }
            }
            _ => {}
        },
        Action::Right => match state.nav.focus {
            Focus::Global => state.global_right(),
            Focus::Source => state.source_adjust(1),
            Focus::Radio => {
                let f = state.radio.field();
                if (state.radio.is_choice(f) || state.radio.is_toggle(f))
                    && !state.radio.is_editing()
                {
                    state.radio.edit_toggle_dir(f, 1);
                } else if state.radio.field() == crate::vfo::radio::RadioField::Audio
                    && !state.radio.is_editing()
                {
                    state.radio.cycle_audio(1);
                } else if crate::vfo::radio::RadioModule::is_rec_check(state.radio.field()) {
                    state.radio.toggle_rec(state.radio.field());
                } else if state.radio.field() == crate::vfo::radio::RadioField::Mute {
                    state.radio.toggle_mute();
                } else {
                    state.radio.edit_right();
                }
            }
            _ => {}
        },
        Action::Up => match state.nav.focus {
            Focus::Global => state.global_up(),
            Focus::Source => state.source.field_prev(),
            Focus::Radio => {
                if state.radio.is_editing() {
                    state.radio.edit_up();
                } else {
                    state.radio.field_prev();
                }
            }
            _ => {}
        },
        Action::Down => match state.nav.focus {
            Focus::Global => state.global_down(),
            Focus::Source => state.source.field_next(),
            Focus::Radio => {
                if state.radio.is_editing() {
                    state.radio.edit_down();
                } else {
                    state.radio.field_next();
                }
            }
            _ => {}
        },
        Action::ZeroRight => match state.nav.focus {
            Focus::Global => state.global_zero_right(),
            Focus::Radio => state.radio.edit_zero_right(),
            _ => {}
        },
        Action::Enter => match state.nav.focus {
            Focus::Global => state.global_enter(),
            Focus::Source => {
                use crate::source::model::SourceField;
                match state.source.field() {
                    // List controls open the selection popup.
                    SourceField::Device
                    | SourceField::SampleRate
                    | SourceField::Decimation
                    | SourceField::Bandwidth
                    | SourceField::Antenna => state.open_popup_for_focus(),
                    // Toggles flip in place.
                    SourceField::Kind => state.toggle_source_kind(),
                    SourceField::Agc => state.source_adjust(1),
                    SourceField::Gain(_) => {}
                    SourceField::FilePath
                    | SourceField::FileTransport
                    | SourceField::FileBack
                    | SourceField::FileFwd => {
                        state.activate_file_field();
                    }
                    SourceField::NetServer | SourceField::NetToken => {
                        state.activate_network_field()
                    }
                    SourceField::NetAudio => state.open_popup_for_focus(),
                    SourceField::NetOpen => state.source.network.open_request = true,
                    SourceField::ShareOn
                    | SourceField::ShareListen
                    | SourceField::ShareToken
                    | SourceField::ShareMute
                    | SourceField::ShareDisconnect => state.activate_share_field(),
                }
            }
            Focus::Radio => {
                // On the Mode field, Enter opens the selection popup instead of
                // inline edit; other fields toggle in-place editing.
                let f = state.radio.field();
                if state.activate_recorder_field(f) {
                    // Recorder controls act at once (toggle / prompt / button).
                } else if state.radio.is_toggle(f) {
                    state.radio.edit_toggle(f);
                } else if (state.radio.is_choice(f) || f == crate::vfo::radio::RadioField::Audio)
                    && !state.radio.is_editing()
                {
                    state.open_popup_for_focus();
                } else {
                    state.radio.toggle_edit(band.0, band.1);
                }
            }
            _ => {}
        },
        Action::Escape => match state.nav.focus {
            Focus::Global => state.global_cancel(),
            Focus::Radio => state.radio.cancel_edit(),
            _ => {}
        },
        Action::TypeDigit(d) => match state.nav.focus {
            Focus::Global => state.global_type_digit(d),
            Focus::Radio => state.radio.edit_type_digit(d),
            _ => {}
        },
        Action::ToggleDbAuto => state.db_range.toggle_auto(),
        Action::DbMaxDown => state
            .db_range
            .adjust_max(-crate::ui::spectrum::DB_ADJUST_STEP),
        Action::DbMaxUp => state
            .db_range
            .adjust_max(crate::ui::spectrum::DB_ADJUST_STEP),
        Action::DbMinDown => state
            .db_range
            .adjust_min(-crate::ui::spectrum::DB_ADJUST_STEP),
        Action::DbMinUp => state
            .db_range
            .adjust_min(crate::ui::spectrum::DB_ADJUST_STEP),
        Action::Find => state.find_bookmark(),
        // `o`: open the focused module's page (spec 19.2).
        Action::Char('o') => state.open_focused_page(),
        // Text-editing keys outside a text field; function keys outside
        // a Work Area view.
        Action::Char(_)
        | Action::FKey(_)
        | Action::Backspace
        | Action::Delete
        | Action::Home
        | Action::End => {}
        Action::Quit => state.request_quit(),
    }
}

/// Enable mouse reporting for clicks, wheel and button-drag only.
///
/// crossterm's `EnableMouseCapture` also turns on "any-motion" tracking
/// (DECSET 1003), which makes the terminal send an event for every cell the
/// pointer crosses even with no button held. ShellSDR has no hover behavior,
/// so that is pure event flood: it woke the UI loop constantly (redraw CPU on
/// the Pi, and it sped up the waterfall). This requests normal tracking
/// (1000: press/release + wheel), button-event tracking (1002: drag, for a
/// future scrollbar/slider drag), and SGR extended coordinates (1006, plus
/// urxvt 1015 as a fallback) so columns beyond 223 are reported correctly.
/// `DisableMouseCapture` still turns all of these off on exit.
struct EnableClickMouseCapture;

impl crossterm::Command for EnableClickMouseCapture {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str("\x1b[?1000h\x1b[?1002h\x1b[?1015h\x1b[?1006h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        crossterm::event::EnableMouseCapture.execute_winapi()
    }
}

/// [Max] playback waits while the Digital worker has more IQ blocks than
/// this to decode (100 ms each at Max: 10 s of the file). The UI loop
/// hands the blocks over, so this also bounds the speed per UI frame.
const MAX_DIGITAL_BACKLOG: usize = 100;

/// Run the TUI event loop until the user quits. Owns terminal setup/teardown.
/// Takes `&mut` so the caller can read final state (e.g. to persist the theme).
pub fn run(state: &mut AppState) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Enter the alternate screen and explicitly wipe it: some terminals (or
    // multiplexers without alt-screen support) keep the shell's text, which
    // would otherwise show through until each cell is redrawn.
    execute!(
        stdout,
        EnterAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0),
        EnableClickMouseCapture
    )?;
    // Restore the terminal even if the UI panics, so the panic message is
    // readable and the shell is not left in raw mode.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(&mut io::stdout());
        default_hook(info);
    }));
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    // Reset ratatui's buffers so the first frame paints every cell.
    terminal.clear()?;

    let result = event_loop(&mut terminal, state, &mut crate::ui::front::TermFront);

    let _ = std::panic::take_hook();
    restore_terminal(terminal.backend_mut());
    result
}

/// Undo `run`'s terminal setup. Clears the screen before leaving the
/// alternate screen so no UI is left behind (nor scrolled into history) on
/// terminals where the alternate screen is unavailable. Best effort: errors
/// are ignored so every step is attempted.
fn restore_terminal<W: io::Write>(out: &mut W) {
    let _ = execute!(
        out,
        DisableMouseCapture,
        ResetColor,
        Clear(ClearType::All),
        MoveTo(0, 0),
        LeaveAlternateScreen,
        Show
    );
    let _ = disable_raw_mode();
}

/// The application loop, drawing to `terminal` and taking input from
/// `front` (the terminal, or the `--gui` window).
pub fn event_loop<B: ratatui::backend::Backend, F: crate::ui::front::Front<B>>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    front: &mut F,
) -> io::Result<()> {
    use crate::app::RunState;
    use crate::engine::{self, Command, EngineHandle, Event as EngineEvent};
    use crate::engine_app::EnginePush;
    use crate::source::model::SourceKind;

    let mut engine: Option<Box<dyn EngineHandle>> = None;
    // The writer side of the running recording (the engine holds the sink).
    let mut rec_session: Vec<crate::record::recorder::RecHandle> = Vec::new();
    // What the running engine was last told (changes become commands).
    let mut push = EnginePush::started(state);
    // What restarts a running engine: the audio device, the stream.
    let mut last_audio = state.radio.active().audio_device.clone();
    let mut last_stream_key = state.source.stream_key();
    // The Network source's compression the running stream was given.
    let mut net_bitrate = state.source.network.bitrate;
    // The engine's counters, from its last Stats event.
    let mut stats = engine::EngineStats::default();
    let mut speed_meter = crate::source::model::SpeedMeter::default();
    // Sharing this screen with a viewer (0.4.1).
    let mut share = crate::ui::share_loop::ShareLoop::default();
    // A window into another, sharing ShellSDR (0.4.1).
    let mut window: Option<crate::ui::viewer_loop::TermViewer> = None;

    // Clickable regions of the last drawn frame (what the user sees).
    let mut hits = crate::ui::mouse::HitMap::default();
    let mut last_disk_check = std::time::Instant::now();
    // The source watchdog (spec 20.6): the samples read, and since when
    // that count has not moved.
    let mut watch = (0u64, std::time::Instant::now());
    state.fax_ui.kitty = front.kitty();
    let mut kitty = KittyPlacement::default();
    state.utc = crate::bookmarks::model::UtcNow::now();
    state.local_hhmm = crate::bookmarks::model::local_hhmm_now();
    while state.running {
        if front.gone() {
            break;
        }
        if front.quit_requested() {
            state.request_quit();
        }
        front.before_draw(terminal, state);
        // a window paints the pictures at its pixels (spec 28)
        state.pixel_pictures = front.graphical();
        if let Ok(size) = terminal.size() {
            crate::ui::screen::prepare(
                state,
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
            );
        }
        // The Network source's [Open window], and a window that closed.
        if std::mem::take(&mut state.source.network.open_request)
            && window.is_none()
            && let Ok(size) = terminal.size()
        {
            window = crate::ui::viewer_loop::TermViewer::open(
                state,
                size.width,
                size.height,
                front.graphical(),
            );
            let _ = terminal.clear();
        }
        if let Some(r) = window.as_ref().and_then(|w| w.closed()) {
            window = None;
            state.show_notice(format!("Window closed: {r}"));
            let _ = terminal.clear();
        }
        state.fax_ui.kitty_request.replace(None);
        if let Some(w) = &window {
            terminal.draw(|f| w.draw(f))?;
            kitty.show(state, None);
            if front.graphical() {
                front.after_draw(terminal, w.overlays(), w.take_rows(), w.pictures());
            }
        } else {
            terminal.draw(|f| hits = crate::ui::render::render_with_hits(f, state))?;
            kitty.show(state, state.fax_ui.kitty_request.borrow().as_ref());
            if front.graphical() {
                // nothing drawn over a dialog
                let overlays = if crate::ui::share_loop::modal_open(state) {
                    Vec::new()
                } else {
                    hits.overlays.clone()
                };
                front.after_draw(terminal, overlays, Vec::new(), hits.pictures.clone());
            }
        }
        // Handle every pending input before the next redraw (one draw per
        // batch, not per event). A mouse event that was acted on ends the
        // batch, so the next click is hit-tested against a fresh frame (the
        // previous one may have opened a popup or scrolled the panel).
        // a viewer gets up to 20 frames a second
        let wait = if window.is_some() {
            25
        } else if state.source.share.viewer.is_some() {
            50
        } else {
            100
        };
        if front.poll(Duration::from_millis(wait))? {
            loop {
                let ev = front.read()?;
                let acted_on_mouse = match &mut window {
                    Some(w) => {
                        let go_on = w.event(ev);
                        // the compression chosen in the window is kept
                        state.source.network.bitrate = w.bitrate();
                        if !go_on {
                            // [Back], F12 or Ctrl+]: back to this ShellSDR
                            window = None;
                            let _ = terminal.clear();
                        }
                        false
                    }
                    None => crate::ui::screen::handle_event(state, &hits, ev),
                };
                if acted_on_mouse || !state.running || !front.poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        // ── Bookmarks: UTC clock for on-air coloring; save changes at once,
        // and pick up another instance's save (checked once a second).
        state.utc = crate::bookmarks::model::UtcNow::now();
        state.local_hhmm = crate::bookmarks::model::local_hhmm_now();
        let check_disk = last_disk_check.elapsed() >= Duration::from_secs(1);
        if check_disk {
            last_disk_check = std::time::Instant::now();
        }
        if let Some(w) = state.sync_bookmarks(check_disk) {
            log::warn!("{w}");
        }
        if check_disk {
            state.report_tick();
        }

        // ── Scheduler: drives the Running / recording switches, which the
        // reconcilers below then apply; saves job changes at once.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        state.utc_offset_min = crate::schedule::window::local_offset_min(now);
        state.schedule.set_utc_offset(state.utc_offset_min);
        state.schedule_tick(now);
        if let Some(w) = state.sync_schedule() {
            log::warn!("{w}");
        }

        // ── DSC: the message service follows the VFO; decoded calls from
        // the engine are recorded at once (spec 14.4).
        let wall = now_f64();
        state.sync_dsc(wall as i64, check_disk);
        state.sync_hfdl(wall as i64, check_disk);
        state.sync_fax(wall as i64);
        state.sync_digital(wall as i64);
        // ── Engine events: decoded data, meters, frames, counters.
        let mut failed: Option<String> = None;
        if let Some(e) = &engine {
            // Each batch is dated by the file position it was decoded at
            // (at Max the file moves on far between two frames).
            let file = state.source.kind == SourceKind::File;
            for ev in e.poll() {
                match ev {
                    // a frame drawn while the DSP thread had stdout
                    // silenced never reached the terminal
                    EngineEvent::Repaint => {
                        let _ = terminal.clear();
                    }
                    EngineEvent::Data { pos, items } => {
                        if file {
                            state.source.file.position = pos;
                        }
                        state.receive_data(items, wall);
                    }
                    EngineEvent::Meter(m) => state.dsc_ui.meter = m,
                    EngineEvent::View(f) => {
                        let (zoom, audio) = (!f.zoom_db.is_empty(), !f.audio_db.is_empty());
                        state.observe_view(f);
                        // the Voice view's rows for a graphical front
                        use crate::remote::wire::{STREAM_AUDIO, STREAM_CHANNEL};
                        if zoom {
                            share.on_rows(STREAM_CHANNEL, &state.zoom_db);
                            front.on_rows(STREAM_CHANNEL, &state.zoom_db);
                        }
                        if audio && let Some(a) = state.audio_history.iter().next() {
                            share.on_rows(STREAM_AUDIO, a);
                            front.on_rows(STREAM_AUDIO, a);
                        }
                    }
                    // The latest spectrum frame for the display: the dB
                    // auto-range follows it, the waterfall keeps it.
                    EngineEvent::Spectrum(frame) => {
                        share.on_rows(crate::remote::wire::STREAM_MAIN, &frame);
                        front.on_rows(crate::remote::wire::STREAM_MAIN, &frame);
                        state.db_range.observe(&frame);
                        if state.waterfall_on {
                            state.waterfall.push(frame.clone());
                        }
                        state.spectrum = frame;
                    }
                    EngineEvent::Stats(st) => stats = st,
                    EngineEvent::Failed(msg) => failed = Some(msg),
                    EngineEvent::Notice(msg) => state.report_event(
                        crate::report::model::Level::Warning,
                        crate::report::model::Category::Source,
                        msg,
                        None,
                    ),
                }
            }
        } else {
            state.dsc_ui.meter = None;
        }
        if state.dsc_take_bell() | state.hfdl_take_bell() {
            front.bell();
        }

        // ── Reconcile the engine with the UI state ───────────────────────
        let want_running = state.run == RunState::Running;
        let is_running = engine.is_some();
        if let Some(msg) = failed {
            // The engine cannot go on (a lost connection): stop it.
            if let Some(e) = engine.take() {
                let data = e.stop();
                state.receive_data(data, now_f64());
            }
            log::warn!("engine failed: {msg}");
            state.report_count(crate::report::model::keys::SOURCE_FAILURES, 1);
            state.report_event(
                crate::report::model::Level::Error,
                crate::report::model::Category::Source,
                format!("Source failed: {msg}"),
                None,
            );
            state.source_failed(msg);
            state.run = RunState::Stopped;
        } else if want_running && !is_running {
            // Start the engine from the current source + radio settings.
            if let Some(cfg) = crate::engine_app::engine_config(state) {
                match crate::engine_app::start_engine(cfg) {
                    Ok(e) => {
                        push = EnginePush::started(state);
                        last_audio = state.radio.active().audio_device.clone();
                        last_stream_key = state.source.stream_key();
                        net_bitrate = state.source.network.bitrate;
                        state.source.file.pending_seek = None;
                        engine = Some(e);
                        stats = engine::EngineStats::default();
                        watch = (0, std::time::Instant::now());
                        let label = state
                            .source
                            .source_label()
                            .unwrap_or_else(|| "source".to_string());
                        state.report_event(
                            crate::report::model::Level::Info,
                            crate::report::model::Category::Source,
                            format!("Started {label}"),
                            None,
                        );
                        // Repair any residual corruption from native driver
                        // prints that may have leaked during device open, then
                        // force a full repaint on the next draw.
                        let _ = terminal.clear();
                    }
                    Err(e) => {
                        // Do not print to stderr here: we are on the alternate
                        // screen and it would corrupt the display. Surface the
                        // error through app state instead.
                        log::warn!("engine start failed: {e}");
                        state.report_count(crate::report::model::keys::SOURCE_FAILURES, 1);
                        state.report_event(
                            crate::report::model::Level::Error,
                            crate::report::model::Category::Source,
                            format!("Start failed: {e}"),
                            None,
                        );
                        state.source_failed(e);
                        state.run = RunState::Stopped;
                    }
                }
            } else {
                state.run = RunState::Stopped;
            }
        } else if !want_running && let Some(e) = engine.take() {
            let data = e.stop();
            state.receive_data(data, now_f64());
            state.report_event(
                crate::report::model::Level::Info,
                crate::report::model::Category::Source,
                "Stopped",
                None,
            );
        }
        // A Network source's compression changes without reconnecting
        // (0.4.3); a server too old for it reconnects (the stream key).
        if let Some(e) = &engine
            && state.source.kind == crate::source::model::SourceKind::Network
            && state.source.network.bitrate != net_bitrate
        {
            net_bitrate = state.source.network.bitrate;
            match e.set_net_audio(net_bitrate) {
                Ok(()) => last_stream_key = state.source.stream_key(),
                Err(m) => state.show_notice(format!("Compression: {m}; reconnecting")),
            }
        }
        state.source.network.link = engine.as_ref().and_then(|e| e.net_audio());
        if want_running
            && engine.is_some()
            && (state.radio.active().audio_device != last_audio
                || state.source.stream_key() != last_stream_key)
        {
            // The audio output device, the source device, or the sample rate
            // changed while running. Each needs a fresh stream (cpal or
            // SoapySDR), so restart the engine: stop now, and the next loop
            // iteration starts it again with the new settings (want_running
            // is still true). The start path refreshes `push`.
            if let Some(e) = engine.take() {
                let data = e.stop();
                state.receive_data(data, now_f64());
            }
            last_audio = state.radio.active().audio_device.clone();
        }

        // Push live changes to a running engine.
        if let Some(e) = &engine {
            // File source: hand a seek to the engine, then mirror its play
            // position; at the end of the file stop (and rewind).
            if state.source.kind == SourceKind::File {
                if let Some(frame) = state.source.file.pending_seek.take() {
                    e.command(Command::Seek(frame));
                    speed_meter.reset();
                } else if stats.file_eof {
                    // After [Max] the Digital worker may still be decoding
                    // the tail: stop (and end the recording) once it is done
                    // and what it found is in.
                    let busy = state.digital.as_ref().is_some_and(|d| d.backlog() > 0);
                    if !busy {
                        state.sync_digital(wall as i64);
                        state.file_finished();
                    }
                } else {
                    state.source.file.position = stats.file_pos;
                }
                let f = &mut state.source.file;
                if f.fast {
                    let rate = f.meta.as_ref().map_or(1.0, |m| f64::from(m.sample_rate));
                    if let Some(x) = speed_meter.observe(now_f64(), f.position, rate) {
                        f.speed = Some(x);
                    }
                } else {
                    speed_meter.reset();
                }
            }
            // [Max]: no pacing; reading waits for a decoder behind.
            let hold = state
                .digital
                .as_ref()
                .is_some_and(|d| d.backlog() > MAX_DIGITAL_BACKLOG);
            for cmd in push.commands(state, hold) {
                if matches!(cmd, Command::SetDecimation(_)) {
                    // The old frames have the old span: drop them.
                    state.waterfall.clear();
                }
                e.command(cmd);
            }
            state.dsp_overflows = stats.overflows;
            state.dsp_load = stats.load;
            // A device that stops delivering (a stuck USB stream) counts as
            // a failure: restart it (live sources only; a file may pause).
            if state.source.kind != SourceKind::File && stats.samples != watch.0 {
                watch = (stats.samples, std::time::Instant::now());
            }
            state.spectrum_span_hz = state
                .source
                .effective_sample_rate()
                .unwrap_or(state.spectrum_span_hz);
        }
        if state.source.kind != SourceKind::File
            && engine.is_some()
            && watch.1.elapsed() >= WATCHDOG
        {
            use crate::report::model::{Category, Level, keys};
            state.report_count(keys::WATCHDOG, 1);
            state.report_count(keys::SOURCE_FAILURES, 1);
            state.report_event(
                Level::Error,
                Category::Source,
                format!(
                    "No samples for {} s: restarting the source",
                    WATCHDOG.as_secs()
                ),
                None,
            );
            if let Some(e) = engine.take() {
                let data = e.stop();
                state.receive_data(data, now_f64());
            }
            // The engine starts again at the next turn (Running is kept).
            watch = (0, std::time::Instant::now());
        }
        reconcile_recording(state, engine.as_deref(), &mut rec_session);
        share.tick(state, engine.as_deref());
        if engine.is_none() && !state.spectrum.is_empty() {
            // Engine stopped: drop the stale frame so the display shows OFF/idle.
            state.spectrum.clear();
            state.waterfall.clear();
            state.clear_view();
        }
    }

    if let Some(e) = engine.take() {
        let data = e.stop();
        state.receive_data(data, now_f64());
    }
    // Every DSC record reaches its day file before the program ends
    // (seed NFR-03, LKP-05).
    if let Some(d) = state.dsc.as_mut() {
        d.shutdown();
    }
    if let Some(h) = state.hfdl.as_mut() {
        h.flush();
    }
    // The engine dropped its sinks: let the writers finish the files.
    finish_recording(state, &mut rec_session);
    // The last decoder output, then a clean quit in the report.
    state.report_tick();
    state.close_report();
    Ok(())
}

/// Seconds without a sample before the source counts as failed (spec 20.6).
const WATCHDOG: Duration = Duration::from_secs(10);

/// The wall clock, UTC seconds.
fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Close every output of the running recording and add it to the
/// session's recordings list (shown in the Voice view).
fn finish_recording(state: &mut AppState, session: &mut Vec<crate::record::recorder::RecHandle>) {
    use std::sync::atomic::Ordering;
    for h in session.drain(..) {
        let st = h.stats();
        let entry = crate::app::RecView {
            file: st
                .current()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            seconds: st.micros.load(Ordering::Relaxed) as f64 / 1e6,
            bytes: st.bytes.load(Ordering::Relaxed),
            dropped: st.dropped.load(Ordering::Relaxed),
        };
        if let Some(err) = h.finish() {
            state.rec_error = Some(err);
        }
        state.push_finished_recording(entry);
    }
}

/// Start or stop the recording to match the active VFO's [Record] switch,
/// and refresh the status the Radio module shows. A recording runs only while
/// the engine does; an engine restart (device or audio change) continues in
/// new files. Each checked output (native, baseband) has its own writer.
/// The real fax picture on a Kitty-protocol terminal: placed after the frame
/// when what it shows changed (a picture still growing at most every 2 s),
/// removed when the view stops asking for it.
#[derive(Default)]
struct KittyPlacement {
    shown: Option<crate::ui::fax_view::KittyRequest>,
    at: Option<std::time::Instant>,
}

impl KittyPlacement {
    fn show(&mut self, state: &AppState, want: Option<&crate::ui::fax_view::KittyRequest>) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let Some(req) = want.copied() else {
            if self.shown.take().is_some() {
                let _ = out.write_all(&crate::fax::kitty::remove());
                let _ = out.flush();
            }
            return;
        };
        let same_view = self.shown.is_some_and(|s| {
            (s.area, s.picture, s.ppm) == (req.area, req.picture, req.ppm)
                && (s.first, s.lines) == (req.first, req.lines)
        });
        let grown = self.shown.is_some_and(|s| s.available != req.available);
        let due = self
            .at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
        if same_view && (!grown || !due) {
            // ratatui redraws the cells under the picture only when they
            // change, and they stay blank: nothing to do
            return;
        }
        let Some(svc) = state.fax.as_ref() else {
            return;
        };
        let (cw, ch) = state.fax_ui.cell_px();
        let width = svc.config().width.max(1) as usize;
        let mut w = req.area.width as usize * cw as usize;
        let mut h = req.area.height as usize * ch as usize;
        if w > width {
            // never more pixels than the picture has
            h = h * width / w;
            w = width;
        }
        let Some(px) = svc.render(req.picture, req.first, req.lines, w, h.max(1)) else {
            return;
        };
        let Ok(png) = svc.lib().encode_png(&px, w, h.max(1)) else {
            return;
        };
        let a = req.area;
        let _ = out.write_all(&crate::fax::kitty::place(&png, a.x, a.y, a.width, a.height));
        let _ = out.flush();
        self.shown = Some(req);
        self.at = Some(std::time::Instant::now());
    }
}

fn reconcile_recording(
    state: &mut AppState,
    engine: Option<&dyn crate::engine::EngineHandle>,
    session: &mut Vec<crate::record::recorder::RecHandle>,
) {
    use crate::engine::{BasebandTap, RecTaps};
    use crate::record::RecKind;
    use crate::record::naming::{UtcDateTime, unique_path};
    use crate::record::recorder::{RecorderSpec, start};
    use std::sync::atomic::Ordering;

    let want = engine.is_some() && state.radio.active().recording;
    // A FAX VFO's native recording is its pictures, written by the fax
    // service (spec 15.4); only a baseband needs a WAV writer.
    let fax_native =
        state.fax_active() && state.radio.active().rec.kinds().contains(&RecKind::Native);
    let dir = std::path::PathBuf::from(crate::app::expand_home(state.active_rec_dir()));
    let was_fax = state.fax.as_ref().is_some_and(|f| f.recording());
    state.fax_record(want && fax_native, &dir);
    if was_fax
        && !state.fax.as_ref().is_some_and(|f| f.recording())
        && let Some(f) = state.fax.as_ref()
    {
        let r = f.rec_status().clone();
        state.push_finished_recording(crate::app::RecView {
            file: r.file,
            seconds: f64::from(r.lines) * 60.0 / f64::from(f.config().lpm.max(1)),
            bytes: r.bytes,
            dropped: 0,
        });
    }
    // Likewise the Digital VFO's Markdown (spec 16.7).
    let digital_native =
        state.digital_active() && state.radio.active().rec.kinds().contains(&RecKind::Native);
    if let Some(r) = state.digital_record(want && digital_native, &dir, now_f64()) {
        state.push_finished_recording(crate::app::RecView {
            file: r.file,
            seconds: now_f64() - r.started,
            bytes: r.bytes,
            dropped: 0,
        });
    }
    let native_elsewhere = fax_native || digital_native;
    let wav_wanted = want && (!native_elsewhere || state.radio.active().rec.kinds().len() > 1);
    if wav_wanted && session.is_empty() {
        let Some(e) = engine else { return };
        let plans = match state.recording_plans(UtcDateTime::now()) {
            Ok(p) => p,
            Err(err) => {
                state.radio.stop_recording();
                state.rec_error = Some(err);
                return;
            }
        };
        // Over the network the IQ stays on the server: no baseband.
        let had = plans.len();
        let plans: Vec<_> = plans
            .into_iter()
            .filter(|p| e.records_baseband() || p.kind != RecKind::Baseband)
            .collect();
        if plans.is_empty() && had > 0 {
            state.radio.stop_recording();
            state.rec_error = Some("No baseband recording over the network".to_string());
            return;
        }
        let mut taps = RecTaps::default();
        for plan in plans {
            let path = unique_path(&dir, &plan.name, |p| p.exists());
            match start(RecorderSpec::new(
                path,
                plan.channels,
                plan.sample_rate,
                plan.info,
            )) {
                Ok((sink, handle)) => {
                    match plan.kind {
                        RecKind::Native => taps.native = Some(sink),
                        RecKind::Baseband => {
                            taps.baseband = Some(BasebandTap {
                                sink,
                                minimize: plan.minimize,
                            })
                        }
                    }
                    session.push(handle);
                }
                Err(err) => {
                    // Close what was opened; nothing reaches the engine.
                    drop(taps);
                    finish_recording(state, session);
                    state.radio.stop_recording();
                    state.rec_error = Some(err.to_string());
                    log::warn!("recording failed to start: {err}");
                    return;
                }
            }
        }
        e.record(taps);
        state.rec_error = None;
    } else if !wav_wanted && !session.is_empty() {
        if let Some(e) = engine {
            e.record(RecTaps::default());
        }
        finish_recording(state, session);
    }
    let fax_rec = state
        .fax
        .as_ref()
        .filter(|f| f.recording())
        .map(|f| (f.rec_status().clone(), f.config().lpm.max(1)));
    let digital_rec = state.digital.as_ref().and_then(|d| d.rec_status().cloned());
    state.rec_view =
        (!session.is_empty() || fax_rec.is_some() || digital_rec.is_some()).then(|| {
            let mut v = crate::app::RecView::default();
            let mut names = Vec::new();
            for h in session.iter() {
                let st = h.stats();
                names.push(
                    st.current()
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                );
                v.seconds = v
                    .seconds
                    .max(st.micros.load(Ordering::Relaxed) as f64 / 1e6);
                v.bytes += st.bytes.load(Ordering::Relaxed);
                v.dropped += st.dropped.load(Ordering::Relaxed);
            }
            if let Some((r, lpm)) = &fax_rec {
                names.push(r.file.clone());
                v.seconds = v.seconds.max(f64::from(r.lines) * 60.0 / f64::from(*lpm));
                v.bytes += r.bytes;
            }
            if let Some(r) = &digital_rec {
                names.push(r.file.clone());
                v.seconds = v.seconds.max(now_f64() - r.started);
                v.bytes += r.bytes;
            }
            v.file = names.join(" + ");
            v
        });
    // A write error (disk full, folder removed) ends the recording.
    if let Some(err) = session.iter().find_map(|h| h.stats().error()) {
        state.radio.stop_recording();
        state.rec_error = Some(err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{RunState, TuneMode};

    #[test]
    fn mouse_capture_requests_clicks_and_drag_but_not_any_motion() {
        let mut s = String::new();
        crossterm::Command::write_ansi(&EnableClickMouseCapture, &mut s).unwrap();
        assert!(s.contains("?1000h"), "button press/release + wheel");
        assert!(s.contains("?1002h"), "drag while a button is held");
        assert!(s.contains("?1006h"), "SGR coordinates");
        assert!(!s.contains("1003"), "no any-motion flood");
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn source_up_down_navigates_fields_and_enter_opens_device_popup() {
        use crate::source::model::SourceField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.source.set_devices(vec![
            crate::source::DeviceInfo {
                label: "A".into(),
                args: "driver=mock,mode=ST".into(),
            },
            crate::source::DeviceInfo {
                label: "B".into(),
                args: "driver=mock,mode=MA".into(),
            },
        ]);
        s.nav.focus = Focus::Source;
        // Down moves Kind -> Device.
        apply_action(&mut s, Action::Down);
        assert_eq!(s.source.field(), SourceField::Device);
        // Enter opens the device popup; confirm selects B.
        apply_action(&mut s, Action::Enter);
        assert!(s.popup_open());
        apply_action(&mut s, Action::Down);
        apply_action(&mut s, Action::Enter);
        assert!(!s.popup_open());
        assert_eq!(s.source.device_idx, 1);
    }

    #[test]
    fn source_left_right_quick_cycles_sample_rate() {
        use crate::source::model::SourceField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        apply_action(&mut s, Action::Down); // Device
        apply_action(&mut s, Action::Down); // SampleRate
        assert_eq!(s.source.field(), SourceField::SampleRate);
        let before = s.source.soapy.as_ref().unwrap().sample_rate_idx;
        apply_action(&mut s, Action::Left);
        let after = s.source.soapy.as_ref().unwrap().sample_rate_idx;
        assert_ne!(after, before, "Left quick-cycles the sample rate");
    }

    #[test]
    fn source_kind_still_toggles_on_kind_field() {
        use crate::source::model::{SourceField, SourceKind};
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        assert_eq!(s.source.field(), SourceField::Kind);
        apply_action(&mut s, Action::Right);
        assert_eq!(s.source.kind, SourceKind::File);
        apply_action(&mut s, Action::Left);
        assert_eq!(s.source.kind, SourceKind::SoapySdr);
    }

    #[test]
    fn audio_field_left_right_cycles_and_enter_opens_popup() {
        use crate::ui::nav::Focus;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.radio
            .set_audio_options(vec![crate::engine::audio::OutputOption {
                label: "Speakers".into(),
                target: crate::engine::audio_target::AudioTarget::CpalDevice("Speakers".into()),
            }]);
        s.nav.focus = Focus::Radio;
        // Navigate to Audio (after Type, Mode, Bandwidth, Snap in NFM).
        for _ in 0..5 {
            apply_action(&mut s, Action::Down);
        }
        assert_eq!(s.radio.field(), RadioField::Audio);
        // Right cycles Default -> Speakers.
        apply_action(&mut s, Action::Right);
        assert_eq!(s.radio.active().audio_device.as_deref(), Some("Speakers"));
        // Enter opens the popup.
        apply_action(&mut s, Action::Enter);
        assert!(s.popup_open(), "Enter on Audio opens the picker");
    }

    #[test]
    fn db_range_keys_adjust_and_toggle_auto() {
        assert_eq!(
            translate_key(press(KeyCode::Char('a'))),
            Some(Action::ToggleDbAuto)
        );
        let mut s = AppState::default();
        assert!(s.db_range.auto);
        apply_action(&mut s, Action::ToggleDbAuto);
        assert!(!s.db_range.auto, "'a' toggles auto-range off");
        let max0 = s.db_range.max;
        apply_action(&mut s, Action::DbMaxUp);
        assert!(s.db_range.max > max0, "'.' raises the dB ceiling");
        let min0 = s.db_range.min;
        apply_action(&mut s, Action::DbMinDown);
        assert!(s.db_range.min < min0, "'<' lowers the dB floor");
    }

    /// A state with a "VOLMET" list of two bookmarks, Bookmarks focused.
    fn bookmarks_state() -> AppState {
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
        let mut list = BookmarkList::new("VOLMET");
        list.bookmarks = vec![
            Bookmark {
                name: "Bangkok".into(),
                frequency_hz: 6_676_000,
                ..Bookmark::default()
            },
            Bookmark {
                name: "Rostov".into(),
                frequency_hz: 8_939_000,
                settings: crate::bookmarks::model::voice_part(
                    crate::vfo::model::Mode::Usb,
                    None,
                    None,
                ),
                ..Bookmark::default()
            },
        ];
        let mut s = AppState::with_mock_source();
        s.bookmarks = crate::bookmarks::manager::BookmarkManager::new(BookmarkStore {
            lists: vec![list],
            ..BookmarkStore::default()
        });
        s.nav.focus = crate::ui::nav::Focus::Bookmarks;
        s
    }

    #[test]
    fn schedule_is_fully_keyboard_driven() {
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
        use crate::schedule::editor::SchedEditorField;
        let mut s = AppState::default();
        let mut l = BookmarkList::new("Ships");
        l.bookmarks.push(Bookmark {
            name: "NAVTEX".into(),
            frequency_hz: 518_000,
            ..Bookmark::default()
        });
        s.bookmarks.replace_store(BookmarkStore {
            lists: vec![l],
            ..BookmarkStore::default()
        });
        // TAB from Global reaches the Schedule module last.
        for _ in 0..4 {
            key(&mut s, KeyCode::Tab);
        }
        assert_eq!(s.nav.focus, crate::ui::nav::Focus::Schedule);
        // ENTER on the master switch turns the scheduler off and on.
        key(&mut s, KeyCode::Enter);
        assert!(!s.schedule.scheduler.enabled);
        key(&mut s, KeyCode::Enter);
        // Up to [Manager]: the page; F2 opens the editor + picker.
        key(&mut s, KeyCode::Up);
        key(&mut s, KeyCode::Enter);
        assert_eq!(s.nav.focus, crate::ui::nav::Focus::WorkArea);
        key(&mut s, KeyCode::F(2));
        assert!(s.sched_editor.is_some() && s.popup_open());
        key(&mut s, KeyCode::Enter); // pick NAVTEX
        // In the form, letters are text: "q" does not quit.
        key(&mut s, KeyCode::Down);
        assert_eq!(
            s.sched_editor.as_ref().unwrap().field(),
            SchedEditorField::Start
        );
        key(&mut s, KeyCode::Char('q'));
        assert!(s.running, "q typed into the form, not Quit");
        key(&mut s, KeyCode::Esc); // revert the edit
        for c in "0700".chars() {
            key(&mut s, KeyCode::Char(c));
        }
        key(&mut s, KeyCode::Enter);
        for c in "0730".chars() {
            key(&mut s, KeyCode::Char(c));
        }
        key(&mut s, KeyCode::Enter);
        // Down past Days, Repeat, Tune only, Record, Native, Baseband, Mute
        // to the buttons and Apply.
        for _ in 0..7 {
            key(&mut s, KeyCode::Down);
        }
        key(&mut s, KeyCode::Enter);
        assert!(s.sched_editor.is_none(), "applied");
        let j = &s.schedule.jobs()[0];
        assert_eq!((j.window.start, j.window.end), (700, 730));
        // Delete on the job row asks, Cancel is the default.
        key(&mut s, KeyCode::Delete);
        assert!(s.popup_open());
        key(&mut s, KeyCode::Enter);
        assert_eq!(s.schedule.jobs().len(), 1);
    }

    /// Feed a key through the same translation the event loop uses.
    fn key(s: &mut AppState, code: KeyCode) {
        let k = press(code);
        let a = if s.text_capture() {
            translate_text_key(k)
        } else {
            translate_key(k)
        };
        if let Some(a) = a {
            apply_action(s, a);
        }
    }

    #[test]
    fn slash_opens_find_and_typed_letters_are_text_not_accelerators() {
        let mut s = AppState::with_mock_source();
        s.nav.collapsed.bookmarks = true;
        key(&mut s, KeyCode::Char('/'));
        assert_eq!(s.nav.focus, crate::ui::nav::Focus::Bookmarks);
        assert!(!s.nav.collapsed.bookmarks, "module expanded");
        assert!(s.bookmarks.is_filter_editing());
        let theme = s.theme().name;
        for c in "qt c".chars() {
            key(&mut s, KeyCode::Char(c));
        }
        assert!(s.running, "'q' typed, not quit");
        assert_eq!(s.theme().name, theme, "'t' typed, theme unchanged");
        assert_eq!(s.bookmarks.filter.text(), "qt c");
        key(&mut s, KeyCode::Backspace);
        key(&mut s, KeyCode::Esc);
        assert!(!s.bookmarks.is_filter_editing());
        assert_eq!(s.bookmarks.filter.text(), "", "Esc clears the filter");
    }

    #[test]
    fn enter_on_a_bookmark_row_tunes_it() {
        let mut s = bookmarks_state();
        key(&mut s, KeyCode::Down); // Rostov
        key(&mut s, KeyCode::Enter);
        assert_eq!(s.radio.active().frequency_hz, 8_939_000);
        assert_eq!(s.radio.active().mode, crate::vfo::model::Mode::Usb);
    }

    #[test]
    fn left_right_on_rows_switch_lists_and_end_jumps_to_last() {
        let mut s = bookmarks_state();
        s.bookmarks.create_list("Empty").unwrap();
        s.bookmarks.select_list(0);
        key(&mut s, KeyCode::Right);
        assert_eq!(s.bookmarks.current_list().unwrap().name, "Empty");
        key(&mut s, KeyCode::Left);
        key(&mut s, KeyCode::End);
        assert_eq!(s.bookmarks.selected_bookmark().unwrap().name, "Rostov");
        key(&mut s, KeyCode::Home);
        assert_eq!(s.bookmarks.selected_bookmark().unwrap().name, "Bangkok");
    }

    #[test]
    fn delete_key_confirms_then_removes() {
        let mut s = bookmarks_state();
        key(&mut s, KeyCode::Delete);
        assert!(s.popup_open(), "confirmation shown");
        key(&mut s, KeyCode::Down);
        key(&mut s, KeyCode::Enter);
        assert_eq!(s.bookmarks.store.lists[0].bookmarks.len(), 1);
    }

    #[test]
    fn page_add_editor_typing_and_apply_by_keyboard() {
        use crate::bookmarks::manager::BookmarkField;
        let mut s = bookmarks_state();
        // Up from the first row: Find, List, [Manager].
        for _ in 0..3 {
            key(&mut s, KeyCode::Up);
        }
        assert_eq!(s.bookmarks.field(), BookmarkField::Manager);
        key(&mut s, KeyCode::Enter);
        assert_eq!(s.nav.focus, crate::ui::nav::Focus::WorkArea, "the page");
        key(&mut s, KeyCode::F(2));
        assert!(s.bm_editor.is_some(), "F2 Add opens the editor");
        // Name field: ENTER edits with the cursor at the end.
        key(&mut s, KeyCode::Enter);
        for _ in 0.."New Bookmark".len() {
            key(&mut s, KeyCode::Backspace);
        }
        for c in "Quito q".chars() {
            key(&mut s, KeyCode::Char(c));
        }
        assert!(s.running, "'q' in a name does not quit");
        key(&mut s, KeyCode::Enter); // commit name, move on
        while s.bm_editor.as_ref().unwrap().field()
            != crate::bookmarks::editor::EditorField::Buttons
        {
            key(&mut s, KeyCode::Down);
        }
        key(&mut s, KeyCode::Enter); // [Apply]
        assert!(s.bm_editor.is_none());
        assert_eq!(s.bookmarks.selected_bookmark().unwrap().name, "Quito q");
    }

    #[test]
    fn editor_esc_twice_cancels_without_saving() {
        let mut s = bookmarks_state();
        s.bookmark_request(crate::bookmarks::manager::Request::Edit(0));
        key(&mut s, KeyCode::Enter); // edit name
        key(&mut s, KeyCode::Char('X'));
        key(&mut s, KeyCode::Esc); // revert field
        key(&mut s, KeyCode::Esc); // close form
        assert!(s.bm_editor.is_none());
        assert_eq!(s.bookmarks.store.lists[0].bookmarks[0].name, "Bangkok");
        assert!(!s.bookmarks.is_dirty());
    }

    #[test]
    fn page_keys_page_the_rows_when_on_them() {
        use crate::bookmarks::model::Bookmark;
        let mut s = bookmarks_state();
        for i in 0..30 {
            s.bookmarks.store.lists[0].bookmarks.push(Bookmark {
                name: format!("X{i:02}"),
                frequency_hz: 10_000_000 + i,
                ..Bookmark::default()
            });
        }
        key(&mut s, KeyCode::PageDown);
        assert_eq!(
            s.bookmarks.cursor(),
            Some(crate::bookmarks::manager::ITEM_ROWS - 1)
        );
    }

    #[test]
    fn q_quits() {
        assert_eq!(translate_key(press(KeyCode::Char('q'))), Some(Action::Quit));
    }

    #[test]
    fn tab_and_backtab_map_to_nav() {
        assert_eq!(
            translate_key(press(KeyCode::Tab)),
            Some(Action::Nav(NavKey::Tab))
        );
        assert_eq!(
            translate_key(press(KeyCode::BackTab)),
            Some(Action::Nav(NavKey::BackTab))
        );
    }

    #[test]
    fn plus_minus_map_to_collapse_expand() {
        assert_eq!(
            translate_key(press(KeyCode::Char('+'))),
            Some(Action::Nav(NavKey::Expand))
        );
        assert_eq!(
            translate_key(press(KeyCode::Char('-'))),
            Some(Action::Nav(NavKey::Collapse))
        );
    }

    #[test]
    fn toggles_map_correctly() {
        assert_eq!(
            translate_key(press(KeyCode::Char(' '))),
            Some(Action::ToggleRun)
        );
        assert_eq!(
            translate_key(press(KeyCode::Char('c'))),
            Some(Action::ToggleTuneMode)
        );
        assert_eq!(
            translate_key(press(KeyCode::Char('['))),
            Some(Action::ToggleSpectrum)
        );
        assert_eq!(
            translate_key(press(KeyCode::Char(']'))),
            Some(Action::ToggleWaterfall)
        );
    }

    #[test]
    fn unmapped_letters_reach_the_work_area_and_f_keys_are_its_own() {
        // A letter ShellSDR does not bind is passed on (the DSC view uses
        // `d`, `f`, `b`); outside the view it does nothing.
        assert_eq!(
            translate_key(press(KeyCode::Char('x'))),
            Some(Action::Char('x'))
        );
        assert_eq!(translate_key(press(KeyCode::F(6))), Some(Action::FKey(6)));
        assert_eq!(translate_key(press(KeyCode::Insert)), None);
        let mut s = AppState::default();
        let before = s.nav;
        apply_action(&mut s, Action::Char('x'));
        apply_action(&mut s, Action::FKey(6));
        assert_eq!(s.nav, before);
        assert!(s.running);
    }

    #[test]
    fn apply_action_quit_stops_loop() {
        let mut s = AppState::default();
        apply_action(&mut s, Action::Quit);
        assert!(!s.running);
    }

    #[test]
    fn q_while_running_shows_dialog_instead_of_quitting() {
        let mut s = AppState {
            run: RunState::Running,
            ..AppState::default()
        };
        key(&mut s, KeyCode::Char('q'));
        assert!(s.running, "q does not quit while receiving");
        let p = s.popup.as_ref().expect("a dialog explains why");
        assert!(p.title.contains("reception"), "title {:?}", p.title);
        // A second q does not get through either; ENTER closes the dialog.
        key(&mut s, KeyCode::Char('q'));
        assert!(s.running);
        key(&mut s, KeyCode::Enter);
        assert!(s.popup.is_none());
        assert_eq!(
            s.run,
            RunState::Running,
            "the dialog does not stop anything"
        );
        // Once stopped, q quits.
        key(&mut s, KeyCode::Char(' '));
        key(&mut s, KeyCode::Char('q'));
        assert!(!s.running);
    }

    #[test]
    fn q_while_playing_a_file_names_playback() {
        let mut s = AppState::default();
        s.source.kind = crate::source::model::SourceKind::File;
        s.toggle_run();
        key(&mut s, KeyCode::Char('q'));
        assert!(s.running);
        let p = s.popup.as_ref().unwrap();
        assert!(p.title.contains("playback"), "title {:?}", p.title);
    }

    #[test]
    fn q_while_running_from_help_or_dialog_is_blocked_too() {
        let mut s = AppState {
            run: RunState::Running,
            ..AppState::default()
        };
        key(&mut s, KeyCode::Char('?'));
        key(&mut s, KeyCode::Char('q'));
        assert!(s.running, "not from the help popup");
        assert!(s.help.is_none(), "help closes so the dialog is visible");
        assert!(s.popup.is_some());
        key(&mut s, KeyCode::Esc);
        s.open_file_browser();
        key(&mut s, KeyCode::Char('q'));
        assert!(s.running, "not from the file dialog");
    }

    #[test]
    fn apply_action_toggles_state() {
        let mut s = AppState::default();
        apply_action(&mut s, Action::ToggleRun);
        assert_eq!(s.run, RunState::Running);
        apply_action(&mut s, Action::ToggleTuneMode);
        assert_eq!(s.tune_mode, TuneMode::Free);
    }

    #[test]
    fn left_right_toggles_source_kind_only_when_source_focused() {
        use crate::source::model::SourceKind;
        use crate::ui::nav::Focus;

        // Not focused on Source: Left/Right do nothing to the kind.
        let mut s = AppState::default();
        assert_eq!(s.nav.focus, Focus::Global);
        apply_action(&mut s, Action::Right);
        assert_eq!(s.source.kind, SourceKind::SoapySdr);

        // Focus Source: Left/Right toggles the kind.
        s.nav.focus = Focus::Source;
        apply_action(&mut s, Action::Right);
        assert_eq!(s.source.kind, SourceKind::File);
        apply_action(&mut s, Action::Left);
        assert_eq!(s.source.kind, SourceKind::SoapySdr);
    }

    #[test]
    fn arrows_map_to_actions() {
        assert_eq!(translate_key(press(KeyCode::Left)), Some(Action::Left));
        assert_eq!(translate_key(press(KeyCode::Right)), Some(Action::Right));
        assert_eq!(translate_key(press(KeyCode::Up)), Some(Action::Up));
        assert_eq!(translate_key(press(KeyCode::Down)), Some(Action::Down));
    }

    #[test]
    fn theme_cycle_key_and_action() {
        assert_eq!(
            translate_key(press(KeyCode::Char('t'))),
            Some(Action::CycleTheme)
        );
        let mut s = AppState::default();
        assert_eq!(s.theme().name, "Default");
        apply_action(&mut s, Action::CycleTheme);
        assert_eq!(s.theme().name, "Retro terminal");
    }

    #[test]
    fn radio_field_nav_then_edit_commit() {
        use crate::ui::nav::Focus;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        let start = s.radio.active().frequency_hz;

        // On Frequency, Enter to edit, Up to change digit (pending), Enter to commit.
        assert_eq!(s.radio.field(), RadioField::Frequency);
        apply_action(&mut s, Action::Enter);
        assert!(s.radio.is_editing());
        apply_action(&mut s, Action::Up);
        assert!(s.radio.freq().is_dirty());
        assert_eq!(s.radio.active().frequency_hz, start, "no live retune");
        apply_action(&mut s, Action::Enter);
        assert!(!s.radio.is_editing());
        assert_ne!(
            s.radio.active().frequency_hz,
            start,
            "committed on edit exit"
        );
    }

    #[test]
    fn radio_up_down_moves_field_when_not_editing() {
        use crate::ui::nav::Focus;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        apply_action(&mut s, Action::Down);
        assert_eq!(s.radio.field(), RadioField::Type);
        apply_action(&mut s, Action::Down);
        assert_eq!(s.radio.field(), RadioField::MODE);
        apply_action(&mut s, Action::Up);
        apply_action(&mut s, Action::Up);
        assert_eq!(s.radio.field(), RadioField::Frequency);
    }

    #[test]
    fn radio_mode_field_edit_cycles_demod() {
        use crate::ui::nav::Focus;
        use crate::vfo::model::Mode;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        apply_action(&mut s, Action::Down); // Type
        apply_action(&mut s, Action::Down); // to Mode
        assert_eq!(s.radio.field(), RadioField::MODE);
        // Left/Right still cycle the mode inline (quick change, no popup).
        apply_action(&mut s, Action::Right); // next mode
        assert_eq!(s.radio.active().mode, Mode::Wfm);
    }

    #[test]
    fn enter_on_mode_field_opens_popup_and_confirm_applies() {
        use crate::ui::nav::Focus;
        use crate::vfo::model::Mode;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        apply_action(&mut s, Action::Down); // Type
        apply_action(&mut s, Action::Down); // Mode
        apply_action(&mut s, Action::Enter); // open popup
        assert!(s.popup_open(), "Enter on Mode opens the popup");
        apply_action(&mut s, Action::Down); // WFM
        apply_action(&mut s, Action::Down); // AM
        apply_action(&mut s, Action::Enter); // confirm
        assert!(!s.popup_open(), "confirm closes the popup");
        assert_eq!(s.radio.active().mode, Mode::Am);
    }

    #[test]
    fn escape_closes_open_popup_without_applying() {
        use crate::ui::nav::Focus;
        use crate::vfo::model::Mode;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        apply_action(&mut s, Action::Down); // Type
        apply_action(&mut s, Action::Down); // Mode
        apply_action(&mut s, Action::Enter); // open
        apply_action(&mut s, Action::Down); // would be WFM
        apply_action(&mut s, Action::Escape);
        assert!(!s.popup_open(), "escape closes the popup");
        assert_eq!(s.radio.active().mode, Mode::Nfm, "nothing applied");
    }

    #[test]
    fn radio_escape_cancels_edit() {
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        apply_action(&mut s, Action::Enter);
        apply_action(&mut s, Action::Up);
        apply_action(&mut s, Action::Escape);
        assert!(!s.radio.is_editing());
        assert!(!s.radio.freq().is_dirty());
    }

    #[test]
    fn editing_actions_ignored_when_not_radio_focused() {
        let mut s = AppState::with_mock_source();
        let start = s.radio.active().frequency_hz;
        apply_action(&mut s, Action::Enter);
        apply_action(&mut s, Action::Up);
        apply_action(&mut s, Action::Enter);
        assert_eq!(s.radio.active().frequency_hz, start);
    }

    #[test]
    fn digit_keys_type_into_frequency_while_editing() {
        use crate::ui::nav::Focus;
        assert_eq!(
            translate_key(press(KeyCode::Char('4'))),
            Some(Action::TypeDigit(4))
        );
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        apply_action(&mut s, Action::Enter); // edit Frequency
        apply_action(&mut s, Action::TypeDigit(1));
        assert!(s.radio.is_editing());
    }

    #[test]
    fn page_keys_map_to_scroll_actions() {
        assert_eq!(translate_key(press(KeyCode::PageUp)), Some(Action::PageUp));
        assert_eq!(
            translate_key(press(KeyCode::PageDown)),
            Some(Action::PageDown)
        );
    }

    #[test]
    fn page_down_then_up_scrolls_panel() {
        let mut s = AppState::with_mock_source();
        // Simulate a short viewport where content overflows.
        s.reconcile_left_scroll(6);
        let before = s.left_scroll;
        apply_action(&mut s, Action::PageDown);
        assert!(s.left_scroll >= before, "page down does not scroll up");
        // With a tall viewport (no overflow), scroll stays at 0.
        s.left_scroll = 0;
        s.reconcile_left_scroll(100);
        apply_action(&mut s, Action::PageDown);
        assert_eq!(s.left_scroll, 0, "no scroll when content fits");
    }

    #[test]
    fn m_mutes_and_question_mark_toggles_help() {
        assert_eq!(
            translate_key(press(KeyCode::Char('m'))),
            Some(Action::ToggleMute)
        );
        assert_eq!(translate_key(press(KeyCode::Char('?'))), Some(Action::Help));
        let mut s = AppState::default();
        key(&mut s, KeyCode::Char('m'));
        assert!(s.muted);
        key(&mut s, KeyCode::Char('?'));
        assert!(s.help.is_some());
        key(&mut s, KeyCode::Char('?'));
        assert!(s.help.is_none(), "? closes it again");
    }

    #[test]
    fn ctrl_shift_arrows_resize_the_panel_from_anywhere() {
        use crossterm::event::{KeyEventKind, KeyEventState};
        let cs = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        assert_eq!(translate_key(cs(KeyCode::Right)), Some(Action::PanelWider));
        assert_eq!(
            translate_key(cs(KeyCode::Left)),
            Some(Action::PanelNarrower)
        );
        // Plain and Shift-only arrows keep their meaning.
        assert_eq!(translate_key(press(KeyCode::Right)), Some(Action::Right));
        let mut s = AppState::default();
        s.nav.focus = crate::ui::nav::Focus::Radio;
        let w = s.left_panel_width;
        apply_action(&mut s, Action::PanelWider);
        assert!(s.left_panel_width > w);
        assert_eq!(s.nav.focus, crate::ui::nav::Focus::Radio, "focus kept");
        apply_action(&mut s, Action::PanelNarrower);
        assert_eq!(s.left_panel_width, w);
    }

    #[test]
    fn help_popup_is_modal_and_scrolls() {
        let mut s = AppState::default();
        key(&mut s, KeyCode::Char('?'));
        s.help
            .as_mut()
            .unwrap()
            .reconcile(ratatui::layout::Rect::new(0, 0, 80, 20));
        key(&mut s, KeyCode::Down);
        key(&mut s, KeyCode::Down);
        assert_eq!(s.help.unwrap().scroll, 2, "arrows scroll the help");
        // Accelerators do not leak to the app behind it.
        let spectrum = s.spectrum_on;
        key(&mut s, KeyCode::Char('['));
        key(&mut s, KeyCode::Char('m'));
        key(&mut s, KeyCode::Tab);
        assert_eq!(s.spectrum_on, spectrum);
        assert!(!s.muted);
        assert_eq!(s.nav.focus, crate::ui::nav::Focus::Global);
        key(&mut s, KeyCode::Esc);
        assert!(s.help.is_none(), "Esc closes");
    }

    #[test]
    fn question_mark_is_text_inside_a_prompt() {
        let mut s = AppState::default();
        key(&mut s, KeyCode::Char('/'));
        assert!(s.text_capture());
        key(&mut s, KeyCode::Char('?'));
        assert!(s.help.is_none(), "typed into the find filter, not help");
    }

    #[test]
    fn w_switches_the_tab_and_l_the_layout_and_both_persist() {
        use crate::config::{WorkAreaLayout, WorkAreaTab};
        let mut s = AppState::with_mock_source();
        assert_eq!(
            translate_key(press(KeyCode::Char('w'))),
            Some(Action::SwitchTab)
        );
        assert_eq!(
            translate_key(press(KeyCode::Char('l'))),
            Some(Action::ToggleLayout)
        );
        apply_action(&mut s, Action::SwitchTab);
        assert_eq!(s.work_area.tab, WorkAreaTab::Vfo);
        apply_action(&mut s, Action::ToggleLayout);
        assert_eq!(s.work_area.layout, WorkAreaLayout::Split);
        let mut t = AppState::with_mock_source();
        t.apply_config(&s.to_config());
        assert_eq!(t.work_area, s.work_area);
        // A spectrum hidden behind the VFO tab costs no FFT.
        s.spectrum_visible = false;
        assert!(!s.fft_wanted());
        s.spectrum_visible = true;
        assert!(s.fft_wanted());
    }
}
