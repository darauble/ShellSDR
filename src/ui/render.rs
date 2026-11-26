//! Rendering: draw the whole app to a ratatui frame.
//!
//! Step 1 seam: a pure `render` function from `AppState` to a frame, tested via
//! `TestBackend` buffer assertions. It realizes the spec layout, labels every
//! control (visibility invariant), frames the focused region, and shows an
//! `OFF` note on a disabled spectrum/waterfall area.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Alignment;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidget, Widget};

use crate::app::{AppState, RunState, TuneMode};
use crate::source::model::{SourceKind, SourceModule};
use crate::ui::global::GlobalField;
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::nav::Focus;
use crate::ui::theme::Theme;
use crate::vfo::radio::{RadioField, RadioModule};

/// A bordered block whose title shows a collapse glyph, with border
/// highlighted when focused.
fn module_block<'a>(title: &str, focused: bool, collapsed: bool, theme: &Theme) -> Block<'a> {
    let glyph = if collapsed { "[+]" } else { "[-]" };
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .title(format!("{glyph} {title}"))
}

/// Left-panel module titles. A folded module adds a summary of its state:
/// the source kind, the active VFO, the selected bookmark list, and whether
/// the scheduler is on.
fn module_titles(state: &AppState, collapsed: [bool; 5]) -> [String; 5] {
    let folded = |c: bool, base: &str, extra: &str| {
        if c {
            format!("{base} [{extra}]")
        } else {
            base.to_string()
        }
    };
    let source = match state.source.kind {
        SourceKind::SoapySdr => "SoapySDR",
        SourceKind::File => "File",
        SourceKind::Network => "Network",
    };
    let list = state
        .bookmarks
        .current_list()
        .map_or("-", |l| l.name.as_str());
    let sched = if state.schedule.scheduler.enabled {
        "On"
    } else {
        "Off"
    };
    [
        folded(collapsed[0], "Source", source),
        folded(collapsed[1], "Radio", &state.radio.active().name),
        folded(collapsed[2], "Bookmarks", list),
        folded(collapsed[3], "Schedule", sched),
        folded(
            collapsed[4],
            "Report",
            &crate::ui::report_view::folded_summary(state),
        ),
    ]
}

/// Draw the whole application.
pub fn render(frame: &mut Frame, state: &AppState) {
    let _ = render_with_hits(frame, state);
}

