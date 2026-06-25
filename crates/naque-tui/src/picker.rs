//! Reusable option picker widget.
//!
//! The picker is a vertical list of labeled options. The user moves through
//! them with Up/Down arrows (selection **clamps** at the ends rather than
//! wrapping, keeping navigation predictable) or by pressing a shortcut key.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::Theme;

/// A single entry in the picker.
#[derive(Debug, Clone)]
pub struct PickerOption {
    pub label: String,
    /// Optional single-character shortcut; shown as "(x)" in the label.
    pub shortcut: Option<char>,
}

/// What the picker resolved to after a key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// The user confirmed the option at the given index.
    Selected(usize),
    /// The user pressed Escape without confirming.
    Cancelled,
}

/// Vertical option picker.
///
/// Selection movement **clamps** (not wraps): `up()` stops at index 0,
/// `down()` stops at the last index.
pub struct Picker {
    options: Vec<PickerOption>,
    selected: usize,
}

impl Picker {
    /// Create a new picker. `selected` starts at 0.
    ///
    /// # Panics
    /// Panics if `options` is empty.
    pub fn new(options: Vec<PickerOption>) -> Self {
        assert!(!options.is_empty(), "Picker requires at least one option");
        Self { options, selected: 0 }
    }

    /// Index of the currently highlighted option.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Move selection up by one, clamping at index 0.
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move selection down by one, clamping at the last index.
    pub fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.options.len() - 1);
    }

    /// Handle a key event.
    ///
    /// - `Up` / `Down` → moves selection, returns `None`.
    /// - `Enter` → `Some(Selected(current))`.
    /// - A character matching an option's shortcut → `Some(Selected(that index))`.
    /// - `Esc` → `Some(Cancelled)`.
    /// - Any other key → `None`.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PickerOutcome> {
        match key.code {
            KeyCode::Up => {
                self.up();
                None
            },
            KeyCode::Down => {
                self.down();
                None
            },
            KeyCode::Enter => Some(PickerOutcome::Selected(self.selected)),
            KeyCode::Char(c) => {
                let c_lower = c.to_ascii_lowercase();
                self.options.iter().enumerate().find_map(|(i, opt)| {
                    opt.shortcut
                        .map(|s| s.to_ascii_lowercase())
                        .filter(|&s| s == c_lower)
                        .map(|_| PickerOutcome::Selected(i))
                })
            },
            KeyCode::Esc => Some(PickerOutcome::Cancelled),
            _ => None,
        }
    }

    /// Render the picker into `buf` within `area`.
    ///
    /// Each option occupies one line. The selected row gets a `❯ ` prefix and
    /// the `theme.selected_style()`; all other rows are indented with `  `.
    /// Shortcut characters are shown as `(x)` before the label.
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        let selected_style = theme.selected_style();

        for (i, opt) in self.options.iter().enumerate() {
            let y = area.y.saturating_add(i as u16);
            if y >= area.y + area.height {
                break;
            }

            let prefix = if i == self.selected { "❯ " } else { "  " };
            let shortcut_part = match opt.shortcut {
                Some(c) => format!("({}) ", c),
                None => String::new(),
            };
            let text = format!("{}{}{}", prefix, shortcut_part, opt.label);

            let line = if i == self.selected {
                Line::from(Span::styled(text, selected_style))
            } else {
                Line::from(Span::raw(text))
            };

            let row_area = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };
            line.render(row_area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    fn make_options() -> Vec<PickerOption> {
        vec![
            PickerOption {
                label: "Accept".into(),
                shortcut: Some('a'),
            },
            PickerOption {
                label: "Edit".into(),
                shortcut: Some('e'),
            },
            PickerOption {
                label: "Reject".into(),
                shortcut: Some('r'),
            },
        ]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE)
    }

    // --- navigation ---

    #[test]
    fn starts_at_zero() {
        let p = Picker::new(make_options());
        assert_eq!(p.selected(), 0);
    }

    #[test]
    fn down_increments_selected() {
        let mut p = Picker::new(make_options());
        p.down();
        assert_eq!(p.selected(), 1);
    }

    #[test]
    fn up_decrements_selected() {
        let mut p = Picker::new(make_options());
        p.down();
        p.down();
        p.up();
        assert_eq!(p.selected(), 1);
    }

    #[test]
    fn up_clamps_at_zero() {
        let mut p = Picker::new(make_options());
        p.up();
        p.up();
        assert_eq!(p.selected(), 0);
    }

    #[test]
    fn down_clamps_at_last() {
        let mut p = Picker::new(make_options());
        for _ in 0..10 {
            p.down();
        }
        assert_eq!(p.selected(), 2); // last index is 2
    }

    // --- handle_key navigation returns None ---

    #[test]
    fn handle_key_down_returns_none_and_moves() {
        let mut p = Picker::new(make_options());
        let result = p.handle_key(key(KeyCode::Down));
        assert!(result.is_none());
        assert_eq!(p.selected(), 1);
    }

    #[test]
    fn handle_key_up_returns_none_and_moves() {
        let mut p = Picker::new(make_options());
        p.down();
        let result = p.handle_key(key(KeyCode::Up));
        assert!(result.is_none());
        assert_eq!(p.selected(), 0);
    }

    // --- handle_key outcomes ---

    #[test]
    fn handle_key_enter_selects_current() {
        let mut p = Picker::new(make_options());
        p.down();
        let result = p.handle_key(key(KeyCode::Enter));
        assert_eq!(result, Some(PickerOutcome::Selected(1)));
    }

    #[test]
    fn handle_key_shortcut_char_selects_matching_option() {
        let mut p = Picker::new(make_options());
        // 'e' should select index 1 (Edit)
        let result = p.handle_key(key(KeyCode::Char('e')));
        assert_eq!(result, Some(PickerOutcome::Selected(1)));
    }

    #[test]
    fn handle_key_shortcut_char_case_insensitive() {
        let mut p = Picker::new(make_options());
        let result = p.handle_key(key(KeyCode::Char('R')));
        assert_eq!(result, Some(PickerOutcome::Selected(2)));
    }

    #[test]
    fn handle_key_esc_returns_cancelled() {
        let mut p = Picker::new(make_options());
        let result = p.handle_key(key(KeyCode::Esc));
        assert_eq!(result, Some(PickerOutcome::Cancelled));
    }

    #[test]
    fn handle_key_unrecognized_returns_none() {
        let mut p = Picker::new(make_options());
        let result = p.handle_key(key(KeyCode::F(1)));
        assert!(result.is_none());
    }

    #[test]
    fn handle_key_char_with_no_matching_shortcut_returns_none() {
        let mut p = Picker::new(make_options());
        // 'z' has no shortcut
        let result = p.handle_key(key(KeyCode::Char('z')));
        assert!(result.is_none());
    }

    // --- render tests ---

    fn render_picker(picker: &Picker, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        picker.render(&Theme::new(false), area, &mut buf);
        buf
    }

    #[test]
    fn render_selected_row_contains_marker() {
        let p = Picker::new(make_options());
        let buf = render_picker(&p, 40, 5);
        // Row 0 is selected; it should contain the ❯ marker
        let row0: String = (0..40)
            .map(|x| buf.cell((x, 0)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
            .collect();
        assert!(row0.contains('❯'), "selected row must contain ❯, got: {row0:?}");
    }

    #[test]
    fn render_non_selected_row_has_no_marker() {
        let p = Picker::new(make_options());
        let buf = render_picker(&p, 40, 5);
        // Row 1 is not selected; it must NOT contain ❯
        let row1: String = (0..40)
            .map(|x| buf.cell((x, 1)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
            .collect();
        assert!(!row1.contains('❯'), "non-selected row must not contain ❯, got: {row1:?}");
    }

    #[test]
    fn render_via_test_backend() {
        let mut p = Picker::new(make_options());
        p.down(); // select index 1 (Edit)

        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                p.render(&Theme::new(false), frame.area(), frame.buffer_mut());
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let row1: String = (0..40)
            .map(|x| buf.cell((x, 1)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
            .collect();
        assert!(row1.contains('❯'), "row 1 (Edit) should be selected: {row1:?}");
    }
}
