//! Single-line text input (pure).
//!
//! Step 7. The shared primitive behind every free-text control (bookmark
//! name/notes/geo info, list names, the bookmark filter, numeric fields of the
//! bookmark editor). Holds the text and a cursor counted in characters (not
//! bytes) so non-ASCII names ("Español", "Rusiškos") edit correctly. An
//! optional character limit and character filter (e.g. digits only) keep
//! constrained fields valid while typing.

/// Which characters an input accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accept {
    /// Any printable character.
    #[default]
    Any,
    /// ASCII digits only (HHMM times, integer Hz values).
    Digits,
}

/// An editable single-line text value with a character cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextInput {
    text: String,
    /// Cursor position in characters, `0..=len`.
    cursor: usize,
    /// Maximum number of characters (0 = unlimited).
    max_len: usize,
    accept: Accept,
    /// The whole text is selected (form "select all on focus"): the next
    /// typed character replaces it, Backspace/Delete clear it, and cursor
    /// movement drops the selection.
    all_selected: bool,
}

impl TextInput {
    /// An unconstrained input holding `text`, cursor at the end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self {
            text,
            cursor,
            max_len: 0,
            accept: Accept::Any,
            all_selected: false,
        }
    }

    /// A digits-only input of at most `max_len` characters.
    pub fn digits(text: impl Into<String>, max_len: usize) -> Self {
        Self {
            max_len,
            accept: Accept::Digits,
            ..Self::new(text)
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cursor position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replace the whole text (cursor to the end).
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.len();
        self.all_selected = false;
    }

    /// Select the whole text (cursor to the end), as when a form field
    /// gains focus for editing.
    pub fn select_all(&mut self) {
        self.cursor = self.len();
        self.all_selected = !self.text.is_empty();
    }

    pub fn is_all_selected(&self) -> bool {
        self.all_selected
    }

    /// Drop a select-all, deleting the text. Returns whether it was selected.
    fn take_selection(&mut self) -> bool {
        if std::mem::take(&mut self.all_selected) {
            self.text.clear();
            self.cursor = 0;
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// Byte offset of character index `ci`.
    fn byte_at(&self, ci: usize) -> usize {
        self.text
            .char_indices()
            .nth(ci)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    /// Insert `c` at the cursor if it is accepted and the limit allows.
    /// Returns whether the text changed.
    pub fn insert(&mut self, c: char) -> bool {
        if c.is_control() {
            return false;
        }
        if self.accept == Accept::Digits && !c.is_ascii_digit() {
            return false;
        }
        self.take_selection();
        if self.max_len > 0 && self.len() >= self.max_len {
            return false;
        }
        let b = self.byte_at(self.cursor);
        self.text.insert(b, c);
        self.cursor += 1;
        true
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.take_selection() {
            return true;
        }
        if self.cursor == 0 {
            return false;
        }
        let b = self.byte_at(self.cursor - 1);
        self.text.remove(b);
        self.cursor -= 1;
        true
    }

    /// Delete the character under the cursor.
    pub fn delete(&mut self) -> bool {
        if self.take_selection() {
            return true;
        }
        if self.cursor >= self.len() {
            return false;
        }
        let b = self.byte_at(self.cursor);
        self.text.remove(b);
        true
    }

    pub fn left(&mut self) {
        self.all_selected = false;
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.all_selected = false;
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.all_selected = false;
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.all_selected = false;
        self.cursor = self.len();
    }

    /// The text windowed to `width` columns so the cursor stays visible,
    /// plus the cursor's column within that window. One extra column is kept
    /// for the cursor when it sits after the last character.
    pub fn window(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        let start = (self.cursor + 1).saturating_sub(width);
        let shown: String = self.text.chars().skip(start).take(width).collect();
        (shown, self.cursor - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut t = TextInput::new("");
        for c in "Volmet".chars() {
            t.insert(c);
        }
        assert_eq!(t.text(), "Volmet");
        t.home();
        t.insert('>');
        assert_eq!(t.text(), ">Volmet");
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn backspace_and_delete_remove_around_the_cursor() {
        let mut t = TextInput::new("abc");
        t.left(); // between b and c
        assert!(t.backspace());
        assert_eq!(t.text(), "ac");
        assert!(t.delete());
        assert_eq!(t.text(), "a");
        assert!(!t.delete(), "nothing under the cursor at the end");
        t.home();
        assert!(!t.backspace(), "nothing before the cursor at the start");
    }

    #[test]
    fn non_ascii_text_edits_by_character() {
        let mut t = TextInput::new("Español");
        t.left();
        t.left();
        t.left(); // before 'ñ'? cursor 4 -> "Espa|ñol"
        assert_eq!(t.cursor(), 4);
        t.delete();
        assert_eq!(t.text(), "Espaol");
        t.insert('n');
        assert_eq!(t.text(), "Espanol");
    }

    #[test]
    fn digits_input_rejects_other_chars_and_respects_the_limit() {
        let mut t = TextInput::digits("", 4);
        for c in "12a:345".chars() {
            t.insert(c);
        }
        assert_eq!(t.text(), "1234", "letters/colon rejected, capped at 4");
    }

    #[test]
    fn control_chars_are_ignored() {
        let mut t = TextInput::new("x");
        assert!(!t.insert('\n'));
        assert_eq!(t.text(), "x");
    }

    #[test]
    fn select_all_is_replaced_by_typing_or_cleared_by_backspace() {
        let mut t = TextInput::digits("0000", 4);
        t.select_all();
        assert!(t.is_all_selected());
        for c in "0530".chars() {
            t.insert(c);
        }
        assert_eq!(t.text(), "0530", "a full fixed-width field is replaced");
        let mut t = TextInput::new("New List");
        t.select_all();
        t.backspace();
        assert_eq!(t.text(), "");
        let mut t = TextInput::new("New List");
        t.select_all();
        t.end();
        t.insert('!');
        assert_eq!(t.text(), "New List!", "moving the cursor keeps the text");
        let mut t = TextInput::new("");
        t.select_all();
        assert!(!t.is_all_selected(), "nothing to select");
    }

    #[test]
    fn window_keeps_the_cursor_visible() {
        let t = TextInput::new("abcdefghij"); // cursor at 10 (end)
        let (shown, col) = t.window(5);
        assert_eq!(shown, "ghij", "last 4 chars + room for the end cursor");
        assert_eq!(col, 4);
        let mut t = t;
        t.home();
        let (shown, col) = t.window(5);
        assert_eq!(shown, "abcde");
        assert_eq!(col, 0);
    }
}
