//! Mouse control (pure): hit map + click/wheel semantics.
//!
//! Step 3.13. Ratatui is immediate-mode, so the only reliable source of "what
//! is where" is the frame that was last drawn. `render_with_hits` records a
//! `HitMap` of (rect, target) pairs while drawing; the event loop hit-tests
//! mouse events against the map of the frame the user is looking at. This
//! avoids re-deriving the layout (and drifting from it) in the input code.
//!
//! Interaction rules (TUI/GUI practice, applied uniformly):
//! - Mouse is additive: every mouse action maps onto an existing keyboard
//!   action (focus a region, select a field, ENTER, arrows, popup confirm),
//!   so keyboard and mouse never diverge.
//! - Act on button press (not release), left button only.
//! - Click focuses the region and selects the field, then activates it the
//!   way ENTER would: toggles flip, list fields open their popup, editable
//!   fields enter edit mode. A click on a frequency digit edits that digit.
//! - Popups are modal: clicking an item selects it immediately; clicking
//!   outside dismisses (click-away = Esc); the wheel moves the highlight.
//! - Focus-out commits: clicking anywhere else ends an in-progress edit the
//!   same way ENTER does (spec 6.3: "applied on ENTER or focus-out").
//! - No scroll hijacking: the wheel changes a value only on the control that
//!   is already selected (clicked or keyboard-selected). Over anything else
//!   in the left panel the wheel scrolls the panel, so scrolling past a
//!   control never changes it by accident.
//! - Wheel on a frequency digit behaves like Up/Down on that digit (the edit
//!   stays pending until ENTER or focus-out, as for the keyboard).

use ratatui::layout::Rect;

use crate::app::AppState;
use crate::bookmarks::editor::EditorOutcome;
use crate::bookmarks::editor::{EditorButton, EditorField};
use crate::bookmarks::manager::{BookmarkField, ListButton, PageField, PanelButton, Request};
use crate::source::model::SourceField;
use crate::ui::global::GlobalField;
use crate::ui::nav::{Focus, NavKey};
use crate::vfo::radio::RadioField;

/// Rows the left panel scrolls per wheel notch.
pub const WHEEL_ROWS: i32 = 3;
/// Volume change per wheel notch on the selected volume control.
const VOLUME_WHEEL_STEP: f32 = 0.05;
/// Segments in the global volume bar.
pub const VOLUME_SEGMENTS: u8 = 8;

/// What a screen cell belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// A Global Controls bar control.
    Global(GlobalField),
    /// One digit of the center frequency (0 = ones).
    CenterDigit(usize),
    /// The VFO view's zoomed channel plot (fine click-to-tune).
    ZoomPlot {
        x: u16,
        cols: u16,
    },
    /// A tab of the Work Area's tab row.
    WorkTab(crate::config::WorkAreaTab),
    /// The DSC view (spec 14.6): anywhere in it, its alert banner, a menu
    /// title, an item of the open menu (title, item), the open menu's frame,
    /// a function key, the message list, a message row, the detail pane.
    DscView,
    DscBanner,
    DscMenuTitle(usize),
    DscMenuItem(usize, usize),
    DscMenuFrame,
    DscFKey(u8),
    DscList,
    DscRow(usize),
    DscDetail,
    /// The HFDL view (0.4.5): as DSC's, plus its tabs.
    HfdlView,
    HfdlBanner,
    HfdlMenuTitle(usize),
    HfdlMenuItem(usize, usize),
    HfdlMenuFrame,
    HfdlFKey(u8),
    HfdlTab(u8),
    HfdlList,
    HfdlRow(usize),
    HfdlDetail,
    /// The FAX view (spec 15.6): anywhere in it, a menu title, an item of
    /// the open menu, the open menu's frame, a function key.
    FaxView,
    FaxMenuTitle(usize),
    FaxMenuItem(usize, usize),
    FaxMenuFrame,
    FaxFKey(u8),
    /// The Digital view (spec 16.9): anywhere in it, a menu title, an item
    /// of the open menu, the open menu's frame, a function key, the
    /// waterfall or its scale (its first column and width: a click sets the
    /// carrier).
    DigitalView,
    DigitalMenuTitle(usize),
    DigitalMenuItem(usize, usize),
    DigitalMenuFrame,
    DigitalFKey(u8),
    DigitalWaterfall(u16, u16),
    /// One segment of the volume bar (0-based).
    VolumeSegment(u8),
    /// An arrow of the `Panel:` width stepper (true = `▸`, wider).
    PanelStep(bool),
    /// A left-panel module's header row.
    ModuleHeader(Focus),
    /// The `[+]`/`[-]` collapse glyph in a module header.
    ModuleCollapse(Focus),
    /// Any other cell inside a left-panel module.
    ModuleBody(Focus),
    /// A Source module field row.
    Source(SourceField),
    /// A File source transport/skip button.
    FileButton(crate::source::model::FileButton),
    /// File dialog: a listed entry (index into its entries), a button, or
    /// any other cell of the dialog.
    BrowserRow(usize),
    BrowserButton(crate::ui::file_browser::BrowserButton),
    BrowserFrame,
    /// A Radio module field row.
    Radio(RadioField),
    /// One digit of the active VFO frequency (0 = ones).
    VfoDigit(usize),
    /// A popup list item (index into the popup's items).
    PopupItem(usize),
    /// Any other cell inside the popup frame.
    PopupFrame,
    /// A Bookmarks module field row (List, Find).
    BmField(BookmarkField),
    /// A list button (New/Rename/Del/Hide) on the Bookmarks page.
    BmListButton(ListButton),
    /// `[Manager]` / `[Quick Add]` in the Bookmarks module.
    BmPanelButton(PanelButton),
    /// A field of the Bookmarks page (strip rows, sort, find).
    BmPageField(PageField),
    /// A list row on the Bookmarks page.
    BmPageList(usize),
    /// A table row on the Bookmarks page (position in the view).
    BmPageRow(usize),
    /// A key of a page's function-key bar.
    PageFKey(u8),
    /// `[Report]` in the Report module.
    ReportButton,
    /// A tab of the Report page; its period arrows (-1 older, +1 newer).
    ReportTab(crate::report_app::ReportTab),
    ReportStep(i32),
    /// A row of the Report page's Settings tab.
    ReportSetting(crate::report_app::ReportSetting),
    /// A bookmark row (position in the module's filtered, sorted view).
    BmRow(usize),
    /// A label on the bookmark strip: (list, bookmark index).
    StripLabel(usize, usize),
    /// The text prompt's input field / buttons / other cells.
    PromptField,
    PromptOk,
    PromptCancel,
    PromptFrame,
    /// A bookmark editor field row.
    EditorField(EditorField),
    /// One digit of the editor's frequency (0 = ones).
    EditorDigit(usize),
    /// One weekday toggle of the editor (0 = Sunday).
    EditorDay(usize),
    /// An editor button.
    EditorButton(EditorButton),
    /// Any other cell inside the editor.
    EditorFrame,
    /// A Schedule module field row ([Manager], On/Off).
    SchedField(crate::schedule::module::SchedField),
    /// A switch on the Schedule page's settings row.
    SchedSetting(crate::schedule::module::SchedSetting),
    /// A job row on the Schedule page (index into the jobs).
    SchedPageRow(usize),
    /// A list row / list button on the Schedule page.
    SchedPageList(usize),
    SchedListButton(crate::schedule::module::SchedListButton),
    /// A job row (position in the visible window).
    SchedRow(usize),
    /// A schedule editor field row.
    SchedEdField(crate::schedule::editor::SchedEditorField),
    /// One weekday toggle of the schedule editor (0 = Sunday).
    SchedEdDay(usize),
    /// A schedule editor button.
    SchedEdButton(EditorButton),
    /// Any other cell inside the schedule editor.
    SchedEdFrame,
    /// Any cell inside the help popup.
    HelpFrame,
    /// The spectrum or waterfall plot (click-to-tune): its first screen
    /// column and its column count (frequency maps as `spectrum::freq_to_col`).
    Plot {
        x: u16,
        cols: u16,
    },
}

/// The clickable regions of the last drawn frame. Later entries are on top.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HitMap {
    regions: Vec<(Rect, HitTarget)>,
    /// The on-screen left-panel viewport (wheel fallback scrolls it).
    pub left_panel: Rect,
    /// What a graphical viewer draws itself (0.4.1), recorded like the
    /// clickable regions.
    pub overlays: Vec<crate::remote::wire::Overlay>,
    /// The pixels of the picture overlays (0.4.8), one per slot.
    pub pictures: Vec<std::sync::Arc<crate::remote::picture::Picture>>,
}

impl HitMap {
    /// Record a picture over `rect` (a graphical front paints it).
    pub fn picture(
        &mut self,
        rect: Rect,
        fit: bool,
        p: std::sync::Arc<crate::remote::picture::Picture>,
    ) {
        if rect.is_empty() {
            return;
        }
        self.overlays.push(crate::remote::wire::Overlay::Picture {
            rect: [rect.x, rect.y, rect.width, rect.height],
            slot: p.slot,
            fit,
        });
        self.pictures.retain(|q| q.slot != p.slot);
        self.pictures.push(p);
    }

    /// Record `target` over `rect` (empty rects are ignored).
    pub fn push(&mut self, rect: Rect, target: HitTarget) {
        if rect.width > 0 && rect.height > 0 {
            self.regions.push((rect, target));
        }
    }

    /// The topmost target under the cell (`col`, `row`), if any.
    pub fn hit(&self, col: u16, row: u16) -> Option<HitTarget> {
        self.regions
            .iter()
            .rev()
            .find(|(r, _)| contains(*r, col, row))
            .map(|(_, t)| *t)
    }

