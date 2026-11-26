//! The Report page in the Work Area (spec 20.2): Today (a day's work,
//! stations, activity by hour), Month (one row per day), Lifetime (one row
//! per month) and Log (a day's events, filtered), with the function keys.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::AppState;
use crate::report::model::{Level, Period, keys};
use crate::report_app::ReportTab;
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::render::{Tagged, record_spans, untag};
use crate::ui::report_view::{bytes, dur_hm};
use crate::ui::theme::Theme;

/// The page's function keys.
pub const FKEYS: [(u8, &str); 6] = [
    (1, "Help"),
    (3, "Tab"),
    (4, "Older"),
    (5, "Newer"),
    (6, "Show"),
    (7, "Local/UTC"),
];

/// Hourly bars: none, then eight heights.
const BARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn spark(hours: &[u64]) -> String {
    let max = hours.iter().copied().max().unwrap_or(0);
    hours
        .iter()
        .map(|&v| {
            if v == 0 || max == 0 {
                '·'
            } else {
                BARS[(1 + (v * 7) / max).min(8) as usize]
            }
        })
        .collect()
}

/// A day's or month's output summary in one cell: `412 msgs`, `6 pics`...
fn output_cell(p: &Period, prefix: &str, key: &str, unit: &str) -> String {
    let n = p.get(&format!("{prefix}{key}"));
    if n == 0 {
        "-".to_string()
    } else if key == keys::DIGITAL_BYTES {
        bytes(n)
    } else {
        format!("{n}{unit}")
    }
}

/// The day's pictures: fax and Digital pictures that have a file.
fn picture_lines(
    events: &[crate::report::model::Event],
    clock: &crate::report::model::Clock,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use crate::report::model::Category;
    let pics: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.category, Category::Fax | Category::Digital))
        .filter_map(|e| e.file.as_ref().map(|f| (e, f)))
        .collect();
    if pics.is_empty() {
        return Vec::new();
    }
    let mut out = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("Pictures ({})", pics.len()),
            theme.dim_style(),
        )),
    ];
    for (e, f) in pics.into_iter().rev() {
        out.push(Line::from(vec![
            Span::styled(
                format!("  {} {:<8} ", clock.time_of_day(e.time), e.category.label()),
                theme.dim_style(),
            ),
            Span::styled(f.clone(), theme.text_style()),
        ]));
    }
    out
}

/// A station's mean noise floor (dB) from its counters.
fn noise_db(c: &crate::report::model::Counters) -> Option<f64> {
    let n = *c.get(keys::STATION_NOISE_N)?;
    let sum = *c.get(keys::STATION_NOISE_SUM)?;
    (n > 0).then(|| sum as f64 / n as f64 / 10.0 - 200.0)
}

/// Month: each station's daily noise floor as bars (spec 20.6).
fn noise_lines(days: &[&Period], theme: &Theme) -> Vec<Line<'static>> {
    let mut stations: Vec<&String> = days
        .iter()
        .flat_map(|p| p.stations.iter())
        .filter(|(_, c)| c.contains_key(keys::STATION_NOISE_N))
        .map(|(s, _)| s)
        .collect();
    stations.sort();
    stations.dedup();
    if stations.is_empty() {
        return Vec::new();
    }
    let mut out = vec![
        Line::default(),
        Line::from(Span::styled(
            "Noise floor by day (dB; one column per day, low to high)",
            theme.dim_style(),
        )),
    ];
    for s in stations {
        let vals: Vec<Option<f64>> = days
            .iter()
            .map(|p| p.stations.get(s).and_then(noise_db))
            .collect();
        let known: Vec<f64> = vals.iter().flatten().copied().collect();
        let (lo, hi) = known
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let bars: String = vals
            .iter()
            .map(|v| match v {
                None => '·',
                Some(v) if hi - lo < 0.5 => BARS[4],
                Some(v) => BARS[(1.0 + (v - lo) / (hi - lo) * 7.0).round() as usize],
            })
            .collect();
        let name: String = s.chars().take(24).collect();
        out.push(Line::from(vec![
            Span::styled(format!("  {name:<24} "), theme.dim_style()),
            Span::styled(bars, theme.text_style()),
            Span::styled(format!("  {lo:.0}..{hi:.0} dB"), theme.dim_style()),
        ]));
    }
    out
}

