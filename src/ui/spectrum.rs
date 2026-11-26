//! Power-spectrum widget (render + pure mapping helpers).
//!
//! Step 4. Draws a DC-centered dB spectrum frame as a Braille line chart
//! across the spectrum region, with a dB Y-axis, a frequency ruler, a center
//! line, and the active VFO marker. The trace and markers are themed: the
//! trace uses the top stop of the theme's `waterfall` gradient (so spectrum
//! and waterfall share one palette, per the design).
//!
//! The frame coming from `dsp::fft::SpectrumAnalyzer` is already DC-centered
//! (bin `len/2` = DC), so column mapping is a straight linear peak-decimation
//! of the whole frame across the plot width. The mapping helpers
//! (`peak_decimate`, `freq_to_col`, `col_span_hz`) are pure and unit-tested;
//! the ratatui drawing is a thin shell.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Widget};

use crate::ui::theme::Theme;

/// Default dB display range (matches SDR++'s default spectrum window).
pub const DEFAULT_DB_MIN: f64 = -70.0;
pub const DEFAULT_DB_MAX: f64 = 0.0;

/// Headroom added above the observed max / below the observed min when
/// auto-ranging (SDR++ uses 5 dB).
const AUTO_HEADROOM_DB: f64 = 5.0;
/// Minimum span so the trace never collapses to a flat line.
const MIN_DB_SPAN: f64 = 10.0;
/// Per-frame follow factor for auto-range (0..1): how far the bounds move
/// toward the latest frame's min/max each update. Small = smooth, slow.
const AUTO_FOLLOW: f64 = 0.1;
/// Step (dB) for manual min/max adjustment.
pub const DB_ADJUST_STEP: f64 = 2.0;

/// The dB display window for the spectrum/waterfall, with an auto-range mode
/// (tracks a slowly-following min/max of recent frames, SDR++-style, with
/// headroom) and a manual mode (user-set min/max via the adjust controls,
/// mirroring SDR++'s Max/Min vertical sliders). The same window drives the
/// waterfall color mapping so both stay consistent.
#[derive(Debug, Clone, PartialEq)]
pub struct DbRange {
    pub min: f64,
    pub max: f64,
    /// When true, `min`/`max` track recent frames automatically.
    pub auto: bool,
}

impl Default for DbRange {
    fn default() -> Self {
        Self {
            min: DEFAULT_DB_MIN,
            max: DEFAULT_DB_MAX,
            auto: true,
        }
    }
}

impl DbRange {
    /// Update the range from a finished frame. In auto mode the bounds ease
    /// toward `frame.min - headroom` and `frame.max + headroom`; in manual
    /// mode the frame is ignored.
    pub fn observe(&mut self, frame_db: &[f32]) {
        if !self.auto || frame_db.is_empty() {
            return;
        }
        let mut fmin = f32::INFINITY;
        let mut fmax = f32::NEG_INFINITY;
        for &v in frame_db {
            if v.is_finite() {
                fmin = fmin.min(v);
                fmax = fmax.max(v);
            }
        }
        if !fmin.is_finite() || !fmax.is_finite() {
            return;
        }
        let target_min = fmin as f64 - AUTO_HEADROOM_DB;
        let target_max = fmax as f64 + AUTO_HEADROOM_DB;
        // Ease toward the targets for a stable, non-jittery display.
        self.min += (target_min - self.min) * AUTO_FOLLOW;
        self.max += (target_max - self.max) * AUTO_FOLLOW;
        self.enforce_span();
    }

    /// Toggle auto-range on/off. Turning it off freezes the current bounds so
    /// the user can then nudge them manually.
    pub fn toggle_auto(&mut self) {
        self.auto = !self.auto;
    }

    /// Nudge the top of the window (manual control). Switches to manual mode.
    pub fn adjust_max(&mut self, delta: f64) {
        self.auto = false;
        self.max += delta;
        self.enforce_span();
    }

    /// Nudge the bottom of the window (manual control). Switches to manual.
    pub fn adjust_min(&mut self, delta: f64) {
        self.auto = false;
        self.min += delta;
        self.enforce_span();
    }

