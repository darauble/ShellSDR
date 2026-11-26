//! The Work Area's VFO view (spec 13.5): the active VFO's type-specific
//! view, next to (Split) or instead of (Tabs) the spectrum. Each type has its
//! own view, looked up by type id; the DSP seam does not depend on the UI.
//!
//! Voice: the zoomed channel (spectrum + waterfall of about +/-1.5 x the
//! bandwidth around the channel, filter edges drawn, fine click-to-tune), the
//! audio waterfall (0..4 kHz), a meters line, and this session's recordings.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::app::AppState;
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::theme::Theme;

/// Upper edge of the audio waterfall (Hz).
pub const AUDIO_VIEW_HZ: f64 = 4_000.0;

/// Half the zoomed channel's span: 1.5 x the bandwidth, at most what the
/// channel IQ at `rate` holds.
pub fn zoom_half_span(bandwidth_hz: f64, rate: f64) -> f64 {
    let half = 1.5 * bandwidth_hz.max(1.0);
    if rate > 0.0 {
        half.min(rate / 2.0)
    } else {
        half
    }
}

/// The bins of a DC-centered `frame` (over `rate`) within +/-`half_span`.
pub fn crop_centered(frame: &[f32], rate: f64, half_span: f64) -> Vec<f32> {
    let n = frame.len();
    if n == 0 || rate <= 0.0 {
        return Vec::new();
    }
    let bin = rate / n as f64;
    let k = ((half_span / bin).floor() as usize).min(n / 2);
    let mid = n / 2;
    frame[mid.saturating_sub(k)..(mid + k + 1).min(n)].to_vec()
}

/// The bins of a DC-centered spectrum of real audio (over `rate`) from DC up
/// to `max_hz` (the upper half; the lower is its mirror).
pub fn audio_bins(frame: &[f32], rate: f64, max_hz: f64) -> Vec<f32> {
    let n = frame.len();
    if n == 0 || rate <= 0.0 {
        return Vec::new();
    }
    let k = ((max_hz / (rate / n as f64)).round() as usize).min(n / 2 - 1);
    frame[n / 2..=n / 2 + k].to_vec()
}

/// Signal-to-noise estimate from a cropped channel spectrum spanning
/// `2 * half_span`: the mean power inside the passband (+/-bw/2 around the
/// center) over the median outside it, in dB. None when either side is
/// missing.
pub fn snr_db(cropped: &[f32], half_span: f64, bandwidth_hz: f64) -> Option<f32> {
    let n = cropped.len();
    if n < 3 || half_span <= 0.0 {
        return None;
    }
    let per_bin = 2.0 * half_span / (n - 1) as f64;
    let mid = (n - 1) as f64 / 2.0;
    let (mut inside, mut outside) = (Vec::new(), Vec::new());
    for (i, &db) in cropped.iter().enumerate() {
        let off = (i as f64 - mid) * per_bin;
        if off.abs() <= bandwidth_hz / 2.0 {
            inside.push(10f64.powf(db as f64 / 10.0));
        } else {
            outside.push(db);
        }
    }
    if inside.is_empty() || outside.is_empty() {
        return None;
    }
    let signal = 10.0 * (inside.iter().sum::<f64>() / inside.len() as f64).log10();
    outside.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let noise = outside[outside.len() / 2] as f64;
    Some((signal - noise) as f32)
}

