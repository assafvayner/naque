//! Status bar rendered at the bottom of the TUI.
//!
//! Format (all on one line):
//! `profile=<name>  mode=<mode>  tx=none|open  tokens=<n>  $<cost>`

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::Widget,
};

use naque_core::PermissionMode;

use crate::Theme;

/// Snapshot of the runtime state rendered in the status bar.
pub struct StatusBar {
    pub profile: String,
    pub mode: PermissionMode,
    pub in_transaction: bool,
    pub tokens: u64,
    pub cost_usd: f64,
}

impl StatusBar {
    /// Render the status bar as a single line into `buf`.
    ///
    /// Segments:
    /// - `profile=<name>` — plain text
    /// - `mode=<mode>` — styled with `theme.mode_style`
    /// - `tx=none` or `tx=open`
    /// - `tokens=<n>`
    /// - `$<cost>` formatted to 2 decimal places (e.g. `$0.03`)
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        let tx_label = if self.in_transaction { "open" } else { "none" };
        let cost = format!("${:.2}", self.cost_usd);

        let profile_seg = format!("profile={}  ", self.profile);
        let mode_prefix = "mode=";
        let mode_value = self.mode.to_string();
        let mode_suffix = format!("  tx={}  tokens={}  {}", tx_label, self.tokens, cost);

        let spans = vec![
            Span::raw(profile_seg),
            Span::raw(mode_prefix),
            Span::styled(mode_value, theme.mode_style(self.mode)),
            Span::raw(mode_suffix),
        ];

        let line = Line::from(spans);
        let row_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        line.render(row_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect};

    fn make_bar(
        profile: &str,
        mode: PermissionMode,
        in_tx: bool,
        tokens: u64,
        cost: f64,
    ) -> StatusBar {
        StatusBar {
            profile: profile.into(),
            mode,
            in_transaction: in_tx,
            tokens,
            cost_usd: cost,
        }
    }

    fn render_bar(bar: &StatusBar) -> String {
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        bar.render(&Theme::new(false), area, &mut buf);
        (0..120)
            .map(|x| {
                buf.cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    }

    #[test]
    fn status_bar_contains_profile() {
        let bar = make_bar("mydb", PermissionMode::ReadOnly, false, 42, 0.03);
        let s = render_bar(&bar);
        assert!(s.contains("mydb"), "expected profile name: {s:?}");
    }

    #[test]
    fn status_bar_contains_mode() {
        let bar = make_bar("mydb", PermissionMode::ReadOnly, false, 42, 0.03);
        let s = render_bar(&bar);
        assert!(s.contains("readonly"), "expected mode string: {s:?}");
    }

    #[test]
    fn status_bar_tx_none_when_not_in_transaction() {
        let bar = make_bar("mydb", PermissionMode::Default, false, 0, 0.0);
        let s = render_bar(&bar);
        assert!(s.contains("tx=none"), "expected tx=none: {s:?}");
    }

    #[test]
    fn status_bar_tx_open_when_in_transaction() {
        let bar = make_bar("mydb", PermissionMode::Default, true, 0, 0.0);
        let s = render_bar(&bar);
        assert!(s.contains("tx=open"), "expected tx=open: {s:?}");
    }

    #[test]
    fn status_bar_formats_cost_to_two_decimal_places() {
        let bar = make_bar("mydb", PermissionMode::Default, false, 100, 0.03);
        let s = render_bar(&bar);
        assert!(s.contains("$0.03"), "expected $0.03: {s:?}");
    }

    #[test]
    fn status_bar_formats_larger_cost() {
        let bar = make_bar("mydb", PermissionMode::Wildcard, false, 5000, 1.50);
        let s = render_bar(&bar);
        assert!(s.contains("$1.50"), "expected $1.50: {s:?}");
    }

    #[test]
    fn status_bar_contains_token_count() {
        let bar = make_bar("mydb", PermissionMode::Strict, false, 999, 0.01);
        let s = render_bar(&bar);
        assert!(s.contains("tokens=999"), "expected tokens=999: {s:?}");
    }
}
