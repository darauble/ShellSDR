//! The Digital Work Area view (0.2.5, spec 16.9): a menu bar (F9), two
//! status lines, a waterfall of the audio band with the signal marked
//! (a click sets the carrier), the received text with the marks, the
//! current picture beside it (half blocks), and the function-key bar.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::AppState;
use crate::digital::service::Line as DocLine;
use crate::digital_app::{DigiCmd, FKEYS, MENUS};
use crate::mfsk::frontend::Sideband;
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::theme::Theme;
use crate::vfo::types::digital as dtype;

/// The waterfall shows 0..this many Hz.
pub const WATERFALL_HZ: f64 = 4000.0;
/// The RSID detector's spectrum: 1024 bins over 0..5512.5 Hz.
const SPECTRUM_HZ: f64 = 5512.5;

/// The view's regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DigitalLayout {
    pub menu: Rect,
    pub status: Rect,
    pub waterfall: Rect,
    /// The frequency scale under the waterfall, with the signal marked.
    pub scale: Rect,
    pub text: Rect,
    pub picture: Rect,
    pub fkeys: Rect,
}

/// Lay out the view in `area`; `picture` = a picture is to be shown.
pub fn layout(area: Rect, picture: bool) -> DigitalLayout {
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
        let h = h.min(end - *y);
        let r = row(*y, h);
        *y += h;
        r
    };
    let status = take(&mut y, 2);
    let left = end - y;
    let wf_h = if left >= 14 {
        6
    } else if left >= 8 {
        3
    } else {
        0
    };
    let waterfall = take(&mut y, wf_h);
    let scale = take(&mut y, u16::from(wf_h > 0));
    let body = row(y, end - y);
    let (text, picture) = if !picture || body.is_empty() {
        (body, Rect::default())
    } else if body.width >= 70 {
        let pw = body.width * 2 / 5;
        (
            Rect::new(body.x, body.y, body.width - pw, body.height),
            Rect::new(body.x + body.width - pw, body.y, pw, body.height),
        )
    } else {
        let ph = body.height / 2;
        (
            Rect::new(body.x, body.y + ph, body.width, body.height - ph),
            Rect::new(body.x, body.y, body.width, ph),
        )
    };
    DigitalLayout {
        menu,
        status,
        waterfall,
        scale,
        text,
        picture,
        fkeys,
    }
}

fn cell(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let n = out.chars().count();
    out.extend(std::iter::repeat_n(' ', w - n));
    out
}

/// The audio frequency at column `x` of a waterfall `width` wide.
pub fn column_hz(x: u16, width: u16) -> f64 {
    (f64::from(x) + 0.5) / f64::from(width.max(1)) * WATERFALL_HZ
}

fn hz_column(hz: f64, width: u16) -> Option<u16> {
    let c = (hz / WATERFALL_HZ * f64::from(width)).floor();
    (c >= 0.0 && c < f64::from(width)).then_some(c as u16)
}

