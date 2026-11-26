//! The FAX Work Area view (0.2.2, spec 15.6): a menu bar (F9), two status
//! lines, the picture and the function-key bar. The picture is drawn with
//! Unicode half blocks (two grey pixels per cell), or, on terminals with the
//! Kitty graphics protocol, as a real picture placed over the area after the
//! frame (`Pixels`, see `fax::kitty` and the run loop).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{AppState, RunState};
use crate::fax::service::{Signal, SlantState};
use crate::fax_app::{Display, FKEYS, FaxCmd, MENUS};
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::theme::Theme;

/// The view's regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FaxLayout {
    pub menu: Rect,
    pub status: Rect,
    pub picture: Rect,
    pub fkeys: Rect,
}

/// Lay out the view in `area` (inside the VFO view's frame).
pub fn layout(area: Rect) -> FaxLayout {
    let row = |y: u16, h: u16| Rect::new(area.x, y, area.width, h);
    let bottom = area.y + area.height;
    let menu = row(area.y, 1.min(area.height));
    let fkeys = if area.height >= 2 {
        row(bottom - 1, 1)
    } else {
        Rect::default()
    };
    let y = menu.y + menu.height;
    let end = fkeys.y.max(y);
    let status_h = 2.min(end - y);
    let status = row(y, status_h);
    let picture = row(y + status_h, end - y - status_h);
    FaxLayout {
        menu,
        status,
        picture,
        fkeys,
    }
}

/// Picture lines that fit in `area` for a picture `width` pixels wide:
/// the width fills the area; a cell is `cell_px` (w, h) pixels.
pub fn lines_in(area: Rect, width: i32, cell_px: (u16, u16)) -> i32 {
    if area.width == 0 || area.height == 0 || width <= 0 {
        return 0;
    }
    let (cw, ch) = (f64::from(cell_px.0.max(1)), f64::from(cell_px.1.max(1)));
    (f64::from(area.height) * ch / cw * f64::from(width) / f64::from(area.width)).round() as i32
}

/// Lines the picture area shows (for scrolling), from the current state.
pub fn visible_lines(area: Rect, state: &AppState) -> i32 {
    let l = layout(area);
    let width = state.fax.as_ref().map_or(1809, |s| s.config().width);
    lines_in(l.picture, width, state.fax_ui.cell_px())
}

/// The picture window: (picture id, first line, lines).
pub fn window(state: &AppState, visible: i32) -> Option<(u64, i32, i32)> {
    let svc = state.fax.as_ref()?;
    let cur = svc.current()?;
    let bottom_top = (cur.lines - visible).max(cur.first_line);
    let top = state
        .fax_ui
        .top
        .unwrap_or(bottom_top)
        .clamp(cur.first_line, bottom_top.max(cur.first_line));
    Some((cur.id, top, visible))
}

/// What the run loop has to show with the Kitty protocol after the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KittyRequest {
    pub area: Rect,
    pub picture: u64,
    pub first: i32,
    pub lines: i32,
    /// The picture's lines available and ppm, to see a change.
    pub available: i32,
    pub ppm: f64,
}

fn cell(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let n = out.chars().count();
    out.extend(std::iter::repeat_n(' ', w - n));
    out
}