/// Draw the active VFO's view in `area`.
pub fn render_vfo_view(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let v = state.radio.active();
    // DSC highlights its own focused box; the other views have none, so the
    // frame shows the Work Area focus.
    let focused = state.nav.focus == crate::ui::nav::Focus::WorkArea
        && v.type_def()
            .is_some_and(|t| t.id() != crate::vfo::types::DSC && t.id() != crate::vfo::types::HFDL);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .title(format!("{}: {}", v.name, state.radio.type_label()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    match v.type_def() {
        Some(t) if t.id() == crate::vfo::types::VOICE => {
            render_voice_view(frame, inner, state, theme, hits)
        }
        Some(t) if t.id() == crate::vfo::types::DSC => {
            crate::ui::dsc_view::render_dsc_view(frame, inner, state, theme, hits)
        }
        Some(t)
            if t.id() == crate::vfo::types::FAX
                && t.availability() == crate::vfo::types::Availability::Ok =>
        {
            crate::ui::fax_view::render_fax_view(frame, inner, state, theme, hits)
        }
        Some(t) if t.id() == crate::vfo::types::HFDL => {
            crate::ui::hfdl_view::render_hfdl_view(frame, inner, state, theme, hits)
        }
        Some(t) if t.id() == crate::vfo::types::DIGITAL => {
            crate::ui::digital_view::render_digital_view(frame, inner, state, theme, hits)
        }
        Some(t) => {
            let text = match t.availability() {
                crate::vfo::types::Availability::Unavailable(why) => {
                    format!("{} is unavailable: {why}", v.vfo_type)
                }
                crate::vfo::types::Availability::Ok => format!("No view for {} yet", v.vfo_type),
            };
            note(frame, inner, theme, &text)
        }
        None => note(
            frame,
            inner,
            theme,
            &format!("{} is not in this build", v.vfo_type),
        ),
    }
}

fn note(frame: &mut Frame, inner: Rect, theme: &Theme, text: &str) {
    frame.render_widget(
        Paragraph::new(text.to_string())
            .style(theme.dim_style())
            .alignment(Alignment::Center),
        Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1),
    );
}

/// "1.5 kHz" / "800 Hz" for a span.
fn span_label(hz: f64) -> String {
    if hz >= 1_000.0 {
        format!("{:.1} kHz", hz / 1_000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// The Voice view: zoomed channel, audio waterfall, meters, recordings.
fn render_voice_view(
    frame: &mut Frame,
    inner: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(25),
            Constraint::Percentage(20),
            Constraint::Length(1),
            Constraint::Min(3),
        ])
        .split(inner);
    let v = state.radio.active();
    let running = state.run == crate::app::RunState::Running;
    let idle = |frame: &mut Frame, r: Rect| {
        let text = if running {
            "(waiting for signal)"
        } else {
            "(stopped)"
        };
        note(frame, r, theme, text);
    };

    // ── Zoomed channel: spectrum ──────────────────────────────────────────
    let center_offset = state.view.as_ref().map_or(0.0, |f| f.center_offset);
    let channel_center = v.frequency_hz as f64 + center_offset;
    let span = state.zoom_span_hz;
    // Before the first frame: the span the zoom will have (3 x bandwidth).
    let shown_span = if span > 0.0 {
        span
    } else {
        2.0 * zoom_half_span(v.bandwidth(), 0.0)
    };
    let zoom_block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.border_style(false))
        .title(format!("Channel {}", span_label(shown_span)));
    let zr = zoom_block.inner(rows[0]);
    frame.render_widget(zoom_block, rows[0]);
    let plot_cols = crate::ui::spectrum::plot_columns(zr.width) as u16;
    let plot = |r: Rect| Rect {
        x: r.x + crate::ui::spectrum::PLOT_LEFT_COLS.min(r.width),
        width: plot_cols.min(r.width),
        ..r
    };
    if state.zoom_db.is_empty() || span <= 0.0 {
        idle(frame, zr);
    } else {
        let view = crate::ui::spectrum::SpectrumView {
            frame_db: &state.zoom_db,
            center_hz: channel_center,
            span_hz: span,
            vfo_hz: v.frequency_hz as f64,
            db_min: state.zoom_range.min,
            db_max: state.zoom_range.max,
        };
        crate::ui::spectrum::render_spectrum(&view, zr, theme, frame.buffer_mut());
        let p = plot(zr);
        hits.overlays.push(crate::remote::wire::Overlay::Spectrum {
            rect: [p.x, zr.y, p.width, zr.height.saturating_sub(1)],
            center_hz: channel_center,
            span_hz: span,
            vfo_hz: v.frequency_hz as f64,
            db_min: state.zoom_range.min,
            db_max: state.zoom_range.max,
            trace: crate::ui::render::rgb(crate::ui::spectrum::trace_color(theme)),
            center: crate::ui::render::rgb(theme.dim),
            marker: crate::ui::render::rgb(theme.field_selected),
            stream: crate::remote::wire::STREAM_CHANNEL,
            edges_hz: v.bandwidth(),
        });
        draw_filter_edges(
            frame,
            p,
            span,
            v.bandwidth(),
            theme,
            zr.height.saturating_sub(1),
        );
        hits.push(
            p,
            HitTarget::ZoomPlot {
                x: p.x,
                cols: p.width,
            },
        );
    }

    // ── Zoomed channel: waterfall ─────────────────────────────────────────
    let zw = plot(rows[1]);
    if !state.zoom_history.is_empty() {
        crate::ui::waterfall::Waterfall {
            history: &state.zoom_history,
            range: &state.zoom_range,
            theme,
        }
        .render(zw, frame.buffer_mut());
        draw_filter_edges(frame, zw, span, v.bandwidth(), theme, zw.height);
        hits.overlays.push(crate::remote::wire::Overlay::Waterfall {
            rect: [zw.x, zw.y, zw.width, zw.height],
            center_hz: channel_center,
            span_hz: span,
            db_min: state.zoom_range.min,
            db_max: state.zoom_range.max,
            gradient: theme.waterfall.iter().map(|&(r, g, b)| [r, g, b]).collect(),
            stream: crate::remote::wire::STREAM_CHANNEL,
            edges_hz: v.bandwidth(),
        });
        hits.push(
            zw,
            HitTarget::ZoomPlot {
                x: zw.x,
                cols: zw.width,
            },
        );
    }

    // ── Audio waterfall ───────────────────────────────────────────────────
    let audio_block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.border_style(false))
        .title(format!("Audio 0..{}", span_label(AUDIO_VIEW_HZ)));
    let ar = audio_block.inner(rows[2]);
    frame.render_widget(audio_block, rows[2]);
    if state.radio.has_audio() && !state.audio_history.is_empty() {
        crate::ui::waterfall::Waterfall {
            history: &state.audio_history,
            range: &state.audio_range,
            theme,
        }
        .render(plot(ar), frame.buffer_mut());
        let a = plot(ar);
        hits.overlays.push(crate::remote::wire::Overlay::Waterfall {
            rect: [a.x, a.y, a.width, a.height],
            center_hz: AUDIO_VIEW_HZ / 2.0,
            span_hz: AUDIO_VIEW_HZ,
            db_min: state.audio_range.min,
            db_max: state.audio_range.max,
            gradient: theme.waterfall.iter().map(|&(r, g, b)| [r, g, b]).collect(),
            stream: crate::remote::wire::STREAM_AUDIO,
            edges_hz: 0.0,
        });
    } else if !ar.is_empty() {
        idle(frame, ar);
    }

    // ── Meters ────────────────────────────────────────────────────────────
    frame.render_widget(
        Paragraph::new(meters_line(state)).style(theme.text_style()),
        rows[3],
    );

    // ── Recordings ────────────────────────────────────────────────────────
    let rec_block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.border_style(false))
        .title("Recordings (this session)");
    let rr = rec_block.inner(rows[4]);
    frame.render_widget(rec_block, rows[4]);
    frame.render_widget(Paragraph::new(recording_lines(state, theme, rr)), rr);
}