/// Draw the Digital view into `area`.
pub fn render_digital_view(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    hits.push(area, HitTarget::DigitalView);
    let pic = shown_picture(state).is_some();
    let l = layout(area, pic);
    render_menu_bar(frame, l.menu, state, theme, hits);
    render_status(frame, l.status, state, theme);
    render_waterfall(frame, l.waterfall, l.scale, state, theme, hits);
    render_text(frame, l.text, state, theme);
    render_picture(frame, l.picture, state, theme, hits);
    render_fkeys(frame, l.fkeys, state, theme, hits);
    if let Some((top, item)) = state.digital_ui.menu {
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
        let open = state.digital_ui.menu.is_some_and(|(top, _)| top == i);
        let style = if open {
            Style::default().fg(theme.editing_fg).bg(theme.editing_bg)
        } else {
            theme.text_style()
        };
        hits.push(Rect::new(x, r.y, w, 1), HitTarget::DigitalMenuTitle(i));
        spans.push(Span::styled(t, style));
        x += w;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}

/// The two status lines.
pub fn status_lines(state: &AppState) -> (String, String) {
    let c = dtype::config(&state.radio.active().type_settings());
    let Some(svc) = state.digital.as_ref() else {
        return (
            format!("{} {:.0} Hz {}", c.mode.name, c.carrier, c.sideband.name()),
            String::new(),
        );
    };
    let st = svc.status();
    let side = match (c.sideband, st.am_offset) {
        (Sideband::Both, Some(off)) => format!("Both (AM {off:+.1} Hz)"),
        (b, _) => b.name().to_string(),
    };
    let bars = (st.metric / 12.5).round() as usize;
    let meter: String = "█".repeat(bars.min(8)) + &"░".repeat(8 - bars.min(8));
    let sql = if c.squelch <= 0.0 {
        "SQL off".to_string()
    } else {
        format!(
            "SQL {:.0} {}",
            c.squelch,
            if st.open { "open" } else { "shut" }
        )
    };
    let a = format!(
        "{}  {:.0} Hz  {side}  AFC {}  RSID {}  {sql}  {meter} {:.0}",
        c.mode.name,
        st.carrier,
        if c.afc { "on" } else { "off" },
        if c.rsid { "on" } else { "off" },
        st.metric,
    );
    let b = if let Some((n, _)) = &state.digital_ui.notice {
        n.clone()
    } else if let Some((done, of)) = st.picture {
        let p = svc.live_picture();
        let size = p.map_or(String::new(), |p| {
            format!(
                "{}x{}{} ",
                p.width,
                p.height,
                if p.color { "C" } else { "" }
            )
        });
        format!(
            "Receiving picture {size}{:.0}%",
            100.0 * done as f64 / of.max(1) as f64
        )
    } else if let Some(r) = svc.rec_status() {
        format!(
            "REC {}  {:.1} kB  {} picture{}",
            r.file,
            r.bytes as f64 / 1000.0,
            r.pictures,
            if r.pictures == 1 { "" } else { "s" }
        )
    } else if state.digital_ui.back > 0 {
        "Holding: End follows the newest text".into()
    } else {
        "Not recording: F2 saves the session, [Record] with Native writes it as it comes".into()
    };
    (a, b)
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
        let style = if state.digital_ui.notice.is_some() {
            Style::default().fg(theme.accent)
        } else {
            theme.dim_style()
        };
        lines.push(Line::from(Span::styled(cell(&b, w), style)));
    }
    frame.render_widget(Paragraph::new(lines), r);
}