    /// Whether (`col`, `row`) is inside the left-panel viewport.
    pub fn in_left_panel(&self, col: u16, row: u16) -> bool {
        contains(self.left_panel, col, row)
    }

    /// All recorded regions (bottom to top), for tests and diagnostics.
    pub fn regions(&self) -> &[(Rect, HitTarget)] {
        &self.regions
    }
}

fn contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// The mouse inputs ShellSDR reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    LeftDown,
    WheelUp,
    WheelDown,
}

/// A decoded mouse input at a terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
}

impl MouseInput {
    /// Decode a crossterm mouse event. Motion, drag, release and non-left
    /// buttons are ignored (`None`).
    pub fn from_crossterm(m: crossterm::event::MouseEvent) -> Option<Self> {
        use crossterm::event::{MouseButton, MouseEventKind};
        let kind = match m.kind {
            MouseEventKind::Down(MouseButton::Left) => MouseKind::LeftDown,
            MouseEventKind::ScrollUp => MouseKind::WheelUp,
            MouseEventKind::ScrollDown => MouseKind::WheelDown,
            _ => return None,
        };
        Some(Self {
            kind,
            col: m.column,
            row: m.row,
        })
    }
}

/// Apply a mouse input to the state using the hit map of the drawn frame.
pub fn apply_mouse(state: &mut AppState, hits: &HitMap, input: MouseInput) {
    let target = hits.hit(input.col, input.row);
    match input.kind {
        MouseKind::LeftDown => click(state, target, input.col),
        MouseKind::WheelUp => wheel(state, hits, input, target, 1),
        MouseKind::WheelDown => wheel(state, hits, input, target, -1),
    }
}

