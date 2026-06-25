//! The pinned action line shown above the status bar while a turn runs.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::Theme;

/// Braille spinner frames, advanced once per UI tick.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Snapshot of in-flight state for the pinned line.
pub struct ActivityLine {
    pub action: String,
    pub spinner_frame: usize,
    pub iteration: u32,
    pub max_iterations: u32,
    pub tokens: u64,
    pub awaiting_approval: bool,
}

impl ActivityLine {
    /// Render as a single line: `⠹ <action> · iter k/n · <tokens> tok · ^C to cancel`.
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
        let action = if self.awaiting_approval {
            "waiting for approval"
        } else {
            &self.action
        };
        let head = format!("{spinner} {action}");
        let tail = format!("  · iter {}/{} · {} tok · ^C to cancel", self.iteration, self.max_iterations, self.tokens);
        let spans = vec![
            Span::styled(head, theme.activity_style()),
            Span::styled(tail, theme.dim_style()),
        ];
        let row = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        Line::from(spans).render(row, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_to_string(line: &ActivityLine, color: bool) -> String {
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        line.render(&Theme::new(color), area, &mut buf);
        (0..120)
            .map(|x| buf.cell((x, 0)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
            .collect()
    }

    fn sample(frame: usize) -> ActivityLine {
        ActivityLine {
            action: "run_query".into(),
            spinner_frame: frame,
            iteration: 2,
            max_iterations: 12,
            tokens: 1200,
            awaiting_approval: false,
        }
    }

    #[test]
    fn shows_action_iter_and_cancel_hint() {
        let s = render_to_string(&sample(0), true);
        assert!(s.contains("run_query"), "{s:?}");
        assert!(s.contains("iter 2/12"), "{s:?}");
        assert!(s.contains("1200 tok"), "{s:?}");
        assert!(s.contains("^C to cancel"), "{s:?}");
    }

    #[test]
    fn spinner_frame_changes_glyph() {
        let a = render_to_string(&sample(0), true);
        let b = render_to_string(&sample(1), true);
        assert_ne!(a.chars().next(), b.chars().next());
    }

    #[test]
    fn legible_without_color() {
        let s = render_to_string(&sample(0), false);
        assert!(s.contains("run_query") && s.contains("iter 2/12"), "{s:?}");
    }

    #[test]
    fn approval_state_changes_label() {
        let mut l = sample(0);
        l.awaiting_approval = true;
        let s = render_to_string(&l, true);
        assert!(s.contains("waiting for approval"), "{s:?}");
    }
}