fn render_waterfall(
    frame: &mut Frame,
    r: Rect,
    scale: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if r.is_empty() {
        return;
    }
    hits.push(r, HitTarget::DigitalWaterfall(r.x, r.width));
    hits.push(scale, HitTarget::DigitalWaterfall(scale.x, scale.width));
    let c = dtype::config(&state.radio.active().type_settings());
    let carrier = state
        .digital
        .as_ref()
        .map_or(c.carrier, |s| s.status().carrier);
    let buf = frame.buffer_mut();
    if let Some(svc) = state.digital.as_ref() {
        let rows = svc.spectrum();
        // a range from what is shown: the floor a little under the median
        let mut all: Vec<f32> = Vec::new();
        let cols: Vec<Vec<f32>> = rows
            .iter()
            .rev()
            .take(r.height as usize)
            .map(|row| {
                (0..r.width)
                    .map(|x| {
                        let lo = f64::from(x) / f64::from(r.width) * WATERFALL_HZ;
                        let hi = f64::from(x + 1) / f64::from(r.width) * WATERFALL_HZ;
                        let b0 = (lo / SPECTRUM_HZ * 1024.0) as usize;
                        let b1 = ((hi / SPECTRUM_HZ * 1024.0) as usize).max(b0 + 1).min(1024);
                        let p = row[b0..b1].iter().copied().fold(0.0f32, f32::max);
                        let db = 10.0 * p.max(1e-20).log10();
                        all.push(db);
                        db
                    })
                    .collect()
            })
            .collect();
        if !all.is_empty() {
            all.sort_by(f32::total_cmp);
            let floor = all[all.len() / 2] - 3.0;
            let top = all[all.len() - 1].max(floor + 20.0);
            let range = crate::ui::spectrum::DbRange {
                min: f64::from(floor),
                max: f64::from(top),
                auto: false,
            };
            for (y, row) in cols.iter().enumerate() {
                for (x, db) in row.iter().enumerate() {
                    let color = crate::ui::waterfall::db_to_color(*db, &range, theme);
                    if let Some(cell) = buf.cell_mut((r.x + x as u16, r.y + y as u16)) {
                        cell.set_char(' ').set_bg(color);
                    }
                }
            }
            if state.pixel_pictures {
                pixel_waterfall(r, rows, &range, theme, state, hits);
            }
        }
    }
    if scale.is_empty() {
        return;
    }
    // the scale: kHz ticks, the signal's tones as a bar, the carrier
    let w = scale.width;
    let mut line: Vec<char> = vec!['─'; w as usize];
    for k in 1..4 {
        if let Some(x) = hz_column(f64::from(k) * 1000.0, w) {
            for (i, ch) in format!("{k}k").chars().enumerate() {
                if let Some(slot) = line.get_mut(x as usize + i) {
                    *slot = ch;
                }
            }
        }
    }
    let half = c.mode.bandwidth() / 2.0;
    let (a, b) = (hz_column(carrier - half, w), hz_column(carrier + half, w));
    if let (Some(a), Some(b)) = (a.or(Some(0)), b.or(Some(w - 1))) {
        for slot in line.iter_mut().take(b as usize + 1).skip(a as usize) {
            *slot = '▀';
        }
    }
    if let Some(x) = hz_column(carrier, w) {
        line[x as usize] = '▼';
    }
    let text: String = line.into_iter().collect();
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.accent),
        ))),
        scale,
    );
}

/// The waterfall for a graphical front (spec 28): every bin of 0..4 kHz
/// across, the rows shown down, newest on top.
fn pixel_waterfall(
    r: Rect,
    rows: &std::collections::VecDeque<Vec<f32>>,
    range: &crate::ui::spectrum::DbRange,
    theme: &Theme,
    state: &AppState,
    hits: &mut HitMap,
) {
    use crate::remote::picture::{Picture, SLOT_DIGITAL_WATERFALL as SLOT, pixels_key};
    let n = ((WATERFALL_HZ / SPECTRUM_HZ * 1024.0) as usize).clamp(1, 1024);
    let shown: Vec<&Vec<f32>> = rows.iter().rev().take(r.height as usize).collect();
    if shown.is_empty() {
        return;
    }
    let span = (range.max - range.min) as f32;
    let mut px = Vec::with_capacity(n * shown.len() * 3);
    for row in &shown {
        for b in 0..n {
            let db = 10.0 * row.get(b).copied().unwrap_or(0.0).max(1e-20).log10();
            let t = if span > 0.0 {
                ((db - range.min as f32) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (cr, cg, cb) = theme.waterfall_color(t);
            px.extend_from_slice(&[cr, cg, cb]);
        }
    }
    let h = shown.len();
    let key = pixels_key(SLOT, n, h, &px);
    if let Some(p) = state.cached_picture(SLOT, key, || Some(Picture::rgb(SLOT, key, n, h, px))) {
        // only the rows there are (the rest stays blank)
        hits.picture(Rect::new(r.x, r.y, r.width, h as u16), false, p);
    }
}

/// The picture the view shows: the selected one, else the one being
/// received, else the newest.
fn shown_picture(state: &AppState) -> Option<(usize, usize, Vec<u8>, String)> {
    let svc = state.digital.as_ref()?;
    let label =
        |w: usize, h: usize, color: bool| format!("{w}x{h}{}", if color { "C" } else { "" });
    if let Some(id) = state.digital_ui.picture
        && let Some(p) = svc.picture(id)
    {
        let n = svc
            .pictures()
            .iter()
            .position(|q| q.id == id)
            .map_or(0, |i| i + 1);
        let t = crate::record::naming::UtcDateTime::from_unix(p.time.max(0.0) as u64);
        return Some((
            p.width,
            p.height,
            p.rgb.clone(),
            format!(
                "Pic {n}/{} {} {:02}:{:02}Z",
                svc.pictures().len(),
                label(p.width, p.height, p.color),
                t.hour,
                t.minute
            ),
        ));
    }
    if let Some(p) = svc.live_picture() {
        return Some((
            p.width,
            p.height,
            p.rgb.clone(),
            format!("Receiving {}", label(p.width, p.height, p.color)),
        ));
    }
    let p = svc.pictures().back()?;
    let t = crate::record::naming::UtcDateTime::from_unix(p.time.max(0.0) as u64);
    Some((
        p.width,
        p.height,
        p.rgb.clone(),
        format!(
            "Pic {}/{} {} {:02}:{:02}Z",
            svc.pictures().len(),
            svc.pictures().len(),
            label(p.width, p.height, p.color),
            t.hour,
            t.minute
        ),
    ))
}

/// Wrap `s` to `width` columns (words where possible).
fn wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        let cut = if end < chars.len() {
            chars[start..end]
                .iter()
                .rposition(|&c| c == ' ')
                .filter(|&p| p > 0)
                .map_or(end, |p| start + p + 1)
        } else {
            end
        };
        out.push(chars[start..cut].iter().collect());
        start = cut;
    }
    out
}

