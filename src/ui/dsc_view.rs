//! The DSC Work Area view (0.2.1, spec 14.6): the ncurses decoder's screen
//! inside the VFO view: a menu bar (F9), the status box with this VFO's
//! channel, the alert banner, the message list, the detail pane and the
//! function-key bar.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{AppState, RunState};
use crate::dsc::message::Severity;
use crate::dsc::service::DetailLayout;
use crate::dsc_app::{DscCmd, DscPart, FKEYS, MENUS};
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::nav::Focus;
use crate::ui::theme::Theme;

/// Height of the status box (border, header, the channel row, border).
const STATUS_HEIGHT: u16 = 4;

/// The view's regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DscLayout {
    pub menu: Rect,
    pub status: Rect,
    pub banner: Rect,
    pub list: Rect,
    pub detail: Rect,
    pub fkeys: Rect,
}

impl DscLayout {
    /// Message rows the list shows (inside its border, below its header).
    pub fn list_rows(&self) -> usize {
        self.list.height.saturating_sub(3) as usize
    }

    /// Lines the detail shows.
    pub fn detail_rows(&self) -> usize {
        self.detail.height.saturating_sub(2) as usize
    }
}

/// Lay out the view in `area` (inside the VFO view's frame).
pub fn layout(area: Rect, detail_open: bool, how: DetailLayout, banner: bool) -> DscLayout {
    let row = |y: u16, h: u16| Rect::new(area.x, y, area.width, h);
    let bottom = area.y + area.height;
    let menu = row(area.y, 1.min(area.height));
    let fkeys = if area.height >= 2 {
        row(bottom - 1, 1)
    } else {
        Rect::default()
    };
    let mut y = menu.y + menu.height;
    let end = fkeys.y.max(y);
    let take = |y: &mut u16, h: u16| {
        let h = h.min(end.saturating_sub(*y));
        let r = row(*y, h);
        *y += h;
        r
    };
    let status = take(&mut y, STATUS_HEIGHT);
    let banner = if banner {
        take(&mut y, 1)
    } else {
        Rect::default()
    };
    let rest = end.saturating_sub(y);
    let body = take(&mut y, rest);
    if !detail_open || body.is_empty() {
        return DscLayout {
            menu,
            status,
            banner,
            list: body,
            detail: Rect::default(),
            fkeys,
        };
    }
    // Terminal cells are about twice as tall as wide: a body more than
    // about 2.2x wider than tall is "wide".
    let beside = match how {
        DetailLayout::Horizontal => true,
        DetailLayout::Vertical => false,
        DetailLayout::Auto => body.width as f32 > 2.2 * body.height as f32,
    };
    let (list, detail) = if beside {
        let lw = body.width * 11 / 20;
        (
            Rect::new(body.x, body.y, lw, body.height),
            Rect::new(body.x + lw, body.y, body.width - lw, body.height),
        )
    } else {
        let lh = body.height / 2;
        (
            Rect::new(body.x, body.y, body.width, lh),
            Rect::new(body.x, body.y + lh, body.width, body.height - lh),
        )
    };
    DscLayout {
        menu,
        status,
        banner,
        list,
        detail,
        fkeys,
    }
}

/// The layout for the current state.
pub fn layout_for(area: Rect, state: &AppState) -> DscLayout {
    let (how, banner) = match &state.dsc {
        Some(s) => (s.prefs().detail_layout, s.alert().is_some()),
        None => (DetailLayout::Auto, false),
    };
    layout(area, state.dsc_ui.detail_open, how, banner)
}

/// The detail text's lines wrapped to `width`.
pub fn detail_lines(state: &AppState, width: usize) -> Vec<String> {
    let Some(fields) = state.dsc.as_ref().and_then(|s| s.detail_fields()) else {
        return Vec::new();
    };
    let label_w = fields
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = Vec::new();
    for (l, v) in fields {
        let first = if l.is_empty() {
            v.clone()
        } else {
            format!("{l:label_w$} : {v}")
        };
        wrap_into(&mut out, &first, width, label_w + 3);
    }
    out
}

