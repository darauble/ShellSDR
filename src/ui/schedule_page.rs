//! The Schedule Manager page in the Work Area (spec 19.5): the switches,
//! the job table with every column, the selected job in full or the editor
//! in its place, and the function keys. Narrow (Split): the table keeps
//! Start, End, Action, Bookmark and State.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::AppState;
use crate::schedule::job::{Job, JobAction};
use crate::schedule::module::{PageField, SchedListButton, SchedSetting};
use crate::schedule::planner::JobState;
use crate::ui::bookmark_page::{fit, grouped_hz};
use crate::ui::bookmark_view::khz_label;
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::nav::Focus;
use crate::ui::render::{Tagged, record_spans, untag};
use crate::ui::theme::Theme;

/// The page's function keys (spec 19.5).
pub const FKEYS: [(u8, &str); 7] = [
    (1, "Help"),
    (2, "Add"),
    (3, "Edit"),
    (4, "Copy"),
    (5, "On/Off"),
    (6, "Move"),
    (8, "Delete"),
];

/// Width of the lists column on a wide page.
const LISTS_COLS: u16 = 24;
/// From this width the lists get a column of their own.
const LISTS_BESIDE: u16 = 120;
/// List rows shown above the table on a narrow page.
const NARROW_LIST_ROWS: usize = 3;

/// Below this width the table keeps only its main columns.
const WIDE_COLS: u16 = 100;
/// Rows of the selected job's details.
const DETAIL_ROWS: u16 = 7;
/// Rows of the day timeline (hour labels, bars).
const TIMELINE_ROWS: u16 = 2;

fn checkbox(on: bool) -> &'static str {
    if on { "[x]" } else { "[ ]" }
}

fn hhmm(t: u16) -> String {
    format!("{:02}:{:02}", t / 100, t % 100)
}

/// "Record Native + Baseband", "Record Native", "Tune only".
fn action_label(j: &Job) -> String {
    match j.action {
        JobAction::Tune => "Tune only".to_string(),
        JobAction::Record => {
            let mut outs = Vec::new();
            if j.record_native {
                outs.push("Native");
            }
            if j.record_baseband {
                outs.push("Baseband");
            }
            format!("Record {}", outs.join(" + "))
        }
    }
}