    /// Keep `max - min >= MIN_DB_SPAN` by pushing whichever bound was crossed.
    fn enforce_span(&mut self) {
        if self.max - self.min < MIN_DB_SPAN {
            // Expand around the midpoint.
            let mid = (self.max + self.min) / 2.0;
            self.min = mid - MIN_DB_SPAN / 2.0;
            self.max = mid + MIN_DB_SPAN / 2.0;
        }
    }
}

/// Peak-decimate a DC-centered dB frame to `cols` columns. Each column is the
/// max over its contiguous chunk of bins (so narrow spikes survive), scanning
/// the whole frame left to right. Returns `(x, dB)` points with `x = column
/// index` for a ratatui `Chart`. When there are fewer bins than columns, the
/// bins are returned 1:1.
pub fn peak_decimate(frame_db: &[f32], cols: usize) -> Vec<(f64, f64)> {
    if cols == 0 || frame_db.is_empty() {
        return Vec::new();
    }
    let chunk = frame_db.len() / cols;
    if chunk == 0 {
        return frame_db
            .iter()
            .enumerate()
            .map(|(i, &v)| (i as f64, v as f64))
            .collect();
    }
    (0..cols)
        .map(|i| {
            let lo = i * chunk;
            let hi = (lo + chunk).min(frame_db.len());
            let peak = frame_db[lo..hi]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            (i as f64, peak as f64)
        })
        .collect()
}

/// Columns left of the plot inside the spectrum's border: a 4-column dB
/// label gutter plus the axis column. Shared with the bookmark strip so its
/// labels sit over the same columns as the trace and the VFO marker.
pub const PLOT_LEFT_COLS: u16 = 5;

/// Plot columns for an inner (inside-border) width.
pub fn plot_columns(inner_width: u16) -> usize {
    inner_width.saturating_sub(PLOT_LEFT_COLS) as usize
}

/// Column (0..cols) for a frequency offset `hz` from center, across a span of
/// `span_hz` (the sample rate). Clamped to `[0, cols-1]`. DC (offset 0) maps to
/// the middle column.
pub fn freq_to_col(offset_hz: f64, span_hz: f64, cols: usize) -> usize {
    if cols == 0 || span_hz <= 0.0 {
        return 0;
    }
    let frac = (offset_hz / span_hz + 0.5).clamp(0.0, 1.0);
    ((frac * (cols as f64 - 1.0)).round() as usize).min(cols - 1)
}

/// Frequency offset from center (Hz) shown at plot column `col` of `cols`:
/// the inverse of `freq_to_col` (the middle column is exactly DC). Columns
/// past the right edge clamp to it. Used by click-to-tune.
pub fn col_to_offset(col: usize, span_hz: f64, cols: usize) -> f64 {
    if cols < 2 {
        return 0.0;
    }
    let frac = col.min(cols - 1) as f64 / (cols as f64 - 1.0);
    (frac - 0.5) * span_hz
}

/// The `[lo_hz, hi_hz]` absolute frequency span shown for a center and sample
/// rate: `center +/- span/2`.
pub fn col_span_hz(center_hz: f64, span_hz: f64) -> (f64, f64) {
    (center_hz - span_hz / 2.0, center_hz + span_hz / 2.0)
}

/// The themed spectrum trace color: the top (highest-power) stop of the
/// waterfall gradient, so the trace matches the hottest waterfall color.
pub fn trace_color(theme: &Theme) -> Color {
    match theme.waterfall.last() {
        Some(&(r, g, b)) => Color::Rgb(r, g, b),
        None => theme.accent,
    }
}

/// Parameters for one spectrum render.
pub struct SpectrumView<'a> {
    /// DC-centered dB frame (may be empty before the first frame arrives).
    pub frame_db: &'a [f32],
    /// Source center frequency (Hz).
    pub center_hz: f64,
    /// Visible span = source sample rate (Hz).
    pub span_hz: f64,
    /// Active VFO absolute frequency (Hz), for the marker.
    pub vfo_hz: f64,
    /// dB display range.
    pub db_min: f64,
    pub db_max: f64,
}

