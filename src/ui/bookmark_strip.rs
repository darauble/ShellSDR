//! Bookmark strip above the spectrum (pure layout + render).
//!
//! Step 7. Shows the bookmarks of every list marked "shown" as centered text
//! labels over their frequency, in the strip above the spectrum (text only,
//! no fill, not inside the spectrum graph: spec section 9). Labels are packed
//! into the user-selected number of rows with SDR++ Bookmark Manager's
//! greedy rule (`fftRedraw`): walking bookmarks by frequency, each label goes
//! on the first row where it does not overlap an already placed label;
//! labels that fit on no row are skipped (overlapping text is unreadable in
//! a terminal). The selected bookmark is placed first so it is always shown.
//! Every bookmark in view also gets a `┬` tick on the strip's bottom border at
//! its exact column, so even skipped labels leave a mark.
//!
//! Colors are theme roles: on-air bookmarks use `text`, off-air (outside the
//! UTC window or weekday) use `dim`; the selected bookmark is drawn reversed
//! and bold in its on/off-air color, so selection never hides the state.
//!
//! Columns use the spectrum's plot geometry (`PLOT_LEFT_COLS`,
//! `plot_columns`, `freq_to_col`) so a label sits exactly over its trace
//! column and the VFO marker.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};

use crate::ui::spectrum::{PLOT_LEFT_COLS, freq_to_col, plot_columns};
use crate::ui::theme::Theme;

/// Longest label drawn (longer names are cut with an ellipsis).
pub const MAX_LABEL_CHARS: usize = 20;

/// A bookmark to draw: `key` is (list index, bookmark index).
#[derive(Debug, Clone, PartialEq)]
pub struct StripItem {
    pub key: (usize, usize),
    pub hz: f64,
    pub label: String,
    pub online: bool,
    pub selected: bool,
}

/// A label placed on the strip, in plot-column coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub key: (usize, usize),
    pub row: u16,
    /// First column of the text.
    pub start: usize,
    pub text: String,
    pub online: bool,
    pub selected: bool,
}

/// A bookmark's tick column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub col: usize,
    pub online: bool,
    pub selected: bool,
}

/// Cut `s` to `max` characters, ending with `…` when cut.
pub fn truncate_label(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

/// Place labels for `items` on a strip of `plot_cols` columns and `rows`
/// rows showing `center_hz +/- span_hz/2`. Returns the placed labels and the
/// tick of every bookmark in view.
pub fn place_labels(
    items: &[StripItem],
    center_hz: f64,
    span_hz: f64,
    plot_cols: usize,
    rows: u16,
) -> (Vec<Placed>, Vec<Tick>) {
    let mut placed = Vec::new();
    let mut ticks = Vec::new();
    if plot_cols == 0 || span_hz <= 0.0 {
        return (placed, ticks);
    }
    let mut order: Vec<&StripItem> = items
        .iter()
        .filter(|it| (it.hz - center_hz).abs() <= span_hz / 2.0)
        .collect();
    order.sort_by(|a, b| b.selected.cmp(&a.selected).then(a.hz.total_cmp(&b.hz)));
    // Occupied [start, end) column ranges per row.
    let mut occupied: Vec<Vec<(usize, usize)>> = vec![Vec::new(); rows as usize];
    for it in order {
        let col = freq_to_col(it.hz - center_hz, span_hz, plot_cols);
        ticks.push(Tick {
            col,
            online: it.online,
            selected: it.selected,
        });
        let text = truncate_label(&it.label, MAX_LABEL_CHARS.min(plot_cols));
        let w = text.chars().count();
        if w == 0 {
            continue;
        }
        // Centered over the column, shifted inward at the edges.
        let start = col.saturating_sub(w / 2).min(plot_cols - w);
        let end = start + w;
        // One blank column between neighbors keeps labels readable.
        let free = |r: &Vec<(usize, usize)>| r.iter().all(|&(s, e)| end < s || e < start);
        if let Some(row) = occupied.iter().position(free) {
            occupied[row].push((start, end));
            placed.push(Placed {
                key: it.key,
                row: row as u16,
                start,
                text,
                online: it.online,
                selected: it.selected,
            });
        }
    }
    (placed, ticks)
}

/// What to draw on the strip.
pub struct StripView<'a> {
    pub items: &'a [StripItem],
    pub center_hz: f64,
    pub span_hz: f64,
    pub rows: u16,
    pub title: String,
}

fn label_style(theme: &Theme, online: bool, selected: bool) -> Style {
    let s = Style::default().fg(if online { theme.text } else { theme.dim });
    if selected {
        s.add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        s
    }
}

