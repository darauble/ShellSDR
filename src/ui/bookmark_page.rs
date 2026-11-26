//! The Bookmarks Manager page in the Work Area (spec 19.4): the lists with
//! their strip visibility and buttons, the selected list as a table, the
//! selected bookmark in full, or the editor in its place, and the function
//! keys. Wide: the lists in a column on the left; narrow (Split): the lists
//! above the table, which keeps kHz, Name and Type.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::AppState;
use crate::bookmarks::manager::{BookmarkManager, ListButton, PageField};
use crate::bookmarks::model::Bookmark;
use crate::ui::bookmark_view::{khz_label, text_spans};
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::nav::Focus;
use crate::ui::render::{Tagged, record_spans, untag};
use crate::ui::theme::Theme;

/// The page's function keys (spec 19.4).
pub const FKEYS: [(u8, &str); 8] = [
    (1, "Help"),
    (2, "Add"),
    (3, "Edit"),
    (4, "Copy"),
    (5, "Move"),
    (6, "Tune"),
    (7, "Find"),
    (8, "Delete"),
];

/// Width of the lists column on a wide page.
const LISTS_COLS: u16 = 26;
/// Below this inner width the lists go above the table.
const WIDE_COLS: u16 = 90;
/// List rows shown above the table on a narrow page.
const NARROW_LIST_ROWS: usize = 4;
/// Rows of the selected bookmark's details.
const DETAIL_ROWS: u16 = 4;

/// Columns of the table, dropped from the right when the page is narrow.
struct Col {
    title: &'static str,
    width: usize,
}

const COLS: [Col; 7] = [
    Col {
        title: "kHz",
        width: 9,
    },
    Col {
        title: "Name",
        width: 22,
    },
    Col {
        title: "Type",
        width: 8,
    },
    Col {
        title: "Settings",
        width: 20,
    },
    Col {
        title: "UTC",
        width: 10,
    },
    Col {
        title: "Days",
        width: 15,
    },
    Col {
        title: "Jobs",
        width: 4,
    },
];

/// How many table columns fit in `width` (at least kHz and Name).
fn cols_for(width: usize) -> usize {
    let mut used = 2; // the selection marker
    let mut n = 0;
    for c in &COLS {
        if used + c.width + 1 > width && n >= 2 {
            break;
        }
        used += c.width + 1;
        n += 1;
    }
    n
}

/// "every day", or the days it airs (`Su Mo Tu`).
fn days_label(b: &Bookmark) -> String {
    if b.days.iter().all(|d| *d) {
        return "every day".to_string();
    }
    crate::bookmarks::model::DAY_LABELS
        .iter()
        .zip(b.days)
        .filter(|(_, on)| *on)
        .map(|(l, _)| *l)
        .collect::<Vec<_>>()
        .join(" ")
}

fn utc_label(b: &Bookmark) -> String {
    if b.has_window() {
        format!("{:04}-{:04}", b.start_utc, b.end_utc)
    } else {
        "all day".to_string()
    }
}

/// The schedule jobs that use bookmark `name` of list `list`.
fn jobs_of<'a>(state: &'a AppState, list: &str, name: &str) -> Vec<&'a crate::schedule::job::Job> {
    state
        .schedule
        .jobs()
        .iter()
        .filter(|j| j.bookmark.list == list && j.bookmark.name == name)
        .collect()
}

/// Pad or cut `s` to `w` columns.
pub(crate) fn fit(s: &str, w: usize) -> String {
    let cut: String = s.chars().take(w).collect();
    format!("{cut:<w$}")
}

/// `4 207 500 Hz`.
pub(crate) fn grouped_hz(hz: u64) -> String {
    let digits = hz.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    format!("{out} Hz")
}