/// Draw the whole application and return the clickable regions of the
/// frame (for mouse hit-testing against what the user actually sees).
pub fn render_with_hits(frame: &mut Frame, state: &AppState) -> HitMap {
    let mut hits = HitMap::default();
    let theme = state.theme();
    let area = frame.area();
    let r = state.work_regions(area);

    // ── Global controls bar ─────────────────────────────────────────────
    render_global_bar(frame, r.global_bar, state, &theme, &mut hits);

    // ── Left panel: Source / Radio / Bookmarks as a scrollable column ────
    // Reconcile scroll with the current viewport (handles resize + keeps the
    // focused field in view), then render the module stack into an off-screen
    // buffer of the full content height and blit the visible slice.
    render_left_panel(frame, r.left_panel, state, &theme, &mut hits);

    // ── Work Area: tab row (Tabs) and the active VFO's view ──────────────
    render_tab_row(frame, r.tab_row, state, &theme, &mut hits);
    if !r.vfo_view.is_empty() {
        if state.work_owner == crate::ui::page::WorkOwner::Vfo {
            crate::ui::vfo_view::render_vfo_view(frame, r.vfo_view, state, &theme, &mut hits);
        } else {
            crate::ui::page::render_page(frame, r.vfo_view, state, &theme, &mut hits);
        }
    }

    // ── Bookmarks strip ─────────────────────────────────────────────────
    let items = state.strip_items();
    let strip = crate::ui::bookmark_strip::StripView {
        items: &items,
        center_hz: state.center_hz() as f64,
        span_hz: state.display_span_hz(),
        rows: state.bookmarks.strip_rows,
        title: strip_title(state),
    };
    for (rect, (l, i)) in
        crate::ui::bookmark_strip::render_strip(&strip, r.bookmarks, &theme, frame.buffer_mut())
    {
        hits.push(rect, HitTarget::StripLabel(l, i));
    }

    // ── Spectrum ────────────────────────────────────────────────────────
    render_spectrum_region(frame, r.spectrum, state, &theme, &mut hits);

    // ── Waterfall ───────────────────────────────────────────────────────
    render_waterfall_region(frame, r.waterfall, state, &theme, &mut hits);

    // ── Status bar ──────────────────────────────────────────────────────
    let source_label = state
        .source
        .source_label()
        .unwrap_or_else(|| "No source".to_string());
    let run_label = match state.run {
        RunState::Running => "Running",
        RunState::Stopped => "Stopped",
    };
    let mode_label = state.radio.active().mode_label();
    let run_style = match state.run {
        RunState::Running => theme.text_style(),
        RunState::Stopped => theme.dim_style(),
    };
    let mut status_spans = vec![
        Span::raw(format!("ShellSDR  |  {source_label}  |  ")),
        Span::styled(run_label, run_style),
        Span::raw(format!("  |  {mode_label}")),
    ];
    if state.run == RunState::Running {
        status_spans.push(Span::raw(format!(
            "  |  DSP {:.0}%",
            state.dsp_load * 100.0
        )));
        if state.dsp_overflows > 0 {
            // Lost IQ blocks are audible clicks: make them stand out.
            status_spans.push(Span::styled(
                format!("  OVF {}", state.dsp_overflows),
                Style::default().fg(theme.accent),
            ));
        }
    }
    if let Some(r) = &state.rec_view {
        status_spans.push(Span::raw("  |  "));
        let who = if state.schedule.scheduler.current_job().is_some() {
            "SCHED "
        } else {
            ""
        };
        status_spans.push(Span::styled(
            format!("{who}REC {}", r.file),
            Style::default().fg(theme.accent),
        ));
    }
    if let Some(l) = state
        .source
        .network
        .link
        .as_ref()
        .filter(|_| state.source.kind == crate::source::model::SourceKind::Network)
    {
        status_spans.push(Span::raw("  |  "));
        status_spans.push(Span::styled(format!("Network: {l}"), theme.text_style()));
    }
    if let Some(v) = &state.source.share.viewer {
        status_spans.push(Span::raw("  |  "));
        let ip = v.split(' ').next().unwrap_or(v);
        // the viewer line ends with its audio: "(window, 120x40, Opus 32 kb/s)"
        let audio = v
            .rsplit(", ")
            .next()
            .map(|a| a.trim_end_matches(')'))
            .unwrap_or("");
        status_spans.push(Span::styled(
            format!("Shared: viewer {ip}, {audio}"),
            Style::default().fg(theme.accent),
        ));
    }
    if let Some(w) = &state.file_warning {
        status_spans.push(Span::raw("  |  "));
        status_spans.push(Span::styled(
            format!("READ-ONLY: {w}"),
            Style::default().fg(theme.accent),
        ));
    }
    // A DSC alert stands out wherever the user looks (seed ALR-02).
    if let Some(a) = state.dsc.as_ref().and_then(|d| d.alert()) {
        status_spans.push(Span::raw("  |  "));
        status_spans.push(Span::styled(
            format!("{}: {}", a.summary, a.body),
            Style::default()
                .fg(ratatui::style::Color::White)
                .bg(ratatui::style::Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
    }
    if let Some(a) = state.hfdl.as_ref().and_then(|d| d.alert()) {
        status_spans.push(Span::raw("  |  "));
        status_spans.push(Span::styled(
            format!("HFDL {}", a.summary),
            Style::default()
                .fg(ratatui::style::Color::White)
                .bg(ratatui::style::Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
    }
    let status = Paragraph::new(Line::from(status_spans)).style(theme.dim_style());
    frame.render_widget(status, r.status_bar);

    // ── Modal dialogs: bookmark editor, then text prompt ─────────────────
    // On the Bookmarks page the editor is drawn in the page (spec 19.4).
    if let Some(ed) = &state.bm_editor
        && !(state.page_shown() && state.work_owner == crate::ui::page::WorkOwner::Bookmarks)
    {
        let full = frame.area();
        crate::ui::bookmark_view::render_editor(ed, full, &theme, frame.buffer_mut(), &mut hits);
    }
    // On the Schedule page the editor is drawn in the page (spec 19.5).
    if let Some(ed) = &state.sched_editor
        && !(state.page_shown() && state.work_owner == crate::ui::page::WorkOwner::Schedule)
    {
        let full = frame.area();
        crate::ui::schedule_view::render_sched_editor(
            ed,
            full,
            &theme,
            frame.buffer_mut(),
            &mut hits,
        );
    }
    if let Some(b) = &state.file_browser {
        let full = frame.area();
        crate::ui::file_browser::render_browser(b, full, &theme, frame.buffer_mut(), &mut hits);
    }
    if let Some(p) = &state.prompt {
        let full = frame.area();
        let ph = crate::ui::prompt::render_prompt(p, full, &theme, frame.buffer_mut());
        hits.push(ph.frame, HitTarget::PromptFrame);
        hits.push(ph.field, HitTarget::PromptField);
        hits.push(ph.ok, HitTarget::PromptOk);
        hits.push(ph.cancel, HitTarget::PromptCancel);
    }

    // ── Modal selection popup (drawn last, over everything) ──────────────
    if let Some(popup) = &state.popup {
        // A page's pickers and confirmations open over the page.
        let full = if state.page_shown() && !r.vfo_view.is_empty() {
            r.vfo_view
        } else {
            frame.area()
        };
        let ph = crate::ui::popup::render_popup(popup, full, &theme, frame.buffer_mut());
        hits.push(ph.frame, HitTarget::PopupFrame);
        for (rect, i) in ph.items {
            hits.push(rect, HitTarget::PopupItem(i));
        }
    }

    // ── Help popup (topmost: it can be opened over anything) ─────────────
    if let Some(h) = &state.help {
        let full = frame.area();
        let rect = crate::ui::help::render_help(h, full, &theme, frame.buffer_mut());
        hits.push(rect, HitTarget::HelpFrame);
    }
    hits
}

/// A span tagged with the hit target its cells belong to (if clickable).
pub(crate) type Tagged = (Span<'static>, Option<HitTarget>);

/// Record the hit rects of a row of tagged spans drawn from (`x0`, `y`),
/// clipped to `max_w` columns. Mirrors how ratatui lays spans out: left to
/// right by display width.
pub(crate) fn record_spans(hits: &mut HitMap, x0: u16, y: u16, max_w: u16, tagged: &[Tagged]) {
    let end = x0.saturating_add(max_w);
    let mut x = x0;
    for (span, target) in tagged {
        let w = span.width() as u16;
        let x1 = x.saturating_add(w).min(end);
        if let Some(t) = target
            && x1 > x
        {
            hits.push(ratatui::layout::Rect::new(x, y, x1 - x, 1), *t);
        }
        x = x.saturating_add(w);
        if x >= end {
            break;
        }
    }
}

/// The plain line of a row of tagged spans.
pub(crate) fn untag(tagged: &[Tagged]) -> Line<'static> {
    Line::from(tagged.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>())
}

/// The `Vol[####----]` bar for a 0.0..=1.0 volume as tagged spans: the
/// brackets select the control, each segment is individually clickable.
fn volume_spans(v: f32, style: Style) -> Vec<Tagged> {
    use crate::ui::mouse::VOLUME_SEGMENTS;
    let n = VOLUME_SEGMENTS;
    let filled = (v.clamp(0.0, 1.0) * f32::from(n)).round() as u8;
    let vol = Some(HitTarget::Global(GlobalField::Volume));
    let mut spans = vec![(Span::styled("Vol[", style), vol)];
    for i in 0..n {
        let glyph = if i < filled { "#" } else { "-" };
        spans.push((
            Span::styled(glyph, style),
            Some(HitTarget::VolumeSegment(i)),
        ));
    }
    spans.push((Span::styled("]", style), vol));
    spans
}

/// Render the Global Controls bar: two lines of controls, each drawn in its
/// idle/selected/editing style, with the center frequency using the shared
/// digit editor (highlighted digit while editing).
fn render_global_bar(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let focused_region = state.nav.focus == Focus::Global;
    let cur = state.global_field;
    let editing = state.global_editing;
    // A control's style: selected only when the Global region is focused and
    // the cursor is on it; editing only for the center frequency.
    let gstyle = |f: GlobalField| {
        theme.field_style(
            focused_region && cur == f,
            focused_region && editing && cur == f,
        )
    };
    let g = |f: GlobalField| Some(HitTarget::Global(f));

    let run_label = match state.run {
        RunState::Stopped => "[Start]",
        RunState::Running => "[Stop]",
    };
    let spectrum_label = if state.spectrum_on {
        "[Spectrum:ON]"
    } else {
        "[Spectrum:OFF]"
    };
    let waterfall_label = if state.waterfall_on {
        "[Waterfall:ON]"
    } else {
        "[Waterfall:OFF]"
    };
    let tune_label = match state.tune_mode {
        TuneMode::Center => "[Center]",
        TuneMode::Free => "[Free]",
    };

    let layout_label = match state.work_area.layout {
        crate::config::WorkAreaLayout::Tabs => "[Tabs]",
        crate::config::WorkAreaLayout::Split => "[Split]",
    };
    let mut line1: Vec<Tagged> = vec![
        (
            Span::styled(run_label, gstyle(GlobalField::StartStop)),
            g(GlobalField::StartStop),
        ),
        (Span::styled("  ", theme.text_style()), None),
    ];
    line1.extend(volume_spans(state.volume, gstyle(GlobalField::Volume)));
    let mute_label = if state.muted { "[x] Mute" } else { "[ ] Mute" };
    line1.extend([
        (Span::styled("  ", theme.text_style()), None),
        (
            Span::styled(mute_label, gstyle(GlobalField::Mute)),
            g(GlobalField::Mute),
        ),
        (Span::styled("  ", theme.text_style()), None),
        (
            Span::styled(spectrum_label, gstyle(GlobalField::Spectrum)),
            g(GlobalField::Spectrum),
        ),
        (Span::styled(" ", theme.text_style()), None),
        (
            Span::styled(waterfall_label, gstyle(GlobalField::Waterfall)),
            g(GlobalField::Waterfall),
        ),
        (Span::styled(" ", theme.text_style()), None),
        (
            Span::styled(layout_label, gstyle(GlobalField::Layout)),
            g(GlobalField::Layout),
        ),
    ]);

    // Center-frequency line: "Center: <digits> Hz" + tune mode + theme.
    let freq_style = gstyle(GlobalField::Frequency);
    let mut line2: Vec<Tagged> = vec![(
        Span::styled("Center: ", freq_style),
        g(GlobalField::Frequency),
    )];
    let freq_spans = frequency_digit_spans(
        &state.center_freq,
        freq_style,
        focused_region && editing && cur == GlobalField::Frequency,
        theme,
    );
    line2.extend(freq_spans.into_iter().map(|(s, pos)| {
        let t = match pos {
            Some(p) => Some(HitTarget::CenterDigit(p)),
            None => g(GlobalField::Frequency),
        };
        (s, t)
    }));
    line2.push((Span::styled("   ", theme.text_style()), None));
    line2.push((
        Span::styled(tune_label, gstyle(GlobalField::CenterFree)),
        g(GlobalField::CenterFree),
    ));
    line2.push((Span::styled("  ", theme.text_style()), None));
    line2.push((
        Span::styled(
            format!("Theme:[{}]", theme.name),
            gstyle(GlobalField::Theme),
        ),
        g(GlobalField::Theme),
    ));
    line2.push((Span::styled("  ", theme.text_style()), None));
    // Panel width stepper: the arrows are click targets of their own.
    let pstyle = gstyle(GlobalField::Panel);
    let shown = crate::ui::layout::left_panel_width(state.left_panel_width, area.width);
    line2.extend([
        (Span::styled("Panel:[", pstyle), g(GlobalField::Panel)),
        (Span::styled("◂", pstyle), Some(HitTarget::PanelStep(false))),
        (
            Span::styled(format!(" {shown} "), pstyle),
            g(GlobalField::Panel),
        ),
        (Span::styled("▸", pstyle), Some(HitTarget::PanelStep(true))),
        (Span::styled("]", pstyle), g(GlobalField::Panel)),
    ]);
    line2.push((Span::styled("  ", theme.text_style()), None));
    line2.push((
        Span::styled("[?] Help", gstyle(GlobalField::Help)),
        g(GlobalField::Help),
    ));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused_region))
        .title(concat!(
            "Global Controls - ShellSDR v",
            env!("CARGO_PKG_VERSION")
        ));
    let inner = block.inner(area);
    for (row, line) in [&line1, &line2].into_iter().enumerate() {
        if (row as u16) < inner.height {
            record_spans(hits, inner.x, inner.y + row as u16, inner.width, line);
        }
    }
    let para = Paragraph::new(vec![untag(&line1), untag(&line2)]).block(block);
    frame.render_widget(para, area);
}

/// The Work Area's tab row (Tabs layout): `[Spectrum]` and
/// `[Master: Voice]`, the shown one highlighted; both are click targets.
fn render_tab_row(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    use crate::config::WorkAreaTab;
    if area.is_empty() {
        return;
    }
    let v = state.radio.active();
    let tab = |t: WorkAreaTab, label: String| {
        let style = if state.work_area.tab == t {
            theme
                .field_style(true, false)
                .add_modifier(ratatui::style::Modifier::REVERSED)
        } else {
            theme.dim_style()
        };
        (
            Span::styled(format!("[{label}]"), style),
            Some(HitTarget::WorkTab(t)),
        )
    };
    // A module's page takes the VFO's tab (spec 19.2).
    let (second, hint) = match state.work_owner {
        crate::ui::page::WorkOwner::Vfo => (
            format!("{}: {}", v.name, state.radio.type_label()),
            "  w: switch, l: Split".to_string(),
        ),
        o => (
            o.label().to_string(),
            format!("  w: switch, l: Split, Esc: back to {}", v.name),
        ),
    };
    let line: Vec<Tagged> = vec![
        (Span::raw(" "), None),
        tab(WorkAreaTab::Spectrum, "Spectrum".to_string()),
        (Span::raw(" "), None),
        tab(WorkAreaTab::Vfo, second),
        (Span::styled(hint, theme.dim_style()), None),
    ];
    record_spans(hits, area.x, area.y, area.width, &line);
    frame.render_widget(Paragraph::new(untag(&line)), area);
}

/// Render the Spectrum region: a titled border, then either an `OFF` note
/// (disabled), an idle note (enabled but no frame yet), or the live spectrum.
fn render_spectrum_region(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    // Title shows the dB window + mode so the (keyboard-adjustable) range is a
    // visible, labeled control (no hidden shortcuts).
    let r = &state.db_range;
    let mode = if r.auto { "auto" } else { "man" };
    let title = format!("Spectrum  [{:.0}..{:.0} dB {}]", r.min, r.max, mode);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(false))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if !state.spectrum_on {
        let off = Paragraph::new("OFF")
            .alignment(Alignment::Center)
            .style(theme.off_style());
        frame.render_widget(off, inner);
        return;
    }
    push_plot_hit(hits, inner);
    if state.spectrum.is_empty() {
        let idle = Paragraph::new("(waiting for signal)")
            .alignment(Alignment::Center)
            .style(theme.dim_style());
        frame.render_widget(idle, inner);
        return;
    }
    let view = crate::ui::spectrum::SpectrumView {
        frame_db: &state.spectrum,
        center_hz: state.center_hz() as f64,
        span_hz: state.display_span_hz(),
        vfo_hz: state.radio.active().frequency_hz as f64,
        db_min: r.min,
        db_max: r.max,
    };
    crate::ui::spectrum::render_spectrum(&view, inner, theme, frame.buffer_mut());
    // The plot (right of the dB labels, above the ruler) for a graphical
    // viewer.
    use crate::ui::spectrum::{PLOT_LEFT_COLS, plot_columns};
    hits.overlays.push(crate::remote::wire::Overlay::Spectrum {
        rect: [
            inner.x + PLOT_LEFT_COLS.min(inner.width),
            inner.y,
            plot_columns(inner.width) as u16,
            inner.height.saturating_sub(1),
        ],
        center_hz: view.center_hz,
        span_hz: view.span_hz,
        vfo_hz: view.vfo_hz,
        db_min: r.min,
        db_max: r.max,
        trace: rgb(crate::ui::spectrum::trace_color(theme)),
        center: rgb(theme.dim),
        marker: rgb(theme.field_selected),
        stream: crate::remote::wire::STREAM_MAIN,
        edges_hz: 0.0,
    });
}

/// Render the Waterfall region: a titled border, then `OFF` (disabled), an
/// idle note (enabled, no frames yet), or the scrolling spectrogram colored
/// from the theme's `waterfall` gradient through the shared dB range.
fn render_waterfall_region(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(false))
        .title("Waterfall");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if !state.waterfall_on {
        let off = Paragraph::new("OFF")
            .alignment(Alignment::Center)
            .style(theme.off_style());
        frame.render_widget(off, inner);
        return;
    }
    let plot = push_plot_hit(hits, inner);
    if state.waterfall.is_empty() {
        let idle = Paragraph::new("(waiting for signal)")
            .alignment(Alignment::Center)
            .style(theme.dim_style());
        frame.render_widget(idle, inner);
        return;
    }
    let w = crate::ui::waterfall::Waterfall {
        history: &state.waterfall,
        range: &state.db_range,
        theme,
    };
    // Drawn in the spectrum's plot columns (blank label gutter on the left)
    // so a signal sits under its trace, the VFO marker and its bookmark.
    w.render(plot, frame.buffer_mut());
    hits.overlays.push(crate::remote::wire::Overlay::Waterfall {
        rect: [plot.x, plot.y, plot.width, plot.height],
        center_hz: state.center_hz() as f64,
        span_hz: state.display_span_hz(),
        db_min: state.db_range.min,
        db_max: state.db_range.max,
        gradient: theme.waterfall.iter().map(|&(r, g, b)| [r, g, b]).collect(),
        stream: crate::remote::wire::STREAM_MAIN,
        edges_hz: 0.0,
    });
}

/// A theme colour for an overlay (RGB; a named colour as a light grey).
pub(crate) fn rgb(c: ratatui::style::Color) -> [u8; 3] {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => [r, g, b],
        _ => [200, 200, 200],
    }
}

/// The spectrum/waterfall plot area inside `inner` (right of the dB label
/// gutter), recorded as a click-to-tune target. Returns the plot rect.
fn push_plot_hit(hits: &mut HitMap, inner: ratatui::layout::Rect) -> ratatui::layout::Rect {
    use crate::ui::spectrum::{PLOT_LEFT_COLS, plot_columns};
    let cols = plot_columns(inner.width) as u16;
    let plot = ratatui::layout::Rect {
        x: inner.x + PLOT_LEFT_COLS.min(inner.width),
        width: cols,
        ..inner
    };
    hits.push(plot, HitTarget::Plot { x: plot.x, cols });
    plot
}

/// Render the Source module body (inside its bordered block) based on the
/// selected kind. Labels every control (visibility invariant) and highlights
/// the focused field when the Source region is focused.
fn render_source_contents(
    buf: &mut Buffer,
    area: ratatui::layout::Rect,
    src: &SourceModule,
    running: bool,
    focused_region: bool,
    theme: &Theme,
    hits: &mut HitMap,
) {
    use crate::source::model::SourceField;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // Clickable rows: every field's whole row (`field_row`, the same mapping
    // `panel::focused_span` relies on); File buttons get their own targets
    // on top, recorded later so they win.
    for f in src.fields() {
        let row = src.field_row(f);
        if row < inner.height {
            hits.push(
                ratatui::layout::Rect::new(inner.x, inner.y + row, inner.width, 1),
                HitTarget::Source(f),
            );
        }
    }
    let cur = src.field();
    // A field is highlighted (selected style) only when the Source region is
    // focused and the cursor is on it.
    let fstyle = |f: SourceField| theme.field_style(focused_region && cur == f, false);

    let mut lines: Vec<Line<'static>> = Vec::new();
    match src.kind {
        SourceKind::SoapySdr => {
            lines.push(Line::from(Span::styled(
                "Kind: [SoapySDR] File Network".to_string(),
                fstyle(SourceField::Kind),
            )));
            let dev = src.selected_device_label().unwrap_or("(no device)");
            lines.push(Line::from(Span::styled(
                format!("Device: {dev}"),
                fstyle(SourceField::Device),
            )));
            lines.extend(soapy_rows(src, &fstyle, theme));
        }
        SourceKind::Network => {
            use crate::source::model::short_bitrate_label;
            let w = inner.width as usize;
            let n = &src.network;
            lines.push(Line::from(Span::styled(
                "Kind: SoapySDR File [Network]".to_string(),
                fstyle(SourceField::Kind),
            )));
            let addr = if n.addr.trim().is_empty() {
                "(ENTER: host:port)".to_string()
            } else {
                ellipsize_start(n.addr.trim(), w.saturating_sub(8))
            };
            lines.push(Line::from(Span::styled(
                format!("Server: {addr}"),
                fstyle(SourceField::NetServer),
            )));
            lines.push(Line::from(match &n.status {
                Some(Ok(info)) => Span::styled(ellipsize_start(info, w), theme.dim_style()),
                Some(Err(e)) => {
                    Span::styled(ellipsize_start(e, w), Style::default().fg(theme.accent))
                }
                None => Span::styled("not asked yet", theme.dim_style()),
            }));
            let token = if n.token.trim().is_empty() {
                "(none)"
            } else {
                "••••••"
            };
            lines.push(Line::from(Span::styled(
                format!("Token: {token}"),
                fstyle(SourceField::NetToken),
            )));
            lines.push(Line::from(Span::styled(
                format!("Compression: {}", short_bitrate_label(n.bitrate)),
                fstyle(SourceField::NetAudio),
            )));
            if n.window {
                lines.push(Line::from(Span::styled(
                    "[Open window]".to_string(),
                    fstyle(SourceField::NetOpen),
                )));
            } else {
                let dev = src.selected_device_label().unwrap_or("(no input)");
                lines.push(Line::from(Span::styled(
                    format!("Device: {dev}"),
                    fstyle(SourceField::Device),
                )));
                lines.extend(soapy_rows(src, &fstyle, theme));
            }
        }
        SourceKind::File => {
            let rows = file_view_rows(src, running, inner.width as usize, &fstyle, theme);
            for (y, r) in rows.iter().enumerate() {
                if (y as u16) < inner.height {
                    record_spans(hits, inner.x, inner.y + y as u16, inner.width, r);
                }
                lines.push(untag(r));
            }
        }
    }
    lines.extend(share_rows(src, inner.width as usize, &fstyle, theme));
    Paragraph::new(lines).render(inner, buf);
}

/// The sharing rows below every kind (0.4.1, spec 22.2): Share, Listen,
/// Token, the status, and while a viewer is connected Mute and Disconnect.
fn share_rows(
    src: &SourceModule,
    w: usize,
    fstyle: &dyn Fn(crate::source::model::SourceField) -> Style,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use crate::source::model::SourceField;
    let sh = &src.share;
    let mut lines = vec![
        Line::from(Span::styled(
            format!("Share: [{}]", if sh.on { "On" } else { "Off" }),
            fstyle(SourceField::ShareOn),
        )),
        Line::from(Span::styled(
            format!(
                "Listen: {}",
                ellipsize_start(&sh.listen, w.saturating_sub(8))
            ),
            fstyle(SourceField::ShareListen),
        )),
        Line::from(Span::styled(
            format!(
                "Token: {}",
                if sh.token.trim().is_empty() {
                    "(none)"
                } else {
                    "••••••"
                }
            ),
            fstyle(SourceField::ShareToken),
        )),
    ];
    let status = match (&sh.viewer, &sh.status) {
        (Some(v), _) => Span::styled(ellipsize_start(v, w), theme.text_style()),
        (None, Some(Ok(t))) => Span::styled(ellipsize_start(t, w), theme.dim_style()),
        (None, Some(Err(e))) => {
            Span::styled(ellipsize_start(e, w), Style::default().fg(theme.accent))
        }
        (None, None) => Span::styled("not shared", theme.dim_style()),
    };
    lines.push(Line::from(status));
    if sh.viewer.is_some() {
        lines.push(Line::from(Span::styled(
            format!(
                "[{}] Mute while viewed",
                if sh.mute_while_viewed { "x" } else { " " }
            ),
            fstyle(SourceField::ShareMute),
        )));
        lines.push(Line::from(Span::styled(
            "[Disconnect]".to_string(),
            fstyle(SourceField::ShareDisconnect),
        )));
    }
    lines
}

/// The rows of an opened device (SoapySDR, or a server's input): rate,
/// decimation, bandwidth, antenna, gains and AGC.
fn soapy_rows(
    src: &SourceModule,
    fstyle: &dyn Fn(crate::source::model::SourceField) -> Style,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use crate::source::model::SourceField;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(v) = &src.soapy {
        let sr = v
            .selected_sample_rate()
            .map(|r| format!("{:.3} MHz", r / 1e6))
            .unwrap_or_else(|| "-".to_string());
        lines.push(Line::from(Span::styled(
            format!("Sample rate: {sr}"),
            fstyle(SourceField::SampleRate),
        )));
        // Decimation with the resulting effective rate, which is what
        // the spectrum, the VFOs and a baseband recording see.
        let dec = crate::dsp::power_decimator::ratio_label(src.decimation());
        let eff = if src.decimation() > 1 {
            src.effective_sample_rate()
                .map(|r| format!(" ({:.3} MHz)", r / 1e6))
                .unwrap_or_default()
        } else {
            String::new()
        };
        lines.push(Line::from(Span::styled(
            format!("Decimation: {dec}{eff}"),
            fstyle(SourceField::Decimation),
        )));
        let bw = v
            .bandwidths
            .get(v.bandwidth_idx)
            .map(|b| format!("{:.3} MHz", b / 1e6))
            .unwrap_or_else(|| "-".to_string());
        lines.push(Line::from(Span::styled(
            format!("Bandwidth: {bw}"),
            fstyle(SourceField::Bandwidth),
        )));
        if v.antennas.len() > 1 {
            let ant = v
                .antennas
                .get(v.antenna_idx)
                .map(String::as_str)
                .unwrap_or("-");
            lines.push(Line::from(Span::styled(
                format!("Antenna: {ant}"),
                fstyle(SourceField::Antenna),
            )));
        }
        for (i, g) in v.gains.iter().enumerate() {
            // RSP IFGR/RFGR are gain REDUCTIONS (higher = less gain);
            // say so, since that is the opposite of a gain slider.
            let suffix = if g.name.ends_with("GR") {
                " (red.)"
            } else {
                ""
            };
            lines.push(Line::from(Span::styled(
                format!("{}: {:.0} dB{suffix}", g.name, g.value_db),
                fstyle(SourceField::Gain(i)),
            )));
        }
        if v.agc_supported {
            lines.push(Line::from(Span::styled(
                format!("AGC: {}", if v.agc_on { "on" } else { "off" }),
                fstyle(SourceField::Agc),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "(no device opened)".to_string(),
            theme.text_style(),
        )));
    }
    lines
}

/// `m:ss.t` for a play position / duration.
pub(crate) fn clock_label(seconds: f64) -> String {
    let t = (seconds.max(0.0) * 10.0).floor() as u64;
    format!("{}:{:02}.{}", t / 600, (t / 10) % 60, t % 10)
}

/// Shorten `s` to `w` columns, keeping its end (a file name's frequency and
/// extension are the informative part).
fn ellipsize_start(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let tail: String = s.chars().skip(n - (w - 1)).collect();
    format!("…{tail}")
}

/// The File view rows (spec 7.2): Kind, File, file info (or the load error),
/// position with a progress bar, `[Start]/[Stop] [Rewind]`, and the skip
/// rows. Each button is its own click target.
fn file_view_rows(
    src: &SourceModule,
    running: bool,
    width: usize,
    fstyle: &dyn Fn(crate::source::model::SourceField) -> Style,
    theme: &Theme,
) -> Vec<Vec<Tagged>> {
    use crate::source::model::{FileButton, SKIP_SECONDS, SourceField};
    let f = &src.file;
    let mut rows: Vec<Vec<Tagged>> = Vec::new();
    rows.push(vec![(
        Span::styled("Kind: SoapySDR [File] Network", fstyle(SourceField::Kind)),
        None,
    )]);
    let name = src.source_label().unwrap_or_else(|| "(choose)".to_string());
    rows.push(vec![(
        Span::styled(
            format!("File: {}", ellipsize_start(&name, width.saturating_sub(6))),
            fstyle(SourceField::FilePath),
        ),
        None,
    )]);
    let info = match (&f.error, &f.meta) {
        (Some(e), _) => Span::styled(ellipsize_start(e, width), Style::default().fg(theme.accent)),
        (None, Some(m)) => {
            let center = m
                .center_hz
                .map(|c| format!("{:.6} MHz", c as f64 / 1e6))
                .unwrap_or_else(|| "center ?".to_string());
            Span::styled(
                format!("{:.0} kHz  {center}", m.sample_rate as f64 / 1e3),
                theme.dim_style(),
            )
        }
        (None, None) => Span::styled("ENTER on File: open a WAV", theme.dim_style()),
    };
    rows.push(vec![(info, None)]);
    let Some(m) = &f.meta else { return rows };

    let pos = format!(
        "{} / {} ",
        clock_label(f.position_s()),
        clock_label(m.duration_s())
    );
    // At Max: the speed reached, after the bar.
    let speed = match (f.fast, f.speed) {
        (true, Some(x)) => format!(" ▶▶{x:.0}×"),
        (true, None) => " ▶▶".to_string(),
        _ => String::new(),
    };
    let bar_w = width
        .saturating_sub(pos.chars().count() + 2 + speed.chars().count())
        .min(12);
    let filled = if m.frames > 0 {
        ((f.position as f64 / m.frames as f64) * bar_w as f64).round() as usize
    } else {
        0
    };
    rows.push(vec![
        (Span::styled(pos, theme.text_style()), None),
        (
            Span::styled(
                format!(
                    "▕{}{}▏",
                    "█".repeat(filled),
                    "░".repeat(bar_w - filled.min(bar_w))
                ),
                theme.dim_style(),
            ),
            None,
        ),
        (Span::styled(speed, theme.text_style()), None),
    ]);

    let button = |label: String, field: SourceField, b: FileButton| -> Tagged {
        // The row's selected button carries the field highlight; the others
        // stay plain so it is clear which one ENTER presses.
        let sel = src.field() == field && f.selected_button(field) == Some(b);
        let st = if sel {
            fstyle(field)
        } else {
            theme.text_style()
        };
        (Span::styled(label, st), Some(HitTarget::FileButton(b)))
    };
    let start = if running { "[Stop]" } else { "[Start]" };
    rows.push(vec![
        button(
            start.to_string(),
            SourceField::FileTransport,
            FileButton::StartStop,
        ),
        (Span::raw(" "), None),
        button(
            if f.fast { "[1×]" } else { "[Max]" }.to_string(),
            SourceField::FileTransport,
            FileButton::Max,
        ),
        (Span::raw(" "), None),
        button(
            "[Rewind]".to_string(),
            SourceField::FileTransport,
            FileButton::Rewind,
        ),
    ]);
    for (label, field, mk) in [
        (
            "Back: ",
            SourceField::FileBack,
            FileButton::Back as fn(usize) -> FileButton,
        ),
        (
            "Fwd:  ",
            SourceField::FileFwd,
            FileButton::Fwd as fn(usize) -> FileButton,
        ),
    ] {
        let mut r: Vec<Tagged> = vec![(Span::styled(label, theme.text_style()), None)];
        for i in 0..SKIP_SECONDS.len() {
            if i > 0 {
                r.push((Span::raw(" "), None));
            }
            r.push(button(
                format!("[{}]", FileButton::skip_label(i)),
                field,
                mk(i),
            ));
        }
        rows.push(r);
    }
    rows
}

/// Build the frequency value as styled digit spans (grouped by thousands,
/// trailing " Hz"), with no label prefix, each tagged with its digit position
/// (`None` for separators/unit). While `editing`, the digit at the cursor is
/// highlighted black-on-white. Reused by the VFO and the center bar.
pub(crate) fn frequency_digit_spans(
    freq: &crate::freq::FreqEntry,
    field_style: Style,
    editing: bool,
    theme: &Theme,
) -> Vec<(Span<'static>, Option<usize>)> {
    use crate::freq::DIGIT_POSITIONS;
    let hz = freq.display_hz();
    let cursor = freq.cursor();
    let mut spans = Vec::new();
    for pos in (0..DIGIT_POSITIONS).rev() {
        let p = 10u64.saturating_pow(pos as u32);
        let digit = ((hz / p) % 10) as u8;
        let style = if editing && pos == cursor {
            theme.digit_highlight_style()
        } else {
            field_style
        };
        spans.push((Span::styled(digit.to_string(), style), Some(pos)));
        if pos % 3 == 0 && pos != 0 {
            spans.push((Span::styled(" ", field_style), None));
        }
    }
    spans.push((Span::styled(" Hz", field_style), None));
    spans
}

/// Build the VFO frequency line ("Freq: <digits> Hz") as tagged spans: each
/// digit is its own click target, the rest selects the Frequency field.
fn frequency_line(
    freq: &crate::freq::FreqEntry,
    field_style: Style,
    editing: bool,
    theme: &Theme,
) -> Vec<Tagged> {
    let field = Some(HitTarget::Radio(RadioField::Frequency));
    let mut spans = vec![(Span::styled("Freq: ", field_style), field)];
    spans.extend(
        frequency_digit_spans(freq, field_style, editing, theme)
            .into_iter()
            .map(|(s, pos)| (s, pos.map(HitTarget::VfoDigit).or(field))),
    );
    spans
}

/// Render the Radio module body for the active VFO. Each field is drawn
/// in its idle/selected/editing style; the frequency shows the digit cursor
/// only while that field is being edited.
fn render_radio_contents(
    buf: &mut Buffer,
    area: ratatui::layout::Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let radio: &RadioModule = &state.radio;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let v = radio.active();
    let cur = radio.field();
    let editing = radio.is_editing();
    let fstyle = |f: RadioField| theme.field_style(cur == f, editing && cur == f);

    // Clickable rows: the "VFO:" header, then one row per visible field in
    // `visible_fields()` order (the order drawn below).
    for (i, f) in radio.visible_fields().into_iter().enumerate() {
        let row = 1 + i as u16;
        if row < inner.height {
            hits.push(
                ratatui::layout::Rect::new(inner.x, inner.y + row, inner.width, 1),
                HitTarget::Radio(f),
            );
        }
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("VFO: {}", v.name),
        Style::default().fg(theme.text),
    )));
    let freq_line = frequency_line(
        radio.freq(),
        fstyle(RadioField::Frequency),
        editing && cur == RadioField::Frequency,
        theme,
    );
    if inner.height > 1 {
        record_spans(hits, inner.x, inner.y + 1, inner.width, &freq_line);
    }
    lines.push(untag(&freq_line));
    lines.push(Line::from(Span::styled(
        format!("Type: {}", radio.type_label()),
        fstyle(RadioField::Type),
    )));
    // The type's own rows, built from its schema (spec 13.3).
    let settings = v.type_settings();
    for (i, f) in radio.setting_fields().iter().enumerate() {
        let text = match f.kind {
            // A folder keeps its end visible.
            crate::vfo::types::FieldKind::Path => {
                let room = (inner.width as usize).saturating_sub(f.label.chars().count() + 2);
                format!(
                    "{}: {}",
                    f.label,
                    fit_start(&f.display(f.value(&settings)), room)
                )
            }
            _ => crate::ui::schema_form::row_text(f, f.value(&settings)),
        };
        lines.push(Line::from(Span::styled(
            text,
            fstyle(RadioField::Setting(i as u8)),
        )));
    }
    if radio.has_audio() {
        lines.push(Line::from(Span::styled(
            format!("Audio: {}", audio_label(v)),
            fstyle(RadioField::Audio),
        )));
        lines.push(Line::from(Span::styled(
            if v.muted {
                "[x] Mute VFO"
            } else {
                "[ ] Mute VFO"
            },
            fstyle(RadioField::Mute),
        )));
    }
    // Recorder: type, file-name template, folder, then the button with the
    // live state.
    let w = inner.width as usize;
    // Recording outputs (spec 13.6): the type's native output, the
    // baseband, and whether the baseband is minimized.
    let check = |on: bool| if on { "[x]" } else { "[ ]" };
    let native = v.type_def().map(|t| t.native_token()).unwrap_or("native");
    let native_row = if radio.native_always_on() {
        format!("Rec: [x] Native ({native}, always)")
    } else {
        format!("Rec: {} Native ({native})", check(v.rec.native))
    };
    lines.push(Line::from(Span::styled(
        native_row,
        fstyle(RadioField::RecNative),
    )));
    lines.push(Line::from(Span::styled(
        format!("     {} Baseband", check(v.rec.baseband)),
        fstyle(RadioField::RecBaseband),
    )));
    lines.push(Line::from(Span::styled(
        format!("     {} Minimize baseband", check(v.rec.minimize)),
        fstyle(RadioField::RecMinimize),
    )));
    lines.push(Line::from(Span::styled(
        fit_end(&format!("Name: {}", v.rec_template), w),
        fstyle(RadioField::RecName),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "Dir: {}",
            fit_start(state.active_rec_dir(), w.saturating_sub(5))
        ),
        fstyle(RadioField::RecDir),
    )));
    lines.push(record_button_line(
        state,
        fstyle(RadioField::Record),
        theme,
        w,
    ));
    Paragraph::new(lines).render(inner, buf);
}