/// Today: the work of one day.
fn today_lines(p: &Period, theme: &Theme) -> Vec<Line<'static>> {
    let dim = theme.dim_style();
    let text = theme.text_style();
    let mut out = Vec::new();
    let app = p.get(keys::TIME_APP);
    let src = p.get(keys::TIME_SOURCE);
    let pct = (src * 100).checked_div(app).unwrap_or(0);
    out.push(Line::from(Span::styled(
        format!(
            "Running   ShellSDR {}   Source {} ({pct}%)   Recording {}   Starts {}   Unclean exits {}   Source failures {}",
            dur_hm(app as i64),
            dur_hm(src as i64),
            dur_hm(p.get(keys::TIME_REC) as i64),
            p.get(keys::APP_STARTS),
            p.get(keys::APP_UNCLEAN),
            p.get(keys::SOURCE_FAILURES),
        ),
        text,
    )));
    out.push(Line::default());
    out.push(Line::from(Span::styled(
        format!("{:<24}{:>10}{:>10}", "Work done", "Live", "File"),
        dim,
    )));
    let f = keys::FILE;
    let row = |label: &str, live: String, file: String, extra: String| {
        Line::from(Span::styled(
            format!("  {label:<22}{live:>10}{file:>10}   {extra}"),
            text,
        ))
    };
    let g = |k: &str| p.get(k);
    let gf = |k: &str| p.get(&format!("{f}{k}"));
    out.push(row(
        "DSC messages",
        g(keys::DSC_MESSAGES).to_string(),
        gf(keys::DSC_MESSAGES).to_string(),
        format!(
            "distress {}, urgency {}, safety {}, routine {}; garbled {}",
            g("dsc.distress") + gf("dsc.distress"),
            g("dsc.urgency") + gf("dsc.urgency"),
            g("dsc.safety") + gf("dsc.safety"),
            g("dsc.routine") + gf("dsc.routine"),
            g(keys::DSC_GARBLED) + gf(keys::DSC_GARBLED)
        ),
    ));
    out.push(row(
        "HFDL messages",
        g(keys::HFDL_MESSAGES).to_string(),
        gf(keys::HFDL_MESSAGES).to_string(),
        format!(
            "emergency {}, caution {}, routine {}, link {}; bad frames {}",
            g("hfdl.emergency") + gf("hfdl.emergency"),
            g("hfdl.caution") + gf("hfdl.caution"),
            g("hfdl.routine") + gf("hfdl.routine"),
            g("hfdl.link") + gf("hfdl.link"),
            g(keys::HFDL_BAD_FRAMES) + gf(keys::HFDL_BAD_FRAMES)
        ),
    ));
    let seen = |k: &str| p.max_of(k).unwrap_or(0).to_string();
    out.push(row(
        "HFDL planes seen",
        seen(keys::HFDL_AIRCRAFT),
        String::new(),
        "distinct aircraft heard (live)".to_string(),
    ));
    out.push(row(
        "HFDL stations received",
        seen(keys::HFDL_STATIONS),
        String::new(),
        "distinct ground stations heard (live)".to_string(),
    ));
    out.push(row(
        "FAX pictures",
        g(keys::FAX_PICTURES).to_string(),
        gf(keys::FAX_PICTURES).to_string(),
        format!(
            "{} lines; saved {}",
            g(keys::FAX_LINES) + gf(keys::FAX_LINES),
            g(keys::FAX_SAVED)
        ),
    ));
    out.push(row(
        "Digital text",
        bytes(g(keys::DIGITAL_BYTES)),
        bytes(gf(keys::DIGITAL_BYTES)),
        format!(
            "{} pictures; RSID {} ({} switches)",
            g(keys::DIGITAL_PICTURES) + gf(keys::DIGITAL_PICTURES),
            g(keys::DIGITAL_RSID) + gf(keys::DIGITAL_RSID),
            g(keys::DIGITAL_SWITCHES) + gf(keys::DIGITAL_SWITCHES)
        ),
    ));
    out.push(row(
        "Recordings",
        format!("{} files", g(keys::REC_FILES)),
        bytes(g(keys::REC_BYTES)),
        String::new(),
    ));
    out.push(row(
        "Schedule",
        format!("{} runs", g(keys::SCHED_RUNS)),
        String::new(),
        format!("{} skipped", g(keys::SCHED_SKIPPED)),
    ));
    out.push(Line::default());
    // Time per VFO type.
    let types: Vec<String> = p
        .counters
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix(keys::TIME_TYPE)
                .map(|t| format!("{t} {}", dur_hm(*v as i64)))
        })
        .collect();
    if !types.is_empty() {
        out.push(Line::from(vec![
            Span::styled("VFO types  ", dim),
            Span::styled(types.join("   "), text),
        ]));
        out.push(Line::default());
    }
    // Stations, the longest tuned first.
    out.push(Line::from(Span::styled(
        format!("{:<32}{:>9}   Output", "By station", "Tuned"),
        dim,
    )));
    let mut st: Vec<(&String, &crate::report::model::Counters)> = p.stations.iter().collect();
    st.sort_by_key(|(_, c)| std::cmp::Reverse(c.get(keys::STATION_TIME).copied().unwrap_or(0)));
    if st.is_empty() {
        out.push(Line::from(Span::styled(
            "  (no station tuned while running)",
            dim,
        )));
    }
    for (name, c) in st {
        let get = |k: &str| c.get(k).copied().unwrap_or(0);
        let mut outp = Vec::new();
        if get(keys::DSC_MESSAGES) > 0 {
            outp.push(format!("{} msgs", get(keys::DSC_MESSAGES)));
        }
        if get(keys::HFDL_MESSAGES) > 0 {
            outp.push(format!("{} HFDL msgs", get(keys::HFDL_MESSAGES)));
        }
        if get(keys::FAX_PICTURES) > 0 {
            outp.push(format!("{} fax pictures", get(keys::FAX_PICTURES)));
        }
        if get(keys::DIGITAL_BYTES) > 0 {
            outp.push(bytes(get(keys::DIGITAL_BYTES)));
        }
        if get(keys::DIGITAL_PICTURES) > 0 {
            outp.push(format!("{} pictures", get(keys::DIGITAL_PICTURES)));
        }
        if let Some(db) = noise_db(c) {
            outp.push(format!("noise {db:.0} dB"));
        }
        let name: String = name.chars().take(30).collect();
        out.push(Line::from(Span::styled(
            format!(
                "  {name:<30}{:>9}   {}",
                dur_hm(get(keys::STATION_TIME) as i64),
                outp.join(", ")
            ),
            text,
        )));
    }
    out.push(Line::default());
    out.push(Line::from(Span::styled(
        "Activity by hour   00  02  04  06  08  10  12  14  16  18  20  22",
        dim,
    )));
    for (label, key) in [
        ("DSC", keys::DSC_MESSAGES),
        ("HFDL", keys::HFDL_MESSAGES),
        ("FAX", keys::FAX_PICTURES),
        ("Digital", keys::DIGITAL_BYTES),
    ] {
        if let Some(h) = p.hourly.get(key) {
            // Two columns per hour, under the labels.
            let bars: String = spark(h).chars().flat_map(|c| [c, c]).collect();
            out.push(Line::from(vec![
                Span::styled(format!("  {label:<17}"), dim),
                Span::styled(bars, text),
            ]));
        }
    }
    out.push(Line::default());
    let maxv = |k: &str| p.max_of(k);
    let mut health = vec![
        format!(
            "DSP max {}",
            maxv(keys::HEALTH_DSP).map_or("-".into(), |v| format!("{}%", v / 10))
        ),
        format!("Drops {}", g(keys::HEALTH_DROPS)),
    ];
    if let Some(t) = maxv(keys::HEALTH_TEMP) {
        health.push(format!("CPU max {:.0}°C", t as f64 / 10.0));
    }
    if let Some(m) = maxv(keys::HEALTH_MEM) {
        health.push(format!("Memory max {} MB", m / 1024));
    }
    if g(keys::WATCHDOG) > 0 {
        health.push(format!("Watchdog restarts {}", g(keys::WATCHDOG)));
    }
    out.push(Line::from(vec![
        Span::styled("Health    ", dim),
        Span::styled(health.join("   "), text),
    ]));
    out.push(Line::from(Span::styled(
        format!(
            "Log   warnings {}, errors {}",
            g(keys::WARNINGS),
            g(keys::ERRORS)
        ),
        dim,
    )));
    out
}

