//! Editable input buffer with a cursor, supporting explicit newlines.
//!
//! The buffer tracks a cursor as a byte offset into the text, always kept on a
//! `char` boundary. Editing acts at the cursor; movement walks whole characters.
//! [`InputLine::wrap`] lays the buffer out into visual rows at a given width
//! (hard-wrapping long lines and splitting on `\n`) and locates the cursor
//! within them, so callers can render multi-line input that never scrolls off
//! screen.
//!
//! Column math treats every character as one column wide. That is correct for
//! ASCII (the overwhelming case for SQL and prompts) and only mildly off for
//! wide CJK glyphs; we avoid a `unicode-width` dependency for it.

/// An editable text buffer (single- or multi-line) tracking a cursor position.
#[derive(Debug, Default, Clone)]
pub struct InputLine {
    text: String,
    /// Byte offset of the cursor into `text`; always on a char boundary.
    cursor: usize,
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

    /// Move the cursor to the start of the current logical line (just after the
    /// preceding `\n`, or the buffer start).
    pub fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
    }

    /// Move the cursor to the end of the current logical line (just before the
    /// next `\n`, or the buffer end).
    pub fn move_end(&mut self) {
        self.cursor = match self.text[self.cursor..].find('\n') {
            Some(i) => self.cursor + i,
            None => self.text.len(),
        };
    }

    /// Number of characters before the cursor (its display column).
    pub fn cursor_char(&self) -> usize {
        self.text[..self.cursor].chars().count()
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

/// A wrapped, multi-row layout of an [`InputLine`] sized to a render width.
///
/// `rows` are the visual lines after hard-wrapping the buffer at `width`
/// columns and splitting on explicit `\n`. The cursor is located at
/// `(cursor_row, cursor_col)` within those rows.
#[derive(Debug, PartialEq, Eq)]
pub struct Wrapped {
    /// Visual rows; each is at most `width` characters wide.
    pub rows: Vec<String>,
    /// Index of the row the cursor sits on.
    pub cursor_row: usize,
    /// Cursor column within `rows[cursor_row]`, in `0..=width`.
    pub cursor_col: u16,
}

impl InputLine {
    /// Hard-wrap the buffer into visual rows at `width` columns and split on
    /// explicit `\n`. Returns the rows alongside the absolute character index at
    /// which each row's content begins (newlines count as one character). The
    /// cursor is *not* located here — see [`InputLine::wrap`].
    fn layout(&self, width: usize) -> (Vec<String>, Vec<usize>) {
        let width = width.max(1);
        let mut rows: Vec<String> = Vec::new();
        let mut starts: Vec<usize> = Vec::new();
        let logical: Vec<&str> = self.text.split('\n').collect();
        let mut idx = 0; // absolute char index at the start of the current logical line
        for (li, line) in logical.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                rows.push(String::new());
                starts.push(idx);
            } else {
                let mut start = 0;
                while start < chars.len() {
                    let end = (start + width).min(chars.len());
                    rows.push(chars[start..end].iter().collect());
                    starts.push(idx + start);
                    start = end;
                }
            }
            idx += chars.len();
            if li + 1 < logical.len() {
                idx += 1; // the '\n' separating logical lines
            }
        }
        (rows, starts)
    }

    /// Lay the buffer out into visual rows wrapped at `width` columns, locating
    /// the cursor at `(cursor_row, cursor_col)`.
    pub fn wrap(&self, width: u16) -> Wrapped {
        let width = (width as usize).max(1);
        let (mut rows, starts) = self.layout(width);
        let cur = self.cursor_char();

        let mut cursor_row = 0;
        let mut cursor_col = 0u16;
        for i in 0..rows.len() {
            let s = starts[i];
            let len = rows[i].chars().count();
            if cur < s + len {
                cursor_row = i;
                cursor_col = (cur - s) as u16;
                break;
            }
            if cur == s + len {
                let next_continues = i + 1 < rows.len() && starts[i + 1] == s + len;
                if next_continues {
                    // Soft-wrap boundary: the cursor belongs to col 0 of the
                    // continuation row, which the next iteration matches.
                    continue;
                }
                if len < width {
                    cursor_row = i;
                    cursor_col = len as u16;
                } else if i + 1 == rows.len() {
                    // End of buffer on a full-width row: float onto a fresh row.
                    rows.push(String::new());
                    cursor_row = i + 1;
                    cursor_col = 0;
                } else {
                    // Full-width row before a hard break (rare): pin to its end;
                    // the renderer clamps the column to stay on screen.
                    cursor_row = i;
                    cursor_col = len as u16;
                }
                break;
            }
        }

        Wrapped {
            rows,
            cursor_row,
            cursor_col,
        }
    }

    /// Move the cursor up one visual row, preserving column where possible.
    /// Returns `false` when already on the first visual row (so the caller can
    /// fall back to history recall).
    pub fn move_up(&mut self, width: u16) -> bool {
        let w = self.wrap(width);
        if w.cursor_row == 0 {
            return false;
        }
        self.move_to_row(width, w.cursor_row - 1, w.cursor_col);
        true
    }

    /// Move the cursor down one visual row, preserving column where possible.
    /// Returns `false` when already on the last visual row.
    pub fn move_down(&mut self, width: u16) -> bool {
        let w = self.wrap(width);
        if w.cursor_row + 1 >= w.rows.len() {
            return false;
        }
        self.move_to_row(width, w.cursor_row + 1, w.cursor_col);
        true
    }

    /// Place the cursor on visual `row` at `col` (clamped to the row's length).
    fn move_to_row(&mut self, width: u16, row: usize, col: u16) {
        let (rows, starts) = self.layout((width as usize).max(1));
        // A target past the last laid-out row is the floated end-of-buffer row.
        let Some(text) = rows.get(row) else {
            self.cursor = self.text.len();
            return;
        };
        let len = text.chars().count();
        let target_char = starts[row] + (col as usize).min(len);
        self.cursor = self.char_to_byte(target_char);
    }

    /// Byte offset of the `n`th character (clamped to the buffer end).
    fn char_to_byte(&self, n: usize) -> usize {
        self.text.char_indices().nth(n).map(|(b, _)| b).unwrap_or(self.text.len())
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

    fn at_char(line: &mut InputLine, n: usize) {
        line.move_home();
        while line.cursor() > 0 {
            line.move_left();
        }
        for _ in 0..n {
            line.move_right();
        }
    }

    #[test]
    fn wrap_short_text_is_single_row() {
        let line = InputLine::from("hello");
        let w = line.wrap(20);
        assert_eq!(w.rows, vec!["hello".to_string()]);
        assert_eq!((w.cursor_row, w.cursor_col), (0, 5));
    }

    #[test]
    fn wrap_empty_buffer_is_one_empty_row() {
        let line = InputLine::new();
        let w = line.wrap(10);
        assert_eq!(w.rows, vec![String::new()]);
        assert_eq!((w.cursor_row, w.cursor_col), (0, 0));
    }

    #[test]
    fn wrap_long_line_breaks_into_width_sized_rows() {
        let mut line = InputLine::from("0123456789");
        at_char(&mut line, 0);
        let w = line.wrap(5);
        assert_eq!(w.rows, vec!["01234".to_string(), "56789".to_string()]);
        assert_eq!((w.cursor_row, w.cursor_col), (0, 0));
    }

    #[test]
    fn wrap_cursor_at_full_width_end_wraps_to_fresh_row() {
        // Cursor at end of a line whose length is an exact multiple of width
        // must stay visible: it sits at col 0 of a trailing empty row.
        let line = InputLine::from("0123456789"); // cursor at end (10)
        let w = line.wrap(5);
        assert_eq!(w.rows, vec!["01234".to_string(), "56789".to_string(), String::new()]);
        assert_eq!((w.cursor_row, w.cursor_col), (2, 0));
    }

    #[test]
    fn wrap_cursor_at_soft_boundary_is_start_of_next_row() {
        let mut line = InputLine::from("0123456789");
        at_char(&mut line, 5);
        let w = line.wrap(5);
        assert_eq!((w.cursor_row, w.cursor_col), (1, 0));
    }

    #[test]
    fn wrap_splits_on_explicit_newlines() {
        let line = InputLine::from("ab\ncd"); // cursor at end
        let w = line.wrap(10);
        assert_eq!(w.rows, vec!["ab".to_string(), "cd".to_string()]);
        assert_eq!((w.cursor_row, w.cursor_col), (1, 2));
    }

    #[test]
    fn wrap_cursor_just_before_newline_stays_on_first_row() {
        let mut line = InputLine::from("ab\ncd");
        at_char(&mut line, 2); // right before the '\n'
        let w = line.wrap(10);
        assert_eq!((w.cursor_row, w.cursor_col), (0, 2));
    }

    #[test]
    fn wrap_trailing_newline_yields_empty_last_row() {
        let line = InputLine::from("ab\n");
        let w = line.wrap(10);
        assert_eq!(w.rows, vec!["ab".to_string(), String::new()]);
        assert_eq!((w.cursor_row, w.cursor_col), (1, 0));
    }

    #[test]
    fn move_up_and_down_traverse_wrapped_rows() {
        let mut line = InputLine::from("0123456789");
        at_char(&mut line, 8); // row 1, col 3 at width 5
        assert_eq!(
            {
                let w = line.wrap(5);
                (w.cursor_row, w.cursor_col)
            },
            (1, 3)
        );
        assert!(line.move_up(5));
        assert_eq!(
            {
                let w = line.wrap(5);
                (w.cursor_row, w.cursor_col)
            },
            (0, 3)
        );
        assert!(line.move_down(5));
        assert_eq!(
            {
                let w = line.wrap(5);
                (w.cursor_row, w.cursor_col)
            },
            (1, 3)
        );
    }

    #[test]
    fn move_up_at_top_row_returns_false() {
        let mut line = InputLine::from("hello");
        at_char(&mut line, 0);
        assert!(!line.move_up(20));
    }

    #[test]
    fn move_down_at_bottom_row_returns_false() {
        let mut line = InputLine::from("hello"); // single row, cursor at end
        assert!(!line.move_down(20));
    }

    #[test]
    fn move_down_crosses_explicit_newline() {
        let mut line = InputLine::from("abc\nxy");
        at_char(&mut line, 0);
        assert!(line.move_down(10));
        assert_eq!(line.wrap(10).cursor_row, 1);
    }

    #[test]
    fn home_and_end_act_on_current_logical_line() {
        let mut line = InputLine::from("abc\nxyz"); // cursor at end (7)
        line.move_home();
        assert_eq!(line.cursor_char(), 4); // start of "xyz"
        line.move_end();
        assert_eq!(line.cursor_char(), 7);
    }
}