/// Render the spectrum into `area`. Draws the Braille dB trace via a ratatui
/// `Chart`, then overlays the center line and the active-VFO marker and a
/// frequency ruler on the bottom row.
pub fn render_spectrum(
    view: &SpectrumView,
    area: Rect,
    theme: &Theme,
    buf: &mut ratatui::buffer::Buffer,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    // Reserve the bottom row for the frequency ruler.
    let body = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    let ruler = Rect {
        x: area.x,
        y: area.y + area.height - 1,
        width: area.width,
        height: 1,
    };

    // A small Y-axis label gutter (e.g. "-70") plus the axis column.
    let plot_cols = plot_columns(body.width);
    let points = peak_decimate(view.frame_db, plot_cols);
    let x_max = if points.len() > 1 {
        (points.len() - 1) as f64
    } else {
        1.0
    };

    let trace = trace_color(theme);
    let dataset = Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(trace))
        .data(&points);

    let y_mid = (view.db_min + view.db_max) / 2.0;
    let axis_style = Style::default().fg(theme.dim);
    let x_axis = Axis::default().bounds([0.0, x_max]).style(axis_style);
    let y_axis = Axis::default()
        .bounds([view.db_min, view.db_max])
        // Fixed-width labels: the Chart sizes its gutter from the widest
        // label, and everything aligned to the plot assumes PLOT_LEFT_COLS.
        .labels(vec![
            y_label(view.db_min),
            y_label(y_mid),
            y_label(view.db_max),
        ])
        .style(axis_style);
    let chart = Chart::new(vec![dataset]).x_axis(x_axis).y_axis(y_axis);
    chart.render(body, buf);

    if plot_cols == 0 {
        return;
    }
    // The chart reserves `PLOT_LEFT_COLS` on the left for labels + axis.
    let plot_x = body.x + PLOT_LEFT_COLS;
    let plot_h = body.height;

    // Center line (DC) at the middle column.
    let center_col = freq_to_col(0.0, view.span_hz, plot_cols);
    overlay_vline(
        buf,
        plot_x + center_col as u16,
        body.y,
        plot_h,
        "│",
        theme.dim,
    );

    // Active VFO marker.
    let vfo_off = view.vfo_hz - view.center_hz;
    if vfo_off.abs() <= view.span_hz / 2.0 {
        let vcol = freq_to_col(vfo_off, view.span_hz, plot_cols);
        overlay_vline(
            buf,
            plot_x + vcol as u16,
            body.y,
            plot_h,
            "┃",
            theme.field_selected,
        );
    }

    // Frequency ruler along the bottom.
    render_ruler(
        buf,
        ruler,
        plot_x,
        plot_cols,
        view.center_hz,
        view.span_hz,
        theme,
    );
}

/// A Y-axis dB label exactly `PLOT_LEFT_COLS - 1` characters wide
/// (right-aligned; clamped so it never widens the gutter).
fn y_label(db: f64) -> String {
    const W: usize = PLOT_LEFT_COLS as usize - 1;
    format!("{:>W$.0}", db.clamp(-999.0, 9999.0))
}

/// Draw a vertical overlay line of `glyph` at column `x` from `y0` for `h` rows.
fn overlay_vline(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y0: u16,
    h: u16,
    glyph: &str,
    color: Color,
) {
    for dy in 0..h {
        if let Some(cell) = buf.cell_mut((x, y0 + dy)) {
            cell.set_symbol(glyph).set_fg(color);
        }
    }
}

