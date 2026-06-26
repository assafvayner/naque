//! Slash-command autocomplete popup.
//!
//! A small floating list of [`SlashCommand`]s matching what the user has typed
//! after a leading `/`. The event loop owns the highlighted index and which
//! commands match; this widget only renders them, windowing the list so the
//! highlighted row stays visible when there are more matches than rows.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::commands::SlashCommand;
use crate::theme::Theme;

/// A rendered autocomplete popup over a set of matching commands.
pub struct SlashSuggest {
    items: Vec<SlashCommand>,
    /// Highlighted row, clamped into `0..items.len()`.
    selected: usize,
}

impl SlashSuggest {
    /// Build a popup for `items` with `selected` highlighted (clamped).
    ///
    /// Accepts the borrowed slice returned by [`crate::commands::matching`].
    pub fn new(items: Vec<&SlashCommand>, selected: usize) -> Self {
        let items: Vec<SlashCommand> = items.into_iter().copied().collect();
        let selected = selected.min(items.len().saturating_sub(1));
        Self { items, selected }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The currently highlighted command, if any.
    pub fn selected_command(&self) -> Option<SlashCommand> {
        self.items.get(self.selected).copied()
    }

    /// Preferred inner width: the longest `head  help` row plus the marker.
    pub fn content_width(&self) -> u16 {
        let w = self
            .items
            .iter()
            .map(|c| 2 + c.head().chars().count() + 2 + c.help.chars().count())
            .max()
            .unwrap_or(0);
        w as u16
    }

    /// Total height needed including the border (capped by caller via `area`).
    pub fn preferred_height(&self) -> u16 {
        self.items.len() as u16 + 2
    }

    /// Render the popup (bordered list) into `area`.
    ///
    /// Rows beyond `area` height scroll so the selected row stays visible.
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        let block = Block::default().borders(Borders::ALL).title(" commands ");
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width == 0 || self.items.is_empty() {
            return;
        }

        let visible = inner.height as usize;
        // Window the list so `selected` is inside [start, start+visible).
        let start = if self.selected < visible {
            0
        } else {
            self.selected - visible + 1
        };
        let end = (start + visible).min(self.items.len());

        let selected_style = theme.selected_style();
        let dim = theme.dim_style();

        for (row, idx) in (start..end).enumerate() {
            let cmd = &self.items[idx];
            let y = inner.y + row as u16;
            let row_area = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            };

            if idx == self.selected {
                let text = format!("❯ {}  {}", cmd.head(), cmd.help);
                Line::from(Span::styled(text, selected_style)).render(row_area, buf);
            } else {
                let line = Line::from(vec![Span::raw(format!("  {}  ", cmd.head())), Span::styled(cmd.help, dim)]);
                line.render(row_area, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::commands::matching;

    fn render_to_string(sg: &SlashSuggest, w: u16, h: u16) -> Vec<String> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| sg.render(&Theme::new(false), f.area(), f.buffer_mut()))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf.cell((x, y)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn new_clamps_selected() {
        let sg = SlashSuggest::new(matching("c"), 99);
        assert_eq!(sg.selected(), sg.len() - 1);
    }

    #[test]
    fn empty_is_empty() {
        let sg = SlashSuggest::new(matching("zzz"), 0);
        assert!(sg.is_empty());
    }

    #[test]
    fn renders_matching_command_heads() {
        let sg = SlashSuggest::new(matching("c"), 0); // clear, cost
        let rows = render_to_string(&sg, 50, 6);
        let joined = rows.join("\n");
        assert!(joined.contains("/clear"), "popup should list /clear:\n{joined}");
        assert!(joined.contains("/cost"), "popup should list /cost:\n{joined}");
    }

    #[test]
    fn selected_row_has_marker() {
        let sg = SlashSuggest::new(matching("c"), 1); // highlight /cost
        let rows = render_to_string(&sg, 50, 6);
        let cost_row = rows.iter().find(|r| r.contains("/cost")).expect("cost row");
        assert!(cost_row.contains('❯'), "selected row should carry the marker: {cost_row:?}");
        let clear_row = rows.iter().find(|r| r.contains("/clear")).expect("clear row");
        assert!(!clear_row.contains('❯'), "non-selected row must not: {clear_row:?}");
    }

    #[test]
    fn windows_to_keep_selection_visible() {
        // 8 commands, but a popup only 3 rows tall (1 inner row + borders is 3).
        let sg = SlashSuggest::new(matching(""), 7); // last command highlighted
        let rows = render_to_string(&sg, 60, 3);
        let joined = rows.join("\n");
        // The last command ("exit") must be visible since it's selected.
        assert!(joined.contains("/exit"), "selected (last) row must be windowed in:\n{joined}");
    }
}
