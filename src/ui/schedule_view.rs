//! Rendering of the Schedule module body and the schedule job editor.
//!
//! Pure functions of the app state drawing into a buffer and recording hit
//! regions: every row, button, checkbox and weekday is a labeled, clickable
//! control ("no hidden shortcuts"). A recording job is drawn in the `accent`
//! role, a switched-off job `dim`, others `text`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::app::AppState;
use crate::bookmarks::editor::EditorButton;
use crate::schedule::editor::{SchedEditorField, ScheduleEditor};
use crate::schedule::module::{JOB_ROWS, SchedField};
use crate::schedule::planner::JobState;
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::render::{Tagged, record_spans, untag};
use crate::ui::theme::Theme;

/// Render the Schedule module body inside the module block at `area`.
pub fn render_schedule_contents(
    buf: &mut Buffer,
    area: Rect,
    state: &AppState,
    focused: bool,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let m = &state.schedule;
    let sched = &m.scheduler;
    let w = inner.width as usize;
    let field = m.field();
    let fstyle = |f: SchedField| theme.field_style(focused && field == f, false);
    let mut rows: Vec<Vec<Tagged>> = Vec::new();

    // [Manager]: the module's page (spec 19.2).
    rows.push(vec![(
        Span::styled("[Manager]", fstyle(SchedField::Manager)),
        Some(HitTarget::SchedField(SchedField::Manager)),
    )]);

    // Summary: what records now / what is next.
    let summary = state.sched_summary();
    let sum_style = if sched.current_job().is_some() {
        Style::default().fg(theme.accent)
    } else {
        theme.dim_style()
    };
    rows.push(vec![(Span::styled(summary, sum_style), None)]);

    // Master switch: the state is the button's label; Off stands out.
    let on_t = Some(HitTarget::SchedField(SchedField::Enabled));
    let switch_style = if focused && field == SchedField::Enabled {
        fstyle(SchedField::Enabled)
    } else if sched.enabled {
        theme.field_style(false, false)
    } else {
        Style::default().fg(theme.accent)
    };
    rows.push(vec![
        (
            Span::styled("Scheduler: ", fstyle(SchedField::Enabled)),
            on_t,
        ),
        (
            Span::styled(if sched.enabled { "[On]" } else { "[Off]" }, switch_style),
            on_t,
        ),
    ]);

    // The list shown: `●` when it is the one that runs (spec 19.8).
    let list_t = Some(HitTarget::SchedField(SchedField::List));
    let mut r: Vec<Tagged> = vec![(
        Span::styled(format!("List: {}", m.view_name()), fstyle(SchedField::List)),
        list_t,
    )];
    if m.viewing_active() {
        r.push((
            Span::styled(" ●", Style::default().fg(theme.accent)),
            list_t,
        ));
    } else {
        r.push((Span::styled(" (not active)", theme.dim_style()), list_t));
    }
    rows.push(r);

    // Job rows (windowed).
    let jobs = m.jobs();
    if jobs.is_empty() {
        rows.push(vec![(
            Span::styled("(no jobs: [Manager], F2 Add)", theme.dim_style()),
            None,
        )]);
    }
    let inp = state.sched_inputs();
    for (pos, (i, job)) in jobs
        .iter()
        .enumerate()
        .skip(m.scroll())
        .take(JOB_ROWS)
        .enumerate()
    {
        let selected = m.cursor() == Some(i);
        let recording =
            m.viewing_active() && matches!(sched.job_state(i, inp), JobState::Active { .. });
        let (mark, mut st) = if recording {
            ('●', Style::default().fg(theme.accent))
        } else if job.enabled {
            ('x', theme.text_style())
        } else {
            ('-', theme.dim_style())
        };
        if selected && focused && field == SchedField::Items {
            st = st.add_modifier(Modifier::REVERSED);
        }
        let window = if m.local_time {
            job.window.shifted(state.utc_offset_min)
        } else {
            job.window
        };
        let kind = job.kind_label();
        let muted = if job.mute { 'm' } else { ' ' };
        let clash = if m.viewing_active() && sched.conflicts(i) {
            '!'
        } else {
            ' '
        };
        let marker = if selected { '›' } else { ' ' };
        let head = format!(
            "{marker}{mark} {:<9} {kind:<2}{muted}{clash}",
            job.time_label(&window)
        );
        let name_w = w.saturating_sub(head.chars().count());
        let name: String = job.name().chars().take(name_w).collect();
        rows.push(vec![(
            Span::styled(format!("{head}{name:<name_w$}"), st),
            Some(HitTarget::SchedRow(pos)),
        )]);
    }

    // The selected job's state (its details are on the page).
    let status = m
        .cursor()
        .map(|i| state.sched_job_status(i))
        .unwrap_or_default();
    rows.push(vec![(Span::styled(status, theme.dim_style()), None)]);

    let mut lines = Vec::new();
    for (y, r) in rows.iter().enumerate() {
        if (y as u16) < inner.height {
            // A whole-row target under field and job rows.
            if let Some((_, Some(t @ (HitTarget::SchedField(_) | HitTarget::SchedRow(_))))) =
                r.first()
            {
                hits.push(Rect::new(inner.x, inner.y + y as u16, inner.width, 1), *t);
            }
            record_spans(hits, inner.x, inner.y + y as u16, inner.width, r);
        }
        lines.push(untag(r));
    }
    ratatui::widgets::Paragraph::new(lines).render(inner, buf);
}