/// Left click on `target` at screen column `col`.
fn click(state: &mut AppState, target: Option<HitTarget>, col: u16) {
    // Help popup: topmost and modal; a click outside closes it.
    if state.help.is_some() {
        if target != Some(HitTarget::HelpFrame) {
            state.help = None;
        }
        return;
    }
    // Modal popup: an item confirms immediately, its frame is inert, anything
    // outside dismisses it.
    if state.popup_open() {
        match target {
            Some(HitTarget::PopupItem(i)) => {
                if let Some(p) = &mut state.popup
                    && i < p.items.len()
                {
                    p.cursor = i;
                }
                state.popup_confirm();
            }
            Some(HitTarget::PopupFrame) => {}
            _ => state.popup_cancel(),
        }
        return;
    }

    // Text prompt: modal; a click outside cancels it (like a popup).
    if state.prompt.is_some() {
        match target {
            Some(HitTarget::PromptOk) => state.prompt_confirm(),
            Some(HitTarget::PromptCancel) => state.prompt_cancel(),
            Some(HitTarget::PromptField | HitTarget::PromptFrame) => {}
            _ => state.prompt_cancel(),
        }
        return;
    }
    // File dialog: modal; rows select (a second click opens), buttons press,
    // a click outside closes it (like a popup).
    if let Some(b) = &mut state.file_browser {
        let outcome = match target {
            Some(HitTarget::BrowserRow(i)) => b.click_row(i),
            Some(HitTarget::BrowserButton(btn)) => {
                b.focus = crate::ui::file_browser::BrowserFocus::Buttons;
                b.press(btn)
            }
            Some(HitTarget::BrowserFrame) => crate::ui::file_browser::Outcome::Stay,
            _ => crate::ui::file_browser::Outcome::Cancel,
        };
        state.file_browser_outcome(outcome);
        return;
    }
    // Bookmark editor: modal; clicks outside are ignored so a stray click
    // never throws away a half-filled form.
    if state.bm_editor.is_some() {
        click_editor(state, target);
        return;
    }
    // Schedule editor: modal the same way.
    if state.sched_editor.is_some() {
        click_sched_editor(state, target);
        return;
    }

    // The views' open menus: an item runs, a title switches menus, a click
    // elsewhere closes it.
    if state.digital_ui.menu.is_some() {
        match target {
            Some(HitTarget::DigitalMenuItem(top, i)) => {
                if let Some(it) = crate::digital_app::MENUS.get(top).and_then(|m| m.1.get(i)) {
                    state.digital_command(it.cmd);
                }
            }
            Some(HitTarget::DigitalMenuTitle(i)) => state.digital_ui.menu = Some((i, 0)),
            Some(HitTarget::DigitalMenuFrame) => {}
            _ => state.digital_ui.menu = None,
        }
        return;
    }
    if state.fax_ui.menu.is_some() {
        match target {
            Some(HitTarget::FaxMenuItem(top, i)) => {
                if let Some(it) = crate::fax_app::MENUS.get(top).and_then(|m| m.1.get(i)) {
                    state.fax_command(it.cmd);
                }
            }
            Some(HitTarget::FaxMenuTitle(i)) => state.fax_ui.menu = Some((i, 0)),
            Some(HitTarget::FaxMenuFrame) => {}
            _ => state.fax_ui.menu = None,
        }
        return;
    }
    if state.hfdl_ui.menu.is_some() {
        match target {
            Some(HitTarget::HfdlMenuItem(top, i)) => {
                if let Some(it) = crate::hfdl_app::MENUS.get(top).and_then(|m| m.1.get(i)) {
                    state.hfdl_command(it.cmd);
                }
            }
            Some(HitTarget::HfdlMenuTitle(i)) => state.hfdl_ui.menu = Some((i, 0)),
            Some(HitTarget::HfdlMenuFrame) => {}
            _ => state.hfdl_ui.menu = None,
        }
        return;
    }
    if state.dsc_ui.menu.is_some() {
        match target {
            Some(HitTarget::DscMenuItem(top, i)) => {
                if let Some(it) = crate::dsc_app::MENUS.get(top).and_then(|m| m.1.get(i)) {
                    state.dsc_command(it.cmd);
                }
            }
            Some(HitTarget::DscMenuTitle(i)) => state.dsc_ui.menu = Some((i, 0)),
            Some(HitTarget::DscMenuFrame) => {}
            _ => state.dsc_ui.menu = None,
        }
        return;
    }
    if let Some(t) = target
        && click_dsc(state, t, col)
    {
        return;
    }

    // Click-to-tune: read the frequency off the frame the user saw, before a
    // focus-out commit can move the center.
    if let Some(HitTarget::ZoomPlot { x, cols }) = target {
        let off = crate::ui::spectrum::col_to_offset(
            usize::from(col.saturating_sub(x)),
            state.zoom_span_hz,
            usize::from(cols),
        );
        let center = state.radio.active().frequency_hz as f64
            + state.view.as_ref().map_or(0.0, |v| v.center_offset);
        blur_edits(state, target);
        state.zoom_tune(center + off);
        return;
    }
    if let Some(HitTarget::Plot { x, cols }) = target {
        let off = crate::ui::spectrum::col_to_offset(
            usize::from(col.saturating_sub(x)),
            state.display_span_hz(),
            usize::from(cols),
        );
        let hz = state.center_hz() as f64 + off;
        blur_edits(state, target);
        state.click_tune(hz);
        return;
    }

    blur_edits(state, target);
    let Some(t) = target else { return };
    match t {
        HitTarget::Global(f) => {
            focus_global(state, f);
            match f {
                GlobalField::Volume | GlobalField::Panel => {}
                GlobalField::Frequency => state.global_editing = true,
                _ => state.global_enter(),
            }
        }
        HitTarget::CenterDigit(pos) => {
            focus_global(state, GlobalField::Frequency);
            state.global_editing = true;
            state.center_freq.set_cursor(pos);
        }
        HitTarget::VolumeSegment(i) => {
            focus_global(state, GlobalField::Volume);
            state.volume = (f32::from(i) + 1.0) / f32::from(VOLUME_SEGMENTS);
        }
        HitTarget::PanelStep(wider) => {
            focus_global(state, GlobalField::Panel);
            state.resize_left_panel(wider);
        }
        HitTarget::WorkTab(tab) => state.select_work_tab(tab),
        HitTarget::ModuleHeader(f) | HitTarget::ModuleBody(f) => state.nav.focus = f,
        HitTarget::ModuleCollapse(f) => {
            state.nav.focus = f;
            let collapsed = match f {
                Focus::Source => state.nav.collapsed.source,
                Focus::Radio => state.nav.collapsed.radio,
                Focus::Bookmarks => state.nav.collapsed.bookmarks,
                Focus::Schedule => state.nav.collapsed.schedule,
                Focus::Report => state.nav.collapsed.report,
                Focus::Global | Focus::WorkArea => return,
            };
            state.on_nav(if collapsed {
                NavKey::Expand
            } else {
                NavKey::Collapse
            });
        }
        HitTarget::Source(f) => {
            state.nav.focus = Focus::Source;
            state.source.select_field(f);
            match f {
                SourceField::Kind => state.toggle_source_kind(),
                SourceField::Agc => state.source_adjust(1),
                SourceField::Device
                | SourceField::SampleRate
                | SourceField::Decimation
                | SourceField::Bandwidth
                | SourceField::Antenna => state.open_popup_for_focus(),
                // Gains are adjusted with the wheel or Left/Right once selected.
                SourceField::Gain(_) => {}
                SourceField::FilePath => state.open_file_browser(),
                SourceField::NetServer | SourceField::NetToken => state.activate_network_field(),
                SourceField::NetAudio => state.open_popup_for_focus(),
                SourceField::NetOpen => state.source.network.open_request = true,
                SourceField::ShareOn
                | SourceField::ShareListen
                | SourceField::ShareToken
                | SourceField::ShareMute
                | SourceField::ShareDisconnect => state.activate_share_field(),
                // Button rows: the buttons have their own targets.
                SourceField::FileTransport | SourceField::FileBack | SourceField::FileFwd => {}
            }
        }
        HitTarget::FileButton(b) => {
            state.nav.focus = Focus::Source;
            state
                .source
                .select_field(crate::source::model::FileView::button_field(b));
            state.source.file.select_button(b);
            state.press_file_button(b);
        }
        HitTarget::Radio(f) => {
            state.nav.focus = Focus::Radio;
            state.radio.select_field(f);
            match f {
                RadioField::Audio => state.open_popup_for_focus(),
                _ if state.radio.is_choice(f) => state.open_popup_for_focus(),
                _ if state.radio.is_toggle(f) => state.radio.edit_toggle(f),
                _ if state.activate_recorder_field(f) => {}
                _ => state.radio.begin_edit(),
            }
        }
        HitTarget::VfoDigit(pos) => {
            state.nav.focus = Focus::Radio;
            state.radio.select_field(RadioField::Frequency);
            state.radio.begin_edit();
            state.radio.freq_mut().set_cursor(pos);
        }
        HitTarget::BmField(f) => {
            let was = state.nav.focus == Focus::Bookmarks && state.bookmarks.field() == f;
            state.nav.focus = Focus::Bookmarks;
            state.bookmarks.select_field(f);
            match f {
                BookmarkField::List => state.bookmark_request(Request::PickList),
                BookmarkField::Manager => state.bookmark_request(Request::OpenPage),
                BookmarkField::Find => state.bookmarks.begin_find(),
                BookmarkField::Items => {}
            }
            let _ = was;
        }
        HitTarget::BmPanelButton(b) => {
            state.nav.focus = Focus::Bookmarks;
            let req = state.bookmarks.press_panel_button(b);
            state.bookmark_request(req);
        }
        HitTarget::BmListButton(b) => {
            state.nav.focus = Focus::WorkArea;
            let req = state.bookmarks.press_list_button(b);
            state.bookmark_request(req);
        }
        HitTarget::BmPageField(f) => {
            let was = state.nav.focus == Focus::WorkArea && state.bookmarks.page_field() == f;
            state.nav.focus = Focus::WorkArea;
            state.bookmarks.select_page_field(f);
            match f {
                PageField::Sort => state.bookmarks.toggle_sort(),
                PageField::Find => state.bookmarks.begin_find(),
                // A stepper: the wheel (or Left/Right) changes it once
                // selected; a click on the selected one steps it like ENTER.
                PageField::StripRows if was => {
                    state.bookmarks.page_enter();
                }
                _ => {}
            }
        }
        HitTarget::BmPageList(i) => {
            state.nav.focus = Focus::WorkArea;
            state.bookmarks.click_page_list(i);
        }
        HitTarget::BmPageRow(pos) => {
            // First click selects; a click on the selected row tunes.
            let again = state.nav.focus == Focus::WorkArea
                && state.bookmarks.page_field() == PageField::Table
                && state.bookmarks.cursor() == Some(pos);
            state.nav.focus = Focus::WorkArea;
            if again {
                let req = state.bookmarks.tune_request();
                state.bookmark_request(req);
            } else {
                state.bookmarks.click_page_row(pos);
            }
        }
        HitTarget::ReportButton => {
            state.nav.focus = Focus::Report;
            state.open_page(crate::ui::page::WorkOwner::Report);
        }
        HitTarget::ReportTab(t) => {
            state.nav.focus = Focus::WorkArea;
            state.report_select_tab(t);
        }
        HitTarget::ReportSetting(row) => {
            state.nav.focus = Focus::WorkArea;
            let i = crate::report_app::ReportSetting::ALL
                .iter()
                .position(|r| *r == row)
                .unwrap_or(0);
            state.report_ui.setting = i;
            state.report_setting_enter(row);
        }
        HitTarget::ReportStep(d) => {
            state.nav.focus = Focus::WorkArea;
            state.report_step(d);
        }
        HitTarget::PageFKey(n) => {
            state.nav.focus = Focus::WorkArea;
            state.page_fkey(n);
        }
        HitTarget::BmRow(pos) => {
            // First click selects; a click on the already-selected row tunes
            // (SDR++'s double-click without a timing window).
            let again = state.nav.focus == Focus::Bookmarks
                && state.bookmarks.field() == BookmarkField::Items
                && state.bookmarks.cursor() == Some(pos);
            state.nav.focus = Focus::Bookmarks;
            if again {
                let req = state.bookmarks.tune_request();
                state.bookmark_request(req);
            } else {
                state.bookmarks.click_row(pos);
            }
        }
        // A strip label tunes at once and selects it in the module (SDR++).
        HitTarget::StripLabel(l, i) => state.tune_bookmark(l, i),
        HitTarget::SchedField(f) => {
            state.nav.focus = Focus::Schedule;
            state.schedule.select_field(f);
            // Every field row is a switch: a click flips it like ENTER.
            let req = state.schedule.enter();
            state.schedule_request(req);
        }
        HitTarget::SchedSetting(x) => {
            // A switch: a click flips it like ENTER.
            state.nav.focus = Focus::WorkArea;
            state.schedule.select_setting(x);
            state.schedule.flip(x);
        }
        HitTarget::SchedPageList(i) => {
            state.nav.focus = Focus::WorkArea;
            state.schedule.click_page_list(i);
        }
        HitTarget::SchedListButton(b) => {
            state.nav.focus = Focus::WorkArea;
            let req = state.schedule.press_list_button(b);
            state.schedule_request(req);
        }
        HitTarget::SchedPageRow(idx) => {
            // First click selects; a click on the selected row edits it.
            use crate::schedule::module::PageField;
            let again = state.nav.focus == Focus::WorkArea
                && state.schedule.page_field() == PageField::Table
                && state.schedule.cursor() == Some(idx);
            state.nav.focus = Focus::WorkArea;
            state.schedule.click_page_row(idx);
            if again {
                let req = state.schedule.edit_request();
                state.schedule_request(req);
            }
        }
        HitTarget::SchedRow(pos) => {
            // First click selects; a click on the selected row edits it.
            use crate::schedule::module::SchedField;
            let idx = state.schedule.scroll() + pos;
            let again = state.nav.focus == Focus::Schedule
                && state.schedule.field() == SchedField::Items
                && state.schedule.cursor() == Some(idx);
            state.nav.focus = Focus::Schedule;
            state.schedule.click_row(pos);
            if again {
                let req = state.schedule.enter();
                state.schedule_request(req);
            }
        }
        // Only reachable with a popup/prompt/editor open (handled above).
        HitTarget::BrowserRow(_) | HitTarget::BrowserButton(_) | HitTarget::BrowserFrame => {}
        HitTarget::PopupItem(_)
        | HitTarget::PopupFrame
        | HitTarget::PromptField
        | HitTarget::PromptOk
        | HitTarget::PromptCancel
        | HitTarget::PromptFrame
        | HitTarget::EditorField(_)
        | HitTarget::EditorDigit(_)
        | HitTarget::EditorDay(_)
        | HitTarget::EditorButton(_)
        | HitTarget::EditorFrame
        | HitTarget::SchedEdField(_)
        | HitTarget::SchedEdDay(_)
        | HitTarget::SchedEdButton(_)
        | HitTarget::SchedEdFrame
        | HitTarget::HelpFrame => {}
        // Handled before the focus-out above.
        HitTarget::Plot { .. } | HitTarget::ZoomPlot { .. } => {}
        // Handled by `click_dsc` before.
        HitTarget::DscView
        | HitTarget::DscBanner
        | HitTarget::DscMenuTitle(_)
        | HitTarget::DscMenuItem(..)
        | HitTarget::DscMenuFrame
        | HitTarget::DscFKey(_)
        | HitTarget::DscList
        | HitTarget::DscRow(_)
        | HitTarget::DscDetail
        | HitTarget::HfdlView
        | HitTarget::HfdlBanner
        | HitTarget::HfdlMenuTitle(_)
        | HitTarget::HfdlMenuItem(..)
        | HitTarget::HfdlMenuFrame
        | HitTarget::HfdlFKey(_)
        | HitTarget::HfdlTab(_)
        | HitTarget::HfdlList
        | HitTarget::HfdlRow(_)
        | HitTarget::HfdlDetail
        | HitTarget::FaxView
        | HitTarget::FaxMenuTitle(_)
        | HitTarget::FaxMenuItem(..)
        | HitTarget::FaxMenuFrame
        | HitTarget::FaxFKey(_)
        | HitTarget::DigitalView
        | HitTarget::DigitalMenuTitle(_)
        | HitTarget::DigitalMenuItem(..)
        | HitTarget::DigitalMenuFrame
        | HitTarget::DigitalFKey(_)
        | HitTarget::DigitalWaterfall(..) => {}
    }
}

/// A click while the schedule editor is open: select the field and activate
/// it like ENTER (the bookmark opens its picker, times start editing,
/// Repeat/Record flip, a weekday toggles, a button presses).
fn click_sched_editor(state: &mut AppState, target: Option<HitTarget>) {
    use crate::schedule::editor::{SchedEditorField, SchedEditorOutcome};
    let Some(ed) = &mut state.sched_editor else {
        return;
    };
    let outcome = match target {
        Some(HitTarget::SchedEdField(f)) => {
            // A disabled field ("Tune only": End, Mute) ignores clicks.
            if (ed.is_editing() && ed.field() == f) || !ed.is_enabled(f) {
                return;
            }
            ed.select_field(f);
            match f {
                SchedEditorField::Days => SchedEditorOutcome::Open,
                _ => ed.enter(),
            }
        }
        Some(HitTarget::SchedEdDay(i)) => {
            ed.toggle_day(i);
            SchedEditorOutcome::Open
        }
        Some(HitTarget::SchedEdButton(b)) => ed.press(b),
        _ => SchedEditorOutcome::Open,
    };
    state.sched_editor_outcome(outcome);
}

