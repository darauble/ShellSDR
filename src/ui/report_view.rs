//! The Report module in the left panel (spec 20.1): `[Report]`, the
//! uptime, the station the active VFO is on and for how long, what was
//! decoded today, and the newest warning or error.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders};

use crate::app::AppState;
use crate::report::model::{Level, Period, keys};
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::render::{Tagged, record_spans, untag};
use crate::ui::theme::Theme;

/// How a panel row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Button,
    Text,
    Dim,
    Problem,
}

/// `3d 04:12` from a day on, else `4:12:33`.
pub fn dur_hm(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86_400, s / 3600 % 24, s / 60 % 60);
    if d > 0 {
        format!("{d}d {h:02}:{m:02}")
    } else {
        dur_hms(s)
    }
}

/// `1:12:33`.
pub fn dur_hms(secs: i64) -> String {
    let s = secs.max(0);
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// `512 B`, `14 kB`, `41 MB`, `1.2 GB`.
pub fn bytes(n: u64) -> String {
    let f = n as f64;
    if n < 1000 {
        format!("{n} B")
    } else if n < 1_000_000 {
        format!("{:.0} kB", f / 1e3)
    } else if n < 1_000_000_000 {
        format!("{:.0} MB", f / 1e6)
    } else {
        format!("{:.1} GB", f / 1e9)
    }
}

/// The work lines of a day for the panel (only what did work).
pub fn work_lines(p: &Period) -> Vec<String> {
    let mut v = Vec::new();
    let dsc = p.get(keys::DSC_MESSAGES);
    if dsc > 0 {
        let distress = p.get("dsc.distress");
        let extra = if distress > 0 {
            format!(" ({distress} distress)")
        } else {
            String::new()
        };
        v.push(format!(" DSC     {dsc:>4} msgs{extra}"));
    }
    let hfdl = p.get(keys::HFDL_MESSAGES);
    let planes = p.max_of(keys::HFDL_AIRCRAFT).unwrap_or(0);
    let gs = p.max_of(keys::HFDL_STATIONS).unwrap_or(0);
    if hfdl > 0 || planes > 0 {
        let em = p.get("hfdl.emergency");
        let extra = if em > 0 {
            format!(" ({em} emergency)")
        } else {
            String::new()
        };
        v.push(format!(" HFDL    {hfdl:>4} msgs{extra}"));
        v.push(format!("         {planes:>4} planes, {gs} stations"));
    }
    let fax = p.get(keys::FAX_PICTURES);
    if fax > 0 {
        v.push(format!(" FAX     {fax:>4} pictures"));
    }
    let (db, dp) = (p.get(keys::DIGITAL_BYTES), p.get(keys::DIGITAL_PICTURES));
    if db > 0 || dp > 0 {
        v.push(format!(" Digital {}, {dp} pictures", bytes(db)));
    }
    let rf = p.get(keys::REC_FILES);
    if rf > 0 {
        v.push(format!(
            " Rec     {rf:>4} files, {}",
            bytes(p.get(keys::REC_BYTES))
        ));
    }
    v
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The module's rows (their count is the module's height).
pub fn panel_rows(state: &AppState) -> Vec<(String, RowKind)> {
    let mut rows = vec![("[Report]".to_string(), RowKind::Button)];
    let Some(r) = &state.report else {
        rows.push(("Report not open".to_string(), RowKind::Dim));
        return rows;
    };
    let now = now_unix();
    rows.push((
        format!(
            "Up {}  Source {}",
            dur_hm(now - r.started),
            dur_hm(state.report_seen.source_secs)
        ),
        RowKind::Text,
    ));
    let v = state.radio.active();
    rows.push((
        format!(
            "{}: {} {} {}",
            v.name,
            v.vfo_type,
            crate::ui::bookmark_view::khz_label(v.frequency_hz),
            dur_hms(state.report_station_secs())
        ),
        RowKind::Text,
    ));
    rows.push(("Today".to_string(), RowKind::Dim));
    let work = r
        .doc()
        .day(r.today_key())
        .map(work_lines)
        .unwrap_or_default();
    if work.is_empty() {
        rows.push((" nothing decoded yet".to_string(), RowKind::Dim));
    }
    rows.extend(work.into_iter().map(|l| (l, RowKind::Text)));
    if let Some((free, days)) = state.report_seen.disk {
        let days = days.map_or(String::new(), |d| format!(" (~{d:.0} days)"));
        rows.push((format!("Disk {} free{days}", bytes(free)), RowKind::Text));
    }
    if let Some(e) = r.last_problem() {
        let clock = r.clock(state.utc_offset_min);
        let t = clock.time_of_day(e.time);
        let mark = if e.level == Level::Error { "!!" } else { "!" };
        let kind = if now - e.time < 3600 {
            RowKind::Problem
        } else {
            RowKind::Dim
        };
        rows.push((format!("{mark} {} {}", &t[..5], e.text), kind));
    }
    rows
}

/// The folded module's summary: the uptime.
pub fn folded_summary(state: &AppState) -> String {
    match &state.report {
        Some(r) => format!("Up {}", dur_hm(now_unix() - r.started)),
        None => "off".to_string(),
    }
}

/// Render the Report module body inside the module block at `area`.
pub fn render_report_contents(
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
    for (y, (text, kind)) in panel_rows(state).into_iter().enumerate() {
        if y as u16 >= inner.height {
            break;
        }
        let (style, target) = match kind {
            RowKind::Button => (
                theme.field_style(focused, false),
                Some(HitTarget::ReportButton),
            ),
            RowKind::Text => (theme.text_style(), None),
            RowKind::Dim => (theme.dim_style(), None),
            RowKind::Problem => (Style::default().fg(theme.accent), None),
        };
        let row: Vec<Tagged> = vec![(Span::styled(text, style), target)];
        let yy = inner.y + y as u16;
        record_spans(hits, inner.x, yy, inner.width, &row);
        buf.set_line(inner.x, yy, &untag(&row), inner.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_sizes_read_short() {
        assert_eq!(dur_hm(3 * 86_400 + 4 * 3600 + 12 * 60 + 5), "3d 04:12");
        assert_eq!(dur_hm(4 * 3600 + 12 * 60), "4:12:00");
        assert_eq!(dur_hms(4353), "1:12:33");
        assert_eq!(bytes(14_200), "14 kB");
        assert_eq!(bytes(1_234_000_000), "1.2 GB");
    }

    #[test]
    fn only_work_done_is_listed() {
        let mut p = Period::new("2026-11-24");
        assert!(work_lines(&p).is_empty());
        p.counters.insert(keys::DSC_MESSAGES.into(), 412);
        p.counters.insert("dsc.distress".into(), 2);
        p.counters.insert(keys::REC_FILES.into(), 4);
        p.counters.insert(keys::REC_BYTES.into(), 1_200_000_000);
        assert_eq!(
            work_lines(&p),
            [
                " DSC      412 msgs (2 distress)",
                " Rec        4 files, 1.2 GB"
            ]
        );
    }
}
