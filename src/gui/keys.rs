//! egui input as the terminal events the sharing ShellSDR applies (0.4.1,
//! spec 22.6): keys, text and clicks in cell coordinates.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use eframe::egui;

/// The terminal modifiers of egui's.
pub fn modifiers(m: egui::Modifiers) -> KeyModifiers {
    let mut k = KeyModifiers::NONE;
    if m.shift {
        k |= KeyModifiers::SHIFT;
    }
    if m.ctrl {
        k |= KeyModifiers::CONTROL;
    }
    if m.alt {
        k |= KeyModifiers::ALT;
    }
    k
}

/// A pressed key as a terminal key (None: it comes as text, or the
/// terminal has no such key).
pub fn key(k: egui::Key, m: egui::Modifiers) -> Option<Event> {
    use egui::Key as K;
    let mods = modifiers(m);
    let code = match k {
        K::Enter => KeyCode::Enter,
        K::Escape => KeyCode::Esc,
        K::Tab if m.shift => KeyCode::BackTab,
        K::Tab => KeyCode::Tab,
        K::Backspace => KeyCode::Backspace,
        K::Delete => KeyCode::Delete,
        K::Insert => KeyCode::Insert,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::ArrowUp => KeyCode::Up,
        K::ArrowDown => KeyCode::Down,
        K::ArrowLeft => KeyCode::Left,
        K::ArrowRight => KeyCode::Right,
        K::F1 => KeyCode::F(1),
        K::F2 => KeyCode::F(2),
        K::F3 => KeyCode::F(3),
        K::F4 => KeyCode::F(4),
        K::F5 => KeyCode::F(5),
        K::F6 => KeyCode::F(6),
        K::F7 => KeyCode::F(7),
        K::F8 => KeyCode::F(8),
        K::F9 => KeyCode::F(9),
        K::F10 => KeyCode::F(10),
        K::F11 => KeyCode::F(11),
        K::F12 => KeyCode::F(12),
        // with Ctrl or Alt a letter, digit or sign comes as a key, not as
        // text (Ctrl+] is CloseBracket by name, "]" by symbol)
        other if m.ctrl || m.alt => {
            let name = other.symbol_or_name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_graphic() => KeyCode::Char(c.to_ascii_lowercase()),
                _ => return None,
            }
        }
        _ => return None,
    };
    // BackTab carries no Shift in a terminal's events
    let mods = if code == KeyCode::BackTab {
        mods - KeyModifiers::SHIFT
    } else {
        mods
    };
    Some(Event::Key(KeyEvent::new(code, mods)))
}

/// Typed text as terminal keys (one per character).
pub fn text(s: &str) -> Vec<Event> {
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
        .collect()
}

/// A click (`down`) or a release at the cell (col, row).
pub fn click(
    button: egui::PointerButton,
    down: bool,
    col: u16,
    row: u16,
    m: egui::Modifiers,
) -> Option<Event> {
    let b = match button {
        egui::PointerButton::Primary => MouseButton::Left,
        egui::PointerButton::Secondary => MouseButton::Right,
        egui::PointerButton::Middle => MouseButton::Middle,
        _ => return None,
    };
    Some(Event::Mouse(MouseEvent {
        kind: if down {
            MouseEventKind::Down(b)
        } else {
            MouseEventKind::Up(b)
        },
        column: col,
        row,
        modifiers: modifiers(m),
    }))
}

/// A wheel step at the cell (col, row): up for a positive `dy`.
pub fn wheel(dy: f32, col: u16, row: u16, m: egui::Modifiers) -> Event {
    Event::Mouse(MouseEvent {
        kind: if dy > 0.0 {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        },
        column: col,
        row,
        modifiers: modifiers(m),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_become_terminal_keys() {
        let none = egui::Modifiers::NONE;
        assert_eq!(
            key(egui::Key::Enter, none),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )))
        );
        assert_eq!(
            key(egui::Key::Tab, egui::Modifiers::SHIFT),
            Some(Event::Key(KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::NONE
            )))
        );
        assert_eq!(key(egui::Key::A, none), None, "comes as text");
        assert_eq!(
            key(egui::Key::S, egui::Modifiers::CTRL),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL
            )))
        );
        assert_eq!(
            key(egui::Key::CloseBracket, egui::Modifiers::CTRL),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char(']'),
                KeyModifiers::CONTROL
            ))),
            "Ctrl+] leaves a window"
        );
        assert_eq!(
            key(egui::Key::M, egui::Modifiers::ALT),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('m'),
                KeyModifiers::ALT
            )))
        );
        assert_eq!(text("a ").len(), 2);
        assert!(matches!(
            click(egui::PointerButton::Primary, true, 3, 4, none),
            Some(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 4,
                ..
            }))
        ));
    }
}