/// A click while the bookmark editor is open: select the field and activate
/// it like ENTER (text fields start editing, List/Mode step to the next
/// value, a weekday toggles, a button presses).
fn click_editor(state: &mut AppState, target: Option<HitTarget>) {
    let Some(ed) = &mut state.bm_editor else {
        return;
    };
    let outcome = match target {
        Some(HitTarget::EditorField(f)) => {
            if ed.is_editing() && ed.field() == f {
                return;
            }
            ed.select_field(f);
            match f {
                EditorField::Days => EditorOutcome::Open,
                EditorField::Frequency => {
                    let pos = ed.freq.cursor();
                    ed.edit_digit(pos);
                    EditorOutcome::Open
                }
                _ => ed.enter(),
            }
        }
        Some(HitTarget::EditorDigit(pos)) => {
            ed.edit_digit(pos);
            EditorOutcome::Open
        }
        Some(HitTarget::EditorDay(i)) => {
            ed.toggle_day(i);
            EditorOutcome::Open
        }
        Some(HitTarget::EditorButton(b)) => ed.press(b),
        _ => EditorOutcome::Open,
    };
    state.editor_outcome(outcome);
}

/// Focus the Global bar on control `f`.
fn focus_global(state: &mut AppState, f: GlobalField) {
    state.nav.focus = Focus::Global;
    state.global_field = f;
}

/// Focus-out: end any edit the click is not continuing, committing it the
/// way ENTER does.
fn blur_edits(state: &mut AppState, target: Option<HitTarget>) {
    let keeps_center = matches!(
        target,
        Some(HitTarget::CenterDigit(_)) | Some(HitTarget::Global(GlobalField::Frequency))
    );
    if state.bookmarks.is_filter_editing()
        && target != Some(HitTarget::BmField(BookmarkField::Find))
    {
        state.bookmarks.end_find(true);
    }
    if state.global_editing && !keeps_center {
        // global_enter on the editing Frequency field commits and exits.
        state.global_field = GlobalField::Frequency;
        state.global_enter();
    }
    if state.radio.is_editing() {
        let cur = state.radio.field();
        let keeps_radio = match target {
            Some(HitTarget::VfoDigit(_)) => cur == RadioField::Frequency,
            Some(HitTarget::Radio(f)) => f == cur,
            _ => false,
        };
        if !keeps_radio {
            let (lo, hi) = state.freq_band();
            state.radio.toggle_edit(lo, hi);
        }
    }
}

/// A click in the DSC view. Returns whether `t` was one of its targets.
fn click_dsc(state: &mut AppState, t: HitTarget, col: u16) -> bool {
    use crate::dsc_app::DscPart;
    match t {
        HitTarget::DscFKey(k) => state.dsc_fkey(k),
        HitTarget::DscMenuTitle(i) => state.dsc_ui.menu = Some((i, 0)),
        HitTarget::DscRow(r) => state.dsc_click_row(r),
        HitTarget::DscBanner => {
            if let Some(s) = state.dsc.as_mut() {
                s.acknowledge_alert();
            }
        }
        HitTarget::DscDetail => {
            state.nav.focus = Focus::WorkArea;
            state.dsc_ui.part = DscPart::Detail;
        }
        HitTarget::DscList | HitTarget::DscView => {
            state.nav.focus = Focus::WorkArea;
            state.dsc_ui.part = DscPart::Messages;
        }
        HitTarget::DscMenuItem(..) | HitTarget::DscMenuFrame => {}
        HitTarget::HfdlFKey(k) => state.hfdl_fkey(k),
        HitTarget::HfdlMenuTitle(i) => state.hfdl_ui.menu = Some((i, 0)),
        HitTarget::HfdlRow(r) => state.hfdl_click_row(r),
        HitTarget::HfdlTab(i) => {
            state.nav.focus = Focus::WorkArea;
            if let Some(s) = state.hfdl.as_mut() {
                s.tab = match i {
                    0 => crate::hfdl::service::Tab::Messages,
                    1 => crate::hfdl::service::Tab::Aircraft,
                    _ => crate::hfdl::service::Tab::Stations,
                };
            }
        }
        HitTarget::HfdlBanner => {
            if let Some(s) = state.hfdl.as_mut() {
                s.acknowledge_alert();
            }
        }
        HitTarget::HfdlDetail => {
            state.nav.focus = Focus::WorkArea;
            state.hfdl_ui.part = crate::hfdl_app::HfdlPart::Detail;
        }
        HitTarget::HfdlList | HitTarget::HfdlView => {
            state.nav.focus = Focus::WorkArea;
            state.hfdl_ui.part = crate::hfdl_app::HfdlPart::List;
        }
        HitTarget::HfdlMenuItem(..) | HitTarget::HfdlMenuFrame => {}
        HitTarget::FaxFKey(k) => state.fax_fkey(k),
        HitTarget::FaxMenuTitle(i) => state.fax_ui.menu = Some((i, 0)),
        HitTarget::FaxView => state.nav.focus = Focus::WorkArea,
        HitTarget::FaxMenuItem(..) | HitTarget::FaxMenuFrame => {}
        HitTarget::DigitalFKey(k) => state.digital_fkey(k),
        HitTarget::DigitalMenuTitle(i) => state.digital_ui.menu = Some((i, 0)),
        HitTarget::DigitalView => state.nav.focus = Focus::WorkArea,
        HitTarget::DigitalWaterfall(x0, w) => {
            state.nav.focus = Focus::WorkArea;
            state.digital_click_carrier(crate::ui::digital_view::column_hz(
                col.saturating_sub(x0),
                w,
            ));
        }
        HitTarget::DigitalMenuItem(..) | HitTarget::DigitalMenuFrame => {}
        _ => return false,
    }
    true
}

/// Wheel notch (`dir` = +1 up / -1 down) over `target`.
fn wheel(
    state: &mut AppState,
    hits: &HitMap,
    input: MouseInput,
    target: Option<HitTarget>,
    dir: i32,
) {
    if let Some(h) = &mut state.help {
        h.scroll_by(-dir * WHEEL_ROWS);
        return;
    }
    if state.popup_open() {
        if dir > 0 {
            state.popup_up();
        } else {
            state.popup_down();
        }
        return;
    }
    if state.prompt.is_some() {
        return;
    }
    if let Some(b) = &mut state.file_browser {
        b.move_by(-(dir as i64) * WHEEL_ROWS as i64);
        return;
    }
    if let Some(ed) = &mut state.bm_editor {
        wheel_editor(ed, target, dir);
        return;
    }
    if state.sched_editor.is_some() {
        return;
    }
    // The DSC view: the wheel moves through the messages, or scrolls the
    // detail under it.
    match target {
        Some(HitTarget::DscList | HitTarget::DscRow(_)) => {
            if let Some(s) = state.dsc.as_mut() {
                s.view_mut().move_by(-dir as isize);
            }
            return;
        }
        Some(HitTarget::HfdlList | HitTarget::HfdlRow(_)) => {
            let up = if dir > 0 {
                crate::ui::run::Action::Up
            } else {
                crate::ui::run::Action::Down
            };
            state.hfdl_ui.part = crate::hfdl_app::HfdlPart::List;
            state.hfdl_key(up);
            return;
        }
        Some(HitTarget::HfdlDetail) => {
            let d = &mut state.hfdl_ui.detail_scroll;
            *d = if dir > 0 {
                d.saturating_sub(WHEEL_ROWS as usize)
            } else {
                *d + WHEEL_ROWS as usize
            };
            return;
        }
        Some(HitTarget::DscDetail) => {
            let d = &mut state.dsc_ui.detail_scroll;
            *d = if dir > 0 {
                d.saturating_sub(WHEEL_ROWS as usize)
            } else {
                *d + WHEEL_ROWS as usize
            };
            return;
        }
        _ => {}
    }
    if wheel_on_selected_control(state, target, dir) {
        return;
    }
    if hits.in_left_panel(input.col, input.row) {
        state.scroll_left(-dir * WHEEL_ROWS);
    }
}