fn render_text(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme) {
    if r.is_empty() {
        return;
    }
    let ui = &state.digital_ui;
    ui.rows.set(r.height as usize);
    let Some(svc) = state.digital.as_ref() else {
        return;
    };
    let w = r.width as usize;
    let want = r.height as usize + ui.back;
    let mark_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::ITALIC);
    let pic_style = Style::default().fg(theme.accent);
    // wrapped rows from the newest back, just as many as needed
    let mut rows: Vec<(String, Style)> = Vec::new();
    let mut all = true;
    for line in svc.lines().iter().rev() {
        let (text, style) = match line {
            DocLine::Text(t) => (t.clone(), theme.text_style()),
            DocLine::Mark(m) => (format!("🔵 {m}"), mark_style),
            DocLine::Picture(id) => {
                let p = svc.picture(*id);
                let what = p.map_or("picture (dropped)".to_string(), |p| {
                    let name = p
                        .file
                        .as_ref()
                        .and_then(|f| f.file_name())
                        .map_or("in memory".to_string(), |n| {
                            n.to_string_lossy().into_owned()
                        });
                    format!(
                        "picture {}x{}{}: {name}",
                        p.width,
                        p.height,
                        if p.color { "C" } else { "" }
                    )
                });
                (format!("▣ {what}"), pic_style)
            }
        };
        for row in wrap(&text, w).into_iter().rev() {
            rows.push((row, style));
        }
        if rows.len() > want {
            all = false;
            break;
        }
    }
    let total = rows.len();
    ui.back_max.set(if all {
        total.saturating_sub(r.height as usize)
    } else {
        usize::MAX
    });
    let back = ui.back.min(total.saturating_sub(r.height as usize));
    let lines: Vec<Line> = rows
        .iter()
        .skip(back)
        .take(r.height as usize)
        .rev()
        .map(|(t, s)| Line::from(Span::styled(t.clone(), *s)))
        .collect();
    // newest at the bottom: pad above when there is less than a screen
    let pad = (r.height as usize).saturating_sub(lines.len());
    let mut out = vec![Line::from(""); pad];
    out.extend(lines);
    frame.render_widget(Paragraph::new(out), r);
}

/// A colour for a pixel (24-bit, or the 256-colour cube).
fn rgb_color(r: u8, g: u8, b: u8, truecolor: bool) -> Color {
    if truecolor {
        return Color::Rgb(r, g, b);
    }
    let q = |v: u8| ((u16::from(v) * 5 + 127) / 255) as u8;
    Color::Indexed(16 + 36 * q(r) + 6 * q(g) + q(b))
}

