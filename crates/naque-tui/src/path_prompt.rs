//! Allow-once / this-session / deny prompt for a filesystem read the agent
//! requested outside the configured allow-list. This is the FS permission
//! dimension's interactive surface, parallel to the SQL [`ApprovalPrompt`].
//!
//! [`ApprovalPrompt`]: crate::ApprovalPrompt

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::Theme;
use crate::picker::{Picker, PickerOption, PickerOutcome};

/// The user's decision from the filesystem-access prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathChoice {
    /// Allow this one access; don't remember it.
    Once,
    /// Allow and remember the path for the rest of the session.
    Session,
    /// Refuse the access.
    Deny,
}

/// Prompt shown when the agent tries to read/list a path outside the allow-list.
pub struct PathPrompt {
    path: String,
    action: String,
    picker: Picker,
}

impl PathPrompt {
    /// Build the prompt. Options are in fixed order: Allow once (o), Allow this
    /// session (s), Deny (d); the default selection is "Allow once".
    pub fn new(path: String, action: String) -> Self {
        let options = vec![
            PickerOption {
                label: "Allow once".into(),
                shortcut: Some('o'),
            },
            PickerOption {
                label: "Allow this session".into(),
                shortcut: Some('s'),
            },
            PickerOption {
                label: "Deny".into(),
                shortcut: Some('d'),
            },
        ];
        Self {
            path,
            action,
            picker: Picker::new(options),
        }
    }

    /// Number of physical lines in the path text (at least 1).
    pub fn path_line_count(&self) -> usize {
        self.path.lines().count().max(1)
    }

    /// Widest content line, for sizing a containing modal.
    pub fn content_width(&self) -> usize {
        let header = format!("Allow the agent to {} this path?", self.action).chars().count();
        let hint = "(o) once   (s) this session   (d) deny — or /allow-dir to pre-grant"
            .chars()
            .count();
        header.max(hint).max(self.path.chars().count())
    }

    /// Forward a key event to the picker; `Cancelled` (Esc) maps to `Deny`.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PathChoice> {
        self.picker.handle_key(key).map(|outcome| match outcome {
            PickerOutcome::Selected(0) => PathChoice::Once,
            PickerOutcome::Selected(1) => PathChoice::Session,
            PickerOutcome::Selected(_) => PathChoice::Deny,
            PickerOutcome::Cancelled => PathChoice::Deny,
        })
    }

    /// Render: header line, blank, the requested path, blank, then the picker.
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        let line_at = |y: u16, line: Line, buf: &mut Buffer| {
            line.render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        };

        if y < area.y + area.height {
            let header = format!("Allow the agent to {} this path?", self.action);
            line_at(y, Line::from(Span::raw(header)), buf);
            y += 1;
        }

        // Blank separator.
        y = y.saturating_add(1).min(area.y + area.height);

        for path_line in self.path.lines() {
            if y >= area.y + area.height {
                break;
            }
            line_at(y, Line::from(Span::styled(path_line.to_string(), theme.dim_style())), buf);
            y += 1;
        }

        // Blank separator before the picker.
        y = y.saturating_add(1).min(area.y + area.height);

        let picker_area = Rect {
            x: area.x,
            y,
            width: area.width,
            height: area.height.saturating_sub(y.saturating_sub(area.y)),
        };
        self.picker.render(theme, picker_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::layout::Rect;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn prompt() -> PathPrompt {
        PathPrompt::new("/home/u/secrets/k.txt".into(), "read".into())
    }

    #[test]
    fn shortcut_o_returns_once() {
        assert_eq!(prompt().handle_key(key(KeyCode::Char('o'))), Some(PathChoice::Once));
    }

    #[test]
    fn shortcut_s_returns_session() {
        assert_eq!(prompt().handle_key(key(KeyCode::Char('s'))), Some(PathChoice::Session));
    }

    #[test]
    fn shortcut_d_and_esc_return_deny() {
        assert_eq!(prompt().handle_key(key(KeyCode::Char('d'))), Some(PathChoice::Deny));
        assert_eq!(prompt().handle_key(key(KeyCode::Esc)), Some(PathChoice::Deny));
    }

    #[test]
    fn enter_on_default_returns_once() {
        // Default selection is index 0 (Allow once).
        assert_eq!(prompt().handle_key(key(KeyCode::Enter)), Some(PathChoice::Once));
    }

    #[test]
    fn render_contains_path_and_question() {
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        prompt().render(&Theme::new(false), area, &mut buf);
        let mut content = String::new();
        for y in 0..12 {
            for x in 0..80 {
                if let Some(c) = buf.cell((x, y)) {
                    content.push_str(c.symbol());
                }
            }
        }
        assert!(content.contains("secrets/k.txt"), "expected path: {content}");
        assert!(content.contains("read this path?"), "expected question: {content}");
    }
}
