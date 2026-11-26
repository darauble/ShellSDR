//! Work Area pages of the side modules (spec 19).
//!
//! The active VFO owns the Work Area by default; a side module takes it
//! when the user opens its page (`[Manager]`, `o`), and `Esc` in the page
//! gives it back. The page is drawn where the VFO view would be (the second
//! tab, or the VFO's half in Split).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::AppState;
use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::nav::Focus;
use crate::ui::theme::Theme;

/// Who the Work Area's page slot belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkOwner {
    /// The active VFO's type view.
    #[default]
    Vfo,
    Bookmarks,
    Schedule,
    Report,
}

impl WorkOwner {
    /// The module page owning the Work Area for a focused module, if it
    /// has one (Source and Radio have none).
    pub fn of_focus(f: Focus) -> Option<WorkOwner> {
        match f {
            Focus::Bookmarks => Some(WorkOwner::Bookmarks),
            Focus::Schedule => Some(WorkOwner::Schedule),
            Focus::Report => Some(WorkOwner::Report),
            _ => None,
        }
    }

    /// The module a page belongs to (where `Esc` returns the focus).
    pub fn module(self) -> Option<Focus> {
        match self {
            WorkOwner::Vfo => None,
            WorkOwner::Bookmarks => Some(Focus::Bookmarks),
            WorkOwner::Schedule => Some(Focus::Schedule),
            WorkOwner::Report => Some(Focus::Report),
        }
    }

    /// The tab label and the page frame's title.
    pub fn label(self) -> &'static str {
        match self {
            WorkOwner::Vfo => "VFO",
            WorkOwner::Bookmarks => "Bookmarks",
            WorkOwner::Schedule => "Schedule",
            WorkOwner::Report => "Report",
        }
    }
}

/// Draw the owner's page in `area` (the VFO view's slot).
pub fn render_page(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let owner = state.work_owner;
    let focused = state.nav.focus == Focus::WorkArea;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .title(match owner {
            WorkOwner::Report => "Report".to_string(),
            o => format!("{}: manager", o.label()),
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    match owner {
        WorkOwner::Bookmarks => {
            return crate::ui::bookmark_page::render_bookmark_page(
                frame, inner, state, theme, hits,
            );
        }
        WorkOwner::Schedule => {
            return crate::ui::schedule_page::render_schedule_page(
                frame, inner, state, theme, hits,
            );
        }
        WorkOwner::Report => {
            return crate::ui::report_page::render_report_page(frame, inner, state, theme, hits);
        }
        WorkOwner::Vfo => {}
    }
    let back = state.radio.active().name.clone();
    let lines = vec![
        Line::from(Span::styled(
            format!("The {} manager comes in the next step.", owner.label()),
            theme.dim_style(),
        )),
        Line::from(Span::styled(
            format!("Esc: back to {back}"),
            theme.dim_style(),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A page's function-key bar (Midnight Commander style, as the VFO views):
/// equal cells, the number bold, the label on the bar colour; each cell is
/// a click target.
pub fn render_fkey_bar(
    frame: &mut Frame,
    r: Rect,
    keys: &[(u8, &str)],
    theme: &Theme,
    hits: &mut HitMap,
) {
    if r.is_empty() || keys.is_empty() {
        return;
    }
    let n = keys.len() as u16;
    let cellw = (r.width / n).max(4);
    let label_style = Style::default().fg(theme.editing_fg).bg(theme.editing_bg);
    let mut spans = Vec::new();
    for (i, (k, label)) in keys.iter().enumerate() {
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
        let text: String = label.chars().take(room).collect();
        spans.push(Span::styled(
            num,
            theme.text_style().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!("{text:<room$}"), label_style));
        hits.push(Rect::new(x, r.y, w, 1), HitTarget::PageFKey(*k));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bookmarks_and_schedule_have_pages_and_return_to_their_module() {
        assert_eq!(WorkOwner::of_focus(Focus::Source), None);
        assert_eq!(WorkOwner::of_focus(Focus::Radio), None);
        assert_eq!(WorkOwner::of_focus(Focus::Global), None);
        for f in [Focus::Bookmarks, Focus::Schedule, Focus::Report] {
            let o = WorkOwner::of_focus(f).unwrap();
            assert_eq!(o.module(), Some(f));
        }
        assert_eq!(WorkOwner::Vfo.module(), None);
        assert_eq!(WorkOwner::default(), WorkOwner::Vfo);
    }
}