/// Width of the editor's label column.
const LABEL_COLS: usize = 11;
/// Editor box width.
const EDITOR_WIDTH: u16 = 60;

/// Render the schedule editor centered in `area`, recording its hit regions.
pub fn render_sched_editor(
    ed: &ScheduleEditor,
    area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
    hits: &mut HitMap,
) {
    let fields = SchedEditorField::ALL;
    // Fields + the bookmark's tuning line + the hint/error line + borders.
    let w = EDITOR_WIDTH.min(area.width);
    let h = (fields.len() as u16 + 4).min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    hits.push(rect, HitTarget::SchedEdFrame);
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(true))
        .title(ed.title.clone());
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let value_w = (inner.width as usize).saturating_sub(LABEL_COLS);
    let mut y = inner.y;
    let bottom = inner.y + inner.height.saturating_sub(1);
    for f in fields {
        if y >= bottom {
            break;
        }
        let sel = ed.field() == f;
        let editing = sel && ed.is_editing();
        let style = theme.field_style(sel, editing);
        let row_target = Some(HitTarget::SchedEdField(f));
        let enabled = ed.is_enabled(f);
        let label_style = if enabled {
            theme.field_style(sel, false)
        } else {
            theme.dim_style()
        };
        let mut r: Vec<Tagged> = vec![(
            Span::styled(format!("{:<LABEL_COLS$}", f.label(ed.local)), label_style),
            row_target,
        )];
        match f {
            // "Tune only": no end and nothing to mute (blank, dim).
            SchedEditorField::End
            | SchedEditorField::Mute
            | SchedEditorField::RecNative
            | SchedEditorField::RecBaseband
                if !enabled =>
            {
                let note = match f {
                    SchedEditorField::End => "     none: tunes at Start only".to_string(),
                    SchedEditorField::Mute => "[ ] tune only: nothing to mute".to_string(),
                    _ => ed.choice_label(f),
                };
                r.push((Span::styled(note, theme.dim_style()), row_target));
            }
            SchedEditorField::Bookmark => {
                let text = match &ed.bookmark {
                    Some(b) => format!("{} ({}) [..]", b.name, b.list),
                    None => "(choose) [..]".to_string(),
                };
                r.push((Span::styled(text, style), row_target));
            }
            SchedEditorField::Days => {
                for (i, (label, on)) in ed.day_labels().into_iter().enumerate() {
                    let cur = sel && ed.day_cursor == i;
                    let st = if cur {
                        theme.field_style(true, true)
                    } else if on {
                        theme.field_style(sel, false)
                    } else {
                        theme.dim_style()
                    };
                    r.push((
                        Span::styled(format!("[{}]{label}", if on { 'x' } else { ' ' }), st),
                        Some(HitTarget::SchedEdDay(i)),
                    ));
                    r.push((Span::raw(" "), row_target));
                }
            }
            SchedEditorField::Repeat => {
                r.push((
                    Span::styled(format!("◂ {} ▸", ed.repeat.label()), style),
                    row_target,
                ));
            }
            SchedEditorField::TuneOnly
            | SchedEditorField::Record
            | SchedEditorField::RecNative
            | SchedEditorField::RecBaseband
            | SchedEditorField::Mute => {
                r.push((Span::styled(ed.choice_label(f), style), row_target));
            }
            SchedEditorField::Buttons => {
                r.clear();
                for b in EditorButton::ALL {
                    let st = theme.field_style(sel && ed.button() == b, false);
                    r.push((
                        Span::styled(b.label(), st),
                        Some(HitTarget::SchedEdButton(b)),
                    ));
                    r.push((Span::raw("  "), None));
                }
            }
            SchedEditorField::Start | SchedEditorField::End => {
                if let Some(t) = ed.text(f) {
                    for s in crate::ui::bookmark_view::text_spans(t, value_w, style, editing) {
                        r.push((s, row_target));
                    }
                    let note = if ed.start.text() == "0000" && ed.end.text() == "0000" {
                        " HHMM (all day)"
                    } else {
                        " HHMM"
                    };
                    r.push((Span::styled(note, theme.dim_style()), row_target));
                }
            }
        }
        if f != SchedEditorField::Buttons {
            hits.push(
                Rect::new(inner.x, y, inner.width, 1),
                HitTarget::SchedEdField(f),
            );
        }
        record_spans(hits, inner.x, y, inner.width, &r);
        buf.set_line(inner.x, y, &untag(&r), inner.width);
        y += 1;
        // The chosen bookmark's tuning, under its row (read-only).
        if f == SchedEditorField::Bookmark && y < bottom {
            let t = &ed.tuning;
            let info = match &ed.bookmark {
                Some(_) => format!(
                    "{:LABEL_COLS$}{} kHz {}",
                    "",
                    crate::ui::bookmark_view::khz_label(t.frequency_hz),
                    t.summary()
                ),
                None => String::new(),
            };
            buf.set_line(
                inner.x,
                y,
                &Line::from(Span::styled(info, theme.dim_style())),
                inner.width,
            );
            y += 1;
        }
    }
    let (msg, st) = match &ed.error {
        Some(e) => (e.clone(), Style::default().fg(theme.accent)),
        None => (
            "↑↓ field  ENTER edit/pick  ◂▸ change  Esc cancel".to_string(),
            theme.dim_style(),
        ),
    };
    buf.set_line(
        inner.x,
        bottom,
        &Line::from(Span::styled(msg, st)),
        inner.width,
    );
}