/// Month or Lifetime: one row per period, then the total.
fn table_lines(
    periods: &[&Period],
    total: &Period,
    first: &str,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let dim = theme.dim_style();
    let text = theme.text_style();
    let mut out = vec![Line::from(Span::styled(
        format!(
            "  {first:<9}{:>10}{:>10}{:>8}{:>8}{:>8}{:>6}{:>8}{:>10}{:>10}{:>10}{:>7}{:>7}",
            "Up",
            "Source",
            "DSC",
            "HFDL",
            "Planes",
            "GS",
            "FAX",
            "Digital",
            "Rec",
            "Rec size",
            "Jobs",
            "Warn"
        ),
        dim,
    ))];
    let row = |label: &str, p: &Period| {
        format!(
            "  {label:<9}{:>10}{:>10}{:>8}{:>8}{:>8}{:>6}{:>8}{:>10}{:>10}{:>10}{:>7}{:>7}",
            dur_hm(p.get(keys::TIME_APP) as i64),
            dur_hm(p.get(keys::TIME_SOURCE) as i64),
            output_cell(p, "", keys::DSC_MESSAGES, ""),
            output_cell(p, "", keys::HFDL_MESSAGES, ""),
            p.max_of(keys::HFDL_AIRCRAFT)
                .map_or("-".to_string(), |n| n.to_string()),
            p.max_of(keys::HFDL_STATIONS)
                .map_or("-".to_string(), |n| n.to_string()),
            output_cell(p, "", keys::FAX_PICTURES, ""),
            output_cell(p, "", keys::DIGITAL_BYTES, ""),
            output_cell(p, "", keys::REC_FILES, ""),
            if p.get(keys::REC_BYTES) > 0 {
                bytes(p.get(keys::REC_BYTES))
            } else {
                "-".to_string()
            },
            p.get(keys::SCHED_RUNS),
            p.get(keys::WARNINGS) + p.get(keys::ERRORS),
        )
    };
    if periods.is_empty() {
        out.push(Line::from(Span::styled("  (nothing counted)", dim)));
    }
    for p in periods {
        let label = if p.key.len() == 10 {
            &p.key[5..]
        } else {
            &p.key[..]
        };
        out.push(Line::from(Span::styled(row(label, p), text)));
    }
    out.push(Line::from(Span::styled(
        row("Total", total),
        text.add_modifier(Modifier::BOLD),
    )));
    out
}

