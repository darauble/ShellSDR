//! Rendering of the Bookmarks module body and the bookmark editor form.
//!
//! Step 7. Pure functions of the bookmark state drawing into a buffer and
//! recording mouse hit regions (every row, button, digit and weekday is a
//! labeled, clickable control: "no hidden shortcuts"). Bookmark rows are
//! colored like the strip: on-air `text`, off-air `dim`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::bookmarks::editor::{BookmarkEditor, EditorButton, EditorField};
use crate::bookmarks::manager::{BookmarkField, BookmarkManager, PanelButton};
use crate::bookmarks::model::{Bookmark, UtcNow};
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::render::{Tagged, frequency_digit_spans, record_spans, untag};
use crate::ui::text_input::TextInput;
use crate::ui::theme::Theme;

/// Width of the right-aligned frequency column of a bookmark row.
const FREQ_COLS: usize = 8;

/// A frequency in kHz without trailing zeros ("8939", "7030.5").
pub fn khz_label(hz: u64) -> String {
    if hz.is_multiple_of(1000) {
        format!("{}", hz / 1000)
    } else {
        let s = format!("{:.3}", hz as f64 / 1e3);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// The details line for the selected bookmark: its type's summary (mode,
/// bandwidth; `?NAVTEX` for a type this build lacks) and schedule.
pub fn details_label(b: &Bookmark) -> String {
    format!("{}  {}", b.summary(), b.schedule_label())
}

/// On-air / off-air color for a bookmark.
fn air_style(theme: &Theme, online: bool) -> Style {
    Style::default().fg(if online { theme.text } else { theme.dim })
}

/// A text value as spans, with a reversed cursor cell while editing.
pub(crate) fn text_spans(
    t: &TextInput,
    width: usize,
    style: Style,
    editing: bool,
) -> Vec<Span<'static>> {
    if !editing {
        let shown: String = t.text().chars().take(width).collect();
        return vec![Span::styled(shown, style)];
    }
    let (shown, cur) = t.window(width.max(1));
    let mut out: Vec<Span<'static>> = shown
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let st = if i == cur || t.is_all_selected() {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            Span::styled(c.to_string(), st)
        })
        .collect();
    if cur >= shown.chars().count() {
        out.push(Span::styled(" ", style.add_modifier(Modifier::REVERSED)));
    }
    out
}

