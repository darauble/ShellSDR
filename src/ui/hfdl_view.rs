//! The HFDL Work Area view (0.4.5, HFDL-ACARS seed `spec/07-ui/`, mockup
//! `shellsdr-workarea.png`): a menu bar (F9), the status line, the channel
//! box, the tabs (messages, aircraft, ground stations), the alert banner,
//! the list with the detail beside or below it, and the function-key bar.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{AppState, RunState};
use crate::hfdl::msg::{AlertKind, Severity};
use crate::hfdl::service::{ChannelState, HfdlService, Tab, aircraft_label, type_label};
use crate::hfdl_app::{FKEYS, HfdlCmd, HfdlPart, MENUS};
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::nav::Focus;
use crate::ui::theme::Theme;

/// Rows of channels shown at most (two per row).
const CHANNEL_ROWS: usize = 3;

/// The view's regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HfdlLayout {
    pub menu: Rect,
    pub status: Rect,
    pub channels: Rect,
    pub tabs: Rect,
    pub banner: Rect,
    pub list: Rect,
    pub detail: Rect,
    pub fkeys: Rect,
}

impl HfdlLayout {
    /// Rows the list shows (inside its border, below its header).
    pub fn list_rows(&self) -> usize {
        self.list.height.saturating_sub(3) as usize
    }

    pub fn detail_rows(&self) -> usize {
        self.detail.height.saturating_sub(2) as usize
    }
}

/// Lay out the view in `area`: `channel_rows` rows of channels (0 = one
/// line saying there are none).
pub fn layout(area: Rect, detail_open: bool, banner: bool, channel_rows: usize) -> HfdlLayout {
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
    let status = take(&mut y, 1);
    // small screens give the channels up first
    let ch_h = if area.height >= 20 {
        channel_rows.clamp(1, CHANNEL_ROWS) as u16 + 3
    } else {
        0
    };
    let channels = take(&mut y, ch_h);
    let tabs = take(&mut y, 1);
    let banner = if banner {
        take(&mut y, 1)
    } else {
        Rect::default()
    };
    let rest = end.saturating_sub(y);
    let body = take(&mut y, rest);
    let (list, detail) = if !detail_open || body.is_empty() {
        (body, Rect::default())
    } else if body.width as f32 > 2.2 * body.height as f32 {
        let lw = body.width * 3 / 5;
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
    HfdlLayout {
        menu,
        status,
        channels,
        tabs,
        banner,
        list,
        detail,
        fkeys,
    }
}

/// The layout for the current state.
pub fn layout_for(area: Rect, state: &AppState) -> HfdlLayout {
    let (banner, n) = match &state.hfdl {
        Some(s) => (s.alert().is_some(), s.channels().len()),
        None => (false, 0),
    };
    let open =
        state.hfdl_ui.detail_open && state.hfdl.as_ref().is_some_and(|s| s.tab == Tab::Messages);
    layout(area, open, banner, n.div_ceil(2))
}

/// The detail text's lines wrapped to `width`.
pub fn detail_lines(state: &AppState, width: usize) -> Vec<(String, bool)> {
    let Some(fields) = state.hfdl.as_ref().and_then(|s| s.detail_fields()) else {
        return Vec::new();
    };
    let label_w = fields
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(10);
    let mut out = Vec::new();
    for (i, (l, v)) in fields.iter().enumerate() {
        let text = if l.is_empty() {
            format!("{:label_w$}   {v}", "")
        } else {
            format!("{l:label_w$} : {v}")
        };
        let mut lines = Vec::new();
        wrap_into(&mut lines, &text, width, label_w + 3);
        for x in lines {
            out.push((x, i == 0));
        }
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
        Severity::Emergency => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Severity::Caution => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Severity::Routine => theme.text_style(),
        Severity::Link => theme.dim_style(),
    }
}

/// `s` cut or padded to exactly `w` columns.
fn cell(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let n = out.chars().count();
    out.extend(std::iter::repeat_n(' ', w - n));
    out
}

/// `s` right-aligned in `w` columns.
fn rcell(s: &str, w: usize) -> String {
    let s: String = s.chars().take(w).collect();
    format!("{s:>w$}")
}

fn header_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD)
}

fn selected_style(theme: &Theme, focused: bool, base: Style) -> Style {
    if focused {
        Style::default().fg(theme.editing_fg).bg(theme.editing_bg)
    } else {
        base.add_modifier(Modifier::REVERSED)
    }
}