/// Draw the page body (inside its frame) in `area`.
pub fn render_report_page(
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
    let Some(r) = &state.report else {
        frame.render_widget(
            Paragraph::new(Span::styled("The report is not open.", theme.dim_style())),
            area,
        );
        return;
    };
    let ui = &state.report_ui;
    let clock = r.clock(state.utc_offset_min);
    let zone = if clock.local { "local" } else { "UTC" };

    // The tab row and the period on the right.
    let mut row: Vec<Tagged> = vec![(Span::raw(" "), None)];
    for t in ReportTab::ALL {
        let st = if t == ui.tab {
            theme
                .field_style(true, false)
                .add_modifier(Modifier::REVERSED)
        } else {
            theme.dim_style()
        };
        row.push((
            Span::styled(format!("[{}]", t.label()), st),
            Some(HitTarget::ReportTab(t)),
        ));
        row.push((Span::raw(" "), None));
    }
    let period = match ui.tab {
        ReportTab::Today | ReportTab::Log => state.report_day().unwrap_or_default(),
        ReportTab::Month => state.report_month().unwrap_or_default(),
        ReportTab::Lifetime => {
            let since = clock.moment(r.doc().first_start).day;
            format!("since {since}")
        }
        ReportTab::Settings => String::new(),
    };
    if ui.tab == ReportTab::Log {
        row.push((
            Span::styled(format!("  Show: [{}]", ui.show.label()), theme.text_style()),
            Some(HitTarget::PageFKey(6)),
        ));
        row.push((
            Span::styled(
                format!("  Local time: [{}]", if clock.local { 'x' } else { ' ' }),
                theme.text_style(),
            ),
            Some(HitTarget::PageFKey(7)),
        ));
    }
    let used: usize = row.iter().map(|(s, _)| s.content.chars().count()).sum();
    let right = format!("{period} ({zone})");
    let need = right.chars().count() + 6;
    if ui.tab != ReportTab::Settings && used + need < area.width as usize {
        row.push((
            Span::raw(" ".repeat(area.width as usize - used - need)),
            None,
        ));
        if ui.tab != ReportTab::Lifetime {
            row.push((
                Span::styled("◂ ", theme.text_style()),
                Some(HitTarget::ReportStep(-1)),
            ));
        } else {
            row.push((Span::raw("  "), None));
        }
        row.push((Span::styled(right, theme.text_style()), None));
        if ui.tab != ReportTab::Lifetime {
            row.push((
                Span::styled(" ▸", theme.text_style()),
                Some(HitTarget::ReportStep(1)),
            ));
        }
    }
    record_spans(hits, area.x, area.y, area.width, &row);
    frame
        .buffer_mut()
        .set_line(area.x, area.y, &untag(&row), area.width);
    if let Some(e) = r.error() {
        let y = area.y + 1;
        frame.buffer_mut().set_stringn(
            area.x,
            y,
            format!(" Report folder not writable: {e}"),
            area.width as usize,
            Style::default().fg(theme.accent),
        );
    }

    // The body.
    let body = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(3),
    );
    if ui.tab == ReportTab::Settings {
        return render_settings(frame, body, state, theme, hits);
    }
    let lines: Vec<Line> = match ui.tab {
        ReportTab::Today => {
            let empty = Period::new("");
            let day = state.report_day().unwrap_or_default();
            let mut l = today_lines(r.doc().day(&day).unwrap_or(&empty), theme);
            l.extend(picture_lines(state.report_day_events(), &clock, theme));
            l
        }
        ReportTab::Month => {
            let month = state.report_month().unwrap_or_default();
            let days: Vec<&Period> = r
                .doc()
                .days
                .iter()
                .filter(|p| p.key.starts_with(&month))
                .collect();
            let empty = Period::new("");
            let total = r.doc().month(&month).unwrap_or(&empty);
            let mut l = table_lines(&days, total, "Day", theme);
            l.extend(noise_lines(&days, theme));
            l
        }
        ReportTab::Lifetime => {
            let months: Vec<&Period> = r.doc().months.iter().collect();
            table_lines(&months, &r.lifetime(), "Month", theme)
        }
        ReportTab::Settings => Vec::new(),
        ReportTab::Log => {
            let events = state.report_log();
            if events.is_empty() {
                vec![Line::from(Span::styled("  (no events)", theme.dim_style()))]
            } else {
                events
                    .iter()
                    .rev()
                    .map(|e| {
                        let st = match e.level {
                            Level::Info => theme.text_style(),
                            _ => Style::default().fg(theme.accent),
                        };
                        let file = e
                            .file
                            .as_ref()
                            .map_or(String::new(), |f| format!("  [{f}]"));
                        Line::from(Span::styled(
                            format!(
                                " {}  {:<3} {:<8} {}{file}",
                                clock.time_of_day(e.time),
                                e.level.mark(),
                                e.category.label(),
                                e.text
                            ),
                            st,
                        ))
                    })
                    .collect()
            }
        }
    };
    let max_scroll = lines.len().saturating_sub(body.height as usize);
    let scroll = ui.scroll.min(max_scroll);
    frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), body);
}

