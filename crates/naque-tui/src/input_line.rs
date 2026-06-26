//! Single-line editable input buffer with a cursor and horizontal scrolling.
//!
//! The buffer tracks a cursor as a byte offset into the text, always kept on a
//! `char` boundary. Editing acts at the cursor; movement walks whole characters.
//! [`InputLine::view`] windows the text so the cursor stays visible on lines
//! wider than the available space.
//!
//! Column math treats every character as one column wide. That is correct for
//! ASCII (the overwhelming case for SQL and prompts) and only mildly off for
//! wide CJK glyphs; we avoid a `unicode-width` dependency for it.

/// An editable single-line text buffer tracking a cursor position.
#[derive(Debug, Default, Clone)]
pub struct InputLine {
    text: String,
    /// Byte offset of the cursor into `text`; always on a char boundary.
    cursor: usize,
}

/// A windowed view of an [`InputLine`] sized to fit a render area.
#[derive(Debug, PartialEq, Eq)]
pub struct InputView {
    /// The visible slice of the text, already trimmed to the area width.
    pub visible: String,
    /// Cursor column within the visible slice, in `0..width`.
    pub cursor_col: u16,
}

impl InputLine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Cursor position as a byte offset (always on a char boundary).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the whole buffer, placing the cursor at the end.
    pub fn set_text(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    /// Clear the buffer and return its previous contents; cursor resets to 0.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Insert a character at the cursor, advancing past it.
    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character before the cursor (Backspace). No-op at the start.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_boundary(self.cursor);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    /// Delete the character at the cursor (Delete). No-op at the end.
    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..next, "");
    }

    /// Move the cursor one character left.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_boundary(self.cursor);
        }
    }

    /// Move the cursor one character right.
    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.next_boundary(self.cursor);
        }
    }

    /// Move the cursor to the start of the line.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end of the line.
    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Number of characters before the cursor (its display column).
    pub fn cursor_char(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    /// Window the text to `width` columns, anchored so the cursor stays visible.
    ///
    /// One column is reserved at the right so the cursor can sit just past the
    /// last character (i.e. at end-of-input) without scrolling content away.
    pub fn view(&self, width: u16) -> InputView {
        let width = width as usize;
        if width == 0 {
            return InputView {
                visible: String::new(),
                cursor_col: 0,
            };
        }
        let chars: Vec<char> = self.text.chars().collect();
        let cursor_pos = self.cursor_char();
        // Anchor the window so the cursor is always inside `0..width`.
        let start = if cursor_pos < width { 0 } else { cursor_pos - width + 1 };
        let end = (start + width).min(chars.len());
        InputView {
            visible: chars[start..end].iter().collect(),
            cursor_col: (cursor_pos - start) as u16,
        }
    }

    fn prev_boundary(&self, idx: usize) -> usize {
        self.text[..idx].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self, idx: usize) -> usize {
        self.text[idx..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| idx + i)
            .unwrap_or(self.text.len())
    }
}

impl From<&str> for InputLine {
    fn from(s: &str) -> Self {
        let mut line = Self::new();
        line.set_text(s.to_string());
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_appends_and_advances_cursor() {
        let mut line = InputLine::new();
        for c in "abc".chars() {
            line.insert(c);
        }
        assert_eq!(line.text(), "abc");
        assert_eq!(line.cursor_char(), 3);
    }

    #[test]
    fn insert_at_cursor_in_middle() {
        let mut line = InputLine::from("ac");
        line.move_home();
        line.move_right(); // between a and c
        line.insert('b');
        assert_eq!(line.text(), "abc");
        assert_eq!(line.cursor_char(), 2);
    }

    #[test]
    fn backspace_deletes_before_cursor() {
        let mut line = InputLine::from("abc");
        line.move_left(); // cursor between b and c
        line.backspace(); // remove b
        assert_eq!(line.text(), "ac");
        assert_eq!(line.cursor_char(), 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut line = InputLine::from("abc");
        line.move_home();
        line.backspace();
        assert_eq!(line.text(), "abc");
        assert_eq!(line.cursor_char(), 0);
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut line = InputLine::from("abc");
        line.move_home();
        line.delete(); // remove a
        assert_eq!(line.text(), "bc");
        assert_eq!(line.cursor_char(), 0);
    }

    #[test]
    fn delete_at_end_is_noop() {
        let mut line = InputLine::from("abc");
        line.delete();
        assert_eq!(line.text(), "abc");
    }

    #[test]
    fn home_and_end_jump() {
        let mut line = InputLine::from("hello");
        line.move_home();
        assert_eq!(line.cursor_char(), 0);
        line.move_end();
        assert_eq!(line.cursor_char(), 5);
    }

    #[test]
    fn movement_stops_at_bounds() {
        let mut line = InputLine::from("ab");
        line.move_home();
        line.move_left(); // already at start
        assert_eq!(line.cursor_char(), 0);
        line.move_end();
        line.move_right(); // already at end
        assert_eq!(line.cursor_char(), 2);
    }

    #[test]
    fn take_clears_and_returns() {
        let mut line = InputLine::from("query");
        let taken = line.take();
        assert_eq!(taken, "query");
        assert!(line.is_empty());
        assert_eq!(line.cursor_char(), 0);
    }

    #[test]
    fn multibyte_editing_respects_char_boundaries() {
        let mut line = InputLine::new();
        for c in "héllo".chars() {
            line.insert(c);
        }
        line.move_left(); // before final 'o'
        line.move_left(); // before 'l'
        line.move_left(); // before second 'l'
        line.move_left(); // before 'é'
        line.backspace(); // remove 'h'
        assert_eq!(line.text(), "éllo");
        assert_eq!(line.cursor_char(), 0);
    }

    #[test]
    fn view_fits_within_width() {
        let line = InputLine::from("hello");
        let view = line.view(20);
        assert_eq!(view.visible, "hello");
        assert_eq!(view.cursor_col, 5);
    }

    #[test]
    fn view_scrolls_to_keep_cursor_visible() {
        // 10 chars, width 5: cursor at end must remain visible.
        let line = InputLine::from("0123456789");
        let view = line.view(5);
        // Window reserves one column for the end cursor: last 4 chars shown.
        assert_eq!(view.visible, "6789");
        assert_eq!(view.cursor_col, 4);
    }

    #[test]
    fn view_at_start_shows_head() {
        let mut line = InputLine::from("0123456789");
        line.move_home();
        let view = line.view(5);
        assert_eq!(view.visible, "01234");
        assert_eq!(view.cursor_col, 0);
    }

    #[test]
    fn view_zero_width_is_empty() {
        let line = InputLine::from("abc");
        let view = line.view(0);
        assert_eq!(view.visible, "");
        assert_eq!(view.cursor_col, 0);
    }
}
