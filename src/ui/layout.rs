//! Layout computation (pure).
//!
//! Step 1 seam: split the terminal area into the regions from the spec:
//! a top global-controls bar, a body (left module panel + right side), and a
//! bottom status bar. The right side stacks a bookmarks strip, the power
//! spectrum, and the waterfall. Pure geometry so it is unit-testable without
//! a terminal.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Height of the top global-controls bar (two text rows + border).
pub const GLOBAL_BAR_HEIGHT: u16 = 4;
/// Height of the bottom status bar.
pub const STATUS_BAR_HEIGHT: u16 = 1;
/// Left module panel width: the default and minimum (the module rows are
/// laid out for it), the maximum, and the resize step (`Panel:` stepper,
/// Ctrl+Shift+Left/Right).
pub const LEFT_PANEL_MIN_WIDTH: u16 = 32;
pub const LEFT_PANEL_MAX_WIDTH: u16 = 96;
pub const LEFT_PANEL_STEP: u16 = 2;
/// Columns always left for the right side (strip, spectrum, waterfall).
pub const MIN_RIGHT_WIDTH: u16 = 40;

/// The left panel width actually used on a screen `screen_width` columns
/// wide: the requested width clamped to its range, then shrunk so the right
/// side keeps `MIN_RIGHT_WIDTH` (never below the minimum). The requested
/// (persisted) width is kept, so widening the terminal restores it.
pub fn left_panel_width(requested: u16, screen_width: u16) -> u16 {
    let room = screen_width
        .saturating_sub(MIN_RIGHT_WIDTH)
        .max(LEFT_PANEL_MIN_WIDTH);
    requested
        .clamp(LEFT_PANEL_MIN_WIDTH, LEFT_PANEL_MAX_WIDTH)
        .min(room)
}

/// Computed screen regions. The right side is the Work Area (spec 13.5):
/// the spectrum stack (bookmark strip, spectrum, waterfall) and the active
/// VFO's view, as tabs or side by side. A region that is not shown is an
/// empty rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regions {
    pub global_bar: Rect,
    pub left_panel: Rect,
    /// The whole Work Area.
    pub work_area: Rect,
    /// The tab row (Tabs layout only).
    pub tab_row: Rect,
    /// The active VFO's view.
    pub vfo_view: Rect,
    pub bookmarks: Rect,
    pub spectrum: Rect,
    pub waterfall: Rect,
    pub status_bar: Rect,
}

impl Regions {
    /// Whether the spectrum stack is on screen.
    pub fn spectrum_shown(&self) -> bool {
        !self.spectrum.is_empty()
    }
}

/// `bookmark_rows`: text rows in the bookmark strip; `panel_width`: the
/// requested left panel width (clamped by `left_panel_width`). The Work Area
/// shows the spectrum tab.
pub fn compute_layout(area: Rect, bookmark_rows: u16, panel_width: u16) -> Regions {
    compute_layout_with(area, bookmark_rows, panel_width, Default::default())
}