/// `path` cut from the left to `w` columns (`…/the/tail`).
fn tail(path: &str, w: usize) -> String {
    let n = path.chars().count();
    if n <= w || w < 2 {
        return path.to_string();
    }
    let keep: String = path.chars().skip(n - (w - 1)).collect();
    format!("…{keep}")
}

/// The Settings tab: the folder, the clock, the retention (spec 20.5).
fn render_settings(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    use crate::report_app::ReportSetting;
    let c = &state.report_config;
    let focused = state.nav.focus == crate::ui::nav::Focus::WorkArea;
    let keep = |n: u32, unit: &str| {
        if n == 0 {
            "forever".to_string()
        } else {
            format!("{n} {unit}")
        }
    };
    let files = state.report.as_ref().map(|r| r.files().clone());
    for (i, row) in ReportSetting::ALL.iter().enumerate() {
        if i as u16 >= area.height {
            return;
        }
        let (label, value, hint) = match row {
            ReportSetting::Folder => (
                "Folder",
                format!(
                    "[{}]",
                    tail(&c.dir, (area.width as usize).saturating_sub(60).max(20))
                ),
                "ENTER: change (Move / Start fresh)",
            ),
            ReportSetting::LocalTime => (
                "Local time",
                format!("[{}]", if c.local_time { 'x' } else { ' ' }),
                "days and log times in local time, else UTC",
            ),
            ReportSetting::KeepDays => (
                "Daily counters",
                format!("◂ {} ▸", keep(c.keep_days, "days")),
                "◂ ▸ 30 days, ENTER: type",
            ),
            ReportSetting::KeepLogs => (
                "Logs",
                format!("◂ {} ▸", keep(c.keep_log_days, "days")),
                "◂ ▸ 10 days, ENTER: type",
            ),
            ReportSetting::KeepMonths => (
                "Monthly counters",
                format!("◂ {} ▸", keep(c.keep_months, "months")),
                "◂ ▸ 12 months, ENTER: type",
            ),
            ReportSetting::Hook => (
                "Event hook",
                if c.hook.trim().is_empty() {
                    "[(none)]".to_string()
                } else {
                    format!(
                        "[{}]",
                        tail(&c.hook, (area.width as usize).saturating_sub(60).max(20))
                    )
                },
                "ENTER: the command ($1 level, $2 category, $3 text)",
            ),
            ReportSetting::HookErrors => (
                "  on errors",
                format!("[{}]", if c.hook_errors { 'x' } else { ' ' }),
                "every error",
            ),
            ReportSetting::HookWarnings => (
                "  on warnings",
                format!("[{}]", if c.hook_warnings { 'x' } else { ' ' }),
                "every warning",
            ),
            ReportSetting::HookDsc => (
                "  on DSC alerts",
                format!("[{}]", if c.hook_dsc { 'x' } else { ' ' }),
                "distress and urgency calls",
            ),
            ReportSetting::HookHfdl => (
                "  on HFDL alerts",
                format!("[{}]", if c.hook_hfdl { 'x' } else { ' ' }),
                "emergency and caution messages",
            ),
            ReportSetting::HookHealth => (
                "  on health",
                format!("[{}]", if c.hook_health { 'x' } else { ' ' }),
                "disk, CPU, power, clock, silence, noise floor",
            ),
            ReportSetting::HookTest => ("", "[Test the hook]".to_string(), "runs it once now"),
        };
        let sel = focused && state.report_ui.setting == i;
        let t = Some(HitTarget::ReportSetting(*row));
        let r: Vec<Tagged> = vec![
            (
                Span::styled(format!(" {label:<18}"), theme.field_style(sel, false)),
                t,
            ),
            (Span::styled(value, theme.field_style(sel, false)), t),
            (Span::styled(format!("   {hint}"), theme.dim_style()), None),
        ];
        let y = area.y + i as u16;
        hits.push(
            Rect::new(area.x, y, area.width, 1),
            HitTarget::ReportSetting(*row),
        );
        record_spans(hits, area.x, y, area.width, &r);
        frame
            .buffer_mut()
            .set_line(area.x, y, &untag(&r), area.width);
    }
    // Where the files are and whether they can be written.
    let mut info = Vec::new();
    let room = (area.width as usize).saturating_sub(12);
    if let (Some(f), Some(rep)) = (&files, &state.report) {
        info.push(format!(
            " Files     {}",
            tail(&f.report_path().display().to_string(), room)
        ));
        let logs = format!(
            "  ({} log files of profile {})",
            f.log_days().len(),
            f.profile
        );
        info.push(format!(
            "           {}{logs}",
            tail(
                &f.log_path("YYYY-MM-DD").display().to_string(),
                room.saturating_sub(logs.len())
            )
        ));
        info.push(match rep.error() {
            Some(e) => format!(" State     not writable, counting in memory: {e}"),
            None if rep.read_only() => {
                " State     read-only (made by a newer ShellSDR, or unreadable)".to_string()
            }
            None => " State     writable".to_string(),
        });
    }
    for (j, l) in info.into_iter().enumerate() {
        let y = area.y + ReportSetting::ALL.len() as u16 + 1 + j as u16;
        if y >= area.y + area.height {
            break;
        }
        frame
            .buffer_mut()
            .set_stringn(area.x, y, l, area.width as usize, theme.dim_style());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_paths_keep_their_tail() {
        assert_eq!(tail("/a/b/c", 10), "/a/b/c");
        assert_eq!(tail("/home/sp/.local/state/ShellSDR", 12), "…te/ShellSDR");
    }

    #[test]
    fn hours_become_bars_scaled_to_the_busiest() {
        let mut h = vec![0u64; 24];
        h[3] = 1;
        h[18] = 8;
        let s = spark(&h);
        assert_eq!(s.chars().count(), 24);
        assert_eq!(s.chars().nth(18), Some('█'));
        assert_eq!(s.chars().next(), Some('·'));
        assert_eq!(s.chars().nth(3), Some('▁'));
    }
}
