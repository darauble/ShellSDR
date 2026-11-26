//! Left-panel content measurement and focused-field mapping (pure).
//!
//! Step 3.6b. Computes each module's natural height (header + content, or just
//! the header when collapsed) and the absolute virtual row span of the
//! currently focused field, so the scroll logic can keep the focused field in
//! view. Pure functions of `AppState`, unit-tested without a terminal.

use crate::app::AppState;
use crate::ui::nav::Focus;
use crate::ui::scroll::Span;

/// Rows a bordered block spends on its frame (top + bottom border).
const BORDER_ROWS: u16 = 2;
/// Height of a collapsed module (header line between borders).
pub const COLLAPSED_HEIGHT: u16 = 3;

/// Number of content lines the Source module renders (its kind's rows and
/// the sharing rows).
fn source_content_lines(state: &AppState) -> u16 {
    state.source.content_rows()
}

/// Number of content lines the Radio module renders (the visible fields plus
/// the VFO name header line).
fn radio_content_lines(state: &AppState) -> u16 {
    // "VFO: name" line + one line per visible field.
    1 + state.radio.visible_fields().len() as u16
}

/// Content rows above the bookmark rows: List, list buttons, bookmark
/// buttons, strip rows, sort, find (one row each, in `BookmarkField` order).
pub const BOOKMARK_HEADER_ROWS: u16 = 3;

/// Number of content lines the Bookmarks module renders: the header rows,
/// the bookmark window, and the selected bookmark's details line.
fn bookmarks_content_lines(state: &AppState) -> u16 {
    BOOKMARK_HEADER_ROWS + state.bookmarks.window_rows() as u16 + 1
}

/// Content rows above the job rows: `[Manager]`, the summary, On/Off, List.
pub const SCHEDULE_HEADER_ROWS: u16 = 4;

/// Number of content lines the Schedule module renders: the header rows, the
/// job window (at least one row for the "(no jobs)" note), and the selected
/// job's state.
fn schedule_content_lines(state: &AppState) -> u16 {
    let jobs = state
        .schedule
        .jobs()
        .len()
        .clamp(1, crate::schedule::module::JOB_ROWS) as u16;
    SCHEDULE_HEADER_ROWS + jobs + 1
}

/// Natural heights of the left-panel modules in order
/// `[Source, Radio, Bookmarks, Schedule, Report]`, honoring the collapsed
/// flags.
pub fn module_heights(state: &AppState) -> [u16; MODULES] {
    let c = state.nav.collapsed;
    let h = |collapsed: bool, content: u16| {
        if collapsed {
            COLLAPSED_HEIGHT
        } else {
            BORDER_ROWS + content
        }
    };
    [
        h(c.source, source_content_lines(state)),
        h(c.radio, radio_content_lines(state)),
        h(c.bookmarks, bookmarks_content_lines(state)),
        h(c.schedule, schedule_content_lines(state)),
        h(
            c.report,
            crate::ui::report_view::panel_rows(state).len() as u16,
        ),
    ]
}

/// The left-panel modules.
pub const MODULES: usize = 5;

/// The virtual start row of each module (prefix sums of the heights).
pub fn module_starts(heights: &[u16; MODULES]) -> [u16; MODULES] {
    let mut starts = [0; MODULES];
    for i in 1..MODULES {
        starts[i] = starts[i - 1] + heights[i - 1];
    }
    starts
}

/// The absolute virtual row span of the focused element, used to keep it in
/// view. For the Radio module this is the focused *field's* row (so a module
/// taller than the viewport still scrolls the field into view); for other
/// modules it is the whole module (so TAB brings the module into view).
pub fn focused_span(state: &AppState) -> Span {
    let heights = module_heights(state);
    let starts = module_starts(&heights);
    match state.nav.focus {
        Focus::Global | Focus::WorkArea => Span { start: 0, end: 0 },
        Focus::Source => {
            if state.nav.collapsed.source {
                return Span::new(starts[0], heights[0]);
            }
            // Inside the Source block: border(1) + the field's content row.
            let idx = state.source.field_row(state.source.field());
            let field_row = starts[0] + 1 /*top border*/ + idx;
            Span::new(field_row, 1)
        }
        Focus::Bookmarks => {
            use crate::bookmarks::manager::BookmarkField;
            if state.nav.collapsed.bookmarks {
                return Span::new(starts[2], heights[2]);
            }
            let bm = &state.bookmarks;
            let row = match bm.field() {
                BookmarkField::Items => {
                    let visible = bm
                        .cursor()
                        .filter(|c| *c >= bm.scroll())
                        .map_or(0, |c| c - bm.scroll());
                    BOOKMARK_HEADER_ROWS as usize + visible
                }
                f => f.row(),
            } as u16;
            Span::new(starts[2] + 1 /*top border*/ + row, 1)
        }
        Focus::Schedule => {
            use crate::schedule::module::SchedField;
            if state.nav.collapsed.schedule {
                return Span::new(starts[3], heights[3]);
            }
            let m = &state.schedule;
            // Row 0 is [Manager], row 1 the summary line; fields follow.
            let row = match m.field() {
                SchedField::Manager => 0,
                SchedField::Enabled => 2,
                SchedField::List => 3,
                SchedField::Items => {
                    let visible = m
                        .cursor()
                        .filter(|c| *c >= m.scroll())
                        .map_or(0, |c| c - m.scroll());
                    SCHEDULE_HEADER_ROWS + visible as u16
                }
            };
            Span::new(starts[3] + 1 /*top border*/ + row, 1)
        }
        Focus::Report => {
            if state.nav.collapsed.report {
                return Span::new(starts[4], heights[4]);
            }
            // Its only control is the `[Report]` button on the first row.
            Span::new(starts[4] + 1, 1)
        }
        Focus::Radio => {
            if state.nav.collapsed.radio {
                return Span::new(starts[1], heights[1]);
            }
            // Inside the Radio block: border(1) + VFO header(1) + field index.
            let fields = state.radio.visible_fields();
            let idx = fields
                .iter()
                .position(|f| *f == state.radio.field())
                .unwrap_or(0) as u16;
            let field_row = starts[1] + 1 /*top border*/ + 1 /*VFO header*/ + idx;
            Span::new(field_row, 1)
        }
    }
}