/// Draw the page body (inside its frame) in `area`.
pub fn render_schedule_page(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if area.height < 4 || area.width < 20 {
        return;
    }
    let fkeys = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    crate::ui::page::render_fkey_bar(frame, fkeys, &FKEYS, theme, hits);
    let focused = state.nav.focus == Focus::WorkArea;
    let m = &state.schedule;
    let w = area.width as usize;

    // The switches, then the clocks on the right.
    let mut row: Vec<Tagged> = vec![(Span::raw(" "), None)];
    for (i, s) in SchedSetting::ALL.iter().enumerate() {
        if i > 0 {
            row.push((Span::raw("   "), None));
        }
        let sel = focused && m.page_field() == PageField::Settings && m.setting() == *s;
        let st = theme.field_style(sel, false);
        let t = Some(HitTarget::SchedSetting(*s));
        let text = match s {
            SchedSetting::Enabled => format!(
                "Scheduler: {}",
                if m.scheduler.enabled { "[On]" } else { "[Off]" }
            ),
            SchedSetting::AutoSource => {
                format!("Auto start/stop: {}", checkbox(m.scheduler.auto_source))
            }
            SchedSetting::LocalTime => format!("Local time: {}", checkbox(m.local_time)),
        };
        let st = if *s == SchedSetting::Enabled && !m.scheduler.enabled && !sel {
            Style::default().fg(theme.accent)
        } else {
            st
        };
        row.push((Span::styled(text, st), t));
    }
    let clock = match state.local_hhmm {
        Some(l) => format!("UTC {}   Local {}", hhmm(state.utc.hhmm), hhmm(l)),
        None => format!("UTC {}", hhmm(state.utc.hhmm)),
    };
    let used: usize = row.iter().map(|(s, _)| s.content.chars().count()).sum();
    if used + clock.len() + 2 < w {
        row.push((Span::raw(" ".repeat(w - used - clock.len() - 1)), None));
        row.push((Span::styled(clock, theme.dim_style()), None));
    }
    draw_row(frame, Rect::new(area.x, area.y, area.width, 1), &row, hits);

    // The lists: a column on the left, or rows above the table.
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    let mut list_rows = lists_rows(state, focused, theme);
    let right = if body.width >= LISTS_BESIDE {
        let left = Rect::new(body.x, body.y, LISTS_COLS, body.height);
        for (y, r) in list_rows.iter().enumerate().take(left.height as usize) {
            draw_row(
                frame,
                Rect::new(left.x, left.y + y as u16, left.width, 1),
                r,
                hits,
            );
        }
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
        let n = m.list_names().len();
        let first = m.view_list().saturating_sub(NARROW_LIST_ROWS - 1);
        let mut rows = vec![list_rows.remove(0)];
        rows.extend(list_rows.drain(..n).skip(first).take(NARROW_LIST_ROWS));
        rows.extend(list_rows.into_iter().filter(|r| !r.is_empty()));
        let h = (rows.len() as u16).min(body.height);
        for (y, r) in rows.iter().enumerate().take(h as usize) {
            draw_row(
                frame,
                Rect::new(body.x, body.y + y as u16, body.width, 1),
                r,
                hits,
            );
        }
        Rect::new(
            body.x,
            body.y + h,
            body.width,
            body.height.saturating_sub(h),
        )
    };
    let area = Rect::new(right.x, right.y - 1, right.width, right.height + 2);
    let w = area.width as usize;

    // Lower part: the editor when open, else the details.
    let editor_h = state
        .sched_editor
        .as_ref()
        .map(|_| crate::schedule::editor::SchedEditorField::ALL.len() as u16 + 4);
    let body_h = area.height - 2; // the settings row and the F-keys
    let lower_h = editor_h
        .unwrap_or(DETAIL_ROWS)
        .saturating_add(1)
        .min(body_h.saturating_sub(3));
    let table = Rect::new(area.x, area.y + 1, area.width, body_h - lower_h);
    let lower = Rect::new(area.x, table.y + table.height, area.width, lower_h);
    render_table(frame, table, state, focused, theme, hits);

    if lower.height == 0 {
        return;
    }
    frame
        .buffer_mut()
        .set_string(lower.x, lower.y, "─".repeat(w), theme.border_style(false));
    let below = Rect::new(lower.x, lower.y + 1, lower.width, lower.height - 1);
    if let Some(ed) = &state.sched_editor {
        // The editor in place of the details (no popup, spec 19.5).
        crate::ui::schedule_view::render_sched_editor(ed, below, theme, frame.buffer_mut(), hits);
        return;
    }
    // The day timeline above the details (spec 19.5).
    let tl = Rect::new(
        below.x,
        below.y,
        below.width,
        TIMELINE_ROWS.min(below.height),
    );
    render_timeline(frame, tl, state, theme);
    let below = Rect::new(
        below.x,
        below.y + tl.height + 1,
        below.width,
        below.height.saturating_sub(tl.height + 1),
    );
    let Some(i) = m.cursor() else {
        return;
    };
    let job = &m.jobs()[i];
    let t = job.resolve_tuning(&state.bookmarks.store);
    let w_shown = if m.local_time {
        job.window.shifted(state.utc_offset_min)
    } else {
        job.window
    };
    let zone = if m.local_time { "local" } else { "UTC" };
    let days = match w_shown.days_label() {
        d if d.is_empty() => "every day".to_string(),
        d => d,
    };
    let gone = if job.find_bookmark(&state.bookmarks.store).is_none() {
        " (bookmark gone)"
    } else {
        ""
    };
    let lines = vec![
        Line::from(Span::styled(
            format!(
                "{}: {} {} {}, {}, {} {} {zone}",
                job.name(),
                grouped_hz(t.frequency_hz),
                t.vfo_type,
                t.summary(),
                action_label(job),
                days,
                job.time_label(&w_shown),
            ),
            theme.text_style(),
        )),
        Line::from(Span::styled(
            format!(
                "Bookmark: {} / {}{gone}   Repeat: {}{}",
                job.bookmark.list,
                job.bookmark.name,
                job.repeat.label(),
                if job.mute { "   Mutes the VFO" } else { "" }
            ),
            theme.dim_style(),
        )),
        Line::from(Span::styled(
            format!("State: {}", state.sched_job_status(i)),
            theme.dim_style(),
        )),
        Line::from(Span::styled(
            last_runs_label(state, job.name()),
            theme.dim_style(),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), below);
}

/// `Last runs: Mo 18:00-18:20; Su 18:00-18:05 skipped; ...` from the
/// Report's log (spec 19.5).
fn last_runs_label(state: &AppState, name: &str) -> String {
    let Some(r) = &state.report else {
        return "Last runs: (no report)".to_string();
    };
    let clock = r.clock(state.utc_offset_min);
    let runs = state.sched_last_runs(name, 4);
    if runs.is_empty() {
        return "Last runs: none in the last week".to_string();
    }
    let day = |t: i64| {
        let d = clock.moment(t).day;
        if d == r.today_key() {
            "today".to_string()
        } else {
            d[5..].to_string()
        }
    };
    let hm = |t: i64| clock.time_of_day(t)[..5].to_string();
    let parts: Vec<String> = runs
        .iter()
        .map(|run| {
            let mut s = match run.end {
                Some(e) => format!("{} {}-{}", day(run.start), hm(run.start), hm(e)),
                None => format!("{} {} running", day(run.start), hm(run.start)),
            };
            if run.skipped {
                s.push_str(" skipped");
            }
            if run.silent {
                s.push_str(" nothing decoded");
            }
            s
        })
        .collect();
    format!("Last runs: {}", parts.join("; "))
}

/// What a timeline cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Free,
    Record,
    /// The selected job's window.
    Selected,
    Tune,
}