/// Render frequency tick labels along `row` using `ruler_labels` (center
/// frequency at the middle, nice offsets on each side). Labels are centered on
/// their column; later ticks skip positions already written to avoid overlap.
fn render_ruler(
    buf: &mut ratatui::buffer::Buffer,
    row: Rect,
    plot_x: u16,
    plot_cols: usize,
    center_hz: f64,
    span_hz: f64,
    theme: &Theme,
) {
    if plot_cols == 0 || span_hz <= 0.0 {
        return;
    }
    let mut chars: Vec<char> = vec![' '; plot_cols];
    // Draw the center label first so it always wins the middle; then the side
    // ticks fill in where there is room.
    let ticks = ruler_labels(center_hz, span_hz, plot_cols);
    for (col, label) in ticks {
        let half = label.chars().count() / 2;
        let start = col.saturating_sub(half);
        // Skip if it would collide with already-written text (plus a space).
        let end = (start + label.chars().count()).min(plot_cols);
        let collides =
            (start.saturating_sub(1)..end.min(plot_cols)).any(|i| i < plot_cols && chars[i] != ' ');
        if collides {
            continue;
        }
        for (i, ch) in label.chars().enumerate() {
            if start + i < plot_cols {
                chars[start + i] = ch;
            }
        }
    }
    let text: String = chars.into_iter().collect();
    let area = Rect {
        x: plot_x,
        y: row.y,
        width: plot_cols as u16,
        height: 1,
    };
    let line = Line::from(Span::styled(text, Style::default().fg(theme.dim)));
    ratatui::widgets::Paragraph::new(line).render(area, buf);
}

/// Snap `raw` Hz to a "nice" step (1/2/5 x 10^k).
fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 {
        return 1.0;
    }
    let exp = raw.log10().floor();
    let base = 10f64.powf(exp);
    let m = raw / base;
    let snapped = if m < 1.5 {
        1.0
    } else if m < 3.5 {
        2.0
    } else if m < 7.5 {
        5.0
    } else {
        10.0
    };
    snapped * base
}