/// The index of the active field within the Radio block, if focused there.
#[allow(dead_code)]
pub fn radio_field_index(state: &AppState) -> Option<usize> {
    if state.nav.focus != Focus::Radio {
        return None;
    }
    state
        .radio
        .visible_fields()
        .iter()
        .position(|f| *f == state.radio.field())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::nav::NavKey;

    #[test]
    fn collapsed_module_is_header_height() {
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        s.on_nav(NavKey::Collapse);
        let h = module_heights(&s);
        assert_eq!(h[0], COLLAPSED_HEIGHT, "collapsed Source is header-height");
    }

    #[test]
    fn expanded_radio_height_tracks_field_count() {
        let s = AppState::with_mock_source();
        let fields = s.radio.visible_fields().len() as u16;
        let h = module_heights(&s);
        // border(2) + VFO header(1) + fields.
        assert_eq!(h[1], BORDER_ROWS + 1 + fields);
    }

    #[test]
    fn module_starts_are_prefix_sums() {
        let s = AppState::with_mock_source();
        let h = module_heights(&s);
        let starts = module_starts(&h);
        assert_eq!(starts[0], 0);
        assert_eq!(starts[1], h[0]);
        assert_eq!(starts[2], h[0] + h[1]);
    }

    #[test]
    fn focused_field_span_advances_with_field_cursor() {
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        let first = focused_span(&s);
        // Move the field cursor down one; the focused span should move down.
        s.radio.field_next();
        let second = focused_span(&s);
        assert_eq!(
            second.start,
            first.start + 1,
            "field span tracks the cursor"
        );
        assert_eq!(second.end, second.start + 1);
    }

    #[test]
    fn bookmarks_height_and_focused_row_follow_the_selection() {
        use crate::bookmarks::manager::BookmarkField;
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
        let mut list = BookmarkList::new("L");
        list.bookmarks = (0..3)
            .map(|i| Bookmark {
                name: format!("B{i}"),
                frequency_hz: 1_000 + i,
                ..Bookmark::default()
            })
            .collect();
        let mut s = AppState::with_mock_source();
        s.bookmarks = crate::bookmarks::manager::BookmarkManager::new(BookmarkStore {
            lists: vec![list],
            ..BookmarkStore::default()
        });
        let h = module_heights(&s);
        // border(2) + 3 header rows + 3 bookmark rows + details line.
        assert_eq!(h[2], BORDER_ROWS + 3 + 3 + 1);
        s.nav.focus = Focus::Bookmarks;
        let start = module_starts(&h)[2];
        assert_eq!(focused_span(&s).start, start + 1 + 3, "first bookmark row");
        s.bookmarks.down();
        assert_eq!(focused_span(&s).start, start + 1 + 4);
        s.bookmarks.select_field(BookmarkField::Find);
        assert_eq!(focused_span(&s).start, start + 1 + 2);
        s.bookmarks.select_field(BookmarkField::Manager);
        assert_eq!(
            focused_span(&s).start,
            start + 1,
            "[Manager] is the first row"
        );
    }

    #[test]
    fn focused_span_of_source_tracks_the_field() {
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        let first = focused_span(&s);
        // Cursor starts on Kind (row 0 inside the block): border(1) + 0.
        assert_eq!(first.start, 1);
        assert_eq!(first.end, 2);
        // Move to the next field; the span advances one row.
        s.source.field_next();
        let second = focused_span(&s);
        assert_eq!(
            second.start,
            first.start + 1,
            "source field span tracks cursor"
        );
    }
}