/// Render the Bookmarks module body inside the module block at `area`.
pub fn render_bookmarks_contents(
    buf: &mut Buffer,
    area: Rect,
    bm: &BookmarkManager,
    utc: UtcNow,
    focused: bool,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let w = inner.width as usize;
    let field = bm.field();
    let fstyle = |f: BookmarkField| theme.field_style(focused && field == f, false);
    let mut rows: Vec<Vec<Tagged>> = Vec::new();

    // `[Manager] [Quick Add]` (spec 19.4).
    let mut r: Vec<Tagged> = Vec::new();
    for (i, b) in PanelButton::ALL.iter().enumerate() {
        if i > 0 {
            r.push((Span::raw(" "), None));
        }
        let sel = focused && field == BookmarkField::Manager && bm.panel_button() == *b;
        r.push((
            Span::styled(b.label(), theme.field_style(sel, false)),
            Some(HitTarget::BmPanelButton(*b)),
        ));
    }
    rows.push(r);

    // List selector.
    let list_text = match bm.current_list() {
        Some(l) if l.show_on_strip => format!("List: {}", l.name),
        Some(l) => format!("List: {} (hidden)", l.name),
        None => "List: (none)".to_string(),
    };
    rows.push(vec![(
        Span::styled(list_text, fstyle(BookmarkField::List)),
        Some(HitTarget::BmField(BookmarkField::List)),
    )]);

    // Find filter.
    let editing = bm.is_filter_editing();
    let find_style = theme.field_style(focused && field == BookmarkField::Find, editing);
    let find_target = Some(HitTarget::BmField(BookmarkField::Find));
    let mut r: Vec<Tagged> = vec![(Span::styled("Find(/): ", find_style), find_target)];
    r.extend(
        text_spans(&bm.filter, w.saturating_sub(9), find_style, editing)
            .into_iter()
            .map(|s| (s, find_target)),
    );
    rows.push(r);

    // Bookmark rows (windowed).
    let view = bm.view();
    let cursor = bm.cursor();
    let scroll = bm.scroll();
    let list = bm.current_list();
    if view.is_empty() {
        let note = if bm.filter.is_empty() {
            "(no bookmarks)"
        } else {
            "(no match)"
        };
        rows.push(vec![(Span::styled(note, theme.dim_style()), None)]);
    }
    for (pos, &i) in view.iter().enumerate().skip(scroll).take(bm.window_rows()) {
        let b = &list.expect("non-empty view has a list").bookmarks[i];
        let selected = cursor == Some(pos);
        let mut st = air_style(theme, b.is_online(utc));
        if selected && focused && field == BookmarkField::Items {
            st = st.add_modifier(Modifier::REVERSED);
        }
        let marker = if selected { '›' } else { ' ' };
        let name_w = w.saturating_sub(FREQ_COLS + 2);
        let name: String = b.name.chars().take(name_w).collect();
        let text = format!(
            "{marker}{:>FREQ_COLS$} {name:<name_w$}",
            khz_label(b.frequency_hz)
        );
        rows.push(vec![(Span::styled(text, st), Some(HitTarget::BmRow(pos)))]);
    }

    // Details of the selected bookmark.
    let details = bm
        .selected_bookmark()
        .map(details_label)
        .unwrap_or_default();
    rows.push(vec![(Span::styled(details, theme.dim_style()), None)]);

    let mut lines = Vec::new();
    for (y, r) in rows.iter().enumerate() {
        if (y as u16) < inner.height {
            // A whole-row target under field and bookmark rows, so a click
            // anywhere on the row (not only on its text) hits it.
            if let Some((_, Some(t @ (HitTarget::BmField(_) | HitTarget::BmRow(_))))) = r.first() {
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

/// Render the bookmark editor centered in `area`, recording its hit regions.
pub fn render_editor(
    ed: &BookmarkEditor,
    area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
    hits: &mut HitMap,
) {
    let fields = ed.visible_fields();
    let w = EDITOR_WIDTH.min(area.width);
    let h = (fields.len() as u16 + 3).min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    hits.push(rect, HitTarget::EditorFrame);
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
    for (y, f) in fields.iter().enumerate() {
        if y as u16 >= inner.height {
            break;
        }
        let sel = ed.field() == *f;
        let editing = sel && ed.is_editing();
        let style = theme.field_style(sel, editing);
        let row_target = Some(HitTarget::EditorField(*f));
        let label: String = ed.label(*f).chars().take(LABEL_COLS - 1).collect();
        let mut r: Vec<Tagged> = vec![(
            Span::styled(
                format!("{label:<LABEL_COLS$}"),
                theme.field_style(sel, false),
            ),
            row_target,
        )];
        match f {
            EditorField::List => {
                let name = ed.list_names.get(ed.list).cloned().unwrap_or_default();
                r.push((Span::styled(format!("◂ {name} ▸"), style), row_target));
            }
            EditorField::Type => {
                let text = match ed.type_def() {
                    Some(_) => format!("◂ {} ▸", ed.type_label()),
                    // Not in this build: read-only, its settings kept as is.
                    None => format!("{} (not in this build, kept)", ed.type_label()),
                };
                r.push((Span::styled(text, style), row_target));
            }
            EditorField::Setting(i) if !ed.is_text_field(*f) => {
                let text = match ed.setting_field(*i as usize).map(|f| f.kind) {
                    Some(crate::vfo::types::FieldKind::Bool) => ed.setting_display(*i as usize),
                    _ => format!("◂ {} ▸", ed.setting_display(*i as usize)),
                };
                r.push((Span::styled(text, style), row_target));
            }
            EditorField::Setting(i) if !editing => {
                r.push((
                    Span::styled(ed.setting_display(*i as usize), style),
                    row_target,
                ));
            }
            EditorField::Frequency => {
                for (s, pos) in frequency_digit_spans(&ed.freq, style, editing, theme) {
                    r.push((s, pos.map(HitTarget::EditorDigit).or(row_target)));
                }
            }
            EditorField::Days => {
                for (i, (label, on)) in ed.day_labels().into_iter().enumerate() {
                    let cur = sel && ed.day_cursor == i;
                    let st = if cur {
                        theme.field_style(true, true)
                    } else if on {
                        theme.field_style(sel, false)
                    } else {
                        theme.dim_style()
                    };
                    let mark = if on { 'x' } else { ' ' };
                    r.push((
                        Span::styled(format!("[{mark}]{label}"), st),
                        Some(HitTarget::EditorDay(i)),
                    ));
                    r.push((Span::raw(" "), row_target));
                }
            }
            EditorField::Buttons => {
                r.clear();
                for b in EditorButton::ALL {
                    let st = theme.field_style(sel && ed.button() == b, false);
                    r.push((
                        Span::styled(b.label(), st),
                        Some(HitTarget::EditorButton(b)),
                    ));
                    r.push((Span::raw("  "), None));
                }
            }
            text => {
                if let Some(t) = ed.text(*text) {
                    for s in text_spans(t, value_w, style, editing) {
                        r.push((s, row_target));
                    }
                    let unit = match text {
                        EditorField::Setting(i) => match ed
                            .setting_field(*i as usize)
                            .map(|f| f.kind)
                        {
                            Some(
                                crate::vfo::types::FieldKind::Number { unit, .. }
                                | crate::vfo::types::FieldKind::Integer { unit, .. },
                            ) if !unit.is_empty() => format!(" {unit}"),
                            Some(crate::vfo::types::FieldKind::FrequencyHz) => " Hz".to_string(),
                            _ => String::new(),
                        },
                        EditorField::Start | EditorField::End => " HHMM".to_string(),
                        _ => String::new(),
                    };
                    if !unit.is_empty() {
                        r.push((Span::styled(unit, theme.dim_style()), row_target));
                    }
                }
            }
        }
        let yy = inner.y + y as u16;
        if *f != EditorField::Buttons {
            hits.push(
                Rect::new(inner.x, yy, inner.width, 1),
                HitTarget::EditorField(*f),
            );
        }
        record_spans(hits, inner.x, yy, inner.width, &r);
        buf.set_line(inner.x, yy, &untag(&r), inner.width);
    }
    // Error or hint on the last row.
    let y = inner.y + inner.height - 1;
    let (msg, st) = match &ed.error {
        Some(e) => (e.clone(), Style::default().fg(theme.accent)),
        None => (
            "↑↓ field  ENTER edit/toggle  ◂▸ change  Esc cancel".to_string(),
            theme.dim_style(),
        ),
    };
    buf.set_line(inner.x, y, &Line::from(Span::styled(msg, st)), inner.width);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn khz_labels_trim_zeros() {
        assert_eq!(khz_label(8_939_000), "8939");
        assert_eq!(khz_label(7_030_500), "7030.5");
        assert_eq!(khz_label(102_600_000), "102600");
        assert_eq!(khz_label(1_234), "1.234");
    }

    #[test]
    fn details_show_mode_bandwidth_and_schedule() {
        use crate::vfo::model::Mode;
        use crate::vfo::types::voice::VoiceSettings;
        let mut usb = VoiceSettings::for_mode(Mode::Usb);
        usb.bandwidth_hz = 2_300.0;
        let b = Bookmark {
            start_utc: 530,
            end_utc: 630,
            ..Bookmark::voice("x", 1, usb)
        };
        assert_eq!(details_label(&b), "USB 2.3 kHz  0530-0630");
        let cw = Bookmark::voice("x", 1, VoiceSettings::for_mode(Mode::Cw));
        assert_eq!(details_label(&cw), "CW 200 Hz 800Hz  all day");
        let am = Bookmark::voice("x", 1, VoiceSettings::for_mode(Mode::Am));
        assert!(details_label(&am).contains("10 kHz"));
        let dsc = Bookmark {
            vfo_type: "NAVTEX".into(),
            ..Bookmark::default()
        };
        assert_eq!(details_label(&dsc), "?NAVTEX  all day");
    }
}