/// The meters line: level, SNR, AGC gain (dashes while nothing is known).
pub fn meters_line(state: &AppState) -> String {
    let Some(f) = &state.view else {
        return "Level --  SNR --  AGC --".to_string();
    };
    let snr = snr_db(
        &state.zoom_db,
        state.zoom_span_hz / 2.0,
        state.radio.active().bandwidth(),
    )
    .map_or("--".to_string(), |s| format!("{s:.0} dB"));
    let agc = f
        .agc_gain_db
        .map_or("off".to_string(), |g| format!("{g:+.0} dB"));
    format!("Level {:.0} dBFS  SNR {snr}  AGC {agc}", f.level_db)
}

/// The recordings list, newest first: the running one (accent), then this
/// session's finished ones, as many as fit.
fn recording_lines(state: &AppState, theme: &Theme, area: Rect) -> Vec<Line<'static>> {
    let row = |r: &crate::app::RecView| {
        let t = r.seconds as u64;
        let mut s = format!(
            "{:02}:{:02}:{:02} {:>9} {}",
            t / 3600,
            t / 60 % 60,
            t % 60,
            crate::ui::render::size_label(r.bytes),
            r.file
        );
        if r.dropped > 0 {
            s.push_str(" drops!");
        }
        s.chars().take(area.width as usize).collect::<String>()
    };
    let mut lines = Vec::new();
    if let Some(r) = &state.rec_view {
        lines.push(Line::from(Span::styled(
            format!("● {}", row(r)),
            Style::default().fg(theme.accent),
        )));
    }
    for r in state.rec_history.iter().rev() {
        lines.push(Line::from(Span::styled(
            format!("  {}", row(r)),
            theme.text_style(),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (none yet)".to_string(),
            theme.dim_style(),
        )));
    }
    lines.truncate(area.height as usize);
    lines
}

