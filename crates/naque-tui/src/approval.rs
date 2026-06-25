//! Accept / Edit / Reject approval flow for a query.

use ratatui::{
    buffer::Buffer,
    crossterm::event::KeyEvent,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

use naque_core::{CatastrophicReason, GateDecision};

use crate::{
    picker::{Picker, PickerOption, PickerOutcome},
    Theme,
};

/// The user's decision from the approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    Accept,
    Edit,
    Reject,
}

/// Approval prompt shown before running a query.
///
/// For a `PromptCatastrophic` decision the default selection is **Reject**
/// (index 2) so the safe choice is already highlighted.
pub struct ApprovalPrompt {
    sql: String,
    label: String,
    catastrophic: Option<CatastrophicReason>,
    picker: Picker,
}

impl ApprovalPrompt {
    /// Build the approval prompt.
    ///
    /// Options are always in this fixed order: Accept (a), Edit (e), Reject (r).
    /// If `decision` is `PromptCatastrophic`, the initial selection is Reject (index 2).
    pub fn new(
        sql: String,
        label: String,
        catastrophic: Option<CatastrophicReason>,
        decision: GateDecision,
    ) -> Self {
        let options = vec![
            PickerOption {
                label: "Accept".into(),
                shortcut: Some('a'),
            },
            PickerOption {
                label: "Edit before run".into(),
                shortcut: Some('e'),
            },
            PickerOption {
                label: "Reject".into(),
                shortcut: Some('r'),
            },
        ];

        let mut picker = Picker::new(options);

        if decision == GateDecision::PromptCatastrophic {
            // Default to Reject (index 2) for catastrophic queries.
            picker.down();
            picker.down();
        }

        Self {
            sql,
            label,
            catastrophic,
            picker,
        }
    }

    /// Forward a key event to the picker and map the outcome.
    ///
    /// - `Selected(0)` → `Accept`
    /// - `Selected(1)` → `Edit`
    /// - `Selected(2)` → `Reject`
    /// - `Cancelled` (Esc) → `Reject`
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<ApprovalChoice> {
        self.picker.handle_key(key).map(|outcome| match outcome {
            PickerOutcome::Selected(0) => ApprovalChoice::Accept,
            PickerOutcome::Selected(1) => ApprovalChoice::Edit,
            PickerOutcome::Selected(_) => ApprovalChoice::Reject,
            PickerOutcome::Cancelled => ApprovalChoice::Reject,
        })
    }

    /// Render: header line, optional catastrophic warning, SQL block, then picker.
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;

        let header = format!("Run this query?  classified: {}", self.label);
        let header_line = Line::from(Span::raw(&header));
        if y < area.y + area.height {
            header_line.render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y += 1;
        }

        // Catastrophic warning — red + bold when color enabled; bold+reversed otherwise.
        if let Some(reason) = self.catastrophic {
            let warning = format!("⚠  CATASTROPHIC: {} — review carefully!", reason.human());
            let warn_style = if theme.color {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
            };
            let warn_line = Line::from(Span::styled(warning, warn_style));
            if y < area.y + area.height {
                warn_line.render(
                    Rect {
                        x: area.x,
                        y,
                        width: area.width,
                        height: 1,
                    },
                    buf,
                );
                y += 1;
            }
        }

        // Blank separator
        y = y.saturating_add(1).min(area.y + area.height);

        // SQL text — one line per physical line in the SQL.
        for sql_line in self.sql.lines() {
            if y >= area.y + area.height {
                break;
            }
            let l = Line::from(Span::raw(sql_line));
            l.render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y += 1;
        }

        // Blank separator before picker
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
    use super::*;
    use naque_core::GateDecision;
    use ratatui::{buffer::Buffer, crossterm::event::KeyCode, layout::Rect};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE)
    }

    fn prompt(decision: GateDecision) -> ApprovalPrompt {
        ApprovalPrompt::new("SELECT 1".into(), "read-only".into(), None, decision)
    }

    fn catastrophic_prompt() -> ApprovalPrompt {
        ApprovalPrompt::new(
            "DROP TABLE users".into(),
            "DDL: DROP".into(),
            Some(CatastrophicReason::DropObject),
            GateDecision::PromptCatastrophic,
        )
    }

    // --- handle_key ---

    #[test]
    fn shortcut_a_returns_accept() {
        let mut p = prompt(GateDecision::Prompt);
        assert_eq!(
            p.handle_key(key(KeyCode::Char('a'))),
            Some(ApprovalChoice::Accept)
        );
    }

    #[test]
    fn shortcut_e_returns_edit() {
        let mut p = prompt(GateDecision::Prompt);
        assert_eq!(
            p.handle_key(key(KeyCode::Char('e'))),
            Some(ApprovalChoice::Edit)
        );
    }

    #[test]
    fn shortcut_r_returns_reject() {
        let mut p = prompt(GateDecision::Prompt);
        assert_eq!(
            p.handle_key(key(KeyCode::Char('r'))),
            Some(ApprovalChoice::Reject)
        );
    }

    #[test]
    fn esc_returns_reject() {
        let mut p = prompt(GateDecision::Prompt);
        assert_eq!(
            p.handle_key(key(KeyCode::Esc)),
            Some(ApprovalChoice::Reject)
        );
    }

    #[test]
    fn enter_on_default_prompt_returns_accept() {
        let mut p = prompt(GateDecision::Prompt);
        // Default selection is 0 (Accept) for a normal Prompt
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            Some(ApprovalChoice::Accept)
        );
    }

    #[test]
    fn enter_on_catastrophic_default_returns_reject() {
        let mut p = catastrophic_prompt();
        // PromptCatastrophic defaults to Reject (index 2)
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            Some(ApprovalChoice::Reject)
        );
    }

    #[test]
    fn non_shortcut_key_returns_none() {
        let mut p = prompt(GateDecision::Prompt);
        assert!(p.handle_key(key(KeyCode::F(5))).is_none());
    }

    // --- render ---

    fn render_prompt(prompt: &ApprovalPrompt) -> Buffer {
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        prompt.render(&Theme::new(false), area, &mut buf);
        buf
    }

    fn buf_to_string(buf: &Buffer, width: u16, height: u16) -> String {
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                if let Some(c) = buf.cell((x, y)) {
                    out.push_str(c.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn render_contains_sql() {
        let p = prompt(GateDecision::Prompt);
        let buf = render_prompt(&p);
        let content = buf_to_string(&buf, 80, 20);
        assert!(
            content.contains("SELECT 1"),
            "expected SQL in render output: {content}"
        );
    }

    #[test]
    fn render_catastrophic_contains_sql_and_reason() {
        let p = catastrophic_prompt();
        let buf = render_prompt(&p);
        let content = buf_to_string(&buf, 80, 20);
        assert!(
            content.contains("DROP TABLE users"),
            "expected SQL: {content}"
        );
        assert!(
            content.contains("DROP"),
            "expected catastrophic reason in render output: {content}"
        );
    }

    #[test]
    fn render_catastrophic_contains_warning_indicator() {
        let p = catastrophic_prompt();
        let buf = render_prompt(&p);
        let content = buf_to_string(&buf, 80, 20);
        assert!(
            content.contains("CATASTROPHIC"),
            "expected CATASTROPHIC warning: {content}"
        );
    }

    #[test]
    fn render_normal_prompt_contains_classification_label() {
        let p = prompt(GateDecision::Prompt);
        let buf = render_prompt(&p);
        let content = buf_to_string(&buf, 80, 20);
        assert!(content.contains("read-only"), "expected label: {content}");
    }
}