/// The bookmark strip title: the UTC clock (on-air times are UTC) and the
/// local time when it is known.
fn strip_title(state: &AppState) -> String {
    match state.local_hhmm {
        Some(l) => format!(
            "Bookmarks  UTC {}, Local: {}",
            state.utc.label(),
            crate::bookmarks::model::hhmm_label(l)
        ),
        None => format!("Bookmarks  UTC {}", state.utc.label()),
    }
}

/// `s` cut to `w` columns with a trailing ellipsis.
fn fit_end(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `s` cut to its last `w` columns with a leading ellipsis (paths: the end
/// is the informative part).
fn fit_start(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - w.saturating_sub(1)).collect();
    format!("…{tail}")
}

/// The [Record]/[Stop] button and what the recorder is doing.
fn record_button_line(state: &AppState, style: Style, theme: &Theme, w: usize) -> Line<'static> {
    let v = state.radio.active();
    if !v.recording {
        let mut spans = vec![Span::styled("[Record]".to_string(), style)];
        if let Some(e) = &state.rec_error {
            spans.push(Span::styled(
                fit_end(&format!(" {e}"), w.saturating_sub(8)),
                Style::default().fg(theme.accent),
            ));
        }
        return Line::from(spans);
    }
    let status = match &state.rec_view {
        Some(r) => {
            let t = r.seconds as u64;
            let mut s = format!(
                " REC {:02}:{:02}:{:02} {}",
                t / 3600,
                t / 60 % 60,
                t % 60,
                size_label(r.bytes)
            );
            if r.dropped > 0 {
                s.push_str(" drops!");
            }
            s
        }
        None if state.run == RunState::Running => " starting".to_string(),
        None => " waits for Start".to_string(),
    };
    Line::from(vec![
        Span::styled("[Stop]".to_string(), style),
        Span::styled(
            fit_end(&status, w.saturating_sub(6)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
    ])
}

/// Byte count as KB/MB/GB (decimal, like file managers).
pub(crate) fn size_label(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} kB", b / 1e3)
    }
}

