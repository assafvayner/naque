//! Color theme for the TUI with NO_COLOR fallback.
//!
//! When `color == false` (NO_COLOR is set), styles carry no foreground color
//! but still use modifiers (BOLD, REVERSED) so catastrophic actions remain
//! visually distinct on monochrome terminals.

use naque_core::{PermissionMode, StatementKind};
use ratatui::style::{Color, Modifier, Style};

/// Color theme for the TUI; `color == false` yields styles with no foreground
/// color (modifiers only), so the UI stays readable on no-color terminals.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub color: bool,
}

impl Theme {
    pub fn new(color: bool) -> Self {
        Self { color }
    }

    /// Detect from the environment: color disabled when `NO_COLOR` is set
    /// (any value) — honor the <https://no-color.org> convention.
    pub fn detect() -> Self {
        Self {
            color: std::env::var_os("NO_COLOR").is_none(),
        }
    }

    /// Style for a classification badge by kind/catastrophic.
    ///
    /// Color mapping:
    /// - read-only → green
    /// - write → yellow
    /// - DDL → magenta
    /// - Transaction / Set → blue
    /// - catastrophic → red + BOLD
    /// - unknown → yellow
    ///
    /// When `!color`, returns a `Style` with no fg color; catastrophic still
    /// gets `BOLD | REVERSED` so it remains distinguishable.
    pub fn classification_style(&self, kind: StatementKind, catastrophic: bool) -> Style {
        if catastrophic {
            let base = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
            if self.color { base.fg(Color::Red) } else { base }
        } else if self.color {
            let color = match kind {
                StatementKind::Read => Color::Green,
                StatementKind::Write => Color::Yellow,
                StatementKind::Ddl => Color::Magenta,
                StatementKind::Transaction | StatementKind::Set => Color::Blue,
                StatementKind::Unknown => Color::Yellow,
            };
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    /// Map a classification *label* (as stored on a transcript entry or
    /// approval prompt, e.g. `"read-only"`, `"WRITE: UPDATE"`, `"DDL: DROP"`)
    /// to the matching [`StatementKind`] and catastrophic flag, then return the
    /// corresponding [`classification_style`](Self::classification_style).
    ///
    /// This keeps SQL badge coloring consistent between the transcript view and
    /// the approval prompt, and degrades correctly under NO_COLOR (kind colors
    /// drop out, catastrophic stays BOLD | REVERSED).
    pub fn label_style(&self, label: &str) -> Style {
        let (kind, catastrophic) = classify_label(label);
        self.classification_style(kind, catastrophic)
    }

    /// Style for the permission-mode segment in the status bar.
    ///
    /// Color mapping (color-on):
    /// - Wildcard → red + BOLD (stands out as dangerous)
    /// - ReadOnly → green
    /// - Strict → cyan
    /// - Default → plain (inherits terminal default)
    pub fn mode_style(&self, mode: PermissionMode) -> Style {
        if self.color {
            match mode {
                PermissionMode::Wildcard => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                PermissionMode::ReadOnly => Style::default().fg(Color::Green),
                PermissionMode::Strict => Style::default().fg(Color::Cyan),
                PermissionMode::Default => Style::default(),
            }
        } else {
            match mode {
                PermissionMode::Wildcard => Style::default().add_modifier(Modifier::BOLD),
                _ => Style::default(),
            }
        }
    }

    /// Style for the spinner + active-action text in the pinned line.
    pub fn activity_style(&self) -> Style {
        let base = Style::default().add_modifier(Modifier::BOLD);
        if self.color { base.fg(Color::Cyan) } else { base }
    }

    /// Style for secondary/de-emphasized text (iteration, token count, hints).
    pub fn dim_style(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// Style for the highlighted (selected) row in the option picker.
    ///
    /// Always `REVERSED`; adds a color tint when color is enabled.
    pub fn selected_style(&self) -> Style {
        let base = Style::default().add_modifier(Modifier::REVERSED);
        if self.color { base.fg(Color::Cyan) } else { base }
    }
}

/// Infer a [`StatementKind`] and catastrophic flag from a classification label.
///
/// Labels are produced by `naque-sql` (e.g. `"read-only"`, `"WRITE: UPDATE"`,
/// `"DDL: DROP"`, `"DDL: TRUNCATE"`). The mapping is a best-effort textual
/// match used purely for badge coloring; the authoritative gate signal lives in
/// `naque-core`. A label that mentions DROP/TRUNCATE or "catastrophic" is
/// treated as catastrophic so it renders with the danger style.
fn classify_label(label: &str) -> (StatementKind, bool) {
    let lower = label.to_ascii_lowercase();

    let catastrophic = lower.contains("catastrophic") || lower.contains("drop") || lower.contains("truncate");

    let kind = if lower.starts_with("read-only") || lower.starts_with("read only") {
        StatementKind::Read
    } else if lower.starts_with("write") {
        StatementKind::Write
    } else if lower.starts_with("ddl") {
        StatementKind::Ddl
    } else if lower.starts_with("transaction") {
        StatementKind::Transaction
    } else if lower.starts_with("set") {
        StatementKind::Set
    } else {
        StatementKind::Unknown
    };

    (kind, catastrophic)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- classify_label / label_style ---

    #[test]
    fn classify_label_read_only() {
        assert_eq!(classify_label("read-only"), (StatementKind::Read, false));
    }

    #[test]
    fn classify_label_write() {
        assert_eq!(classify_label("WRITE: UPDATE"), (StatementKind::Write, false));
    }

    #[test]
    fn classify_label_ddl_drop_is_catastrophic() {
        assert_eq!(classify_label("DDL: DROP"), (StatementKind::Ddl, true));
    }

    #[test]
    fn classify_label_ddl_truncate_is_catastrophic() {
        assert_eq!(classify_label("DDL: TRUNCATE"), (StatementKind::Ddl, true));
    }

    #[test]
    fn classify_label_plain_ddl_not_catastrophic() {
        assert_eq!(classify_label("DDL: CREATE"), (StatementKind::Ddl, false));
    }

    #[test]
    fn classify_label_unknown_default() {
        assert_eq!(classify_label("SQL"), (StatementKind::Unknown, false));
    }

    #[test]
    fn label_style_write_has_yellow_fg_with_color() {
        let style = Theme::new(true).label_style("WRITE: UPDATE");
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn label_style_read_has_green_fg_with_color() {
        let style = Theme::new(true).label_style("read-only");
        assert_eq!(style.fg, Some(Color::Green));
    }

    #[test]
    fn label_style_catastrophic_drop_is_red_bold_reversed() {
        let style = Theme::new(true).label_style("DDL: DROP");
        assert_eq!(style.fg, Some(Color::Red));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn label_style_no_color_drops_fg_keeps_catastrophic_modifiers() {
        let style = Theme::new(false).label_style("DDL: DROP");
        assert!(style.fg.is_none());
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::REVERSED));
    }

    // --- classification_style ---

    #[test]
    fn write_with_color_has_fg() {
        let style = Theme::new(true).classification_style(StatementKind::Write, false);
        assert!(style.fg.is_some(), "expected a foreground color when color=true");
    }

    #[test]
    fn write_without_color_has_no_fg() {
        let style = Theme::new(false).classification_style(StatementKind::Write, false);
        assert!(style.fg.is_none(), "expected no foreground color when color=false");
    }

    #[test]
    fn catastrophic_with_color_has_fg_and_bold() {
        let style = Theme::new(true).classification_style(StatementKind::Ddl, true);
        assert!(style.fg.is_some());
        assert!(style.add_modifier.contains(Modifier::BOLD), "catastrophic must include BOLD modifier");
    }

    #[test]
    fn catastrophic_without_color_has_no_fg_but_still_bold() {
        let style = Theme::new(false).classification_style(StatementKind::Ddl, true);
        assert!(style.fg.is_none(), "no fg color in no-color mode");
        assert!(
            style.add_modifier.contains(Modifier::BOLD),
            "catastrophic must include BOLD modifier even in no-color mode"
        );
    }

    #[test]
    fn catastrophic_without_color_has_reversed() {
        let style = Theme::new(false).classification_style(StatementKind::Write, true);
        assert!(style.add_modifier.contains(Modifier::REVERSED));
    }

    // --- mode_style ---

    #[test]
    fn wildcard_differs_from_default_when_color_on() {
        let theme = Theme::new(true);
        let wildcard = theme.mode_style(PermissionMode::Wildcard);
        let default = theme.mode_style(PermissionMode::Default);
        assert_ne!(wildcard, default, "wildcard and default must differ when color=true");
    }

    #[test]
    fn wildcard_has_fg_when_color_on() {
        let style = Theme::new(true).mode_style(PermissionMode::Wildcard);
        assert!(style.fg.is_some());
    }

    #[test]
    fn readonly_has_fg_when_color_on() {
        let style = Theme::new(true).mode_style(PermissionMode::ReadOnly);
        assert!(style.fg.is_some());
    }

    #[test]
    fn modes_have_no_fg_when_color_off() {
        let theme = Theme::new(false);
        for mode in [
            PermissionMode::Wildcard,
            PermissionMode::Strict,
            PermissionMode::ReadOnly,
            PermissionMode::Default,
        ] {
            let style = theme.mode_style(mode);
            assert!(style.fg.is_none(), "mode {:?} should have no fg in no-color mode", mode);
        }
    }

    // --- selected_style ---

    #[test]
    fn selected_always_reversed() {
        assert!(Theme::new(true).selected_style().add_modifier.contains(Modifier::REVERSED));
        assert!(Theme::new(false).selected_style().add_modifier.contains(Modifier::REVERSED));
    }

    // --- detect() ---

    #[test]
    fn detect_color_false_when_no_color_set() {
        // Serialize env access to avoid data races with other tests.
        unsafe { std::env::set_var("NO_COLOR", "1") };
        let theme = Theme::detect();
        unsafe { std::env::remove_var("NO_COLOR") };
        assert!(!theme.color);
    }

    #[test]
    fn detect_color_true_when_no_color_absent() {
        unsafe { std::env::remove_var("NO_COLOR") };
        // Only valid if NO_COLOR is truly absent; guard against other tests.
        if std::env::var_os("NO_COLOR").is_none() {
            assert!(Theme::detect().color);
        }
    }

    #[test]
    fn activity_style_has_fg_with_color_none_without() {
        assert!(Theme::new(true).activity_style().fg.is_some());
        assert!(Theme::new(false).activity_style().fg.is_none());
    }

    #[test]
    fn dim_style_uses_dim_modifier() {
        assert!(Theme::new(true).dim_style().add_modifier.contains(Modifier::DIM));
        assert!(Theme::new(false).dim_style().add_modifier.contains(Modifier::DIM));
    }
}