/// Draw the strip (border, title, labels, ticks) into `area` and return the
/// on-screen rect of every drawn label with its bookmark key (for clicks).
pub fn render_strip(
    view: &StripView,
    area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) -> Vec<(Rect, (usize, usize))> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(false))
        .title(view.title.clone());
    let inner = block.inner(area);
    block.render(area, buf);
    let mut hits = Vec::new();
    if inner.width <= PLOT_LEFT_COLS || inner.height == 0 {
        return hits;
    }
    let plot_x = inner.x + PLOT_LEFT_COLS;
    let plot_cols = plot_columns(inner.width);
    let rows = view.rows.min(inner.height);
    let (placed, ticks) = place_labels(view.items, view.center_hz, view.span_hz, plot_cols, rows);
    let bottom = area.y + area.height - 1;
    for t in ticks {
        if let Some(cell) = buf.cell_mut((plot_x + t.col as u16, bottom)) {
            cell.set_symbol("┬")
                .set_style(label_style(theme, t.online, false));
        }
    }
    for p in placed {
        let x = plot_x + p.start as u16;
        let y = inner.y + p.row;
        let w = p.text.chars().count() as u16;
        buf.set_string(x, y, &p.text, label_style(theme, p.online, p.selected));
        hits.push((Rect::new(x, y, w, 1), p.key));
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: usize, khz: f64, label: &str) -> StripItem {
        StripItem {
            key: (0, key),
            hz: khz * 1e3,
            label: label.into(),
            online: true,
            selected: false,
        }
    }

    // 100 columns over 1000 kHz centered at 1000 kHz: 10 kHz per column,
    // offset 0 maps to column round(0.5 * 99) = 50 (spectrum::freq_to_col).
    const C: f64 = 1_000_000.0;
    const S: f64 = 1_000_000.0;

    #[test]
    fn label_is_centered_over_its_column() {
        let (p, t) = place_labels(&[item(0, 1000.0, "ABCD")], C, S, 100, 2);
        assert_eq!(t[0].col, 50);
        assert_eq!(p[0].start, 48, "4 chars centered on column 50");
        assert_eq!(p[0].row, 0);
    }

    #[test]
    fn overlapping_labels_go_to_the_next_row_then_are_skipped() {
        let items = [
            item(0, 1000.0, "AAAA"),
            item(1, 1010.0, "BBBB"),
            item(2, 1020.0, "CCCC"),
        ];
        let (p, t) = place_labels(&items, C, S, 100, 2);
        let rows: Vec<(usize, u16)> = p.iter().map(|x| (x.key.1, x.row)).collect();
        assert_eq!(rows, [(0, 0), (1, 1)], "third fits on no row");
        assert_eq!(t.len(), 3, "but every bookmark in view gets a tick");
    }

    #[test]
    fn labels_need_a_blank_column_between_them() {
        // "AA" at col 50 -> [49,51); "BB" at 1030 kHz -> col 52 -> [51,53):
        // touching, no blank column between them.
        let items = [item(0, 1000.0, "AA"), item(1, 1030.0, "BB")];
        let (p, _) = place_labels(&items, C, S, 100, 2);
        assert_eq!(p[1].row, 1, "adjacent labels are not packed together");
        // 1040 kHz -> col 53 -> [52,54) leaves column 51 blank: same row.
        let items = [item(0, 1000.0, "AA"), item(1, 1040.0, "BB")];
        let (p, _) = place_labels(&items, C, S, 100, 2);
        assert_eq!(p[1].row, 0);
    }

    #[test]
    fn out_of_view_bookmarks_are_ignored_and_edges_shift_inward() {
        let items = [
            item(0, 400.0, "far"),
            item(1, 500.0, "EDGE"),
            item(2, 1500.0, "TOP"),
        ];
        let (p, t) = place_labels(&items, C, S, 100, 1);
        assert_eq!(t.len(), 2, "400 kHz is outside 500..1500 kHz");
        let edge = p.iter().find(|x| x.key.1 == 1).unwrap();
        assert_eq!(edge.start, 0, "left edge label shifted inside");
        let top = p.iter().find(|x| x.key.1 == 2).unwrap();
        assert_eq!(top.start, 97, "right edge label ends at the last column");
    }

    #[test]
    fn selected_label_is_placed_first() {
        let mut items = [item(0, 1000.0, "AAAA"), item(1, 1010.0, "BBBB")];
        items[1].selected = true;
        let (p, _) = place_labels(&items, C, S, 100, 1);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].key.1, 1, "the selected one wins the only row");
    }

    #[test]
    fn long_names_are_truncated_with_an_ellipsis() {
        assert_eq!(truncate_label("Short", 20), "Short");
        let t = truncate_label("All India Radio (AR) 0530-0630 A25", 20);
        assert_eq!(t.chars().count(), 20);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn render_draws_labels_ticks_and_returns_hit_rects() {
        let theme = Theme::builtin_default();
        let mut items = vec![item(0, 1000.0, "LIVE"), item(1, 1200.0, "OFF")];
        items[1].online = false;
        let view = StripView {
            items: &items,
            center_hz: C,
            span_hz: S,
            rows: 2,
            title: "Bookmarks".into(),
        };
        // Strip 2 rows + border; inner width 105 -> 100 plot columns.
        let area = Rect::new(0, 0, 107, 4);
        let mut buf = Buffer::empty(area);
        let hits = render_strip(&view, area, &theme, &mut buf);
        assert_eq!(hits.len(), 2);
        let (r, key) = hits[0];
        assert_eq!(key, (0, 0));
        // plot_x = 1 (border) + 5; label start 48.
        assert_eq!((r.x, r.y, r.width), (54, 1, 4));
        assert_eq!(buf[(54, 1)].symbol(), "L");
        assert_eq!(buf[(54, 1)].style().fg, Some(theme.text), "on-air: text");
        let off = hits[1].0;
        assert_eq!(
            buf[(off.x, off.y)].style().fg,
            Some(theme.dim),
            "off-air: dim"
        );
        // Tick on the bottom border at plot column 50.
        assert_eq!(buf[(56, 3)].symbol(), "┬");
    }
}