fn render_picture(frame: &mut Frame, r: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    if r.is_empty() {
        return;
    }
    let Some((pw, ph, rgb, title)) = shown_picture(state) else {
        return;
    };
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.border_style(false))
        .title(Span::styled(title, Style::default().fg(theme.accent)));
    let inner = block.inner(r);
    frame.render_widget(block, r);
    let inner = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    if inner.is_empty() || pw == 0 || ph == 0 {
        return;
    }
    // half blocks: a cell is one pixel wide and two high (square pixels)
    let scale = (f64::from(inner.width) / pw as f64).min(f64::from(inner.height) * 2.0 / ph as f64);
    let cols = ((pw as f64 * scale).floor() as u16).clamp(1, inner.width);
    let px_rows = ((ph as f64 * scale).floor() as usize).max(1);
    let cell_rows = (px_rows.div_ceil(2) as u16).min(inner.height);
    let x0 = inner.x + (inner.width - cols) / 2;
    let tc = crate::ui::fax_view::truecolor();
    let sample = |x: u16, y: usize| -> (u8, u8, u8) {
        let sx = ((f64::from(x) + 0.5) / f64::from(cols) * pw as f64) as usize;
        let sy = ((y as f64 + 0.5) / px_rows as f64 * ph as f64) as usize;
        let i = (sy.min(ph - 1) * pw + sx.min(pw - 1)) * 3;
        (rgb[i], rgb[i + 1], rgb[i + 2])
    };
    let buf = frame.buffer_mut();
    for cy in 0..cell_rows {
        for x in 0..cols {
            let top = sample(x, 2 * cy as usize);
            let bottom_y = 2 * cy as usize + 1;
            let Some(cell) = buf.cell_mut((x0 + x, inner.y + cy)) else {
                continue;
            };
            cell.set_char('▀')
                .set_fg(rgb_color(top.0, top.1, top.2, tc));
            if bottom_y < px_rows {
                let b = sample(x, bottom_y);
                cell.set_bg(rgb_color(b.0, b.1, b.2, tc));
            } else {
                cell.set_bg(Color::Reset);
            }
        }
    } // a graphical front paints the picture itself, as large as fits
    if state.pixel_pictures {
        use crate::remote::picture::{Picture, SLOT_MFSK, pixels_key};
        let key = pixels_key(SLOT_MFSK, pw, ph, &rgb);
        if let Some(p) = state.cached_picture(SLOT_MFSK, key, || {
            Some(Picture::rgb(SLOT_MFSK, key, pw, ph, rgb))
        }) {
            hits.picture(inner, true, p);
        }
    }
}