/// Format an absolute frequency with an auto-scaled unit (Hz / kHz / MHz /
/// GHz) and up to `decimals` fractional digits, trailing zeros trimmed.
pub fn format_freq(hz: f64, decimals: usize) -> String {
    let (val, unit, scale) = freq_unit(hz);
    let _ = scale;
    let mut s = format!("{val:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    format!("{s} {unit}")
}

/// Pick the display unit for a frequency: `(scaled_value, unit, divisor)`.
fn freq_unit(hz: f64) -> (f64, &'static str, f64) {
    if hz.abs() >= 1e9 {
        (hz / 1e9, "GHz", 1e9)
    } else if hz.abs() >= 1e6 {
        (hz / 1e6, "MHz", 1e6)
    } else if hz.abs() >= 1e3 {
        (hz / 1e3, "kHz", 1e3)
    } else {
        (hz, "Hz", 1.0)
    }
}

/// Format `hz` with enough decimals that a tick `step_hz` is distinguishable
/// once both are scaled to `hz`'s display unit.
fn format_freq_stepped(hz: f64, step_hz: f64) -> String {
    let (_, _, scale) = freq_unit(hz);
    let step_in_unit = step_hz / scale;
    // Decimals needed so step_in_unit has a non-zero last digit, capped at 3.
    let mut decimals = 0usize;
    let mut s = step_in_unit;
    while decimals < 3 && (s - s.round()).abs() > 1e-9 {
        s *= 10.0;
        decimals += 1;
    }
    format_freq(hz, decimals)
}

/// Compute ruler ticks as `(column, label)` pairs. The source center frequency
/// is always labeled at the middle column; additional ticks are placed at
/// "nice" offsets (±1/2/5·10^k) from center on each side, out to ±span/2.
/// Labels are absolute frequencies with an auto-scaled unit and enough
/// precision to reflect the tick step.
pub fn ruler_labels(center_hz: f64, span_hz: f64, plot_cols: usize) -> Vec<(usize, String)> {
    if plot_cols == 0 || span_hz <= 0.0 {
        return Vec::new();
    }
    let step = nice_step(span_hz / 4.0);

    let mut ticks: Vec<(usize, String)> = Vec::new();
    // Center tick (exact tuned frequency) at the middle column.
    let mid = freq_to_col(0.0, span_hz, plot_cols);
    ticks.push((mid, format_freq_stepped(center_hz, step)));

    // Offset ticks each side (skip center, already labeled).
    let half = span_hz / 2.0;
    let max_k = (half / step).floor() as i64;
    for k in -max_k..=max_k {
        if k == 0 {
            continue;
        }
        let offset = k as f64 * step;
        if offset.abs() > half {
            continue;
        }
        let col = freq_to_col(offset, span_hz, plot_cols);
        ticks.push((col, format_freq_stepped(center_hz + offset, step)));
    }
    ticks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_range_auto_follows_frame_min_max_with_headroom() {
        let mut r = DbRange::default();
        assert!(r.auto);
        // A frame spanning -100..-40 dB. After enough updates the window eases
        // to roughly [-105, -35] (±5 dB headroom).
        let frame = vec![-100.0f32, -70.0, -40.0];
        for _ in 0..500 {
            r.observe(&frame);
        }
        assert!((r.min - (-105.0)).abs() < 1.0, "min ~ -105 (got {})", r.min);
        assert!((r.max - (-35.0)).abs() < 1.0, "max ~ -35 (got {})", r.max);
    }

    #[test]
    fn db_range_manual_adjust_disables_auto_and_moves_bounds() {
        let mut r = DbRange::default();
        r.adjust_max(-10.0);
        assert!(!r.auto, "adjusting switches to manual");
        assert!((r.max - (DEFAULT_DB_MAX - 10.0)).abs() < 1e-9);
        let before = r.min;
        r.adjust_min(6.0);
        assert!(r.min > before);
    }

    #[test]
    fn db_range_enforces_minimum_span() {
        let mut r = DbRange {
            min: -50.0,
            max: -48.0,
            auto: false,
        };
        r.adjust_min(0.0); // triggers enforce
        assert!(
            r.max - r.min >= MIN_DB_SPAN - 1e-9,
            "span widened to the floor"
        );
    }

    #[test]
    fn db_range_manual_ignores_frames() {
        let mut r = DbRange {
            min: -80.0,
            max: -20.0,
            auto: false,
        };
        r.observe(&[-120.0, 0.0]);
        assert_eq!(r.min, -80.0, "manual min unchanged by frames");
        assert_eq!(r.max, -20.0, "manual max unchanged by frames");
    }

    #[test]
    fn peak_decimate_keeps_spikes_and_sizes_output() {
        // 1024 bins -> 128 cols: a spike must survive in its column.
        let mut frame = vec![-100.0f32; 1024];
        frame[600] = 0.0; // spike
        let pts = peak_decimate(&frame, 128);
        assert_eq!(pts.len(), 128);
        // Column covering bin 600: 600 / (1024/128=8) = 75.
        assert_eq!(pts[75].1, 0.0, "spike preserved in its column");
        assert!(pts[0].1 <= -99.0, "quiet columns stay at the floor");
    }

    #[test]
    fn peak_decimate_fewer_bins_than_cols_is_one_to_one() {
        let pts = peak_decimate(&[-10.0, -20.0], 8);
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].1, -10.0);
        assert_eq!(pts[1].1, -20.0);
    }

    #[test]
    fn freq_to_col_centers_dc_and_maps_edges() {
        let cols = 101;
        assert_eq!(freq_to_col(0.0, 1_000_000.0, cols), 50, "DC is the middle");
        assert_eq!(freq_to_col(-500_000.0, 1_000_000.0, cols), 0, "low edge");
        assert_eq!(
            freq_to_col(500_000.0, 1_000_000.0, cols),
            cols - 1,
            "high edge"
        );
        // +quarter span -> three-quarters across.
        assert_eq!(freq_to_col(250_000.0, 1_000_000.0, cols), 75);
    }

    #[test]
    fn freq_to_col_clamps_out_of_span() {
        let cols = 100;
        assert_eq!(freq_to_col(9_000_000.0, 1_000_000.0, cols), cols - 1);
        assert_eq!(freq_to_col(-9_000_000.0, 1_000_000.0, cols), 0);
    }

    #[test]
    fn col_to_offset_inverts_freq_to_col() {
        let cols = 101;
        assert_eq!(col_to_offset(50, 1_000_000.0, cols), 0.0, "middle is DC");
        assert_eq!(col_to_offset(0, 1_000_000.0, cols), -500_000.0);
        assert_eq!(col_to_offset(100, 1_000_000.0, cols), 500_000.0);
        assert_eq!(col_to_offset(75, 1_000_000.0, cols), 250_000.0);
        for cols in [1usize, 2, 7, 80, 115] {
            for c in 0..cols {
                let off = col_to_offset(c, 2_048_000.0, cols);
                assert_eq!(freq_to_col(off, 2_048_000.0, cols), c, "{cols} cols");
            }
        }
        // Past the right edge clamps to the edge.
        assert_eq!(col_to_offset(500, 1_000_000.0, cols), 500_000.0);
    }

    #[test]
    fn plot_starts_after_a_fixed_label_gutter() {
        // Three-character labels ("-70") must not narrow the gutter: the
        // markers, the bookmark strip and click-to-tune assume
        // `PLOT_LEFT_COLS`, with the Y axis line in the column before it.
        for (min, max) in [(-70.0, -20.0), (-120.0, 0.0), (-5.0, 5.0)] {
            let area = Rect::new(0, 0, 40, 8);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            let view = SpectrumView {
                frame_db: &[-50.0; 64],
                center_hz: 100e6,
                span_hz: 2e6,
                vfo_hz: 100e6,
                db_min: min,
                db_max: max,
            };
            render_spectrum(&view, area, &Theme::builtin_default(), &mut buf);
            let axis = PLOT_LEFT_COLS - 1;
            assert_eq!(buf[(axis, 2)].symbol(), "│", "axis for {min}..{max}");
            assert_ne!(buf[(axis - 1, 2)].symbol(), "│", "{min}..{max}");
        }
    }

    #[test]
    fn col_span_is_center_plus_minus_half() {
        let (lo, hi) = col_span_hz(100_000_000.0, 2_000_000.0);
        assert_eq!(lo, 99_000_000.0);
        assert_eq!(hi, 101_000_000.0);
    }

    #[test]
    fn nice_step_snaps_to_125_decades() {
        assert_eq!(nice_step(1_000_000.0 / 5.0), 200_000.0); // 200k
        assert_eq!(nice_step(0.0), 1.0);
        assert!((nice_step(430.0) - 500.0).abs() < 1e-6);
    }

    #[test]
    fn trace_color_uses_top_waterfall_stop() {
        let t = Theme::builtin_retro();
        // Retro top stop is yellow (0xFF,0xFF,0x00).
        assert_eq!(trace_color(&t), Color::Rgb(0xFF, 0xFF, 0x00));
    }

    #[test]
    fn format_freq_auto_scales_units_and_trims() {
        assert_eq!(format_freq(225_000.0, 3), "225 kHz");
        assert_eq!(format_freq(100_000_000.0, 3), "100 MHz");
        assert_eq!(format_freq(100_225_000.0, 3), "100.225 MHz");
        assert_eq!(format_freq(500.0, 0), "500 Hz");
        assert_eq!(format_freq(1_500_000_000.0, 1), "1.5 GHz");
    }

    #[test]
    fn ruler_center_label_is_the_tuned_frequency() {
        // Center 225 kHz, 2 MHz span: the middle tick must read the actual
        // tuned frequency, not 0.
        let cols = 101;
        let ticks = ruler_labels(225_000.0, 2_000_000.0, cols);
        let mid = cols / 2;
        let center_tick = ticks
            .iter()
            .find(|(c, _)| *c == mid)
            .expect("a tick at the middle column");
        assert_eq!(center_tick.1, "225 kHz", "center shows the tuned frequency");
    }

    #[test]
    fn ruler_side_ticks_are_absolute_offsets_from_center() {
        // Center 100 MHz, 2 MHz span, step = nice(500k) = 500k. A tick one step
        // above center reads 100.5 MHz.
        let cols = 201;
        let ticks = ruler_labels(100_000_000.0, 2_000_000.0, cols);
        assert!(
            ticks.iter().any(|(_, l)| l == "100.5 MHz"),
            "has a +500 kHz absolute tick: {ticks:?}"
        );
        assert!(
            ticks.iter().any(|(_, l)| l == "99.5 MHz"),
            "has a -500 kHz absolute tick"
        );
    }
}