/// Apply the wheel to `target` if it is the already-selected control.
/// Returns false when the target is not selected (the caller falls back to
/// scrolling the panel).
fn wheel_on_selected_control(state: &mut AppState, target: Option<HitTarget>, dir: i32) -> bool {
    let Some(t) = target else { return false };
    let up = dir > 0;
    match t {
        HitTarget::CenterDigit(pos) => {
            if !(state.nav.focus == Focus::Global && state.global_field == GlobalField::Frequency) {
                return false;
            }
            state.global_editing = true;
            state.center_freq.set_cursor(pos);
            if up {
                state.global_up();
            } else {
                state.global_down();
            }
            true
        }
        HitTarget::Global(GlobalField::Frequency) => {
            if !(state.nav.focus == Focus::Global && state.global_field == GlobalField::Frequency) {
                return false;
            }
            state.global_editing = true;
            if up {
                state.global_up();
            } else {
                state.global_down();
            }
            true
        }
        HitTarget::Global(GlobalField::Panel) | HitTarget::PanelStep(_) => {
            if !(state.nav.focus == Focus::Global && state.global_field == GlobalField::Panel) {
                return false;
            }
            state.resize_left_panel(up);
            true
        }
        HitTarget::Global(GlobalField::Volume) | HitTarget::VolumeSegment(_) => {
            if !(state.nav.focus == Focus::Global && state.global_field == GlobalField::Volume) {
                return false;
            }
            let step = if up {
                VOLUME_WHEEL_STEP
            } else {
                -VOLUME_WHEEL_STEP
            };
            state.volume = (state.volume + step).clamp(0.0, 1.0);
            true
        }
        HitTarget::VfoDigit(pos) => {
            if !(state.nav.focus == Focus::Radio && state.radio.field() == RadioField::Frequency) {
                return false;
            }
            state.radio.begin_edit();
            state.radio.freq_mut().set_cursor(pos);
            radio_step(state, up);
            true
        }
        HitTarget::Radio(f) => {
            if !(state.nav.focus == Focus::Radio && state.radio.field() == f) {
                return false;
            }
            match f {
                _ if state.radio.is_choice(f) || state.radio.is_toggle(f) => {
                    state.radio.edit_toggle_dir(f, dir);
                }
                RadioField::Audio => state.radio.cycle_audio(dir),
                f if crate::vfo::radio::RadioModule::is_rec_check(f) => state.radio.toggle_rec(f),
                RadioField::Mute => state.radio.toggle_mute(),
                // Buttons and text fields do not step with the wheel.
                RadioField::RecName | RadioField::RecDir | RadioField::Record => return false,
                f if state.radio.is_path(f) => return false,
                _ => {
                    state.radio.begin_edit();
                    radio_step(state, up);
                }
            }
            true
        }
        // The job list scrolls under the wheel (it changes no value).
        HitTarget::SchedRow(_) => {
            state.schedule.scroll_view(-dir);
            true
        }
        // A bookmark list scrolls under the wheel (it changes no value).
        HitTarget::BmRow(_) => {
            state.bookmarks.scroll_view(-dir * WHEEL_ROWS);
            true
        }
        HitTarget::BmField(f) => {
            if !(state.nav.focus == Focus::Bookmarks && state.bookmarks.field() == f) {
                return false;
            }
            match f {
                BookmarkField::List => state.bookmarks.cycle_list(-dir),
                _ => return false,
            }
            true
        }
        HitTarget::BmPageRow(_) => {
            state.bookmarks.page_wheel(-dir);
            true
        }
        HitTarget::BmPageField(PageField::StripRows)
            if state.nav.focus == Focus::WorkArea
                && state.bookmarks.page_field() == PageField::StripRows =>
        {
            state.bookmarks.adjust_rows(dir);
            true
        }
        HitTarget::Source(f) => {
            if !(state.nav.focus == Focus::Source && state.source.field() == f) {
                return false;
            }
            match f {
                // Value steppers and lists step with the wheel.
                SourceField::SampleRate
                | SourceField::Decimation
                | SourceField::Bandwidth
                | SourceField::Antenna
                | SourceField::NetAudio
                | SourceField::Gain(_) => {
                    state.source_adjust(dir);
                    true
                }
                // Toggles, the device (re-probes hardware) and the File
                // controls are click-only.
                SourceField::Kind
                | SourceField::Device
                | SourceField::Agc
                | SourceField::FilePath
                | SourceField::FileTransport
                | SourceField::FileBack
                | SourceField::FileFwd
                | SourceField::NetServer
                | SourceField::NetToken
                | SourceField::NetOpen
                | SourceField::ShareOn
                | SourceField::ShareListen
                | SourceField::ShareToken
                | SourceField::ShareMute
                | SourceField::ShareDisconnect => false,
            }
        }
        _ => false,
    }
}

/// Wheel in the editor: steps the frequency digit or the List/Mode value of
/// the already-selected field (no scroll hijacking).
fn wheel_editor(
    ed: &mut crate::bookmarks::editor::BookmarkEditor,
    target: Option<HitTarget>,
    dir: i32,
) {
    match target {
        Some(HitTarget::EditorDigit(pos)) if ed.field() == EditorField::Frequency => {
            ed.edit_digit(pos);
            if dir > 0 { ed.up() } else { ed.down() }
        }
        Some(HitTarget::EditorField(f)) if ed.field() == f && !ed.is_editing() => ed.wheel(dir),
        _ => {}
    }
}