/// Draw the FAX view into `area`.
pub fn render_fax_view(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    hits.push(area, HitTarget::FaxView);
    let l = layout(area);
    render_menu_bar(frame, l.menu, state, theme, hits);
    render_status(frame, l.status, state, theme);
    render_picture(frame, l.picture, state, theme, hits);
    render_fkeys(frame, l.fkeys, state, theme, hits);
    if let Some((top, item)) = state.fax_ui.menu {
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
        let open = state.fax_ui.menu.is_some_and(|(top, _)| top == i);
        let style = if open {
            Style::default().fg(theme.editing_fg).bg(theme.editing_bg)
        } else {
            theme.text_style()
        };
        hits.push(
            Rect::new(x, r.y, w.min(r.x + r.width - x.min(r.x + r.width)), 1),
            HitTarget::FaxMenuTitle(i),
        );
        spans.push(Span::styled(t, style));
        x += w;
        if x >= r.x + r.width {
            break;
        }
    }
    let used = (x.min(r.x + r.width) - r.x) as usize;
    let right = match &state.fax_ui.notice {
        Some((n, _)) => n.clone(),
        None => "F9 menu".to_string(),
    };
    let room = (r.width as usize).saturating_sub(used + 1);
    let right: String = right.chars().take(room).collect();
    let pad = (r.width as usize).saturating_sub(used + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    let style = if state.fax_ui.notice.is_some() {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.dim_style()
    };
    spans.push(Span::styled(right, style));
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}

/// The two status lines.
pub fn status_lines(state: &AppState) -> (String, String) {
    let Some(svc) = state.fax.as_ref() else {
        let why = match crate::fax::ffi::shared() {
            Ok(_) => "starting".to_string(),
            Err(e) => e,
        };
        return (format!("FAX: {why}"), String::new());
    };
    let signal = if state.run != RunState::Running {
        "stopped"
    } else {
        match svc.signal() {
            Signal::Waiting => "waiting",
            Signal::Receiving => "receiving",
            Signal::Start => "START",
            Signal::Phasing => "phasing",
            Signal::Stop => "STOP",
        }
    };
    let c = svc.config();
    let pics = svc.pictures();
    let mut first = format!("{signal:<9}");
    let mut second = String::new();
    match svc.current() {
        None => first.push_str("  no picture yet"),
        Some(p) => {
            let i = pics.iter().position(|q| q.id == p.id).map_or(0, |i| i + 1);
            let t =
                crate::record::naming::UtcDateTime::from_unix(svc.picture_time(&p).max(0.0) as u64);
            let kind = match (p.started != 0, p.aligned != 0) {
                (true, true) => "fax, aligned",
                (true, false) => "fax",
                (false, _) => "no START",
            };
            let follow = if svc.following() && state.fax_ui.top.is_none() {
                "follow"
            } else {
                "hold"
            };
            first.push_str(&format!(
                "  Picture {i}/{} ({kind})  {:02}:{:02} UTC  {} lines  {}/{}  {follow}",
                pics.len(),
                t.hour,
                t.minute,
                p.lines,
                c.lpm,
                (f64::from(c.width) / std::f64::consts::PI).round(),
            ));
            second = match svc.slant_state(&p) {
                SlantState::Waiting { lines } => {
                    format!(
                        "Slant: waiting ({lines} of {} lines)",
                        crate::fax::service::SLANT_FIRST
                    )
                }
                SlantState::Measuring { lines } => format!(
                    "Slant: measuring ({lines} lines){}",
                    if svc.measuring() { "..." } else { "" }
                ),
                SlantState::Found {
                    ppm,
                    confidence,
                    lines,
                } => {
                    format!("Slant: {ppm:+.2} ppm auto ({lines} lines, confidence {confidence:.1})")
                }
                SlantState::Manual => format!("Slant: {:+.2} ppm manual", p.ppm),
            };
        }
    }
    if let Some(st) = svc.status() {
        second.push_str(&format!("  Cache {} MB", st.cache_bytes >> 20));
    }
    if svc.recording() {
        let r = svc.rec_status();
        second.push_str(&format!("  REC {} ({} lines)", r.file, r.lines));
    }
    (first, second)
}

fn render_status(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme) {
    if r.is_empty() {
        return;
    }
    let (a, b) = status_lines(state);
    let w = r.width as usize;
    let mut lines = vec![Line::from(Span::styled(
        cell(&a, w),
        theme.text_style().add_modifier(Modifier::BOLD),
    ))];
    if r.height > 1 {
        let measuring = b.starts_with("Slant: measuring") || b.starts_with("Slant: waiting");
        let style = if measuring {
            Style::default().fg(theme.accent)
        } else {
            theme.text_style()
        };
        lines.push(Line::from(Span::styled(cell(&b, w), style)));
    }
    frame.render_widget(Paragraph::new(lines), r);
}

/// Whether the terminal takes 24-bit colours (else the 256-colour greys).
pub fn truecolor() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false)
            || crate::fax::kitty::supported()
    })
}

/// A grey level as a terminal colour.
pub fn grey(v: u8, truecolor: bool) -> Color {
    if truecolor {
        Color::Rgb(v, v, v)
    } else {
        // the 24 greys 232..255 (8..238), black and white from the cube
        match v {
            0..=3 => Color::Indexed(16),
            252..=255 => Color::Indexed(231),
            _ => Color::Indexed(
                232 + ((u16::from(v.saturating_sub(8)) * 23 + 115) / 230).min(23) as u8,
            ),
        }
    }
}