/// The day `[start, start + 1 day)` in `cols` cells: Record windows (the
/// selected job's apart), Tune job starts.
fn timeline_cells(jobs: &[Job], selected: Option<usize>, start: u64, cols: usize) -> Vec<Cell> {
    let step = 86_400.0 / cols as f64;
    (0..cols)
        .map(|c| {
            let a = start + (c as f64 * step) as u64;
            let b = start + ((c + 1) as f64 * step) as u64;
            let mid = (a + b) / 2;
            let mut cell = Cell::Free;
            for (i, j) in jobs.iter().enumerate() {
                if !j.can_fire() {
                    continue;
                }
                if j.records() {
                    if j.window.occurrence_at(mid).is_some() {
                        if selected == Some(i) {
                            return Cell::Selected;
                        }
                        cell = Cell::Record;
                    }
                } else if cell == Cell::Free
                    && j.fire_window()
                        .last_start_at_or_before(b.saturating_sub(1))
                        .is_some_and(|s| s >= a)
                {
                    cell = Cell::Tune;
                }
            }
            cell
        })
        .collect()
}

/// The timeline of today (in the shown clock): hour labels, then the bars
/// with now reversed.
fn render_timeline(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if area.height < 2 || area.width < 40 {
        return;
    }
    let m = &state.schedule;
    let off = if m.local_time {
        i64::from(state.utc_offset_min) * 60
    } else {
        0
    };
    let now = state.now_secs as i64;
    let start = ((now + off).div_euclid(86_400) * 86_400 - off).max(0) as u64;
    let label = if m.local_time {
        "Today (local) "
    } else {
        "Today (UTC)   "
    };
    let lw = label.chars().count() as u16;
    // Whole cells per hour: 1, 2 or 4.
    let per_hour = match area.width.saturating_sub(lw + 1) / 24 {
        0 => 1,
        n => n.min(4),
    } as usize;
    let cols = 24 * per_hour;
    // The list shown (also a list that is not active: planning).
    let cells = timeline_cells(m.jobs(), m.cursor(), start, cols);
    // Hour labels every 2 hours (every 4 when cramped).
    let every = if per_hour == 1 { 4 } else { 2 };
    let mut labels = " ".repeat(cols);
    for h in (0..24).step_by(every) {
        let at = h * per_hour;
        let text = format!("{h:02}");
        labels.replace_range(at..(at + 2).min(cols), &text[..(cols - at).min(2)]);
    }
    let buf = frame.buffer_mut();
    buf.set_string(area.x, area.y, label, theme.dim_style());
    buf.set_string(area.x + lw, area.y, labels, theme.dim_style());
    let now_col = ((now - start as i64).max(0) as usize * cols / 86_400).min(cols - 1);
    for (c, cell) in cells.iter().enumerate() {
        let (ch, mut st) = match cell {
            Cell::Free => ('·', theme.dim_style()),
            Cell::Record => ('█', theme.text_style()),
            Cell::Selected => ('█', Style::default().fg(theme.accent)),
            Cell::Tune => ('╷', theme.text_style()),
        };
        if c == now_col {
            st = st.add_modifier(Modifier::REVERSED);
        }
        buf.set_string(area.x + lw + c as u16, area.y + 1, ch.to_string(), st);
    }
}