/// Wrap `text` at `width` columns; continuation lines are indented.
fn wrap_into(out: &mut Vec<String>, text: &str, width: usize, indent: usize) {
    let width = width.max(8);
    let indent = indent.min(width / 2);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut first = true;
    while i < chars.len() || first {
        let room = if first { width } else { width - indent };
        let end = (i + room).min(chars.len());
        let mut cut = end;
        if end < chars.len()
            && let Some(sp) = chars[i..end].iter().rposition(|c| *c == ' ')
            && sp > room / 3
        {
            cut = i + sp + 1;
        }
        let piece: String = chars[i..cut].iter().collect();
        out.push(if first {
            piece
        } else {
            format!("{}{}", " ".repeat(indent), piece)
        });
        i = cut;
        first = false;
    }
}

fn severity_style(s: Severity, theme: &Theme) -> Style {
    match s {
        Severity::Distress => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Severity::Urgency => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Severity::Safety => Style::default().fg(theme.accent),
        Severity::Routine => theme.text_style(),
    }
}

/// `s` cut or padded to exactly `w` columns.
fn cell(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let n = out.chars().count();
    out.extend(std::iter::repeat_n(' ', w - n));
    out
}

/// Draw the DSC view into `area`.
pub fn render_dsc_view(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    hits.push(area, HitTarget::DscView);
    let l = layout_for(area, state);
    render_menu_bar(frame, l.menu, state, theme, hits);
    render_status(frame, l.status, state, theme);
    if let Some(a) = state.dsc.as_ref().and_then(|s| s.alert()) {
        let text = format!(" {}: {}  [a: acknowledge] ", a.summary, a.body);
        frame.render_widget(
            Paragraph::new(cell(&text, l.banner.width as usize)).style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            l.banner,
        );
        hits.push(l.banner, HitTarget::DscBanner);
    }
    render_list(frame, l.list, state, theme, hits);
    if !l.detail.is_empty() {
        render_detail(frame, l.detail, state, theme, hits);
    }
    render_fkeys(frame, l.fkeys, state, theme, hits);
    if let Some((top, item)) = state.dsc_ui.menu {
        render_menu(frame, area, l.menu, top, item, state, theme, hits);
    }
}

