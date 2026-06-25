//! Startup help text and database-connection guidance.
//!
//! These messages are printed to the terminal *before* the TUI launches, so
//! they are plain strings with optional ANSI styling rather than ratatui
//! widgets. Color is opt-in via [`color_enabled`] so we honor `--no-color`,
//! the `NO_COLOR` convention, and non-tty output.

use std::fmt;

const RESET: &str = "\x1b[0m";
const BOLD_RED: &str = "\x1b[1;31m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const BOLD_GREEN: &str = "\x1b[1;32m";

/// Error signalling that no database connection could be resolved.
///
/// `bare` is true when the user launched `naque` without any
/// connection-related arguments. In that case the binary shows friendly
/// getting-started guidance and exits 0 instead of treating it as an error.
#[derive(Debug)]
pub struct NoConnection {
    pub bare: bool,
}

impl fmt::Display for NoConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no database connection configured")
    }
}

impl std::error::Error for NoConnection {}

/// Decide whether ANSI styling should be emitted.
///
/// Pure so it can be unit-tested: the caller supplies the `NO_COLOR` and tty
/// state. Color is on only when output is a terminal and neither `--no-color`
/// nor `NO_COLOR` is set.
pub fn color_enabled(no_color_flag: bool, no_color_env: bool, is_tty: bool) -> bool {
    is_tty && !no_color_flag && !no_color_env
}

fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Section header / banner styling (matches the clap `--help` header color).
pub(crate) fn header(text: &str, color: bool) -> String {
    paint(text, BOLD_CYAN, color)
}

/// A connection token / literal (matches the clap `--help` literal color).
pub(crate) fn token(text: &str, color: bool) -> String {
    paint(text, BOLD_GREEN, color)
}

/// The red `Error:` label.
fn error_label(color: bool) -> String {
    paint("Error:", BOLD_RED, color)
}

/// The ways to provide a connection, as `(token, description)` pairs.
const OPTIONS: &[(&str, &str)] = &[
    ("--url <CONN>", "explicit connection string"),
    ("DATABASE_URL", "environment variable"),
    ("<profile>", "a profile name (see naque.toml)"),
    ("naque.toml", "add [profiles.<name>] with a connection"),
];

/// Render the bulleted connection options, aligned and optionally bolded.
fn options_block(color: bool) -> String {
    const COL: usize = 16;
    OPTIONS
        .iter()
        .map(|(name, desc)| {
            let pad = COL.saturating_sub(name.len()) + 1;
            format!("  {}{}{}", token(name, color), " ".repeat(pad), desc)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Message for when a connection was expected but none resolved (an error).
pub fn render_no_connection_error(color: bool) -> String {
    format!(
        "{} no database connection configured.\n\n\
         {}\n{}\n\n\
         Run `naque --help` for setup details.",
        error_label(color),
        header("Set one of:", color),
        options_block(color),
    )
}

/// Friendly first-run guidance for a bare launch with nothing configured.
pub fn render_getting_started(color: bool) -> String {
    format!(
        "{}\n\n\
         No database connection is configured yet. Set one of:\n{}\n\n\
         Run `naque --help` for all options and examples.",
        header("naque — agentic AI query tool over databases", color),
        options_block(color),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_enabled_only_when_tty_and_no_overrides() {
        assert!(color_enabled(false, false, true));
        assert!(!color_enabled(true, false, true)); // --no-color
        assert!(!color_enabled(false, true, true)); // NO_COLOR set
        assert!(!color_enabled(false, false, false)); // piped / not a tty
    }

    #[test]
    fn no_connection_error_lists_every_option() {
        let msg = render_no_connection_error(false);
        assert!(msg.contains("no database connection configured"));
        assert!(msg.contains("--url"));
        assert!(msg.contains("DATABASE_URL"));
        assert!(msg.contains("<profile>"));
        assert!(msg.contains("naque.toml"));
        assert!(msg.contains("naque --help"));
    }

    #[test]
    fn getting_started_is_friendly_and_lists_options() {
        let msg = render_getting_started(false);
        assert!(msg.contains("agentic AI query tool"));
        assert!(msg.contains("--url"));
        assert!(msg.contains("DATABASE_URL"));
        assert!(msg.contains("naque --help"));
        // Friendly framing, not an error.
        assert!(!msg.contains("Error:"));
    }

    #[test]
    fn plain_output_has_no_ansi_escapes() {
        assert!(!render_no_connection_error(false).contains('\x1b'));
        assert!(!render_getting_started(false).contains('\x1b'));
    }

    #[test]
    fn colored_output_has_ansi_escapes() {
        assert!(render_no_connection_error(true).contains('\x1b'));
        assert!(render_getting_started(true).contains('\x1b'));
    }

    #[test]
    fn colored_error_uses_themed_styles() {
        let msg = render_no_connection_error(true);
        assert!(msg.contains("\x1b[1;31mError:"), "red Error label");
        assert!(msg.contains("\x1b[1;36mSet one of:"), "cyan header");
        assert!(msg.contains("\x1b[1;32m--url"), "green option token");
    }

    #[test]
    fn colored_getting_started_uses_cyan_banner() {
        let msg = render_getting_started(true);
        assert!(msg.contains("\x1b[1;36mnaque — agentic"), "cyan banner");
        assert!(msg.contains("\x1b[1;32mDATABASE_URL"), "green option token");
    }
}
