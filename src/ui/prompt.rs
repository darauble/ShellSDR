//! Modal single-line text prompt (pure model + render helper).
//!
//! Step 7. A titled one-field dialog with visible [OK] and [Cancel] buttons,
//! used to name a new bookmark list or rename one. While it is open every
//! printable key is text (no accelerators), ENTER confirms and Esc cancels;
//! the buttons and the field are clickable. A validation error from the
//! caller is shown under the field and keeps the prompt open.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::ui::text_input::TextInput;
use crate::ui::theme::Theme;

/// A text prompt dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPrompt {
    pub title: String,
    pub input: TextInput,
    /// Error from the last confirm attempt.
    pub error: Option<String>,
}

impl TextPrompt {
    pub fn new(title: impl Into<String>, initial: impl Into<String>) -> Self {
        let mut input = TextInput::new(initial);
        input.select_all();
        Self {
            title: title.into(),
            input,
            error: None,
        }
    }
}

/// On-screen parts of a drawn prompt (for mouse hit-testing).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptHits {
    pub frame: Rect,
    pub field: Rect,
    pub ok: Rect,
    pub cancel: Rect,
}

/// Width of the prompt box.
const PROMPT_WIDTH: u16 = 44;

/// Render `p` centered in `area`.
pub fn render_prompt(p: &TextPrompt, area: Rect, theme: &Theme, buf: &mut Buffer) -> PromptHits {
    let w = PROMPT_WIDTH.min(area.width);
    let h = 6u16.min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    let mut hits = PromptHits {
        frame: rect,
        ..PromptHits::default()
    };
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(true))
        .title(p.title.clone());
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.width < 4 || inner.height < 3 {
        return hits;
    }
    // Row 0: the input, cursor cell reversed.
    let (shown, cur) = p.input.window(inner.width as usize);
    let field_style = theme.field_style(true, true);
    let mut spans = Vec::new();
    for (i, ch) in shown
        .chars()
        .chain(std::iter::repeat(' '))
        .take(inner.width as usize)
        .enumerate()
    {
        let st = if i == cur || (p.input.is_all_selected() && i < shown.chars().count()) {
            field_style.add_modifier(Modifier::REVERSED)
        } else {
            field_style
        };
        spans.push(Span::styled(ch.to_string(), st));
    }
    buf.set_line(inner.x, inner.y, &Line::from(spans), inner.width);
    hits.field = Rect::new(inner.x, inner.y, inner.width, 1);
    // Row 1: error (accent) or hint (dim).
    let (msg, st) = match &p.error {
        Some(e) => (e.clone(), Style::default().fg(theme.accent)),
        None => ("ENTER = OK, Esc = Cancel".to_string(), theme.dim_style()),
    };
    buf.set_stringn(inner.x, inner.y + 1, msg, inner.width as usize, st);
    // Last row: buttons.
    let y = inner.y + inner.height - 1;
    let ok = "[OK]";
    let cancel = "[Cancel]";
    buf.set_string(inner.x, y, ok, theme.field_style(true, false));
    let cx = inner.x + ok.len() as u16 + 2;
    buf.set_string(cx, y, cancel, theme.field_style(false, false));
    hits.ok = Rect::new(inner.x, y, ok.len() as u16, 1);
    hits.cancel = Rect::new(cx, y, cancel.len() as u16, 1);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_title_text_buttons_and_error() {
        let theme = Theme::builtin_default();
        let mut p = TextPrompt::new("New list", "Laivyba");
        p.error = Some("A list named \"Laivyba\" exists".into());
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        let hits = render_prompt(&p, area, &theme, &mut buf);
        let text: String = (0..20)
            .flat_map(|y| (0..60).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("New list"));
        assert!(text.contains("Laivyba"));
        assert!(text.contains("exists"));
        assert_eq!(buf[(hits.ok.x, hits.ok.y)].symbol(), "[");
        assert_eq!(hits.cancel.width, 8);
        assert!(hits.frame.x <= hits.field.x && hits.field.y > hits.frame.y);
    }
}