fn render_menu_bar(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    if r.is_empty() {
        return;
    }
    let mut spans = Vec::new();
    let mut x = r.x;
    for (i, (title, _)) in MENUS.iter().enumerate() {
        let t = format!(" {title} ");
        let w = t.chars().count() as u16;
        let open = state.dsc_ui.menu.is_some_and(|(top, _)| top == i);
        let style = if open {
            Style::default().fg(theme.editing_fg).bg(theme.editing_bg)
        } else {
            theme.text_style()
        };
        hits.push(
            Rect::new(x, r.y, w.min(r.x + r.width - x.min(r.x + r.width)), 1),
            HitTarget::DscMenuTitle(i),
        );
        spans.push(Span::styled(t, style));
        x += w;
        if x >= r.x + r.width {
            break;
        }
    }
    let used = (x - r.x) as usize;
    let right = match &state.dsc_ui.notice {
        Some((n, _)) => n.clone(),
        None => "F9 menu".to_string(),
    };
    let room = (r.width as usize).saturating_sub(used + 1);
    let right: String = right.chars().take(room).collect();
    let pad = (r.width as usize).saturating_sub(used + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    let style = if state.dsc_ui.notice.is_some() {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.dim_style()
    };
    spans.push(Span::styled(right, style));
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}

fn render_status(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme) {
    if r.height < 2 {
        return;
    }
    let live = state.run == RunState::Running;
    let count = state.dsc.as_ref().map_or(0, |s| s.count());
    let mode = if live {
        Span::styled(
            " LIVE* ",
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" Stopped ", theme.text_style().add_modifier(Modifier::BOLD))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(false))
        .title(Line::from(vec![
            mode,
            Span::styled(format!(" msgs: {count} "), theme.text_style()),
        ]));
    let inner = block.inner(r);
    frame.render_widget(block, r);
    if inner.is_empty() {
        return;
    }
    let w = inner.width as usize;
    // Channel 12, Status 13, level 8, Source the rest.
    let (cw, sw, lw) = (12usize, 13usize, 8usize);
    let srcw = w.saturating_sub(cw + sw + lw + 3).max(1);
    let header = format!(
        "{} {} {} {}",
        cell("Channel", cw),
        cell("Source", srcw),
        cell("Status", sw),
        cell("Level", lw)
    );
    let v = state.radio.active();
    let source = state
        .source
        .source_label()
        .unwrap_or_else(|| "No source".to_string());
    let meter = state.dsc_ui.meter.filter(|_| live);
    let lookup = state.dsc.as_ref().is_some_and(|s| s.lookup_pending());
    let (status, hot) = match meter {
        None => ("idle", false),
        Some(m) if m.decoding => ("DECODING...", true),
        _ if lookup => ("API_REQ...", true),
        Some(m) if m.level > 0.6 => ("activity", false),
        Some(_) => ("silent", false),
    };
    let level = meter.map_or(0.0, |m| m.level.clamp(0.0, 1.0));
    let bars = (level * lw as f32).round() as usize;
    let status_style = if hot {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.text_style()
    };
    let lines = vec![
        Line::from(Span::styled(
            cell(&header, w),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                format!(
                    "{} {} ",
                    cell(&crate::dsc_app::channel_label(v.frequency_hz), cw),
                    cell(&source, srcw)
                ),
                theme.text_style(),
            ),
            Span::styled(cell(status, sw), status_style),
            Span::styled(
                format!(" {}", cell(&"|".repeat(bars), lw)),
                theme.text_style(),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Column widths of the message list for `w` columns: (time, sev+ecc,
/// source, format, category, from, to, subject); zero = not shown.
fn columns(w: usize) -> [usize; 8] {
    let base = [8, 3, 0, 4, 8, 9, 9, 0];
    let mut c = base;
    let used = |c: &[usize; 8]| c.iter().filter(|x| **x > 0).map(|x| x + 1).sum::<usize>();
    if used(&c) + 12 <= w {
        c[2] = 11;
    }
    let rest = w.saturating_sub(used(&c) + 1);
    if rest >= 8 {
        c[7] = rest;
    }
    c
}

fn render_list(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    if r.height < 3 {
        return;
    }
    let focused = state.nav.focus == Focus::WorkArea && state.dsc_ui.part == DscPart::Messages;
    let Some(svc) = state.dsc.as_ref() else {
        return;
    };
    let view = svc.view();
    let rows = r.height.saturating_sub(3) as usize;
    let n = view.len();
    let pages = n.div_ceil(rows.max(1)).max(1);
    let sel = view.selected_row();
    let page = sel.map_or(pages, |s| s / rows.max(1) + 1);
    let mut title = format!(
        " Messages: {} of {n}  Pages: {page} of {pages}  File: {} ",
        sel.map_or(0, |s| s + 1),
        svc.day()
    );
    if !view.query().is_empty() {
        title.push_str(&format!("[/{}] ", view.query()));
    }
    if view.distress_only() {
        title.push_str("[D/U only] ");
    }
    if !view.follow() {
        title.push_str("[Hold] ");
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .title(title);
    let inner = block.inner(r);
    frame.render_widget(block, r);
    hits.push(inner, HitTarget::DscList);
    let w = inner.width as usize;
    let c = columns(w);
    let heads = [
        "TIME", "S", "SRC", "FMT", "CATEGORY", "FROM", "TO", "SUBJECT",
    ];
    let join = |cells: [String; 8]| {
        cells
            .iter()
            .zip(c)
            .filter(|(_, w)| *w > 0)
            .map(|(s, w)| cell(s, w))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut lines = vec![Line::from(Span::styled(
        cell(&join(heads.map(String::from)), w),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ))];
    let top = state.dsc_ui.list_top.min(n.saturating_sub(rows));
    for row in top..(top + rows).min(n) {
        let Some(e) = view.entry(row) else { break };
        let lr = crate::dsc::presentation::list_row(&e.record);
        let sev = format!(
            "{}{}",
            lr.severity,
            if lr.ecc == "OK" { "  " } else { "!!" }
        );
        let text = cell(
            &join([
                lr.time,
                sev,
                lr.source,
                lr.format,
                lr.category,
                lr.from,
                lr.to,
                lr.subject,
            ]),
            w,
        );
        let mut style = if lr.ecc == "OK" {
            severity_style(e.record.message.severity, theme)
        } else {
            theme.dim_style()
        };
        if Some(row) == sel {
            style = if focused {
                Style::default().fg(theme.editing_fg).bg(theme.editing_bg)
            } else {
                style.add_modifier(Modifier::REVERSED)
            };
        }
        hits.push(
            Rect::new(inner.x, inner.y + 1 + (row - top) as u16, inner.width, 1),
            HitTarget::DscRow(row),
        );
        lines.push(Line::from(Span::styled(text, style)));
    }
    if n == 0 {
        lines.push(Line::from(Span::styled(
            "(no messages today yet)",
            theme.dim_style(),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_detail(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    let focused = state.nav.focus == Focus::WorkArea && state.dsc_ui.part == DscPart::Detail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .title(" Message details ");
    let inner = block.inner(r);
    frame.render_widget(block, r);
    hits.push(r, HitTarget::DscDetail);
    if inner.is_empty() {
        return;
    }
    let all = detail_lines(state, inner.width as usize);
    let rows = inner.height as usize;
    let top = state
        .dsc_ui
        .detail_scroll
        .min(all.len().saturating_sub(rows));
    let lines: Vec<Line> = all
        .iter()
        .enumerate()
        .skip(top)
        .take(rows)
        .map(|(i, l)| {
            let style = if i == 0 {
                theme.text_style().add_modifier(Modifier::BOLD)
            } else {
                theme.text_style()
            };
            Line::from(Span::styled(l.clone(), style))
        })
        .collect();
    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new("(no message selected)").style(theme.dim_style()),
            inner,
        );
        return;
    }
    frame.render_widget(Paragraph::new(lines), inner);
    if all.len() > rows && r.width > 12 {
        let ind = format!(" {}/{} ", top + 1, all.len() - rows + 1);
        let x = r.x + r.width - 1 - ind.chars().count() as u16;
        frame.render_widget(
            Paragraph::new(ind).style(theme.dim_style()),
            Rect::new(x, r.y + r.height - 1, r.width - (x - r.x) - 1, 1),
        );
    }
}

/// The label of a function key now.
fn fkey_label(cmd: DscCmd, state: &AppState) -> String {
    match cmd {
        DscCmd::Layout => state
            .dsc
            .as_ref()
            .map_or("Layout", |s| s.prefs().detail_layout.label())
            .to_string(),
        DscCmd::Follow => {
            if state.dsc_toggle_state(DscCmd::Follow).unwrap_or(true) {
                "Follow".into()
            } else {
                "Hold".into()
            }
        }
        DscCmd::Help => "Help".into(),
        DscCmd::Save => "Save".into(),
        DscCmd::Copy => "Copy".into(),
        DscCmd::Detail => "Detail".into(),
        DscCmd::Search => "Search".into(),
        DscCmd::Menu => "Menu".into(),
        _ => String::new(),
    }
}

fn render_fkeys(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    if r.is_empty() {
        return;
    }
    let n = FKEYS.len() as u16;
    let cellw = (r.width / n).max(4);
    let mut spans = Vec::new();
    let label_style = Style::default().fg(theme.editing_fg).bg(theme.editing_bg);
    for (i, (k, cmd)) in FKEYS.iter().enumerate() {
        let x = r.x + i as u16 * cellw;
        if x >= r.x + r.width {
            break;
        }
        let w = if i as u16 == n - 1 {
            r.x + r.width - x
        } else {
            cellw.min(r.x + r.width - x)
        };
        let num = k.to_string();
        let room = (w as usize).saturating_sub(num.len());
        let mut text = fkey_label(*cmd, state);
        if text.chars().count() > room {
            text = text
                .replace("Auto View", "Auto")
                .replace("Horizontal", "Horiz")
                .replace("Vertical", "Vert");
        }
        let label = cell(&text, room);
        spans.push(Span::styled(
            num,
            theme.text_style().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(label, label_style));
        hits.push(Rect::new(x, r.y, w, 1), HitTarget::DscFKey(*k));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}

#[allow(clippy::too_many_arguments)]
fn render_menu(
    frame: &mut Frame,
    area: Rect,
    bar: Rect,
    top: usize,
    item: usize,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let x0 = bar.x
        + MENUS[..top]
            .iter()
            .map(|(t, _)| t.chars().count() as u16 + 2)
            .sum::<u16>();
    let items = MENUS[top].1;
    let rows: Vec<String> = items
        .iter()
        .map(|it| {
            let state_txt = match state.dsc_toggle_state(it.cmd) {
                Some(true) => "On",
                Some(false) => "Off",
                None => "",
            };
            let right = format!("{} {state_txt}", it.key);
            format!("{:<24}{:>8}", it.label, right.trim())
        })
        .collect();
    let w = (rows.iter().map(|r| r.chars().count()).max().unwrap_or(10) as u16 + 4).min(area.width);
    let h = (items.len() as u16 + 2).min(area.height.saturating_sub(1));
    let x = x0.min(area.x + area.width - w);
    let r = Rect::new(x, bar.y + 1, w, h);
    frame.render_widget(Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(true));
    let inner = block.inner(r);
    frame.render_widget(block, r);
    hits.push(r, HitTarget::DscMenuFrame);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .take(inner.height as usize)
        .map(|(i, t)| {
            hits.push(
                Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                HitTarget::DscMenuItem(top, i),
            );
            let style = if i == item {
                Style::default().fg(theme.editing_fg).bg(theme.editing_bg)
            } else {
                theme.text_style()
            };
            Line::from(Span::styled(
                cell(&format!(" {t}"), inner.width as usize),
                style,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_stacks_menu_status_list_and_function_keys() {
        let l = layout(Rect::new(0, 0, 80, 30), false, DetailLayout::Auto, false);
        assert_eq!(l.menu, Rect::new(0, 0, 80, 1));
        assert_eq!(l.status, Rect::new(0, 1, 80, 4));
        assert!(l.banner.is_empty() && l.detail.is_empty());
        assert_eq!(l.list, Rect::new(0, 5, 80, 24));
        assert_eq!(l.fkeys, Rect::new(0, 29, 80, 1));
        assert_eq!(l.list_rows(), 21);
        let b = layout(Rect::new(0, 0, 80, 30), false, DetailLayout::Auto, true);
        assert_eq!(b.banner, Rect::new(0, 5, 80, 1));
        assert_eq!(b.list.y, 6);
    }

    #[test]
    fn the_detail_goes_beside_on_a_wide_view_and_below_on_a_tall_one() {
        let wide = layout(Rect::new(0, 0, 160, 30), true, DetailLayout::Auto, false);
        assert_eq!(wide.list.y, wide.detail.y, "beside");
        assert_eq!(wide.list.width + wide.detail.width, 160);
        let tall = layout(Rect::new(0, 0, 44, 40), true, DetailLayout::Auto, false);
        assert_eq!(tall.list.x, tall.detail.x, "below");
        let forced = layout(
            Rect::new(0, 0, 44, 40),
            true,
            DetailLayout::Horizontal,
            false,
        );
        assert_eq!(forced.list.y, forced.detail.y);
    }

    #[test]
    fn a_tiny_view_does_not_panic() {
        for h in 0..8 {
            let _ = layout(Rect::new(0, 0, 20, h), true, DetailLayout::Auto, true);
        }
    }

    #[test]
    fn columns_drop_source_and_subject_when_narrow() {
        let narrow = columns(44);
        assert_eq!(narrow[2], 0, "no SRC");
        let wide = columns(100);
        assert_eq!(wide[2], 11);
        assert!(wide[7] >= 8, "SUBJECT takes the rest");
    }

    #[test]
    fn wrapping_indents_continuations() {
        let mut out = Vec::new();
        wrap_into(
            &mut out,
            "Raw : 120 120 0 23 71 0 0 108 63 60 25 89 20",
            20,
            6,
        );
        assert!(out.len() > 1);
        assert!(out.iter().all(|l| l.chars().count() <= 20), "{out:?}");
        assert!(out[1].starts_with("      "));
    }
}