/// Draw the page body (inside its frame) in `area`.
pub fn render_bookmark_page(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if area.height < 3 {
        return;
    }
    let fkeys = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    crate::ui::page::render_fkey_bar(frame, fkeys, &FKEYS, theme, hits);
    let body = Rect::new(area.x, area.y, area.width, area.height - 1);
    let focused = state.nav.focus == Focus::WorkArea;
    let bm = &state.bookmarks;
    let mut left_rows = lists_rows(bm, focused, theme);
    let right = if body.width >= WIDE_COLS {
        let left = Rect::new(body.x, body.y, LISTS_COLS, body.height);
        draw_rows(frame, left, &left_rows, hits);
        // A vertical rule between the columns.
        for y in body.y..body.y + body.height {
            frame
                .buffer_mut()
                .set_string(body.x + LISTS_COLS, y, "│", theme.border_style(false));
        }
        Rect::new(
            body.x + LISTS_COLS + 2,
            body.y,
            body.width - LISTS_COLS - 2,
            body.height,
        )
    } else {
        // Narrow: a window of the lists, the buttons, then the table.
        let n_lists = bm.store.lists.len();
        let sel = bm.list_index().unwrap_or(0);
        let first = sel.saturating_sub(NARROW_LIST_ROWS - 1);
        let keep = 1 + n_lists.min(NARROW_LIST_ROWS);
        let mut rows = vec![left_rows.remove(0)];
        rows.extend(left_rows.drain(..n_lists).skip(first).take(keep - 1));
        // the blank line under the lists is dropped
        rows.extend(left_rows.into_iter().filter(|r| !r.is_empty()));
        let h = (rows.len() as u16).min(body.height);
        draw_rows(frame, Rect::new(body.x, body.y, body.width, h), &rows, hits);
        let y = body.y + h;
        Rect::new(body.x, y, body.width, body.height.saturating_sub(h))
    };
    render_table_side(frame, right, state, focused, theme, hits);
}

/// The lists column: header, one row per list, buttons, strip rows.
fn lists_rows(bm: &BookmarkManager, focused: bool, theme: &Theme) -> Vec<Vec<Tagged>> {
    let pf = bm.page_field();
    let mut rows: Vec<Vec<Tagged>> = vec![vec![(
        Span::styled(format!("{:<18}Strip", "Lists"), theme.dim_style()),
        None,
    )]];
    let sel = bm.list_index();
    for (i, l) in bm.store.lists.iter().enumerate() {
        let cur = sel == Some(i);
        let marker = if cur { '›' } else { ' ' };
        let mut st = theme.field_style(false, false);
        if cur && focused && pf == PageField::Lists {
            st = theme.field_style(true, false);
        }
        let mark = if l.show_on_strip { '◉' } else { '○' };
        let text = format!(
            "{marker} {} {:>4}  {mark}",
            fit(&l.name, 13),
            l.bookmarks.len()
        );
        rows.push(vec![(
            Span::styled(text, st),
            Some(HitTarget::BmPageList(i)),
        )]);
    }
    rows.push(Vec::new());
    let shown = bm.current_list().is_none_or(|l| l.show_on_strip);
    for pair in [
        [ListButton::New, ListButton::Rename],
        [ListButton::Delete, ListButton::Visibility],
    ] {
        let mut r: Vec<Tagged> = Vec::new();
        for (i, b) in pair.iter().enumerate() {
            if i > 0 {
                r.push((Span::raw(" "), None));
            }
            let s = focused && pf == PageField::ListButtons && bm.list_button() == *b;
            let label = match b {
                ListButton::Delete => "[Delete]",
                other => other.label(shown),
            };
            r.push((
                Span::styled(label, theme.field_style(s, false)),
                Some(HitTarget::BmListButton(*b)),
            ));
        }
        rows.push(r);
    }
    let st = theme.field_style(focused && pf == PageField::StripRows, false);
    let t = Some(HitTarget::BmPageField(PageField::StripRows));
    rows.push(vec![
        (Span::styled("Strip rows: ", st), t),
        (Span::styled(format!("[◂ {} ▸]", bm.strip_rows), st), t),
    ]);
    rows
}