/// The lists column: a header, one row per list (`●` the active one), the
/// list buttons.
fn lists_rows(state: &AppState, focused: bool, theme: &Theme) -> Vec<Vec<Tagged>> {
    let m = &state.schedule;
    let pf = m.page_field();
    let mut rows: Vec<Vec<Tagged>> = vec![vec![(
        Span::styled(format!("{:<17}Jobs", "Lists"), theme.dim_style()),
        None,
    )]];
    for (i, name) in m.list_names().iter().enumerate() {
        let cur = m.view_list() == i;
        let mut st = theme.field_style(cur && focused && pf == PageField::Lists, false);
        if i == m.active_list() && !(cur && focused && pf == PageField::Lists) {
            st = st.fg(theme.accent);
        }
        let marker = if cur { '›' } else { ' ' };
        let active = if i == m.active_list() { '●' } else { ' ' };
        let text = format!("{marker}{active} {} {:>4}", fit(name, 14), m.list_len(i));
        rows.push(vec![(
            Span::styled(text, st),
            Some(HitTarget::SchedPageList(i)),
        )]);
    }
    rows.push(Vec::new());
    for pair in [
        [SchedListButton::New, SchedListButton::Rename],
        [SchedListButton::Delete, SchedListButton::Activate],
    ] {
        let mut r: Vec<Tagged> = Vec::new();
        for (i, b) in pair.iter().enumerate() {
            if i > 0 {
                r.push((Span::raw(" "), None));
            }
            let sel = focused && pf == PageField::ListButtons && m.list_button() == *b;
            r.push((
                Span::styled(b.label(), theme.field_style(sel, false)),
                Some(HitTarget::SchedListButton(*b)),
            ));
        }
        rows.push(r);
    }
    rows
}