/// Draw the HFDL view into `area`.
pub fn render_hfdl_view(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    hits.push(area, HitTarget::HfdlView);
    let Some(svc) = state.hfdl.as_ref() else {
        return;
    };
    let l = layout_for(area, state);
    render_menu_bar(frame, l.menu, state, theme, hits);
    render_status(frame, l.status, state, svc, theme);
    render_channels(frame, l.channels, state, svc, theme);
    render_tabs(frame, l.tabs, svc, theme, hits);
    if let Some(a) = svc.alert() {
        let text = format!(" {}  [a: acknowledge] ", a.summary);
        let bg = if a.kind == AlertKind::Emergency {
            Color::Red
        } else {
            Color::Yellow
        };
        frame.render_widget(
            Paragraph::new(cell(&text, l.banner.width as usize)).style(
                Style::default()
                    .fg(if bg == Color::Red {
                        Color::White
                    } else {
                        Color::Black
                    })
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            l.banner,
        );
        hits.push(l.banner, HitTarget::HfdlBanner);
    }
    match svc.tab {
        Tab::Messages => render_messages(frame, l.list, state, svc, theme, hits),
        Tab::Aircraft => render_aircraft(frame, l.list, state, svc, theme, hits),
        Tab::Stations => render_stations(frame, l.list, state, svc, theme, hits),
    }
    if !l.detail.is_empty() {
        render_detail(frame, l.detail, state, theme, hits);
    }
    render_fkeys(frame, l.fkeys, state, theme, hits);
    if let Some((top, item)) = state.hfdl_ui.menu {
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
        let open = state.hfdl_ui.menu.is_some_and(|(top, _)| top == i);
        let style = if open {
            Style::default().fg(theme.editing_fg).bg(theme.editing_bg)
        } else {
            theme.text_style()
        };
        hits.push(
            Rect::new(x, r.y, w.min((r.x + r.width).saturating_sub(x)), 1),
            HitTarget::HfdlMenuTitle(i),
        );
        spans.push(Span::styled(t, style));
        x += w;
        if x >= r.x + r.width {
            break;
        }
    }
    let used = (x - r.x) as usize;
    let right = match &state.hfdl_ui.notice {
        Some((n, _)) => n.clone(),
        None => "F9 menu".to_string(),
    };
    let room = (r.width as usize).saturating_sub(used + 1);
    let right: String = right.chars().take(room).collect();
    let pad = (r.width as usize).saturating_sub(used + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    let style = if state.hfdl_ui.notice.is_some() {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.dim_style()
    };
    spans.push(Span::styled(right, style));
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}

fn now_utc() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn render_status(frame: &mut Frame, r: Rect, state: &AppState, svc: &HfdlService, theme: &Theme) {
    if r.is_empty() {
        return;
    }
    let live = state.run == RunState::Running;
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
    let now = svc.clock(now_utc());
    let ch = svc.channels();
    let decoding = ch
        .values()
        .filter(|c| live && c.state(now) == ChannelState::Decoding)
        .count();
    let p = svc.prefs();
    let flag = |on: bool| if on { 'x' } else { ' ' };
    let text = format!(
        " msgs: {}  channels: {} ({decoding} decoding)  aircraft: {}  alerts: {}  [{}] hide link  [{}] hide joined{}",
        svc.total(),
        ch.len(),
        svc.aircraft_count(),
        svc.alerts_count(),
        flag(p.filter_link),
        flag(p.filter_joined),
        if svc.alerts_only() {
            "  [x] alerts only"
        } else {
            ""
        }
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            mode,
            Span::styled(
                cell(&text, (r.width as usize).saturating_sub(8)),
                theme.text_style(),
            ),
        ])),
        r,
    );
}