/// Dim `┊` marks at the channel filter's edges (+/-bw/2 around the center)
/// in the top `rows` rows of `plot`, where the cell is empty.
fn draw_filter_edges(frame: &mut Frame, plot: Rect, span: f64, bw: f64, theme: &Theme, rows: u16) {
    if span <= 0.0 || plot.width == 0 {
        return;
    }
    let cols = plot.width as usize;
    for off in [-bw / 2.0, bw / 2.0] {
        if off.abs() > span / 2.0 {
            continue;
        }
        let c = crate::ui::spectrum::freq_to_col(off, span, cols) as u16;
        for y in plot.y..plot.y + rows.min(plot.height) {
            if let Some(cell) = frame.buffer_mut().cell_mut((plot.x + c, y))
                && cell.symbol() == " "
            {
                cell.set_symbol("┊");
                cell.set_style(theme.dim_style());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_is_one_and_a_half_bandwidths_within_the_channel_rate() {
        assert_eq!(zoom_half_span(2_800.0, 24_000.0), 4_200.0);
        assert_eq!(
            zoom_half_span(10_000.0, 15_625.0),
            7_812.5,
            "AM: what the IF holds"
        );
    }

    #[test]
    fn crop_keeps_the_bins_around_dc() {
        // 8 bins over 8 Hz: 1 Hz per bin, DC at index 4.
        let f: Vec<f32> = (0..8).map(|i| i as f32).collect();
        assert_eq!(crop_centered(&f, 8.0, 2.0), [2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(
            crop_centered(&f, 8.0, 100.0).len(),
            8,
            "never more than the frame"
        );
        assert!(crop_centered(&[], 8.0, 1.0).is_empty());
    }

    #[test]
    fn audio_bins_run_from_dc_up() {
        // 16 bins over 16 kHz: 1 kHz per bin, DC at 8.
        let f: Vec<f32> = (0..16).map(|i| i as f32).collect();
        assert_eq!(
            audio_bins(&f, 16_000.0, 4_000.0),
            [8.0, 9.0, 10.0, 11.0, 12.0]
        );
    }

    #[test]
    fn snr_compares_the_passband_with_the_noise_floor() {
        // 21 bins over +/-10 Hz; the middle 5 (+/-2 Hz) are 20 dB up.
        let f: Vec<f32> = (0..21)
            .map(|i| if (8..=12).contains(&i) { -40.0 } else { -60.0 })
            .collect();
        let snr = snr_db(&f, 10.0, 4.0).unwrap();
        assert!((snr - 20.0).abs() < 0.01, "{snr}");
        assert_eq!(snr_db(&f, 10.0, 100.0), None, "no noise bins");
    }

    #[test]
    fn meters_show_dashes_until_a_frame_arrives() {
        let mut s = AppState::default();
        assert_eq!(meters_line(&s), "Level --  SNR --  AGC --");
        s.zoom_span_hz = 8_400.0;
        s.view = Some(crate::engine::VfoViewFrame {
            level_db: -42.4,
            agc_gain_db: Some(23.0),
            ..Default::default()
        });
        assert_eq!(meters_line(&s), "Level -42 dBFS  SNR --  AGC +23 dB");
    }
}