/// The job table: a header row, then one row per job (the window follows
/// the selection).
fn render_table(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    focused: bool,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if area.height < 2 {
        return;
    }
    let m = &state.schedule;
    let sched = &m.scheduler;
    let w = area.width as usize;
    let wide = area.width >= WIDE_COLS;
    let header = if wide {
        format!(
            "    {:<5} {:<5} {:<13} {:<7} {:<5} {:<22} {:>9} {:<8} State",
            "Start", "End", "Days", "Repeat", "Do", "Bookmark", "kHz", "Type"
        )
    } else {
        format!(
            "    {:<5} {:<5} {:<5} {:<18} State",
            "Start", "End", "Do", "Bookmark"
        )
    };
    draw_row(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        &[(Span::styled(fit(&header, w), theme.dim_style()), None)],
        hits,
    );
    let jobs = m.jobs();
    let rows = (area.height - 1) as usize;
    if jobs.is_empty() {
        draw_row(
            frame,
            Rect::new(area.x, area.y + 1, area.width, 1),
            &[(Span::styled("(no jobs: F2 Add)", theme.dim_style()), None)],
            hits,
        );
        return;
    }
    let cursor = m.cursor();
    let first = cursor.map_or(0, |c| (c + 1).saturating_sub(rows));
    let inp = state.sched_inputs();
    for (n, (i, job)) in jobs.iter().enumerate().skip(first).take(rows).enumerate() {
        let sel = cursor == Some(i);
        let live = m.viewing_active();
        let running = live && matches!(sched.job_state(i, inp), JobState::Active { .. });
        let (mark, mut st) = if running {
            ('●', Style::default().fg(theme.accent))
        } else if job.enabled {
            ('x', theme.text_style())
        } else {
            ('-', theme.dim_style())
        };
        if sel && focused && m.page_field() == PageField::Table {
            st = st.add_modifier(Modifier::REVERSED);
        }
        let win = if m.local_time {
            job.window.shifted(state.utc_offset_min)
        } else {
            job.window
        };
        let start = format!("{:04}", win.start);
        let end = if job.action == JobAction::Tune {
            String::new()
        } else {
            format!("{:04}", win.end)
        };
        let act = format!(
            "{}{}{}",
            job.kind_label(),
            if job.mute { "m" } else { "" },
            if live && sched.conflicts(i) { "!" } else { "" }
        );
        let t = job.resolve_tuning(&state.bookmarks.store);
        let status = state.sched_job_status(i);
        let marker = if sel { '›' } else { ' ' };
        let text = if wide {
            let days = win.days_label();
            let days = if days.is_empty() {
                "every day".to_string()
            } else {
                days
            };
            format!(
                "{marker} {mark} {start:<5} {end:<5} {} {:<7} {:<5} {} {:>9} {} {status}",
                fit(&days, 13),
                job.repeat.label(),
                act,
                fit(job.name(), 22),
                khz_label(t.frequency_hz),
                fit(&t.vfo_type, 8),
            )
        } else {
            format!(
                "{marker} {mark} {start:<5} {end:<5} {act:<5} {} {status}",
                fit(job.name(), 18)
            )
        };
        draw_row(
            frame,
            Rect::new(area.x, area.y + 1 + n as u16, area.width, 1),
            &[(
                Span::styled(fit(&text, w), st),
                Some(HitTarget::SchedPageRow(i)),
            )],
            hits,
        );
    }
}

/// Draw one tagged row, the whole width a target when it is a job row.
fn draw_row(frame: &mut Frame, r: Rect, row: &[Tagged], hits: &mut HitMap) {
    if let Some((_, Some(t @ (HitTarget::SchedPageRow(_) | HitTarget::SchedPageList(_))))) =
        row.first()
    {
        hits.push(r, *t);
    }
    record_spans(hits, r.x, r.y, r.width, row);
    frame.buffer_mut().set_line(r.x, r.y, &untag(row), r.width);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timeline_shows_windows_the_selected_one_and_tune_starts() {
        use crate::schedule::job::BookmarkRef;
        use crate::schedule::window::Window;
        let mon = 1_794_182_400; // 2026-11-09 00:00 UTC
        let rec = |id, s, e| Job {
            id,
            bookmark: BookmarkRef {
                list: "L".into(),
                name: format!("j{id}"),
            },
            window: Window::new(s, e, [true; 7]),
            ..Job::default()
        };
        let tune = Job {
            action: JobAction::Tune,
            ..rec(3, 1600, 1600)
        };
        let jobs = vec![rec(1, 600, 700), rec(2, 1800, 1830), tune];
        let c = timeline_cells(&jobs, Some(1), mon, 48);
        assert_eq!(c[12], Cell::Record, "06:00-06:30");
        assert_eq!(c[13], Cell::Record);
        assert_eq!(c[14], Cell::Free, "07:00 is the end");
        assert_eq!(c[36], Cell::Selected, "18:00, the selected job");
        assert_eq!(c[32], Cell::Tune, "16:00 tune");
        assert_eq!(c[33], Cell::Free);
    }

    #[test]
    fn action_labels_name_the_outputs() {
        let mut j = Job {
            action: JobAction::Record,
            record_native: true,
            record_baseband: true,
            ..Job::default()
        };
        assert_eq!(action_label(&j), "Record Native + Baseband");
        j.record_baseband = false;
        assert_eq!(action_label(&j), "Record Native");
        j.action = JobAction::Tune;
        assert_eq!(action_label(&j), "Tune only");
        assert_eq!(hhmm(905), "09:05");
    }
}