/// One Up/Down step on the edited Radio field.
fn radio_step(state: &mut AppState, up: bool) {
    if up {
        state.radio.edit_up();
    } else {
        state.radio.edit_down();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::render::render_with_hits;
    use crate::vfo::model::Mode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Draw `state` at `w`x`h` (scroll reconciled like the event loop) and
    /// return the screen rows plus the recorded hit map.
    fn draw(state: &mut AppState, w: u16, h: u16) -> (Vec<String>, HitMap) {
        let full = Rect::new(0, 0, w, h);
        let regions = crate::ui::layout::compute_layout(
            full,
            state.bookmarks.strip_rows,
            state.left_panel_width,
        );
        state.reconcile_left_scroll(regions.left_panel.height);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitMap::default();
        terminal
            .draw(|f| hits = render_with_hits(f, state))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let rows = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (rows, hits)
    }

    fn draw_std(state: &mut AppState) -> (Vec<String>, HitMap) {
        draw(state, 120, 40)
    }

    /// Cell (col, row) of the first occurrence of `needle` on screen.
    fn find(rows: &[String], needle: &str) -> (u16, u16) {
        for (y, line) in rows.iter().enumerate() {
            if let Some(byte) = line.find(needle) {
                let col = line[..byte].chars().count();
                return (col as u16, y as u16);
            }
        }
        panic!("{needle:?} not on screen:\n{}", rows.join("\n"));
    }

    fn click_at(state: &mut AppState, hits: &HitMap, (col, row): (u16, u16)) {
        apply_mouse(
            state,
            hits,
            MouseInput {
                kind: MouseKind::LeftDown,
                col,
                row,
            },
        );
    }

    fn wheel_at(state: &mut AppState, hits: &HitMap, (col, row): (u16, u16), up: bool) {
        let kind = if up {
            MouseKind::WheelUp
        } else {
            MouseKind::WheelDown
        };
        apply_mouse(state, hits, MouseInput { kind, col, row });
    }

    /// Click `needle` on a freshly drawn frame.
    fn click_text(state: &mut AppState, needle: &str) {
        let (rows, hits) = draw_std(state);
        let at = find(&rows, needle);
        click_at(state, &hits, at);
    }

    /// A state showing only the Schedule module expanded, with one NAVTEX
    /// bookmark (0600-0630 UTC) to schedule.
    fn schedule_state() -> AppState {
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
        let mut s = AppState::default();
        s.nav.collapsed.source = true;
        s.nav.collapsed.radio = true;
        s.nav.collapsed.bookmarks = true;
        let mut l = BookmarkList::new("Ships");
        l.bookmarks.push(Bookmark {
            name: "NAVTEX".into(),
            frequency_hz: 518_000,
            start_utc: 600,
            end_utc: 630,
            settings: crate::bookmarks::model::voice_part(Mode::Usb, None, None),
            ..Bookmark::default()
        });
        s.bookmarks.replace_store(BookmarkStore {
            lists: vec![l],
            ..BookmarkStore::default()
        });
        s
    }

    #[test]
    fn schedule_module_shows_its_controls_and_the_master_switch_clicks() {
        let mut s = schedule_state();
        let (rows, _) = draw_std(&mut s);
        let screen = rows.join("\n");
        for label in [
            "Schedule",
            "Scheduler: [On]",
            "(no jobs: [Manager], F2 Add)",
        ] {
            assert!(screen.contains(label), "{label:?} drawn:\n{screen}");
        }
        assert!(!screen.contains("Auto start/stop"), "on the page only");
        click_text(&mut s, "[On]");
        assert!(!s.schedule.scheduler.enabled, "a click switches it off");
        assert_eq!(s.nav.focus, Focus::Schedule);
        let (rows, _) = draw_std(&mut s);
        find(&rows, "Scheduler: [Off]");
        click_text(&mut s, "[Manager]");
        let (rows, _) = draw_std(&mut s);
        find(&rows, "Auto start/stop: [x]");
        click_text(&mut s, "Local time");
        assert!(s.schedule.local_time);
        assert_eq!(s.nav.focus, Focus::WorkArea);
    }

    #[test]
    fn add_pick_apply_by_mouse_creates_a_job_row() {
        let mut s = schedule_state();
        click_text(&mut s, "[Manager]");
        click_text(&mut s, "2Add");
        assert!(s.sched_editor.is_some(), "editor open");
        assert!(s.popup.is_some(), "bookmark picker offered at once");
        click_text(&mut s, "Ships: NAVTEX (518 kHz)");
        assert!(s.popup.is_none());
        let (rows, _) = draw_std(&mut s);
        find(&rows, "NAVTEX (Ships) [..]");
        find(&rows, "518 kHz USB");
        find(&rows, "Start UTC");
        click_text(&mut s, "[Apply]");
        assert!(s.sched_editor.is_none());
        // The new job is selected; a click on the selected page row edits it.
        click_text(&mut s, "0600  0630  N");
        assert_eq!(s.sched_editor.as_ref().map(|e| e.original), Some(Some(0)));
    }

    #[test]
    fn editor_action_and_mute_click_through_and_show_in_the_job_row() {
        let mut s = schedule_state();
        click_text(&mut s, "[Manager]");
        click_text(&mut s, "2Add");
        click_text(&mut s, "Ships: NAVTEX (518 kHz)");
        click_text(&mut s, "[ ] while it runs");
        let (rows, _) = draw_std(&mut s);
        find(&rows, "[x] while it runs");
        click_text(&mut s, "[ ] Baseband");
        let (rows, _) = draw_std(&mut s);
        find(&rows, "[x] Baseband");
        click_text(&mut s, "[Apply]");
        let (rows, _) = draw_std(&mut s);
        find(&rows, "x 0600-0630 NBm NAVTEX");
        // "Tune only": End, the outputs and Mute are disabled.
        click_text(&mut s, "3Edit");
        click_text(&mut s, "( ) Tune only");
        let (rows, _) = draw_std(&mut s);
        find(&rows, "(•) Tune only");
        find(&rows, "none: tunes at Start only");
        click_text(&mut s, "tune only: nothing to mute");
        assert_eq!(
            s.sched_editor.as_ref().unwrap().field(),
            crate::schedule::editor::SchedEditorField::TuneOnly,
            "a disabled field is not selected"
        );
        click_text(&mut s, "[Apply]");
        let (rows, _) = draw_std(&mut s);
        find(&rows, "x 0600      T   NAVTEX");
    }

    #[test]
    fn clicking_the_vfo_mute_mutes_only_that_vfo() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "[ ] Mute VFO");
        assert!(s.radio.active().muted);
        assert!(!s.muted, "not the global mute");
        assert_eq!(s.radio.field(), RadioField::Mute);
        click_text(&mut s, "[x] Mute VFO");
        assert!(!s.radio.active().muted);
    }

    /// Screen cell of digit position `pos` (0 = ones) of a frequency shown
    /// right after `label` ("D DDD DDD DDD Hz").
    fn digit_cell(rows: &[String], label: &str, pos: usize) -> (u16, u16) {
        let (col, row) = find(rows, label);
        let start = col + label.chars().count() as u16;
        // Index of the digit from the left (pos 9 first), plus one group space
        // after positions 9, 6 and 3.
        let from_left = 9 - pos;
        let spaces = [9, 6, 3].iter().filter(|&&g| pos < g).count();
        (start + (from_left + spaces) as u16, row)
    }

    /// A state with a "VOLMET" list (Bangkok 6676, Rostov 8939 kHz USB),
    /// centered on Rostov so both fit the 2 MHz mock span.
    fn bm_state() -> AppState {
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
        let mut list = BookmarkList::new("VOLMET");
        list.bookmarks = vec![
            Bookmark {
                name: "Bangkok".into(),
                frequency_hz: 8_676_000,
                ..Bookmark::default()
            },
            Bookmark {
                name: "Rostov".into(),
                frequency_hz: 8_939_000,
                settings: crate::bookmarks::model::voice_part(Mode::Usb, None, None),
                ..Bookmark::default()
            },
        ];
        let mut s = AppState::with_mock_source();
        s.bookmarks = crate::bookmarks::manager::BookmarkManager::new(BookmarkStore {
            lists: vec![list],
            ..BookmarkStore::default()
        });
        s.center_freq = crate::freq::FreqEntry::new(8_900_000);
        s.nav.collapsed.source = true; // room for the Bookmarks module
        s
    }

    #[test]
    fn clicking_a_bookmark_row_selects_and_a_second_click_tunes() {
        let mut s = bm_state();
        let f0 = s.radio.active().frequency_hz;
        click_text(&mut s, "8939 Rostov");
        assert_eq!(s.nav.focus, Focus::Bookmarks);
        assert_eq!(s.bookmarks.selected_bookmark().unwrap().name, "Rostov");
        assert_eq!(
            s.radio.active().frequency_hz,
            f0,
            "first click only selects"
        );
        click_text(&mut s, "8939 Rostov");
        assert_eq!(
            s.radio.active().frequency_hz,
            8_939_000,
            "second click tunes"
        );
        assert_eq!(s.radio.active().mode, Mode::Usb);
    }

    #[test]
    fn clicking_a_strip_label_tunes_at_once() {
        let mut s = bm_state();
        s.bookmarks.strip_rows = 2;
        let (rows, hits) = draw_std(&mut s);
        // The strip is the first right-side box; find the label there.
        let (col, row) = find(&rows, "Bangkok");
        assert!(row < 8, "label drawn in the strip (row {row})");
        click_at(&mut s, &hits, (col, row));
        assert_eq!(s.radio.active().frequency_hz, 8_676_000);
        assert_eq!(s.bookmarks.selected_bookmark().unwrap().name, "Bangkok");
    }

    #[test]
    fn clicking_add_opens_the_editor_and_apply_saves() {
        let mut s = bm_state();
        click_text(&mut s, "[Manager]");
        click_text(&mut s, "2Add");
        assert!(s.bm_editor.is_some());
        let (rows, _) = draw_std(&mut s);
        let below_tabs = rows.iter().skip(5).any(|r| r.contains("[Apply]"));
        assert!(
            below_tabs,
            "the editor is drawn in the page:\n{}",
            rows.join("\n")
        );
        click_text(&mut s, "[Apply]");
        assert!(s.bm_editor.is_none());
        assert_eq!(s.bookmarks.store.lists[0].bookmarks.len(), 3);
    }

    #[test]
    fn editor_ignores_clicks_outside_and_toggles_days() {
        let mut s = bm_state();
        click_text(&mut s, "[Manager]");
        click_text(&mut s, "2Add");
        let (rows, hits) = draw_std(&mut s);
        click_at(&mut s, &hits, (0, rows.len() as u16 - 1));
        assert!(s.bm_editor.is_some(), "a stray click keeps the form");
        click_at(&mut s, &hits, find(&rows, "[x]Mo"));
        assert!(!s.bm_editor.as_ref().unwrap().days[1], "Monday unchecked");
        click_text(&mut s, "[Cancel]");
        assert!(s.bm_editor.is_none());
        assert_eq!(s.bookmarks.store.lists[0].bookmarks.len(), 2);
    }

    #[test]
    fn clicking_find_starts_typing_and_clicking_away_keeps_the_filter() {
        let mut s = bm_state();
        click_text(&mut s, "Find(/):");
        assert!(s.bookmarks.is_filter_editing());
        s.bookmarks.find_char('r');
        click_text(&mut s, "Snap:");
        assert!(!s.bookmarks.is_filter_editing());
        assert_eq!(s.bookmarks.filter.text(), "r");
    }

    #[test]
    fn hide_button_toggles_the_list_on_the_strip() {
        let mut s = bm_state();
        click_text(&mut s, "[Manager]");
        click_text(&mut s, "[Hide]");
        assert!(!s.bookmarks.current_list().unwrap().show_on_strip);
        let (rows, _) = draw_std(&mut s);
        assert!(rows.iter().any(|r| r.contains("(hidden)")));
        assert!(rows.iter().any(|r| r.contains("[Show]")));
    }

    #[test]
    fn new_list_prompt_by_mouse() {
        let mut s = bm_state();
        click_text(&mut s, "[Manager]");
        click_text(&mut s, "[New]");
        assert!(s.prompt.is_some());
        click_text(&mut s, "[OK]");
        assert!(s.prompt.is_none());
        assert_eq!(s.bookmarks.current_list().unwrap().name, "New List");
    }

    #[test]
    fn decodes_only_left_press_and_wheel() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let ev = |kind| MouseEvent {
            kind,
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            MouseInput::from_crossterm(ev(MouseEventKind::Down(MouseButton::Left))),
            Some(MouseInput {
                kind: MouseKind::LeftDown,
                col: 3,
                row: 4
            })
        );
        assert!(MouseInput::from_crossterm(ev(MouseEventKind::ScrollUp)).is_some());
        assert!(MouseInput::from_crossterm(ev(MouseEventKind::Moved)).is_none());
        assert!(MouseInput::from_crossterm(ev(MouseEventKind::Up(MouseButton::Left))).is_none());
        assert!(MouseInput::from_crossterm(ev(MouseEventKind::Down(MouseButton::Right))).is_none());
        assert!(MouseInput::from_crossterm(ev(MouseEventKind::Drag(MouseButton::Left))).is_none());
    }

    #[test]
    fn hit_map_returns_the_topmost_region() {
        let mut h = HitMap::default();
        h.push(
            Rect::new(0, 0, 10, 10),
            HitTarget::ModuleBody(Focus::Source),
        );
        h.push(Rect::new(2, 2, 3, 1), HitTarget::Source(SourceField::Kind));
        h.push(Rect::new(0, 0, 0, 5), HitTarget::PopupFrame); // empty: ignored
        assert_eq!(h.hit(3, 2), Some(HitTarget::Source(SourceField::Kind)));
        assert_eq!(h.hit(0, 0), Some(HitTarget::ModuleBody(Focus::Source)));
        assert_eq!(h.hit(10, 0), None);
    }

    #[test]
    fn clicking_the_mode_field_opens_its_popup_and_an_item_applies_at_once() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "Mode: NFM");
        assert_eq!(s.nav.focus, Focus::Radio, "click focuses the Radio module");
        assert_eq!(s.radio.field(), RadioField::MODE);
        assert!(s.popup_open(), "mode popup opens on click");

        click_text(&mut s, "USB");
        assert!(!s.popup_open(), "a click on an item closes the popup");
        assert_eq!(s.radio.active().mode, Mode::Usb, "and applies it at once");
    }

    #[test]
    fn clicking_outside_a_popup_dismisses_it_without_applying() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "Mode: NFM");
        assert!(s.popup_open());
        let (rows, hits) = draw_std(&mut s);
        let last = rows.len() as u16 - 1;
        click_at(&mut s, &hits, (0, last));
        assert!(!s.popup_open(), "click-away dismisses");
        assert_eq!(s.radio.active().mode, Mode::Nfm, "nothing applied");
    }

    #[test]
    fn wheel_in_a_popup_moves_the_highlight() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "Mode: NFM");
        let before = s.popup.as_ref().unwrap().cursor;
        let (rows, hits) = draw_std(&mut s);
        wheel_at(&mut s, &hits, find(&rows, "USB"), false);
        assert_eq!(s.popup.as_ref().unwrap().cursor, before + 1);
    }

    #[test]
    fn clicking_a_vfo_digit_edits_that_digit() {
        let mut s = AppState::with_mock_source();
        let (rows, hits) = draw_std(&mut s);
        click_at(&mut s, &hits, digit_cell(&rows, "Freq: ", 6));
        assert_eq!(s.nav.focus, Focus::Radio);
        assert_eq!(s.radio.field(), RadioField::Frequency);
        assert!(s.radio.is_editing(), "a digit click enters edit mode");
        assert_eq!(s.radio.freq().cursor(), 6, "on the clicked digit (MHz)");
    }

    #[test]
    fn wheel_changes_the_clicked_digit_and_focus_out_commits() {
        let mut s = AppState::with_mock_source();
        let f0 = s.radio.active().frequency_hz;
        let (rows, hits) = draw_std(&mut s);
        let cell = digit_cell(&rows, "Freq: ", 6);
        click_at(&mut s, &hits, cell);
        let (_, hits) = draw_std(&mut s);
        wheel_at(&mut s, &hits, cell, true);
        wheel_at(&mut s, &hits, cell, true);
        assert_eq!(
            s.radio.freq().display_hz(),
            f0 + 2_000_000,
            "+2 MHz pending"
        );
        assert_eq!(s.radio.active().frequency_hz, f0, "pending until commit");

        // Clicking another field is a focus-out: the edit is committed.
        click_text(&mut s, "Snap:");
        assert_eq!(s.radio.active().frequency_hz, f0 + 2_000_000);
        assert_eq!(s.radio.field(), RadioField::SNAP);
    }

    #[test]
    fn wheel_over_an_unselected_digit_does_not_tune() {
        let mut s = AppState::with_mock_source();
        let f0 = s.radio.active().frequency_hz;
        let (rows, hits) = draw_std(&mut s);
        wheel_at(&mut s, &hits, digit_cell(&rows, "Freq: ", 6), true);
        assert_eq!(s.radio.freq().display_hz(), f0, "no scroll hijacking");
        assert!(!s.radio.is_editing());
    }

    #[test]
    fn center_digit_click_then_wheel_edits_the_center() {
        let mut s = AppState::with_mock_source();
        let c0 = s.center_hz();
        let (rows, hits) = draw_std(&mut s);
        let cell = digit_cell(&rows, "Center: ", 3);
        click_at(&mut s, &hits, cell);
        assert_eq!(s.nav.focus, Focus::Global);
        assert_eq!(s.global_field, GlobalField::Frequency);
        assert!(s.global_editing);
        assert_eq!(s.center_freq.cursor(), 3);
        let (_, hits) = draw_std(&mut s);
        wheel_at(&mut s, &hits, cell, false);
        assert_eq!(s.center_freq.display_hz(), c0 - 1_000);
        assert_eq!(
            s.center_hz(),
            c0,
            "center applies on commit only (spec 6.3)"
        );

        // Focus-out (click on a VFO field) commits the center edit.
        click_text(&mut s, "Snap:");
        assert!(!s.global_editing);
        assert_eq!(s.center_hz(), c0 - 1_000);
    }

    #[test]
    fn clicking_a_global_toggle_activates_it() {
        let mut s = AppState::with_mock_source();
        assert!(s.spectrum_on);
        click_text(&mut s, "[Spectrum:ON]");
        assert!(!s.spectrum_on, "click toggles like ENTER");
        assert_eq!(s.nav.focus, Focus::Global);
        assert_eq!(s.global_field, GlobalField::Spectrum);

        click_text(&mut s, "[Start]");
        assert_eq!(s.run, crate::app::RunState::Running);
    }

    #[test]
    fn clicking_a_volume_segment_sets_the_volume() {
        let mut s = AppState::with_mock_source();
        let (rows, hits) = draw_std(&mut s);
        let (col, row) = find(&rows, "Vol[");
        // Fourth segment -> 4/8.
        click_at(&mut s, &hits, (col + 4 + 3, row));
        assert!((s.volume - 0.5).abs() < 1e-6, "volume {}", s.volume);
        assert_eq!(s.global_field, GlobalField::Volume);
        // Wheel on the now-selected bar nudges it.
        let (_, hits) = draw_std(&mut s);
        wheel_at(&mut s, &hits, (col, row), true);
        assert!((s.volume - 0.55).abs() < 1e-6);
    }

    #[test]
    fn clicking_the_collapse_glyph_toggles_the_module() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "[-] Source");
        assert!(s.nav.collapsed.source, "collapsed");
        assert_eq!(s.nav.focus, Focus::Source);
        click_text(&mut s, "[+] Source");
        assert!(!s.nav.collapsed.source, "expanded again");
    }

    #[test]
    fn clicking_a_module_header_focuses_it() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "] Bookmarks");
        assert_eq!(s.nav.focus, Focus::Bookmarks);
    }

    #[test]
    fn clicking_a_source_list_field_opens_its_popup() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "Sample rate:");
        assert_eq!(s.nav.focus, Focus::Source);
        assert_eq!(s.source.field(), SourceField::SampleRate);
        assert!(s.popup_open());
    }

    #[test]
    fn gain_changes_with_the_wheel_only_once_selected() {
        let mut s = AppState::with_mock_source();
        let g0 = s.source.soapy.as_ref().unwrap().gains[0].value_db;
        let (rows, hits) = draw_std(&mut s);
        let cell = find(&rows, "IFGR");
        wheel_at(&mut s, &hits, cell, true);
        assert_eq!(s.source.soapy.as_ref().unwrap().gains[0].value_db, g0);

        click_at(&mut s, &hits, cell);
        assert_eq!(s.source.field(), SourceField::Gain(0));
        let (_, hits) = draw_std(&mut s);
        wheel_at(&mut s, &hits, cell, true);
        assert_eq!(s.source.soapy.as_ref().unwrap().gains[0].value_db, g0 + 1.0);
    }

    #[test]
    fn clicking_a_radio_stepper_enters_edit_and_wheel_steps_it() {
        let mut s = AppState::with_mock_source();
        let bw0 = s.radio.active().bandwidth_hz;
        let (rows, hits) = draw_std(&mut s);
        // The Radio bandwidth row comes from the Voice schema (Hz).
        let cell = find(&rows, &format!("Bandwidth: {bw0} Hz"));
        click_at(&mut s, &hits, cell);
        assert_eq!(s.nav.focus, Focus::Radio);
        assert_eq!(s.radio.field(), RadioField::BANDWIDTH);
        assert!(s.radio.is_editing());
        let (_, hits) = draw_std(&mut s);
        wheel_at(&mut s, &hits, cell, true);
        assert!(s.radio.active().bandwidth_hz > bw0);
    }

    #[test]
    fn clicks_land_on_the_right_field_when_the_panel_is_scrolled() {
        let mut s = AppState::with_mock_source();
        let _ = draw(&mut s, 80, 14);
        s.scroll_left(4);
        assert!(s.left_scroll > 0, "panel actually scrolled");
        let (rows, hits) = draw(&mut s, 80, 14);
        click_at(&mut s, &hits, find(&rows, "RFGR"));
        assert_eq!(s.nav.focus, Focus::Source);
        assert_eq!(s.source.field(), SourceField::Gain(1), "RFGR is gain 1");
    }

    #[test]
    fn wheel_elsewhere_in_the_left_panel_scrolls_it() {
        let mut s = AppState::with_mock_source();
        let (rows, hits) = draw(&mut s, 80, 14);
        assert_eq!(s.left_scroll, 0);
        let (col, row) = find(&rows, "Kind:");
        wheel_at(&mut s, &hits, (col, row), false);
        assert_eq!(s.left_scroll, WHEEL_ROWS as u16, "wheel down scrolls down");
    }

    #[test]
    fn clicking_mute_and_help_activates_them() {
        let mut s = AppState::with_mock_source();
        click_text(&mut s, "[ ] Mute");
        assert!(s.muted);
        assert_eq!(s.global_field, GlobalField::Mute);
        click_text(&mut s, "[?] Help");
        assert!(s.help.is_some(), "the button opens the help");
        // A click inside the help is inert; one outside closes it.
        let (rows, hits) = draw_std(&mut s);
        let inside = find(&rows, "Help: keys");
        click_at(&mut s, &hits, inside);
        assert!(s.help.is_some());
        click_at(&mut s, &hits, (0, 0));
        assert!(s.help.is_none());
        assert!(s.muted, "the closing click does not reach the bar");
    }

    /// The click-to-tune plot regions of a frame: (rect, first column, columns).
    fn plots(hits: &HitMap) -> Vec<(Rect, u16, u16)> {
        hits.regions()
            .iter()
            .filter_map(|(r, t)| match t {
                HitTarget::Plot { x, cols } => Some((*r, *x, *cols)),
                _ => None,
            })
            .collect()
    }

    /// A state with live spectrum and waterfall frames.
    fn live_display() -> AppState {
        let mut s = AppState::with_mock_source();
        s.spectrum = vec![-70.0; 4096];
        for _ in 0..8 {
            s.waterfall.push(vec![-70.0; 4096]);
        }
        s.center_freq = crate::freq::FreqEntry::new(100_000_000);
        s.radio.active_mut_freq(100_000_000);
        s
    }

    #[test]
    fn spectrum_and_waterfall_share_the_plot_columns() {
        let mut s = live_display();
        let (_, hits) = draw_std(&mut s);
        let p = plots(&hits);
        assert_eq!(p.len(), 2, "spectrum + waterfall: {p:?}");
        assert_eq!((p[0].1, p[0].2), (p[1].1, p[1].2), "same columns");
        assert!(p[0].0.y < p[1].0.y, "spectrum above waterfall");
        // The plot excludes the dB label gutter.
        assert!(p[0].0.x >= crate::ui::spectrum::PLOT_LEFT_COLS);
    }

    #[test]
    fn clicking_the_spectrum_or_waterfall_tunes_to_that_column() {
        use crate::app::TuneMode;
        use crate::ui::spectrum::col_to_offset;
        use crate::vfo::model::snap;
        for which in 0..2 {
            let mut s = live_display();
            s.tune_mode = TuneMode::Free;
            let (_, hits) = draw_std(&mut s);
            let (rect, x, cols) = plots(&hits)[which];
            let col = x + cols / 4;
            let off = col_to_offset(
                usize::from(cols / 4),
                s.display_span_hz(),
                usize::from(cols),
            );
            click_at(&mut s, &hits, (col, rect.y + rect.height / 2));
            let want = snap(
                (100_000_000.0 + off).round() as u64,
                s.radio.active().snap_hz,
            );
            assert_eq!(s.radio.active().frequency_hz, want, "plot {which}");
            assert_eq!(s.center_hz(), 100_000_000, "Free: source unchanged");
        }
        // Center mode: the clicked frequency becomes the center.
        let mut s = live_display();
        let (_, hits) = draw_std(&mut s);
        let (rect, x, cols) = plots(&hits)[0];
        click_at(&mut s, &hits, (x + cols - 1, rect.y));
        assert!(s.center_hz() > 100_000_000);
        assert_eq!(s.center_hz(), s.radio.active().frequency_hz);
    }

    #[test]
    fn the_label_gutter_and_disabled_displays_do_not_tune() {
        let mut s = live_display();
        let (_, hits) = draw_std(&mut s);
        let (rect, x, _) = plots(&hits)[0];
        click_at(&mut s, &hits, (x - 2, rect.y + 1));
        assert_eq!(s.radio.active().frequency_hz, 100_000_000);
        s.spectrum_on = false;
        s.waterfall_on = false;
        let (_, hits) = draw_std(&mut s);
        assert!(plots(&hits).is_empty(), "nothing to click when OFF");
    }

    #[test]
    fn clicking_the_plot_commits_a_pending_vfo_edit_first() {
        let mut s = live_display();
        s.nav.focus = Focus::Radio;
        s.radio.select_field(RadioField::Frequency);
        s.radio.begin_edit();
        let (_, hits) = draw_std(&mut s);
        let (rect, x, cols) = plots(&hits)[0];
        click_at(&mut s, &hits, (x + cols / 3, rect.y));
        assert!(!s.radio.is_editing(), "the edit is closed");
        assert_ne!(s.radio.active().frequency_hz, 100_000_000, "click tuned");
    }

    #[test]
    fn panel_stepper_arrows_and_wheel_resize_the_panel() {
        use crate::ui::layout::{LEFT_PANEL_MIN_WIDTH, LEFT_PANEL_STEP};
        let mut s = AppState::default();
        let (rows, _) = draw_std(&mut s);
        assert!(rows[2].contains("Panel:[◂ 32 ▸]"), "{}", rows[2]);
        click_text(&mut s, "▸");
        assert_eq!(s.left_panel_width, LEFT_PANEL_MIN_WIDTH + LEFT_PANEL_STEP);
        assert_eq!(s.global_field, GlobalField::Panel, "selected");
        let (rows, hits) = draw_std(&mut s);
        assert!(rows[2].contains("Panel:[◂ 34 ▸]"));
        // The right side (bookmark strip frame) now starts at column 34.
        assert_eq!(find(&rows, "┌Bookmarks").0, 34, "panel redrawn wider");
        let at = find(&rows, "Panel:");
        wheel_at(&mut s, &hits, at, true);
        assert_eq!(
            s.left_panel_width,
            LEFT_PANEL_MIN_WIDTH + 2 * LEFT_PANEL_STEP
        );
        click_text(&mut s, "◂");
        assert_eq!(s.left_panel_width, LEFT_PANEL_MIN_WIDTH + LEFT_PANEL_STEP);
    }

    #[test]
    fn the_work_area_tabs_and_layout_are_clickable() {
        use crate::config::{WorkAreaLayout, WorkAreaTab};
        let mut s = AppState::with_mock_source();
        let (rows, _) = draw_std(&mut s);
        find(&rows, "[Spectrum] [Master: Voice]");
        click_text(&mut s, "[Master: Voice]");
        assert_eq!(s.work_area.tab, WorkAreaTab::Vfo);
        let (rows, _) = draw_std(&mut s);
        assert!(
            !rows.iter().any(|r| r.contains("Spectrum  [")),
            "spectrum hidden"
        );
        find(&rows, "Master: Voice"); // the VFO view's title
        click_text(&mut s, "[Spectrum]");
        assert_eq!(s.work_area.tab, WorkAreaTab::Spectrum);
        click_text(&mut s, "[Tabs]");
        assert_eq!(s.work_area.layout, WorkAreaLayout::Split);
        assert_eq!(
            s.global_field,
            GlobalField::Layout,
            "the control is focused"
        );
        let (rows, _) = draw_std(&mut s);
        find(&rows, "[Split]");
        assert!(
            !rows.iter().any(|r| r.contains("[Spectrum] [")),
            "no tab row in Split"
        );
        let both = rows
            .iter()
            .any(|r| r.contains("Master: Voice") && r.contains("Bookmarks"));
        assert!(both, "the VFO view and the strip side by side");
    }

    #[test]
    fn the_voice_view_draws_its_parts_and_a_zoom_click_fine_tunes() {
        let mut s = AppState::with_mock_source();
        s.radio.set_mode(Mode::Usb);
        s.radio.active_mut_freq(7_074_000);
        s.select_work_tab(crate::config::WorkAreaTab::Vfo);
        let (rows, _) = draw_std(&mut s);
        for t in [
            "Channel",
            "Audio 0..4.0 kHz",
            "Level --",
            "Recordings (this session)",
            "(none yet)",
        ] {
            find(&rows, t);
        }
        // A frame: a flat channel spectrum at the 24 kHz IF.
        s.observe_view(crate::engine::VfoViewFrame {
            zoom_db: vec![-80.0; 512],
            zoom_rate: 31_250.0,
            center_offset: 1_400.0,
            audio_db: vec![-90.0; 512],
            level_db: -50.0,
            agc_gain_db: Some(30.0),
        });
        assert_eq!(s.zoom_span_hz, 8_400.0, "+/-1.5 x 2800 Hz");
        let (rows, hits) = draw_std(&mut s);
        find(&rows, "Channel 8.4 kHz");
        find(&rows, "Level -50 dBFS");
        find(&rows, "AGC +30 dB");
        // Click the zoom plot's left edge: about 4.2 kHz below the channel
        // center (dial + 1.4 kHz), snapped to 10 Hz; the source stays.
        let center = s.center_hz();
        let (x, y) = hits
            .regions()
            .iter()
            .find_map(|(r, t)| match t {
                HitTarget::ZoomPlot { x, .. } => Some((*x, r.y)),
                _ => None,
            })
            .expect("zoom plot is clickable");
        click_at(&mut s, &hits, (x, y));
        let f = s.radio.active().frequency_hz;
        assert!((7_071_000..=7_071_400).contains(&f), "{f}");
        assert_eq!(f % 10, 0);
        assert_eq!(s.center_hz(), center);
    }

    #[test]
    fn clicking_manager_opens_the_page_in_the_vfo_tab_and_esc_closes_it() {
        use crate::ui::page::WorkOwner;
        let mut s = schedule_state();
        click_text(&mut s, "[Manager]");
        assert_eq!(s.work_owner, WorkOwner::Schedule);
        assert_eq!(s.nav.focus, Focus::WorkArea);
        let (rows, _) = draw_std(&mut s);
        let screen = rows.join("\n");
        assert!(
            screen.contains("[Schedule]"),
            "the page takes the VFO's tab:\n{screen}"
        );
        assert!(screen.contains("Esc: back to Master"), "{screen}");
        assert!(screen.contains("Schedule: manager"), "{screen}");
        crate::ui::run::apply_action(&mut s, crate::ui::run::Action::Escape);
        assert_eq!(s.work_owner, WorkOwner::Vfo);
        assert_eq!(s.nav.focus, Focus::Schedule);
        let (rows, _) = draw_std(&mut s);
        assert!(rows.join("\n").contains("[Master: Voice]"));
    }

    #[test]
    fn quick_add_saves_the_vfo_without_the_editor() {
        let mut s = bm_state();
        click_text(&mut s, "[Quick Add]");
        assert!(s.bm_editor.is_none(), "no editor");
        assert_eq!(s.bookmarks.store.lists[0].bookmarks.len(), 3);
        assert_eq!(
            s.bookmarks.selected_bookmark().unwrap().name,
            "New Bookmark"
        );
        click_text(&mut s, "[Quick Add]");
        let names: Vec<_> = s.bookmarks.store.lists[0]
            .bookmarks
            .iter()
            .map(|b| b.name.clone())
            .collect();
        assert_eq!(
            s.bookmarks.selected_bookmark().unwrap().name,
            "New Bookmark (1)",
            "{names:?}"
        );
    }

    #[test]
    fn the_page_table_selects_then_tunes_and_its_keys_copy_and_delete() {
        let mut s = bm_state();
        click_text(&mut s, "[Manager]");
        let (rows, _) = draw_std(&mut s);
        let screen = rows.join("\n");
        for needle in [
            "Lists",
            "Strip rows: [◂",
            "Sort: [Frequency]",
            "1Help",
            "8Delete",
        ] {
            assert!(screen.contains(needle), "{needle:?}:\n{screen}");
        }
        let f0 = s.radio.active().frequency_hz;
        // the table row (the panel shows "8939 Rostov" without the padding)
        click_text(&mut s, "   8939 Rostov");
        assert_eq!(s.bookmarks.selected_bookmark().unwrap().name, "Rostov");
        assert_eq!(s.nav.focus, Focus::WorkArea);
        assert_eq!(s.radio.active().frequency_hz, f0, "first click selects");
        click_text(&mut s, "   8939 Rostov");
        assert_eq!(
            s.radio.active().frequency_hz,
            8_939_000,
            "second click tunes"
        );
        click_text(&mut s, "8Delete");
        assert!(
            s.popup.as_ref().unwrap().title.contains("Rostov"),
            "asks first"
        );
    }
}