fn fkey_label(cmd: DigiCmd, state: &AppState) -> String {
    let c = dtype::config(&state.radio.active().type_settings());
    match cmd {
        DigiCmd::Help => "Help".into(),
        DigiCmd::Save => "Save".into(),
        DigiCmd::Mode => "Mode".into(),
        DigiCmd::Sideband => "Side".into(),
        DigiCmd::Afc => if c.afc { "AFC" } else { "afc" }.into(),
        DigiCmd::Rsid => if c.rsid { "RSID" } else { "rsid" }.into(),
        DigiCmd::SqlDown => "Sql-".into(),
        DigiCmd::SqlUp => "Sql+".into(),
        DigiCmd::Menu => "Menu".into(),
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
        hits.push(Rect::new(x, r.y, w, 1), HitTarget::DigitalFKey(*k));
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
            let state_txt = match state.digital_toggle_state(it.cmd) {
                Some(true) => "On",
                Some(false) => "Off",
                None => "",
            };
            let right = format!("{} {state_txt}", it.key);
            format!("{:<20}{:>10}", it.label, right.trim())
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
    hits.push(r, HitTarget::DigitalMenuFrame);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .take(inner.height as usize)
        .map(|(i, t)| {
            hits.push(
                Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                HitTarget::DigitalMenuItem(top, i),
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
    fn a_window_gets_the_picture_and_the_waterfall_as_pixels() {
        use crate::remote::picture::{SLOT_DIGITAL_WATERFALL, SLOT_MFSK};
        use crate::remote::wire::Overlay;
        let mut s = AppState::with_mock_source();
        s.radio.set_type(crate::vfo::types::DIGITAL);
        s.select_work_tab(crate::config::WorkAreaTab::Vfo);
        let mut svc = crate::digital::service::DigitalService::new(dtype::config(
            &s.radio.active().type_settings(),
        ));
        let rgb: Vec<u8> = (0..32 * 16 * 3).map(|i| (i % 256) as u8).collect();
        svc.add_for_test(
            crate::mfsk::rx::Picture {
                width: 32,
                height: 16,
                color: true,
                rgb: rgb.clone(),
                done: 16,
            },
            vec![1e-6; 1024],
        );
        s.digital = Some(svc);
        let full = Rect::new(0, 0, 160, 50);
        let slots = |s: &mut AppState| {
            crate::ui::screen::prepare(s, full);
            let (_, hits) = crate::ui::screen::render_offscreen(s, 160, 50);
            let o: Vec<(u8, bool)> = hits
                .overlays
                .iter()
                .filter_map(|o| match o {
                    Overlay::Picture { slot, fit, .. } => Some((*slot, *fit)),
                    _ => None,
                })
                .collect();
            (o, hits.pictures)
        };
        // a terminal: half blocks only
        let (o, p) = slots(&mut s);
        assert!(o.is_empty() && p.is_empty(), "{o:?}");
        // a window: the picture as it is, fitted, and the waterfall
        s.pixel_pictures = true;
        let (o, p) = slots(&mut s);
        assert_eq!(o, [(SLOT_DIGITAL_WATERFALL, false), (SLOT_MFSK, true)]);
        let pic = p.iter().find(|p| p.slot == SLOT_MFSK).unwrap();
        assert_eq!((pic.width, pic.height, pic.rgb), (32, 16, true));
        assert_eq!(pic.pixels, rgb);
        let wf = p.iter().find(|p| p.slot == SLOT_DIGITAL_WATERFALL).unwrap();
        assert_eq!((wf.width, wf.height), (743, 1), "every bin of 0..4 kHz");
        // the next frame makes nothing anew
        let (_, again) = slots(&mut s);
        assert!(
            again
                .iter()
                .all(|q| p.iter().any(|a| std::sync::Arc::ptr_eq(a, q)))
        );
    }

    #[test]
    fn the_layout_puts_the_picture_beside_or_above_the_text() {
        let l = layout(Rect::new(0, 0, 100, 30), true);
        assert_eq!(l.menu, Rect::new(0, 0, 100, 1));
        assert_eq!(l.status, Rect::new(0, 1, 100, 2));
        assert_eq!(l.waterfall, Rect::new(0, 3, 100, 6));
        assert_eq!(l.scale, Rect::new(0, 9, 100, 1));
        assert_eq!(l.text, Rect::new(0, 10, 60, 19));
        assert_eq!(l.picture, Rect::new(60, 10, 40, 19));
        assert_eq!(l.fkeys, Rect::new(0, 29, 100, 1));
        let narrow = layout(Rect::new(0, 0, 50, 30), true);
        assert_eq!(narrow.picture.width, 50);
        assert!(narrow.picture.y < narrow.text.y);
        assert!(layout(Rect::new(0, 0, 100, 30), false).picture.is_empty());
        for h in 0..8 {
            let _ = layout(Rect::new(0, 0, 20, h), true);
        }
    }

    #[test]
    fn columns_and_hz_agree() {
        assert_eq!(column_hz(0, 80), 25.0);
        assert_eq!(hz_column(1500.0, 80), Some(30));
        assert_eq!(hz_column(4100.0, 80), None);
    }

    #[test]
    fn wrapping_breaks_at_spaces() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa ", "bbb ccc"]);
        assert_eq!(wrap("aaa bbb ccc", 6), ["aaa ", "bbb ", "ccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
    }

    #[test]
    fn the_256_colour_cube() {
        assert_eq!(rgb_color(0, 0, 0, false), Color::Indexed(16));
        assert_eq!(rgb_color(255, 255, 255, false), Color::Indexed(231));
        assert_eq!(rgb_color(255, 0, 0, false), Color::Indexed(196));
    }
}
