//! Reusable list-selection popup (pure model + render helper).
//!
//! A `ListPopup` is a modal dropdown: a titled list of string options with a
//! highlighted cursor. It is the shared primitive behind every "pick one of
//! these" control (VFO mode, source device, sample rate, bandwidth). The model
//! is pure and keyboard-driven (Up/Down move, confirm returns the chosen index,
//! cancel discards); the render helper draws a centered bordered overlay. UI
//! wiring decides what to do with the confirmed index.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::ui::theme::Theme;

/// A modal single-choice list. Holds its own cursor; the opener seeds it with
/// the currently-selected index so the popup opens on the active value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPopup {
    /// Short title shown in the popup border (e.g. "Mode").
    pub title: String,
    /// The selectable option labels, in order.
    pub items: Vec<String>,
    /// The highlighted index (always valid when `items` is non-empty).
    pub cursor: usize,
}

impl ListPopup {
    /// Create a popup over `items`, opening on `selected` (clamped). An empty
    /// item list yields a popup with cursor 0 that confirms to `None`.
    pub fn new(title: impl Into<String>, items: Vec<String>, selected: usize) -> Self {
        let cursor = if items.is_empty() {
            0
        } else {
            selected.min(items.len() - 1)
        };
        Self {
            title: title.into(),
            items,
            cursor,
        }
    }

    /// Move the cursor down one (wraps to the top).
    pub fn down(&mut self) {
        if !self.items.is_empty() {
            self.cursor = (self.cursor + 1) % self.items.len();
        }
    }

    /// Move the cursor up one (wraps to the bottom).
    pub fn up(&mut self) {
        if !self.items.is_empty() {
            self.cursor = (self.cursor + self.items.len() - 1) % self.items.len();
        }
    }

    /// Confirm the current selection: the chosen index, or `None` if empty.
    pub fn confirm(&self) -> Option<usize> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.cursor)
        }
    }
}

/// Compute a centered rectangle for a popup sized to its content, clamped to
/// `area`. Width fits the longest item (and title) plus borders; height fits
/// all items plus borders, capped to the available area.
pub fn centered_rect(popup: &ListPopup, area: Rect) -> Rect {
    let longest = popup
        .items
        .iter()
        .map(|s| s.chars().count())
        .chain(std::iter::once(popup.title.chars().count()))
        .max()
        .unwrap_or(0) as u16;
    // +2 for borders, +2 side padding.
    let want_w = longest.saturating_add(4).max(10);
    let want_h = (popup.items.len() as u16).saturating_add(2).max(3);
    let w = want_w.min(area.width);
    let h = want_h.min(area.height.max(3));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Where a rendered popup's frame and visible items are on screen (item
/// rects carry the index into `items`, accounting for windowing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PopupHits {
    pub frame: Rect,
    pub items: Vec<(Rect, usize)>,
}

/// Render the popup as a centered overlay in `area`: clears the region behind
/// it, draws a bordered titled block, and lists the items with the cursor row
/// highlighted (editing style: fg/bg swap). Returns the on-screen rects of
/// the frame and of each visible item, for mouse hit-testing.
pub fn render_popup(popup: &ListPopup, area: Rect, theme: &Theme, buf: &mut Buffer) -> PopupHits {
    let rect = centered_rect(popup, area);
    let mut hits = PopupHits {
        frame: rect,
        items: Vec::new(),
    };
    // Blank the area behind the popup so underlying content does not show
    // through, then draw the frame.
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(true))
        .title(popup.title.clone());
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.width == 0 || inner.height == 0 {
        return hits;
    }

    // Window the items so the cursor stays visible when the list is taller
    // than the inner area.
    let visible = inner.height as usize;
    let start = if popup.cursor >= visible {
        popup.cursor + 1 - visible
    } else {
        0
    };
    for (row, (i, label)) in popup
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let selected = i == popup.cursor;
        let style = theme.field_style(selected, selected);
        let y = inner.y + row as u16;
        let line = Line::from(Span::styled(label.clone(), style));
        buf.set_line(inner.x, y, &line, inner.width);
        hits.items.push((Rect::new(inner.x, y, inner.width, 1), i));
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn items() -> Vec<String> {
        vec!["NFM".into(), "WFM".into(), "AM".into(), "USB".into()]
    }

    #[test]
    fn opens_on_selected_index_clamped() {
        let p = ListPopup::new("Mode", items(), 2);
        assert_eq!(p.cursor, 2);
        // Out-of-range selection clamps to the last item.
        let p = ListPopup::new("Mode", items(), 99);
        assert_eq!(p.cursor, 3);
    }

    #[test]
    fn up_down_wrap_around() {
        let mut p = ListPopup::new("Mode", items(), 0);
        p.up();
        assert_eq!(p.cursor, 3, "up from top wraps to bottom");
        p.down();
        assert_eq!(p.cursor, 0, "down from bottom wraps to top");
        p.down();
        assert_eq!(p.cursor, 1);
    }

    #[test]
    fn confirm_returns_cursor_index() {
        let mut p = ListPopup::new("Mode", items(), 0);
        p.down();
        p.down();
        assert_eq!(p.confirm(), Some(2));
    }

    #[test]
    fn empty_popup_confirms_to_none() {
        let p = ListPopup::new("Empty", vec![], 0);
        assert_eq!(p.confirm(), None);
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn item_hits_carry_the_real_index_when_the_list_is_windowed() {
        // 10 items in a 6-row area: only 4 fit inside the border. With the
        // cursor on the last item the window starts at 6, and the hit rects
        // must report indices 6..=9, not 0..=3.
        let items: Vec<String> = (0..10).map(|i| format!("item{i}")).collect();
        let p = ListPopup::new("L", items, 9);
        let theme = Theme::builtin_default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 6));
        let hits = render_popup(&p, Rect::new(0, 0, 30, 6), &theme, &mut buf);
        let idx: Vec<usize> = hits.items.iter().map(|(_, i)| *i).collect();
        assert_eq!(idx, vec![6, 7, 8, 9]);
        // Each item rect is one row inside the frame.
        for (r, _) in &hits.items {
            assert_eq!(r.height, 1);
            assert!(r.y > hits.frame.y && r.y < hits.frame.y + hits.frame.height - 1);
        }
    }

    #[test]
    fn renders_items_and_highlights_cursor() {
        let p = ListPopup::new("Mode", items(), 1);
        let theme = Theme::builtin_default();
        let backend = TestBackend::new(40, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_popup(&p, area, &theme, f.buffer_mut());
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        // All items are drawn somewhere on screen.
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("NFM"));
        assert!(text.contains("WFM"));
        assert!(text.contains("USB"));
        assert!(text.contains("Mode"), "title drawn in the border");

        // The cursor row (WFM) uses the editing fg/bg (highlighted).
        let mut highlighted = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].symbol() == "W"
                    && buffer[(x, y)].style().bg == Some(theme.editing_bg)
                {
                    highlighted = true;
                }
            }
        }
        assert!(highlighted, "selected item is highlighted with editing bg");
    }
}