/// Draw tagged rows top-down in `area`, recording their targets (a list or
/// table row is clickable across the whole width).
fn draw_rows(frame: &mut Frame, area: Rect, rows: &[Vec<Tagged>], hits: &mut HitMap) {
    for (y, r) in rows.iter().enumerate() {
        if y as u16 >= area.height {
            break;
        }
        let yy = area.y + y as u16;
        if let Some((_, Some(t @ (HitTarget::BmPageList(_) | HitTarget::BmPageRow(_))))) = r.first()
        {
            hits.push(Rect::new(area.x, yy, area.width, 1), *t);
        }
        record_spans(hits, area.x, yy, area.width, r);
        frame
            .buffer_mut()
            .set_line(area.x, yy, &untag(r), area.width);
    }
}

/// The right side: the header row, the table, then the details or the
/// editor.
fn render_table_side(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    focused: bool,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if area.height < 3 || area.width < 10 {
        return;
    }
    let bm = &state.bookmarks;
    let pf = bm.page_field();
    let w = area.width as usize;

    // Header: the list, the count, Sort and Find.
    let view = bm.view();
    let total = bm.current_list().map_or(0, |l| l.bookmarks.len());
    let name = bm
        .current_list()
        .map_or("(no list)".to_string(), |l| l.name.clone());
    let sort_st = theme.field_style(focused && pf == PageField::Sort, false);
    let sort_t = Some(HitTarget::BmPageField(PageField::Sort));
    let editing = bm.is_filter_editing();
    let find_st = theme.field_style(focused && pf == PageField::Find, editing);
    let find_t = Some(HitTarget::BmPageField(PageField::Find));
    let mut head: Vec<Tagged> = vec![
        (
            Span::styled(
                format!("{name}: {} of {total}   ", view.len()),
                theme.text_style(),
            ),
            None,
        ),
        (Span::styled("Sort: ", sort_st), sort_t),
        (
            Span::styled(format!("[{}]", bm.sort.label()), sort_st),
            sort_t,
        ),
        (Span::raw("   "), None),
        (Span::styled("Find(/): ", find_st), find_t),
    ];
    let used: usize = head.iter().map(|(s, _)| s.content.chars().count()).sum();
    head.extend(
        text_spans(&bm.filter, w.saturating_sub(used).max(4), find_st, editing)
            .into_iter()
            .map(|s| (s, find_t)),
    );
    draw_rows(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        &[head],
        hits,
    );

    // The lower part: the editor when open, else the details.
    let editor_h = state
        .bm_editor
        .as_ref()
        .map(|e| e.visible_fields().len() as u16 + 3);
    // + the rule above it; the editor may squeeze the table to two rows.
    let lower_h = editor_h
        .unwrap_or(DETAIL_ROWS)
        .saturating_add(1)
        .min(area.height.saturating_sub(4));
    let table = Rect::new(area.x, area.y + 1, area.width, area.height - 1 - lower_h);
    let lower = Rect::new(area.x, table.y + table.height, area.width, lower_h);

    // Table.
    let ncols = cols_for(w);
    let mut title = String::from("  ");
    for c in COLS.iter().take(ncols) {
        let t = if c.title == "kHz" {
            format!("{:>width$} ", c.title, width = c.width)
        } else {
            format!("{} ", fit(c.title, c.width))
        };
        title.push_str(&t);
    }
    let mut rows: Vec<Vec<Tagged>> = vec![vec![(Span::styled(title, theme.dim_style()), None)]];
    let body_rows = table.height.saturating_sub(1) as usize;
    let cursor = bm.cursor();
    // The window follows the selection (no state of its own).
    let first = cursor.map_or(0, |c| (c + 1).saturating_sub(body_rows));
    let list = bm.current_list();
    if view.is_empty() {
        let note = if bm.filter.is_empty() {
            "(no bookmarks: F2 Add)"
        } else {
            "(no match)"
        };
        rows.push(vec![(Span::styled(note, theme.dim_style()), None)]);
    }
    for (pos, &i) in view.iter().enumerate().skip(first).take(body_rows) {
        let b = &list.expect("non-empty view has a list").bookmarks[i];
        let sel = cursor == Some(pos);
        let mut st = Style::default().fg(if b.is_online(state.utc) {
            theme.text
        } else {
            theme.dim
        });
        if sel && focused && pf == PageField::Table {
            st = st.add_modifier(Modifier::REVERSED);
        }
        let jobs = jobs_of(state, &list.expect("has a list").name, &b.name).len();
        let cells = [
            khz_label(b.frequency_hz),
            b.name.clone(),
            b.vfo_type.clone(),
            b.summary(),
            utc_label(b),
            days_label(b),
            if jobs > 0 {
                jobs.to_string()
            } else {
                String::new()
            },
        ];
        let mut text = String::from(if sel { "› " } else { "  " });
        for (c, v) in COLS.iter().zip(cells.iter()).take(ncols) {
            if c.title == "kHz" {
                text.push_str(&format!("{v:>width$} ", width = c.width));
            } else {
                text.push_str(&format!("{} ", fit(v, c.width)));
            }
        }
        rows.push(vec![(
            Span::styled(fit(&text, w), st),
            Some(HitTarget::BmPageRow(pos)),
        )]);
    }
    draw_rows(frame, table, &rows, hits);

    if lower.height == 0 {
        return;
    }
    let rule = "─".repeat(w);
    frame
        .buffer_mut()
        .set_string(lower.x, lower.y, &rule, theme.border_style(false));
    let below = Rect::new(lower.x, lower.y + 1, lower.width, lower.height - 1);
    if let Some(ed) = &state.bm_editor {
        // The editor in place of the details (no popup, spec 19.4).
        crate::ui::bookmark_view::render_editor(ed, below, theme, frame.buffer_mut(), hits);
        return;
    }
    let Some(b) = bm.selected_bookmark() else {
        return;
    };
    let list_name = bm.current_list().map_or(String::new(), |l| l.name.clone());
    let jobs: Vec<String> = jobs_of(state, &list_name, &b.name)
        .iter()
        .map(|j| {
            format!("{} {}", j.time_label(&j.window), j.kind_label())
                .trim_end()
                .to_string()
        })
        .collect();
    let dash = |s: &str| {
        if s.is_empty() {
            "-".to_string()
        } else {
            s.to_string()
        }
    };
    let lines = vec![
        Line::from(Span::styled(
            format!(
                "{}  {}  {}  {}, {}",
                b.name,
                grouped_hz(b.frequency_hz),
                b.summary(),
                utc_label(b),
                days_label(b)
            ),
            theme.text_style(),
        )),
        Line::from(Span::styled(
            format!("Geo: {}", dash(&b.geoinfo)),
            theme.dim_style(),
        )),
        Line::from(Span::styled(
            format!("Notes: {}", dash(&b.notes)),
            theme.dim_style(),
        )),
        Line::from(Span::styled(
            format!(
                "Jobs: {}",
                if jobs.is_empty() {
                    "-".to_string()
                } else {
                    jobs.join("; ")
                }
            ),
            theme.dim_style(),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), below);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_tables_keep_khz_name_and_type() {
        assert_eq!(cols_for(200), COLS.len());
        assert_eq!(cols_for(46), 3, "kHz, Name, Type");
        assert_eq!(cols_for(20), 2, "never fewer than kHz and Name");
    }

    #[test]
    fn days_and_window_labels() {
        let mut b = Bookmark::default();
        assert_eq!(days_label(&b), "every day");
        assert_eq!(utc_label(&b), "all day");
        b.days = [true, true, false, false, false, false, false];
        b.start_utc = 1330;
        b.end_utc = 1400;
        assert_eq!(days_label(&b), "Su Mo");
        assert_eq!(utc_label(&b), "1330-1400");
        assert_eq!(grouped_hz(4_207_500), "4 207 500 Hz");
        assert_eq!(grouped_hz(518_000), "518 000 Hz");
    }
}