fn render_picture(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    if r.is_empty() {
        return;
    }
    let Some(svc) = state.fax.as_ref() else {
        return;
    };
    let visible = lines_in(r, svc.config().width, state.fax_ui.cell_px());
    let Some((id, first, lines)) = window(state, visible) else {
        let msg = if state.run == RunState::Running {
            "Waiting for the fax signal..."
        } else {
            "Start the receiver to decode faxes"
        };
        frame.render_widget(
            Paragraph::new(msg).style(theme.dim_style()),
            Rect::new(r.x, r.y + r.height / 2, r.width, 1),
        );
        return;
    };
    let cur = svc.current().expect("window found it");

    if state.fax_display() == Display::Pixels && state.fax_ui.kitty_allowed {
        // blank cells; the run loop places the picture over them
        frame.render_widget(Clear, r);
        *state.fax_ui.kitty_request.borrow_mut() = Some(KittyRequest {
            area: r,
            picture: id,
            first,
            lines,
            available: cur.lines,
            ppm: cur.ppm,
        });
        return;
    }

    let (w, h) = (r.width as usize, r.height as usize * 2);
    let key = (
        id,
        first,
        lines,
        w,
        h,
        cur.lines,
        cur.first_line,
        cur.ppm.to_bits(),
    );
    let mut cache = state.fax_ui.blocks.borrow_mut();
    if cache.as_ref().is_none_or(|(k, _)| *k != key) {
        let px = svc
            .render(id, first, lines, w, h)
            .unwrap_or_else(|| vec![255; w * h]);
        *cache = Some((key, px));
    }
    let px = &cache.as_ref().expect("filled").1;
    let tc = truecolor();
    let buf = frame.buffer_mut();
    for y in 0..r.height as usize {
        for x in 0..w {
            let top = px[2 * y * w + x];
            let bottom = px[(2 * y + 1) * w + x];
            if let Some(c) = buf.cell_mut((r.x + x as u16, r.y + y as u16)) {
                c.set_symbol("▀")
                    .set_fg(grey(top, tc))
                    .set_bg(grey(bottom, tc));
            }
        }
    }
    drop(cache);
    // a graphical front paints the same lines at its pixels (spec 28)
    if state.pixel_pictures {
        let (cw, ch) = state.fax_ui.cell_px();
        let (pw, ph) = crate::remote::picture::capped(
            r.width as usize * cw as usize,
            r.height as usize * ch as usize,
            svc.config().width.max(1) as usize,
        );
        let key = crate::remote::picture::key_of((
            id,
            first,
            lines,
            pw,
            ph,
            cur.lines,
            cur.first_line,
            cur.ppm.to_bits(),
        ));
        let slot = crate::remote::picture::SLOT_FAX;
        if let Some(p) = state.cached_picture(slot, key, || {
            let px = svc.render(id, first, lines, pw, ph)?;
            Some(crate::remote::picture::Picture::grey(slot, key, pw, ph, px))
        }) {
            hits.picture(r, false, p);
        }
    }
}

fn fkey_label(cmd: FaxCmd, state: &AppState) -> String {
    match cmd {
        FaxCmd::Help => "Help".into(),
        FaxCmd::Save => "Save".into(),
        FaxCmd::Display => match state.fax_display() {
            Display::Blocks => "Pixels".into(),
            Display::Pixels => "Blocks".into(),
        },
        FaxCmd::PpmDown => format!("-{}", state.fax_ppm_step()),
        FaxCmd::PpmUp => format!("+{}", state.fax_ppm_step()),
        FaxCmd::Auto => {
            if state.fax.as_ref().is_some_and(|s| s.auto()) {
                "Auto".into()
            } else {
                "Manual".into()
            }
        }
        FaxCmd::Prev => "Older".into(),
        FaxCmd::Next => "Newer".into(),
        FaxCmd::Menu => "Menu".into(),
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
        spans.push(Span::styled(
            num,
            theme.text_style().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            cell(&fkey_label(*cmd, state), room),
            label_style,
        ));
        hits.push(Rect::new(x, r.y, w, 1), HitTarget::FaxFKey(*k));
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
            let state_txt = match state.fax_toggle_state(it.cmd) {
                Some(true) => "On",
                Some(false) => "Off",
                None => "",
            };
            let right = format!("{} {state_txt}", it.key);
            format!("{:<22}{:>8}", it.label, right.trim())
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
    hits.push(r, HitTarget::FaxMenuFrame);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .take(inner.height as usize)
        .map(|(i, t)| {
            hits.push(
                Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                HitTarget::FaxMenuItem(top, i),
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
    fn the_layout_stacks_menu_status_picture_and_keys() {
        let l = layout(Rect::new(0, 0, 80, 30));
        assert_eq!(l.menu, Rect::new(0, 0, 80, 1));
        assert_eq!(l.status, Rect::new(0, 1, 80, 2));
        assert_eq!(l.picture, Rect::new(0, 3, 80, 26));
        assert_eq!(l.fkeys, Rect::new(0, 29, 80, 1));
        for h in 0..5 {
            let _ = layout(Rect::new(0, 0, 20, h));
        }
    }

    #[test]
    fn the_picture_width_fills_the_area_with_square_pixels() {
        // 1809 px into 180 columns: ~10 px per column; 40 rows of 1:2 cells
        // are 80 square half-block pixels = ~804 lines
        let n = lines_in(Rect::new(0, 0, 180, 40), 1809, (8, 16));
        assert_eq!(n, 804);
        assert_eq!(lines_in(Rect::new(0, 0, 0, 40), 1809, (8, 16)), 0);
    }

    #[test]
    fn greys_map_onto_the_256_colour_ramp() {
        assert_eq!(grey(0, false), Color::Indexed(16));
        assert_eq!(grey(255, false), Color::Indexed(231));
        assert_eq!(grey(128, false), Color::Indexed(244));
        assert_eq!(grey(77, true), Color::Rgb(77, 77, 77));
    }
}