/// Human label for a VFO's audio output: the device/sink name or "Default".
/// Resolves the stored canonical string (e.g. strips the `pw:` prefix).
fn audio_label(v: &crate::vfo::model::Vfo) -> String {
    crate::engine::audio_target::AudioTarget::from_stored(v.audio_device.as_deref()).label()
}

/// Render the whole left panel as a vertically-scrollable column. Draws the
/// three module blocks into an off-screen buffer sized to the full content
/// height, then blits the slice `[state.left_scroll, +viewport)` into `area`,
/// drawing a scrollbar when the content overflows.
fn render_left_panel(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    use crate::ui::panel::{module_heights, module_starts};
    use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

    hits.left_panel = area;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let heights = module_heights(state);
    let starts = module_starts(&heights);
    let content_h = heights
        .iter()
        .copied()
        .fold(0u16, |a, h| a.saturating_add(h));
    let viewport = area.height;
    let offset = crate::ui::scroll::clamp_offset(state.left_scroll, content_h, viewport);
    let overflow = crate::ui::scroll::needs_scrollbar(content_h, viewport);

    // The content column is narrower by one when a scrollbar is shown.
    let content_w = if overflow {
        area.width.saturating_sub(1)
    } else {
        area.width
    };

    // Off-screen buffer covering the full virtual content height.
    let virt = ratatui::layout::Rect::new(0, 0, content_w, content_h.max(1));
    let mut vbuf = Buffer::empty(virt);

    let focus = state.nav.focus;
    let collapsed = [
        state.nav.collapsed.source,
        state.nav.collapsed.radio,
        state.nav.collapsed.bookmarks,
        state.nav.collapsed.schedule,
        state.nav.collapsed.report,
    ];
    let titles = module_titles(state, collapsed);
    let regions = [
        Focus::Source,
        Focus::Radio,
        Focus::Bookmarks,
        Focus::Schedule,
        Focus::Report,
    ];
    let focused = regions.map(|r| focus == r);
    // Hit regions in virtual (content) coordinates; mapped on screen below.
    let mut vhits = HitMap::default();
    for i in 0..regions.len() {
        let rect = ratatui::layout::Rect::new(0, starts[i], content_w, heights[i]);
        module_block(&titles[i], focused[i], collapsed[i], theme).render(rect, &mut vbuf);
        // Whole module (focus), header row (focus), and the `[+]`/`[-]`
        // glyph drawn by `module_block` at the title start (collapse).
        vhits.push(rect, HitTarget::ModuleBody(regions[i]));
        vhits.push(
            ratatui::layout::Rect::new(0, starts[i], content_w, 1),
            HitTarget::ModuleHeader(regions[i]),
        );
        vhits.push(
            ratatui::layout::Rect::new(1, starts[i], 3, 1),
            HitTarget::ModuleCollapse(regions[i]),
        );
        if !collapsed[i] {
            match i {
                0 => render_source_contents(
                    &mut vbuf,
                    rect,
                    &state.source,
                    state.run == RunState::Running,
                    focus == Focus::Source,
                    theme,
                    &mut vhits,
                ),
                1 => render_radio_contents(&mut vbuf, rect, state, theme, &mut vhits),
                4 => crate::ui::report_view::render_report_contents(
                    &mut vbuf,
                    rect,
                    state,
                    focus == Focus::Report,
                    theme,
                    &mut vhits,
                ),
                3 => crate::ui::schedule_view::render_schedule_contents(
                    &mut vbuf,
                    rect,
                    state,
                    focus == Focus::Schedule,
                    theme,
                    &mut vhits,
                ),
                _ => crate::ui::bookmark_view::render_bookmarks_contents(
                    &mut vbuf,
                    rect,
                    &state.bookmarks,
                    state.utc,
                    focus == Focus::Bookmarks,
                    theme,
                    &mut vhits,
                ),
            }
        }
    }

    // Map the virtual hit regions onto the visible slice of the viewport.
    let visible_end = offset.saturating_add(viewport).min(content_h);
    for (r, t) in vhits.regions() {
        let top = r.y.max(offset);
        let bottom = r.y.saturating_add(r.height).min(visible_end);
        let w = r.width.min(content_w.saturating_sub(r.x));
        if bottom > top {
            hits.push(
                ratatui::layout::Rect::new(area.x + r.x, area.y + (top - offset), w, bottom - top),
                *t,
            );
        }
    }

    // Blit the visible slice into the on-screen area.
    let buf = frame.buffer_mut();
    for row in 0..viewport.min(content_h) {
        let src_y = offset + row;
        if src_y >= content_h {
            break;
        }
        for x in 0..content_w {
            let src = vbuf[(x, src_y)].clone();
            let dx = area.x + x;
            let dy = area.y + row;
            buf[(dx, dy)] = src;
        }
    }

    // Scrollbar on the panel's right edge when content overflows.
    if overflow {
        let mut sb_state = ScrollbarState::new(content_h as usize)
            .viewport_content_length(viewport as usize)
            .position(offset as usize);
        StatefulWidget::render(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼")),
            area,
            frame.buffer_mut(),
            &mut sb_state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::nav::NavKey;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Render `state` to a fixed-size buffer and return it as one string per
    /// row joined by newlines, for substring assertions.
    fn render_to_string(state: &AppState) -> String {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn shows_global_controls_and_module_labels() {
        let s = AppState::default();
        let screen = render_to_string(&s);
        assert!(screen.contains("Start"), "start/stop control visible");
        assert!(screen.contains("Source"), "source module visible");
        assert!(screen.contains("[-] Radio─"), "radio module visible");
        assert!(screen.contains("Bookmarks"), "bookmarks module visible");
        assert!(screen.contains("Center"), "center/free control visible");
    }

    #[test]
    fn folded_modules_summarize_their_state_in_the_title() {
        let mut s = AppState::default();
        let screen = render_to_string(&s);
        for plain in [
            "[-] Source─",
            "[-] Radio─",
            "[-] Bookmarks─",
            "[-] Schedule─",
        ] {
            assert!(screen.contains(plain), "{plain:?} unfolded:\n{screen}");
        }
        s.nav.collapsed.source = true;
        s.nav.collapsed.radio = true;
        s.nav.collapsed.bookmarks = true;
        s.nav.collapsed.schedule = true;
        s.schedule.scheduler.enabled = false;
        s.bookmarks
            .replace_store(crate::bookmarks::model::BookmarkStore {
                lists: vec![crate::bookmarks::model::BookmarkList {
                    name: "Utility".into(),
                    ..Default::default()
                }],
                ..Default::default()
            });
        let screen = render_to_string(&s);
        for folded in [
            "[+] Source [SoapySDR]",
            "[+] Radio [Master]",
            "[+] Bookmarks [Utility]",
            "[+] Schedule [Off]",
        ] {
            assert!(screen.contains(folded), "{folded:?} folded:\n{screen}");
        }
        s.source.kind = SourceKind::File;
        s.schedule.scheduler.enabled = true;
        let screen = render_to_string(&s);
        assert!(screen.contains("[+] Source [File]"), "{screen}");
        assert!(screen.contains("[+] Schedule [On]"), "{screen}");
    }

    #[test]
    fn status_bar_reflects_source_and_run_state() {
        // Stopped with a selected device: shows the device label + "Stopped".
        let mut s = AppState::with_mock_source();
        s.source.set_devices(vec![crate::source::DeviceInfo {
            label: "RSPduo Master".to_string(),
            args: "driver=sdrplay,mode=MA".to_string(),
        }]);
        let screen = render_to_string(&s);
        assert!(
            screen.contains("RSPduo Master"),
            "status bar names the selected device"
        );
        assert!(
            screen.contains("Stopped"),
            "status bar shows the stopped run state"
        );

        // Running: shows "Running".
        s.run = RunState::Running;
        let screen = render_to_string(&s);
        assert!(
            screen.contains("Running"),
            "status bar shows the running state"
        );
    }

    /// Foreground color of the first cell of `needle` on the screen.
    fn fg_at_needle(state: &AppState, needle: &str) -> Option<ratatui::style::Color> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        for y in 0..buffer.area.height {
            let row: Vec<&str> = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            let line: String = row.concat();
            if let Some(byte) = line.find(needle) {
                // Status bar is ASCII up to the run label, so byte == column.
                let col = line[..byte].chars().count() as u16;
                return buffer[(col, y)].style().fg;
            }
        }
        None
    }

    #[test]
    fn status_bar_run_state_uses_text_when_running_dim_when_stopped() {
        let mut s = AppState::default();
        let theme = s.theme();
        assert_ne!(theme.text, theme.dim, "test needs distinct roles");
        assert_eq!(fg_at_needle(&s, "Stopped"), Some(theme.dim));
        s.run = RunState::Running;
        assert_eq!(fg_at_needle(&s, "Running"), Some(theme.text));
        // The rest of the status bar stays dim.
        assert_eq!(fg_at_needle(&s, "ShellSDR  |"), Some(theme.dim));
    }

    #[test]
    fn status_bar_shows_no_source_when_none_selected() {
        let mut s = AppState::default();
        s.source.set_devices(vec![]);
        let screen = render_to_string(&s);
        assert!(
            screen.contains("No source"),
            "status bar shows no source when nothing is selectable"
        );
    }

    #[test]
    fn open_popup_overlays_its_items_on_screen() {
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        s.radio.field_next(); // Type
        s.radio.field_next(); // Mode
        s.open_popup_for_focus();
        let screen = render_to_string(&s);
        // The popup title and a couple of mode options are visible.
        assert!(screen.contains("Mode"), "popup title drawn");
        assert!(screen.contains("USB"), "a mode option drawn in the popup");
        assert!(screen.contains("CW"), "another mode option drawn");
    }

    #[test]
    fn global_controls_title_shows_program_name_and_version() {
        let screen = render_to_string(&AppState::default());
        let expected = format!("Global Controls - ShellSDR v{}", env!("CARGO_PKG_VERSION"));
        assert!(screen.contains(&expected), "title is {expected:?}");
    }

    #[test]
    fn spectrum_title_shows_db_range_and_mode() {
        let s = AppState {
            spectrum_on: true,
            ..AppState::default()
        };
        let screen = render_to_string(&s);
        assert!(screen.contains("dB"), "spectrum title shows the dB window");
        assert!(screen.contains("auto"), "shows auto-range mode by default");
    }

    #[test]
    fn spectrum_region_shows_off_idle_and_live_states() {
        // OFF when disabled.
        let mut s = AppState {
            spectrum_on: false,
            ..AppState::default()
        };
        assert!(render_to_string(&s).contains("OFF"));

        // Idle note when enabled but no frame yet.
        s.spectrum_on = true;
        s.spectrum.clear();
        assert!(
            render_to_string(&s).contains("waiting"),
            "idle note before first frame"
        );

        // A live frame renders without panicking and draws the ruler (MHz).
        s.spectrum = vec![-70.0; 4096];
        s.spectrum[2048] = 0.0; // DC spike
        s.spectrum_span_hz = 2_000_000.0;
        let screen = render_to_string(&s);
        assert!(screen.contains('M'), "frequency ruler shows MHz ticks");
    }

    #[test]
    fn waterfall_region_shows_off_idle_and_live() {
        // OFF when disabled.
        let s = AppState {
            waterfall_on: false,
            ..AppState::default()
        };
        assert!(render_to_string(&s).contains("OFF"));

        // Idle when enabled but empty history.
        let mut s = AppState::default();
        assert!(s.waterfall_on);
        assert!(s.waterfall.is_empty());
        assert!(
            render_to_string(&s).contains("waiting"),
            "idle note before any frames"
        );

        // Live: push frames and confirm it renders without panicking.
        for _ in 0..8 {
            s.waterfall.push(vec![-50.0; 4096]);
        }
        let _ = render_to_string(&s); // must not panic
        assert!(!s.waterfall.is_empty());
    }

    #[test]
    fn spectrum_and_waterfall_show_off_when_disabled() {
        let s = AppState {
            spectrum_on: false,
            waterfall_on: false,
            ..AppState::default()
        };
        let screen = render_to_string(&s);
        // Two OFF notes (spectrum + waterfall).
        let count = screen.matches("OFF").count();
        assert!(
            count >= 2,
            "both disabled displays show OFF (found {count})"
        );
    }

    #[test]
    fn collapsed_module_shows_plus_glyph() {
        let mut s = AppState {
            nav: crate::ui::nav::NavState {
                focus: Focus::Source,
                ..crate::ui::nav::NavState::default()
            },
            ..AppState::default()
        };
        s.on_nav(NavKey::Collapse);
        let screen = render_to_string(&s);
        assert!(
            screen.contains("[+] Source"),
            "collapsed Source shows [+] glyph"
        );
    }

    #[test]
    fn renders_without_panicking_on_small_area() {
        // Guard against layout math panics on tiny terminals.
        let s = AppState::default();
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &s)).unwrap();
    }

    #[test]
    fn source_view_shows_device_line_and_highlights_focused_field() {
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.source.set_devices(vec![crate::source::DeviceInfo {
            label: "RSPduo Master".into(),
            args: "driver=sdrplay,mode=MA".into(),
        }]);
        s.nav.focus = Focus::Source;
        // Move the cursor onto Device.
        s.source.field_next();
        let screen = render_to_string(&s);
        assert!(
            screen.contains("Device: RSPduo Master"),
            "source view names the selected device"
        );
    }

    #[test]
    fn soapy_source_view_shows_rsp_controls() {
        // Tall enough that the whole Source module fits without scrolling.
        let mut s = AppState::with_mock_source();
        let screen = render_scrolled(&mut s, 120, 60);
        assert!(screen.contains("SoapySDR"), "kind selector visible");
        assert!(
            screen.contains("Sample rate"),
            "sample rate control visible"
        );
        assert!(
            screen.contains("Decimation: None"),
            "decimation control visible\n{screen}"
        );
        assert!(screen.contains("Bandwidth"), "bandwidth control visible");
        assert!(screen.contains("IFGR"), "IFGR gain control visible");
        assert!(screen.contains("RFGR"), "RFGR gain control visible");
        assert!(screen.contains("AGC"), "AGC control visible\n{screen}");
    }

    #[test]
    fn decimation_line_shows_the_effective_rate() {
        let mut s = AppState::with_mock_source();
        s.source.set_decimation(8);
        let screen = render_scrolled(&mut s, 120, 60);
        assert!(
            screen.contains("Decimation: 8x (0.250 MHz)"),
            "ratio and resulting rate shown"
        );
    }

    #[test]
    fn the_dsc_view_shows_status_messages_detail_alert_and_function_keys() {
        use crate::dsc::enrich::samples;
        use crate::vfo::types::DataItem;
        let dir = std::env::temp_dir().join(format!("shellsdr-dscview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = AppState::with_mock_source();
        s.radio.set_type(crate::vfo::types::DSC);
        let mut st = s.radio.active().type_settings();
        st.insert(
            "message_dir".into(),
            dir.to_string_lossy().into_owned().into(),
        );
        st.insert("desktop_notify".into(), false.into());
        s.radio.set_active_type_settings(&st);
        s.radio.active_mut_freq(8_414_500);
        s.select_work_tab(crate::config::WorkAreaTab::Vfo);
        s.vfo_view_visible = true;
        s.sync_dsc(1_792_497_600, false);
        let call = |sym: &[u8], age| DataItem::DscCall {
            symbols: sym.to_vec(),
            age_s: age,
        };
        s.dsc_receive(
            vec![call(&samples::SAFETY, 9.0), call(&samples::DISTRESS, 1.0)],
            1_792_497_610.0,
        );
        s.dsc_fkey(6);
        let screen = render_to_string(&s);
        assert!(screen.contains("Stopped"), "{screen}");
        assert!(screen.contains("msgs: 2"));
        assert!(screen.contains("8414.5 kHz"));
        assert!(screen.contains("Messages: 2 of 2"));
        assert!(screen.contains("TIME"));
        assert!(screen.contains("Message details"));
        assert!(screen.contains("DSC DISTRESS"), "banner and status bar");
        assert!(screen.contains("1Help"));
        assert!(screen.contains("9Menu"));
        s.dsc_fkey(9);
        let screen = render_to_string(&s);
        assert!(screen.contains("Detail layout"), "{screen}");
        s.dsc_ui.menu = Some((2, 0));
        let screen = render_to_string(&s);
        assert!(screen.contains("Online ship lookup"));
        assert!(screen.contains("Off"));
        s.dsc = None;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hfdl_view_shows_status_channels_messages_detail_and_alert() {
        use crate::vfo::types::DataItem;
        let dir = std::env::temp_dir().join(format!("shellsdr-hfdlview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = AppState::with_mock_source();
        s.radio.set_type(crate::vfo::types::HFDL);
        let mut st = s.radio.active().type_settings();
        st.insert(
            "message_dir".into(),
            dir.to_string_lossy().into_owned().into(),
        );
        st.insert("notifications".into(), false.into());
        s.radio.set_active_type_settings(&st);
        s.radio.active_mut_freq(8_902_500);
        s.select_work_tab(crate::config::WorkAreaTab::Vfo);
        s.vfo_view_visible = true;
        s.sync_hfdl(1_795_600_689, false);
        let frame = |text: &str, age| DataItem::HfdlFrame {
            freq_khz: 8942,
            m1: 1,
            octets: crate::hfdl::service::acars_frame(text, "H1"),
            age_s: age,
            level_db: -30.0,
            noise_db: -50.0,
            ferr_hz: 1.0,
        };
        s.hfdl_receive(
            vec![
                frame("REQPOS037B", 9.0),
                frame("ENG 1 FIRE LOOP B FAULT", 1.0),
            ],
            1_795_600_689.0,
        );
        s.hfdl_fkey(6);
        let screen = render_to_string(&s);
        assert!(screen.contains("msgs: 2"), "{screen}");
        assert!(screen.contains("8942"));
        assert!(screen.contains("Shannon"));
        assert!(screen.contains("Messages: 2 of 2"));
        assert!(screen.contains("ACARS H1"));
        assert!(screen.contains("Message details"));
        assert!(screen.contains("CAUTION"), "banner and detail");
        assert!(screen.contains("1Help"));
        assert!(screen.contains("4Watch"));
        s.hfdl_fkey(3);
        let screen = render_to_string(&s);
        assert!(screen.contains("RA7382"), "aircraft view\n{screen}");
        s.hfdl_fkey(3);
        let screen = render_to_string(&s);
        assert!(screen.contains("Krasnoyarsk"), "ground stations\n{screen}");
        s.hfdl_fkey(9);
        s.hfdl_ui.menu = Some((3, 0));
        let screen = render_to_string(&s);
        assert!(screen.contains("Emergency keywords"), "{screen}");
        s.hfdl = None;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_fax_view_shows_its_keys_when_the_library_is_there() {
        let mut s = AppState::with_mock_source();
        s.radio.set_type(crate::vfo::types::FAX);
        s.radio.active_mut_freq(3_855_000);
        s.select_work_tab(crate::config::WorkAreaTab::Vfo);
        s.vfo_view_visible = true;
        s.sync_fax(1_792_497_600);
        let screen = render_to_string(&s);
        match crate::fax::ffi::shared() {
            Ok(_) => {
                assert!(screen.contains("1Help"), "{screen}");
                assert!(screen.contains("2Save"));
                assert!(screen.contains("6Auto"));
                assert!(screen.contains("9Menu"));
                assert!(screen.contains("Start the receiver"));
                assert!(s.nav.work_area, "the view joins the TAB order");
                s.fax_fkey(9);
                let screen = render_to_string(&s);
                assert!(screen.contains("Save the picture"), "{screen}");
            }
            // without libfax.so.1 the type cannot be chosen
            Err(_) => assert_eq!(s.radio.active().vfo_type, crate::vfo::types::VOICE),
        }
    }

    #[test]
    fn a_dsc_vfo_shows_its_folder_and_the_always_on_native_row() {
        let mut s = AppState::with_mock_source();
        s.radio.set_type(crate::vfo::types::DSC);
        let screen = render_scrolled(&mut s, 120, 70);
        assert!(screen.contains("Type: DSC"), "{screen}");
        assert!(screen.contains("Messages: (default)"));
        assert!(screen.contains("Rec: [x] Native (dsc, always)"));
        assert!(!screen.contains("Mute VFO"), "no audio");
    }

    #[test]
    fn record_button_shows_idle_waiting_running_and_error_states() {
        let mut s = AppState::with_mock_source();
        let screen = render_scrolled(&mut s, 120, 70);
        assert!(screen.contains("Rec: [x] Native (audio)"));
        assert!(screen.contains("[ ] Baseband"));
        assert!(screen.contains("[ ] Minimize baseband"));
        assert!(screen.contains("Name: $t_$f_$h-$m-$s_$d-$M-$y"));
        assert!(screen.contains("[Record]"));
        s.toggle_recording();
        let screen = render_scrolled(&mut s, 120, 70);
        assert!(
            screen.contains("[Stop] waits for Start"),
            "armed while stopped"
        );
        s.run = RunState::Running;
        s.rec_view = Some(crate::app::RecView {
            file: "a.wav".into(),
            seconds: 3723.0,
            bytes: 12_340_000,
            dropped: 0,
        });
        let screen = render_scrolled(&mut s, 120, 70);
        assert!(screen.contains("[Stop] REC 01:02:03 12.3 MB"), "{screen}");
        s.radio.stop_recording();
        s.rec_error = Some("Permission denied".into());
        let screen = render_scrolled(&mut s, 120, 70);
        assert!(screen.contains("[Record] Permission"), "error shown");
    }

    #[test]
    fn status_bar_shows_dsp_load_and_overflows_while_running() {
        let mut s = AppState::with_mock_source();
        s.dsp_load = 0.123;
        assert!(!render_to_string(&s).contains("DSP"), "hidden when stopped");
        s.run = RunState::Running;
        let screen = render_to_string(&s);
        assert!(screen.contains("DSP 12%"));
        assert!(!screen.contains("OVF"), "no overflow mark when none");
        s.dsp_overflows = 3;
        assert!(render_to_string(&s).contains("OVF 3"));
    }

    #[test]
    fn strip_title_shows_utc_and_local_time() {
        let mut s = AppState::with_mock_source();
        s.utc = crate::bookmarks::model::UtcNow {
            hhmm: 1835,
            weekday: 3,
        };
        assert!(render_to_string(&s).contains("Bookmarks  UTC 18:35"));
        assert!(
            !render_to_string(&s).contains("Local"),
            "omitted when unknown"
        );
        s.local_hhmm = Some(2035);
        assert!(render_to_string(&s).contains("Bookmarks  UTC 18:35, Local: 20:35"));
    }

    #[test]
    fn long_folder_keeps_its_end_visible() {
        assert_eq!(
            fit_start("/home/user/ShellSDR/recordings", 12),
            "…/recordings"
        );
        assert_eq!(fit_end("abcdef", 4), "abc…");
        assert_eq!(fit_end("abc", 4), "abc");
    }

    #[test]
    fn file_source_view_without_a_file_asks_for_one() {
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        let screen = render_to_string(&s);
        assert!(screen.contains("Kind: SoapySDR [File]"));
        assert!(screen.contains("File: (choose)"));
        assert!(screen.contains("ENTER on File: open a WAV"));
        assert!(!screen.contains("[Rewind]"), "no transport without a file");
    }

    #[test]
    fn file_source_view_shows_info_position_and_transport() {
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.source.file.path = Some("/x/20261104T100029_baseband_13264000Hz.wav".into());
        s.source.file.meta = Some(crate::source::file::FileMeta {
            sample_rate: 250_000,
            frames: 250_000 * 40 + 50_000,
            center_hz: Some(13_264_000),
            format: crate::source::file::SampleFormat::I16,
            tuning: None,
        });
        s.source.file.position = 250_000 * 12 + 100_000;
        s.left_panel_width = 40;
        let screen = render_to_string(&s);
        assert!(screen.contains("250 kHz  13.264000 MHz"), "{screen}");
        assert!(screen.contains("0:12.4 / 0:40.2"), "position / duration");
        assert!(screen.contains("[Start] [Max] [Rewind]"));
        assert!(screen.contains("Back: [5s] [10s] [30s] [1m]"));
        assert!(screen.contains("Fwd:  [5s] [10s] [30s] [1m]"));
        assert!(
            screen.contains("baseband_13264000Hz.wav"),
            "status bar names the file"
        );
        s.run = RunState::Running;
        assert!(render_to_string(&s).contains("[Stop] [Max] [Rewind]"));
        s.source.file.fast = true;
        s.source.file.speed = Some(23.4);
        let screen = render_to_string(&s);
        assert!(screen.contains("[Stop] [1×] [Rewind]"), "{screen}");
        assert!(screen.contains("▶▶23×"), "the speed reached");
    }

    #[test]
    fn file_buttons_are_clickable_targets() {
        use crate::source::model::FileButton;
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.source.file.meta = Some(crate::source::file::FileMeta {
            sample_rate: 1000,
            frames: 100_000,
            center_hz: None,
            format: crate::source::file::SampleFormat::I16,
            tuning: None,
        });
        let backend = TestBackend::new(120, 50);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = crate::ui::mouse::HitMap::default();
        terminal.draw(|f| hits = render_with_hits(f, &s)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let find = |text: &str| -> (u16, u16) {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect();
                if let Some(i) = row.find(text) {
                    let x = row[..i].chars().count() as u16;
                    return (x + 1, y);
                }
            }
            panic!("{text} not on screen");
        };
        let (x, y) = find("[Rewind]");
        assert_eq!(
            hits.hit(x, y),
            Some(HitTarget::FileButton(FileButton::Rewind))
        );
        let (x, y) = find("[1m]");
        assert_eq!(
            hits.hit(x, y),
            Some(HitTarget::FileButton(FileButton::Back(3))),
            "the first [1m] is on the Back row"
        );
    }

    #[test]
    fn radio_audio_line_shows_selected_device() {
        let mut s = AppState::with_mock_source();
        s.radio
            .set_audio_options(vec![crate::engine::audio::OutputOption {
                label: "USB Out".into(),
                target: crate::engine::audio_target::AudioTarget::CpalDevice("USB Out".into()),
            }]);
        s.radio.set_active_audio(Some("USB Out".to_string()));
        let screen = render_to_string(&s);
        assert!(
            screen.contains("Audio: USB Out"),
            "radio view shows the selected audio device"
        );
    }

    #[test]
    fn radio_audio_line_shows_pipewire_sink_without_prefix() {
        let mut s = AppState::with_mock_source();
        s.radio.set_active_audio(Some("pw:VirtualSink".to_string()));
        let screen = render_to_string(&s);
        assert!(
            screen.contains("Audio: VirtualSink"),
            "PipeWire sink shown by node name, not the pw: prefix"
        );
    }

    #[test]
    fn radio_module_shows_master_vfo_controls() {
        let s = AppState::with_mock_source();
        let screen = render_to_string(&s);
        assert!(screen.contains("Master"), "Master VFO name visible");
        assert!(screen.contains("Mode: NFM"), "mode control visible");
        assert!(screen.contains("Bandwidth"), "bandwidth visible");
        assert!(screen.contains("Snap"), "snap visible");
        assert!(screen.contains("Audio"), "audio device line visible");
        assert!(screen.contains("Hz"), "frequency displayed");
    }

    #[test]
    fn cw_mode_shows_tone_control() {
        let mut s = AppState::with_mock_source();
        // Cycle to CW.
        s.radio.set_mode(crate::vfo::model::Mode::Cw);
        let screen = render_to_string(&s);
        assert!(
            screen.contains("CW tone"),
            "CW tone control visible in CW mode"
        );
    }

    #[test]
    fn global_bar_shows_theme_label() {
        let s = AppState::default();
        let screen = render_to_string(&s);
        assert!(
            screen.contains("Theme:"),
            "theme control visible in global bar"
        );
        assert!(screen.contains("Default"), "default theme labelled");
    }

    #[test]
    fn theme_cycles_to_retro() {
        let mut s = AppState::default();
        s.cycle_theme();
        let screen = render_to_string(&s);
        assert!(screen.contains("Retro"), "retro theme labelled after cycle");
    }

    #[test]
    fn editing_frequency_marks_the_field() {
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        // Enter edit on the frequency field.
        s.radio.toggle_edit(0, 2_000_000_000);
        assert!(s.radio.is_editing());
        // Render should not panic and still shows the frequency.
        let screen = render_to_string(&s);
        assert!(
            screen.contains("Freq:"),
            "frequency field rendered while editing"
        );
    }

    /// Render at a given size with scroll reconciled (as the event loop does).
    fn render_scrolled(state: &mut AppState, w: u16, h: u16) -> String {
        let full = ratatui::layout::Rect::new(0, 0, w, h);
        let regions = crate::ui::layout::compute_layout(
            full,
            state.bookmarks.strip_rows,
            state.left_panel_width,
        );
        state.reconcile_left_scroll(regions.left_panel.height);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn small_terminal_keeps_focused_field_visible_not_clipped() {
        // A short terminal where the Radio module can't fully fit. Focus the
        // last field ([Record]) and confirm it is visible (scrolled into
        // view), which it would not be if content clipped behind the frame.
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        // Move to the last visible field ([Record]).
        let n = s.radio.visible_fields().len();
        for _ in 0..(n - 1) {
            s.radio.field_next();
        }
        assert_eq!(s.radio.field(), crate::vfo::radio::RadioField::Record);
        let screen = render_scrolled(&mut s, 80, 14);
        assert!(
            screen.contains("[Record]"),
            "focused last field scrolled into view on a short terminal"
        );
    }

    #[test]
    fn overflowing_panel_shows_a_scrollbar() {
        // Force overflow by using a very short terminal; a scrollbar thumb or
        // arrow should appear in the left panel column.
        let mut s = AppState::with_mock_source();
        let screen = render_scrolled(&mut s, 80, 12);
        let has_sb = screen.contains('▲') || screen.contains('▼') || screen.contains('█');
        assert!(has_sb, "scrollbar indicator present when content overflows");
    }

    /// Return the foreground color of the first cell whose symbol matches the
    /// first char of `needle`, searching row by row. Used to assert theming
    /// reaches non-VFO regions.
    fn fg_of_text(state: &AppState, needle: &str) -> Option<ratatui::style::Color> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let first = needle.chars().next().unwrap().to_string();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].symbol() == first {
                    return buffer[(x, y)].style().fg;
                }
            }
        }
        None
    }

    #[test]
    fn retro_theme_colors_non_vfo_text_green() {
        use ratatui::style::Color;
        // The status bar text ("ShellSDR...") should be the retro mid-green,
        // not the default gray, proving theming reaches outside the VFO.
        let mut s = AppState::default();
        s.cycle_theme(); // Retro
        let fg = fg_of_text(&s, "ShellSDR");
        assert!(
            matches!(fg, Some(Color::Rgb(_, g, _)) if g > 0x50),
            "status bar text uses retro green, got {fg:?}"
        );
    }

    #[test]
    fn bookmarks_module_shows_controls_rows_and_details() {
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore, UtcNow};
        let mut list = BookmarkList::new("VOLMET");
        list.bookmarks = vec![Bookmark {
            name: "Rostov".into(),
            frequency_hz: 8_939_000,
            start_utc: 100,
            end_utc: 200,
            settings: crate::bookmarks::model::voice_part(
                crate::vfo::model::Mode::Usb,
                Some(2_300.0),
                None,
            ),
            ..Bookmark::default()
        }];
        let mut s = AppState::with_mock_source();
        s.bookmarks = crate::bookmarks::manager::BookmarkManager::new(BookmarkStore {
            lists: vec![list],
            ..BookmarkStore::default()
        });
        s.nav.collapsed.source = true;
        s.utc = UtcNow {
            hhmm: 1200,
            weekday: 0,
        };
        let screen = render_to_string(&s);
        for needle in [
            "[Manager] [Quick Add]",
            "List: VOLMET",
            "Find(/):",
            "8939 Rostov",
            "USB 2.3 kHz  0100-0200",
            "UTC 12:00",
        ] {
            assert!(screen.contains(needle), "{needle:?} visible");
        }
        // Off air at 12:00: the row is dim.
        let theme = s.theme();
        assert_eq!(fg_at_needle(&s, "8939 Rostov"), Some(theme.dim));
    }

    #[test]
    fn strip_tick_sits_on_the_vfo_marker_column() {
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
        let mut list = BookmarkList::new("FM");
        list.bookmarks = vec![Bookmark {
            name: "Radiocentras".into(),
            frequency_hz: 100_437_000,
            ..Bookmark::default()
        }];
        let mut s = AppState::with_mock_source();
        s.bookmarks = crate::bookmarks::manager::BookmarkManager::new(BookmarkStore {
            lists: vec![list],
            ..BookmarkStore::default()
        });
        s.radio.active_mut_freq(100_437_000);
        s.spectrum = vec![-70.0; 4096];
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &s)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let col_of = |sym: &str| {
            (0..buf.area.height)
                .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
                .find(|&(x, y)| buf[(x, y)].symbol() == sym)
                .map(|(x, _)| x)
        };
        let tick = col_of("┬").expect("strip tick drawn");
        let marker = col_of("┃").expect("VFO marker drawn");
        assert_eq!(tick, marker, "bookmark tick aligned with the VFO marker");
    }

    #[test]
    fn retro_theme_colors_borders_green() {
        use ratatui::style::Color;
        // A border cell should be retro green under the retro theme. Border
        // glyphs are box-drawing chars; find one and check its fg.
        let mut s = AppState::default();
        s.cycle_theme();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &s)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut found_green_border = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let sym = buffer[(x, y)].symbol();
                if (sym == "─" || sym == "│")
                    && let Some(Color::Rgb(_, g, _)) = buffer[(x, y)].style().fg
                    && g > 0x50
                {
                    found_green_border = true;
                }
            }
        }
        assert!(found_green_border, "at least one border is retro green");
    }

    #[test]
    fn global_bar_shows_mute_after_volume_and_a_help_button() {
        let mut s = AppState::default();
        let screen = render_to_string(&s);
        let vol = screen.find("Vol[").unwrap();
        let mute = screen.find("[ ] Mute").expect("unmuted checkbox");
        let spec = screen.find("[Spectrum:").unwrap();
        assert!(vol < mute && mute < spec, "Vol, then Mute, then Spectrum");
        assert!(screen.contains("[?] Help"));
        s.muted = true;
        assert!(render_to_string(&s).contains("[x] Mute"));
    }

    #[test]
    fn help_popup_lists_key_bindings() {
        let mut s = AppState::default();
        s.toggle_help();
        let screen = render_to_string(&s);
        assert!(screen.contains("Help: keys"));
        assert!(screen.contains("Spectrum on / off"));
        assert!(screen.contains("Waterfall on / off"));
        assert!(screen.contains("Mute / unmute the audio"));
    }

    #[test]
    fn radio_module_shows_the_type_and_schema_rows() {
        let mut s = AppState::with_mock_source();
        s.radio.set_mode(crate::vfo::model::Mode::Cw);
        let screen = render_to_string(&s);
        for t in [
            "Type: Voice",
            "Mode: CW",
            "Bandwidth: 200 Hz",
            "Snap: 10 Hz",
            "CW tone: 800 Hz",
        ] {
            assert!(screen.contains(t), "{t} drawn");
        }
    }
}