/// As [`compute_layout`], with the Work Area's layout and tab.
pub fn compute_layout_with(
    area: Rect,
    bookmark_rows: u16,
    panel_width: u16,
    work: crate::config::WorkAreaConfig,
) -> Regions {
    use crate::config::{WorkAreaLayout, WorkAreaTab};
    // Vertical: global bar / body / status bar.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(GLOBAL_BAR_HEIGHT),
            Constraint::Min(1),
            Constraint::Length(STATUS_BAR_HEIGHT),
        ])
        .split(area);
    let global_bar = rows[0];
    let body = rows[1];
    let status_bar = rows[2];

    // Body horizontal: left panel / right side.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_panel_width(panel_width, area.width)),
            Constraint::Min(1),
        ])
        .split(body);
    let left_panel = cols[0];
    let work_area = cols[1];
    let empty = Rect::new(work_area.x, work_area.y, 0, 0);
    // Tabs: a one-line tab row, then the chosen tab. Split: the VFO view on
    // the left half (next to the left panel), the spectrum on the right.
    let (tab_row, vfo_view, right) = match work.layout {
        WorkAreaLayout::Tabs => {
            let tab_row = Rect::new(
                work_area.x,
                work_area.y,
                work_area.width,
                1.min(work_area.height),
            );
            let rest = Rect::new(
                work_area.x,
                work_area.y + tab_row.height,
                work_area.width,
                work_area.height - tab_row.height,
            );
            match work.tab {
                WorkAreaTab::Spectrum => (tab_row, empty, rest),
                WorkAreaTab::Vfo => (tab_row, rest, empty),
            }
        }
        WorkAreaLayout::Split => {
            let halves = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(work_area);
            (empty, halves[0], halves[1])
        }
    };

    // Right side vertical: bookmarks strip / spectrum / waterfall.
    // Bookmarks strip gets `bookmark_rows` plus a border row. Spectrum and
    // waterfall split the remainder evenly.
    let strip_height = bookmark_rows.saturating_add(2); // +2 for top/bottom border
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(strip_height),
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(right);

    let shown = |r: Rect| if right.is_empty() { empty } else { r };
    Regions {
        global_bar,
        left_panel,
        work_area,
        tab_row,
        vfo_view,
        bookmarks: shown(right_rows[0]),
        spectrum: shown(right_rows[1]),
        waterfall: shown(right_rows[2]),
        status_bar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Rect {
        Rect::new(0, 0, 120, 40)
    }

    #[test]
    fn vertical_split_places_bars_top_and_bottom() {
        let r = compute_layout(full(), 1, LEFT_PANEL_MIN_WIDTH);
        // Global bar at the very top.
        assert_eq!(r.global_bar.y, 0);
        assert_eq!(r.global_bar.height, GLOBAL_BAR_HEIGHT);
        // Status bar at the very bottom, one row tall.
        assert_eq!(r.status_bar.height, STATUS_BAR_HEIGHT);
        assert_eq!(r.status_bar.y + r.status_bar.height, 40);
    }

    #[test]
    fn body_splits_left_panel_and_right_side() {
        let r = compute_layout(full(), 1, LEFT_PANEL_MIN_WIDTH);
        assert_eq!(r.left_panel.x, 0);
        assert_eq!(r.left_panel.width, LEFT_PANEL_MIN_WIDTH);
        // Spectrum (right side) starts just after the left panel.
        assert_eq!(r.spectrum.x, LEFT_PANEL_MIN_WIDTH);
    }

    #[test]
    fn right_side_stacks_bookmarks_spectrum_waterfall() {
        let r = compute_layout(full(), 1, LEFT_PANEL_MIN_WIDTH);
        // Order top-to-bottom: bookmarks, then spectrum, then waterfall.
        assert!(r.bookmarks.y < r.spectrum.y);
        assert!(r.spectrum.y < r.waterfall.y);
        // All three share the same x/width (right column).
        assert_eq!(r.bookmarks.x, r.spectrum.x);
        assert_eq!(r.spectrum.x, r.waterfall.x);
        assert_eq!(r.bookmarks.width, r.waterfall.width);
    }

    #[test]
    fn bookmark_strip_height_tracks_row_count() {
        let r1 = compute_layout(full(), 1, LEFT_PANEL_MIN_WIDTH);
        let r3 = compute_layout(full(), 3, LEFT_PANEL_MIN_WIDTH);
        assert_eq!(
            r3.bookmarks.height,
            r1.bookmarks.height + 2,
            "two extra rows for two extra bookmark lines"
        );
    }

    #[test]
    fn spectrum_and_waterfall_non_empty() {
        let r = compute_layout(full(), 1, LEFT_PANEL_MIN_WIDTH);
        assert!(r.spectrum.width > 0 && r.spectrum.height > 0);
        assert!(r.waterfall.width > 0 && r.waterfall.height > 0);
    }

    #[test]
    fn a_wider_panel_narrows_the_right_side() {
        let r = compute_layout(full(), 1, 50);
        assert_eq!(r.left_panel.width, 50);
        assert_eq!(r.spectrum.x, 50);
        assert_eq!(r.spectrum.width, 70);
    }

    #[test]
    fn panel_width_is_clamped_to_its_range_and_the_screen() {
        assert_eq!(left_panel_width(10, 200), LEFT_PANEL_MIN_WIDTH, "min");
        assert_eq!(left_panel_width(500, 400), LEFT_PANEL_MAX_WIDTH, "max");
        // The right side keeps MIN_RIGHT_WIDTH columns on a narrow screen...
        assert_eq!(left_panel_width(90, 100), 100 - MIN_RIGHT_WIDTH);
        // ...but the panel never goes below its minimum.
        assert_eq!(left_panel_width(90, 50), LEFT_PANEL_MIN_WIDTH);
        let r = compute_layout(Rect::new(0, 0, 100, 40), 1, 90);
        assert_eq!(r.left_panel.width, 100 - MIN_RIGHT_WIDTH);
    }

    #[test]
    fn tabs_show_a_tab_row_and_one_view() {
        use crate::config::{WorkAreaConfig, WorkAreaLayout, WorkAreaTab};
        let spec = compute_layout(full(), 1, 32);
        assert_eq!(spec.tab_row.height, 1);
        assert_eq!(spec.tab_row.y, spec.work_area.y);
        assert_eq!(
            spec.bookmarks.y,
            spec.work_area.y + 1,
            "strip below the tabs"
        );
        assert!(spec.vfo_view.is_empty() && spec.spectrum_shown());
        let vfo = compute_layout_with(
            full(),
            1,
            32,
            WorkAreaConfig {
                layout: WorkAreaLayout::Tabs,
                tab: WorkAreaTab::Vfo,
            },
        );
        assert!(!vfo.spectrum_shown() && vfo.bookmarks.is_empty() && vfo.waterfall.is_empty());
        assert_eq!(vfo.vfo_view.y, vfo.work_area.y + 1);
        assert_eq!(vfo.vfo_view.height, vfo.work_area.height - 1);
    }

    #[test]
    fn split_puts_the_vfo_view_left_and_the_spectrum_right_in_equal_halves() {
        use crate::config::{WorkAreaConfig, WorkAreaLayout, WorkAreaTab};
        let r = compute_layout_with(
            full(),
            1,
            32,
            WorkAreaConfig {
                layout: WorkAreaLayout::Split,
                tab: WorkAreaTab::Spectrum,
            },
        );
        assert!(r.tab_row.is_empty(), "no tab row in Split");
        assert_eq!(r.vfo_view.x, 32, "next to the left panel");
        assert_eq!(r.vfo_view.width, 44);
        assert_eq!(r.spectrum.x, 32 + 44);
        assert_eq!(r.spectrum.width, 44);
        assert_eq!(r.vfo_view.height, r.work_area.height);
    }
}