fn render_channels(frame: &mut Frame, r: Rect, state: &AppState, svc: &HfdlService, theme: &Theme) {
    if r.height < 3 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(false));
    let inner = block.inner(r);
    let live = state.run == RunState::Running;
    let now = svc.clock(now_utc());
    let mut chans: Vec<(&u32, &crate::hfdl::service::ChannelInfo)> =
        svc.channels().iter().collect();
    // decoding, then activity, then the busiest
    let rank = |c: &crate::hfdl::service::ChannelInfo| match c.state(now) {
        ChannelState::Decoding => 0,
        ChannelState::Activity => 1,
        ChannelState::Silent => 2,
    };
    chans.sort_by(|a, b| {
        rank(a.1)
            .cmp(&rank(b.1))
            .then(b.1.frames_ok.cmp(&a.1.frames_ok))
            .then(a.0.cmp(b.0))
    });
    let rows = inner.height.saturating_sub(1) as usize;
    let shown = (rows * 2).min(chans.len());
    let hidden = chans.len() - shown;
    let block = if hidden > 0 {
        block.title_bottom(format!(" … {hidden} more channels "))
    } else {
        block
    };
    frame.render_widget(block, r);
    if inner.is_empty() {
        return;
    }
    let w = inner.width as usize;
    let half = w / 2;
    let gs_w = half.saturating_sub(6 + 10 + 7 + 5 + 9 + 5).max(4);
    let col = |s: &[String; 6]| {
        format!(
            "{} {} {} {} {} {}",
            cell(&s[0], 6),
            cell(&s[1], gs_w),
            cell(&s[2], 10),
            rcell(&s[3], 6),
            rcell(&s[4], 4),
            cell(&s[5], 9)
        )
    };
    let head = col(&[
        "Chan".into(),
        "GS".into(),
        "State".into(),
        "Frames".into(),
        "Err".into(),
        "Last".into(),
    ]);
    let mut lines = vec![Line::from(Span::styled(
        cell(&format!("{}{}", cell(&head, half), head), w),
        header_style(theme),
    ))];
    if chans.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no channels: tune the VFO to an HFDL band center, F1 for the list)",
            theme.dim_style(),
        )));
    }
    let stations = |k: u32| -> String {
        crate::hfdl::stations::all()
            .iter()
            .filter(|s| s.freqs_khz.iter().any(|f| f.round() as u32 == k))
            .map(|s| format!("{} {}", s.id, short_name(&s.name)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    for rw in 0..rows.min(shown.div_ceil(2)) {
        let mut spans = Vec::new();
        for c in 0..2 {
            let Some((k, info)) = chans.get(rw * 2 + c) else {
                continue;
            };
            let st = if live {
                info.state(now)
            } else {
                ChannelState::Silent
            };
            let last = info
                .last_ok
                .map(|t| crate::dsc::time::local_time(t.floor() as i64, state.utc_offset_min * 60))
                .unwrap_or_default();
            let text = col(&[
                k.to_string(),
                stations(**k),
                if live {
                    st.name().into()
                } else {
                    "idle".into()
                },
                info.frames_ok.to_string(),
                info.frames_err.to_string(),
                last,
            ]);
            let style = match st {
                ChannelState::Decoding if live => Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                ChannelState::Activity if live => theme.text_style(),
                _ => theme.dim_style(),
            };
            spans.push(Span::styled(cell(&text, half), style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A station name without its country (`Shannon, Ireland` -> `Shannon`).
fn short_name(n: &str) -> &str {
    n.split(',').next().unwrap_or(n).trim()
}

fn render_tabs(frame: &mut Frame, r: Rect, svc: &HfdlService, theme: &Theme, hits: &mut HitMap) {
    if r.is_empty() {
        return;
    }
    let tabs = [
        (Tab::Messages, "Messages".to_string()),
        (
            Tab::Aircraft,
            format!("Aircraft ({})", svc.aircraft_count()),
        ),
        (
            Tab::Stations,
            format!("Ground stations ({})", crate::hfdl::stations::count()),
        ),
    ];
    let mut spans = vec![Span::raw(" ")];
    let mut x = r.x + 1;
    for (i, (t, label)) in tabs.iter().enumerate() {
        let text = if *t == svc.tab {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        };
        let w = text.chars().count() as u16;
        let style = if *t == svc.tab {
            theme.text_style().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.accent)
        };
        if x < r.x + r.width {
            hits.push(
                Rect::new(x, r.y, w.min(r.x + r.width - x), 1),
                HitTarget::HfdlTab(i as u8),
            );
        }
        spans.push(Span::styled(text, style));
        spans.push(Span::raw("  "));
        x += w + 2;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}

/// Column widths of the message list for `w` columns: time, S, freq, dir,
/// aircraft, flight, type, subject.
fn columns(w: usize) -> [usize; 8] {
    let mut c = [8, 1, 5, 1, 9, 8, 13, 0];
    let used = |c: &[usize; 8]| c.iter().filter(|x| **x > 0).map(|x| x + 1).sum::<usize>();
    if used(&c) + 8 > w {
        c[5] = 0;
    }
    let rest = w.saturating_sub(used(&c) + 1);
    if rest >= 6 {
        c[7] = rest;
    }
    c
}

fn list_block<'a>(title: String, focused: bool, theme: &Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .title(title)
}

fn render_messages(
    frame: &mut Frame,
    r: Rect,
    state: &AppState,
    svc: &HfdlService,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if r.height < 3 {
        return;
    }
    let focused = state.nav.focus == Focus::WorkArea && state.hfdl_ui.part == HfdlPart::List;
    let rows = r.height.saturating_sub(3) as usize;
    let n = svc.len();
    let pages = n.div_ceil(rows.max(1)).max(1);
    let sel = svc.selected_row();
    let page = sel.map_or(pages, |s| s / rows.max(1) + 1);
    let mut title = format!(
        " Messages: {} of {n}  Pages: {page} of {pages}  File: {} ",
        sel.map_or(0, |s| s + 1),
        svc.day()
    );
    if !svc.query().is_empty() {
        title.push_str(&format!("[/{}] ", svc.query()));
    }
    if !svc.follow() {
        title.push_str("[Hold] ");
    }
    let block = list_block(title, focused, theme);
    let inner = block.inner(r);
    frame.render_widget(block, r);
    hits.push(inner, HitTarget::HfdlList);
    let w = inner.width as usize;
    let c = columns(w);
    let join = |cells: [String; 8]| {
        cells
            .iter()
            .zip(c)
            .filter(|(_, w)| *w > 0)
            .map(|(s, w)| cell(s, w))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let heads = [
        "TIME", "S", "FREQ", "", "AIRCRAFT", "FLIGHT", "TYPE", "SUBJECT",
    ];
    let mut lines = vec![Line::from(Span::styled(
        cell(&join(heads.map(String::from)), w),
        header_style(theme),
    ))];
    let top = state.hfdl_ui.list_top.min(n.saturating_sub(rows));
    for row in top..(top + rows).min(n) {
        let Some(e) = svc.row(row) else { break };
        let m = &e.msg;
        let dir = if crate::hfdl::msg::is_downlink(&m.tree) {
            "▲"
        } else {
            "▼"
        };
        let sev = if e.watched && m.sev < Severity::Caution {
            '★'
        } else {
            m.sev.letter()
        };
        let text = cell(
            &join([
                crate::dsc::time::local_time(m.utc.floor() as i64, m.off),
                sev.to_string(),
                m.freq_khz.to_string(),
                dir.into(),
                aircraft_label(m),
                crate::hfdl::msg::flight(&m.tree).unwrap_or_default(),
                type_label(m),
                e.subject.clone(),
            ]),
            w,
        );
        let mut style = severity_style(m.sev, theme);
        if Some(row) == sel {
            style = selected_style(theme, focused, style);
        }
        hits.push(
            Rect::new(inner.x, inner.y + 1 + (row - top) as u16, inner.width, 1),
            HitTarget::HfdlRow(row),
        );
        lines.push(Line::from(Span::styled(text, style)));
    }
    if n == 0 {
        let why = if svc.total() > 0 {
            "(all messages filtered: Esc clears the search, n shows link traffic)"
        } else {
            "(no messages today yet)"
        };
        lines.push(Line::from(Span::styled(why, theme.dim_style())));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// "12 min ago", "2 h 15 min ago".
fn age_text(s: f64) -> String {
    let m = (s.max(0.0) / 60.0) as u64;
    if m < 60 {
        format!("{m} min ago")
    } else {
        format!("{} h {} min ago", m / 60, m % 60)
    }
}

fn render_aircraft(
    frame: &mut Frame,
    r: Rect,
    state: &AppState,
    svc: &HfdlService,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if r.height < 3 {
        return;
    }
    let focused = state.nav.focus == Focus::WorkArea;
    let list = svc.aircraft();
    let n = list.len();
    let block = list_block(
        format!(" Aircraft: {n}  (Enter: messages, w: watch) "),
        focused,
        theme,
    );
    let inner = block.inner(r);
    frame.render_widget(block, r);
    hits.push(inner, HitTarget::HfdlList);
    let w = inner.width as usize;
    let now = svc.clock(now_utc());
    let head = format!(
        "  {} {} {} {} {} {} {} {} {}  OPERATOR / AIRLINE",
        cell("ICAO", 6),
        cell("REG", 9),
        cell("FLIGHT", 8),
        cell("TYPE", 4),
        cell("POSITION", 18),
        rcell("KM", 6),
        cell("FREQ", 5),
        cell("LAST", 8),
        rcell("MSGS", 5)
    );
    let mut lines = vec![Line::from(Span::styled(
        cell(&head, w),
        header_style(theme),
    ))];
    let rows = inner.height.saturating_sub(1) as usize;
    let top = state.hfdl_ui.list_top.min(n.saturating_sub(rows));
    for (i, a) in list.iter().enumerate().skip(top).take(rows) {
        let (pos, km) = match &a.position {
            Some((p, t)) => {
                let old = now - t > 7200.0;
                let txt = format!("{:.2} {:.2}", p.lat, p.lon);
                let txt = if old {
                    format!("{txt} ({})", age_text(now - t))
                } else {
                    txt
                };
                let km = state
                    .location
                    .map(|(la, lo)| {
                        format!("{:.0}", crate::dsc::geo::distance_km(la, lo, p.lat, p.lon))
                    })
                    .unwrap_or_default();
                (txt, km)
            }
            None => ("--".into(), String::new()),
        };
        let mark = if svc.is_watched(a) { "★ " } else { "  " };
        let e = a.acft.as_ref();
        let reg = a
            .registration
            .as_deref()
            .or(e.and_then(|e| e.reg.as_deref()))
            .unwrap_or("--");
        let ty = e.and_then(|e| e.icao_type.as_deref()).unwrap_or("");
        let who: Vec<&str> = e
            .map(|e| [&e.operator, &e.airline])
            .into_iter()
            .flatten()
            .flatten()
            .map(String::as_str)
            .collect();
        let mut who = who;
        who.dedup();
        let text = format!(
            "{mark}{} {} {} {} {} {} {} {} {}  {}",
            cell(a.icao.as_deref().unwrap_or("--"), 6),
            cell(reg, 9),
            cell(a.flight.as_deref().unwrap_or("--"), 8),
            cell(ty, 4),
            cell(&pos, 18),
            rcell(&km, 6),
            cell(&a.last_freq_khz.to_string(), 5),
            cell(
                &crate::dsc::time::local_time(a.last_t.floor() as i64, state.utc_offset_min * 60),
                8
            ),
            rcell(&a.messages.to_string(), 5),
            who.join(" / ")
        );
        let mut style = theme.text_style();
        if i == state.hfdl_ui.ac_sel {
            style = selected_style(theme, focused, style);
        }
        hits.push(
            Rect::new(inner.x, inner.y + 1 + (i - top) as u16, inner.width, 1),
            HitTarget::HfdlRow(i),
        );
        lines.push(Line::from(Span::styled(cell(&text, w), style)));
    }
    if n == 0 {
        lines.push(Line::from(Span::styled(
            "(no aircraft heard today yet)",
            theme.dim_style(),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_stations(
    frame: &mut Frame,
    r: Rect,
    state: &AppState,
    svc: &HfdlService,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if r.height < 3 {
        return;
    }
    let focused = state.nav.focus == Focus::WorkArea;
    let all = crate::hfdl::stations::all();
    let mut title = format!(
        " Ground stations: {}  system table {} built in",
        all.len(),
        crate::hfdl::service::SYSTABLE_BUILT_IN
    );
    if let Some(v) = svc.systable_on_air() {
        title.push_str(&format!(", {v} on the air"));
    }
    title.push(' ');
    let block = list_block(title, focused, theme);
    let inner = block.inner(r);
    frame.render_widget(block, r);
    hits.push(inner, HitTarget::HfdlList);
    let w = inner.width as usize;
    let head = format!(
        "{} {} {} {} {} {}",
        rcell("ID", 3),
        cell("NAME", 22),
        rcell("KM", 6),
        cell("LAST", 8),
        cell("HEARD", 5),
        "FREQUENCIES (kHz, * heard)"
    );
    let mut lines = vec![Line::from(Span::styled(
        cell(&head, w),
        header_style(theme),
    ))];
    let rows = inner.height.saturating_sub(1) as usize;
    let n = all.len();
    let top = state.hfdl_ui.list_top.min(n.saturating_sub(rows));
    for (i, s) in all.iter().enumerate().skip(top).take(rows) {
        let h = svc.heard(s.id);
        let km = state
            .location
            .map(|(la, lo)| format!("{:.0}", crate::dsc::geo::distance_km(la, lo, s.lat, s.lon)))
            .unwrap_or_default();
        let freqs: Vec<String> = s
            .freqs_khz
            .iter()
            .map(|f| {
                let k = f.round() as u32;
                if h.is_some_and(|h| h.freqs.contains(&k)) {
                    format!("{k}*")
                } else {
                    k.to_string()
                }
            })
            .collect();
        let text = format!(
            "{} {} {} {} {} {}",
            rcell(&s.id.to_string(), 3),
            cell(&s.name, 22),
            rcell(&km, 6),
            cell(
                &h.map(|h| crate::dsc::time::local_time(
                    h.last_utc.floor() as i64,
                    state.utc_offset_min * 60
                ))
                .unwrap_or_else(|| "--".into()),
                8
            ),
            cell(
                &h.map(|h| h.last_freq_khz.to_string()).unwrap_or_default(),
                5
            ),
            freqs.join(" ")
        );
        let mut style = if h.is_some() {
            theme.text_style()
        } else {
            theme.dim_style()
        };
        if i == state.hfdl_ui.gs_sel {
            style = selected_style(theme, focused, style);
        }
        hits.push(
            Rect::new(inner.x, inner.y + 1 + (i - top) as u16, inner.width, 1),
            HitTarget::HfdlRow(i),
        );
        lines.push(Line::from(Span::styled(cell(&text, w), style)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_detail(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    let focused = state.nav.focus == Focus::WorkArea && state.hfdl_ui.part == HfdlPart::Detail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .title(" Message details ");
    let inner = block.inner(r);
    frame.render_widget(block, r);
    hits.push(r, HitTarget::HfdlDetail);
    if inner.is_empty() {
        return;
    }
    let all = detail_lines(state, inner.width as usize);
    let rows = inner.height as usize;
    let top = state
        .hfdl_ui
        .detail_scroll
        .min(all.len().saturating_sub(rows));
    let sev = state
        .hfdl
        .as_ref()
        .and_then(|s| s.selected_entry())
        .map(|e| e.msg.sev);
    let lines: Vec<Line> = all
        .iter()
        .skip(top)
        .take(rows)
        .map(|(l, first)| {
            let style = if *first {
                sev.map_or(theme.text_style(), |s| severity_style(s, theme))
                    .add_modifier(Modifier::BOLD)
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

fn fkey_text(cmd: HfdlCmd, state: &AppState) -> String {
    match cmd {
        HfdlCmd::Follow => {
            if state.hfdl_toggle_state(HfdlCmd::Follow).unwrap_or(true) {
                "Follow".into()
            } else {
                "Hold".into()
            }
        }
        c => crate::hfdl_app::fkey_label(c).into(),
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
        spans.push(Span::styled(
            num,
            theme.text_style().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            cell(&fkey_text(*cmd, state), room),
            label_style,
        ));
        hits.push(Rect::new(x, r.y, w, 1), HitTarget::HfdlFKey(*k));
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
            let state_txt = match state.hfdl_toggle_state(it.cmd) {
                Some(true) => "On",
                Some(false) => "Off",
                None => "",
            };
            let right = format!("{} {state_txt}", it.key);
            format!("{:<26}{:>8}", it.label, right.trim())
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
    hits.push(r, HitTarget::HfdlMenuFrame);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .take(inner.height as usize)
        .map(|(i, t)| {
            hits.push(
                Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                HitTarget::HfdlMenuItem(top, i),
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
    fn the_layout_stacks_its_parts() {
        let l = layout(Rect::new(0, 0, 100, 30), false, false, 2);
        assert_eq!(l.menu, Rect::new(0, 0, 100, 1));
        assert_eq!(l.status, Rect::new(0, 1, 100, 1));
        assert_eq!(l.channels, Rect::new(0, 2, 100, 5));
        assert_eq!(l.tabs.y, 7);
        assert_eq!(l.list.y, 8);
        assert_eq!(l.fkeys, Rect::new(0, 29, 100, 1));
        let d = layout(Rect::new(0, 0, 160, 40), true, true, 9);
        assert_eq!(d.channels.height, CHANNEL_ROWS as u16 + 3);
        assert_eq!(d.list.y, d.detail.y, "beside on a wide view");
        let small = layout(Rect::new(0, 0, 60, 12), true, true, 4);
        assert!(
            small.channels.is_empty(),
            "a small view gives the channels up"
        );
    }

    #[test]
    fn a_tiny_view_does_not_panic() {
        for h in 0..10 {
            let _ = layout(Rect::new(0, 0, 20, h), true, true, 3);
        }
    }

    #[test]
    fn columns_keep_the_subject() {
        assert!(columns(100)[7] >= 30);
        assert_eq!(columns(50)[5], 0, "no flight column when narrow");
    }
}
