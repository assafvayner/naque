//! Command-line argument parser for the `naque` binary.

use std::io::IsTerminal;

use clap::builder::styling::{AnsiColor, Styles};

/// Color palette for clap-rendered `--help` (headers cyan, literals green),
/// matching the startup-guidance colors in [`crate::help`]. clap gates this on
/// the terminal / `NO_COLOR` / `CLICOLOR*` itself.
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Cyan.on_default().bold())
    .usage(AnsiColor::Cyan.on_default().bold())
    .literal(AnsiColor::Green.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default());

/// Longer description shown with `naque --help`.
const LONG_ABOUT: &str = "\
naque is an agentic AI query tool for relational databases. Ask questions in \
plain language; naque writes and runs SQL against your configured database, \
gating writes and catastrophic statements behind a permission mode.";

/// Whether the `--help` trailer should be colored.
///
/// clap colors its own sections via `anstream`, but it does not style raw
/// `after_help` text, so we apply (and gate) color ourselves to match. The
/// `--no-color` flag isn't parsed yet at help-render time, so we honor the
/// same environment signals clap does.
fn help_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if let Some(force) = std::env::var_os("CLICOLOR_FORCE") {
        if force != "0" {
            return true;
        }
    }
    std::io::stdout().is_terminal()
}

/// The examples / precedence trailer appended to `naque --help`.
fn after_long_help() -> String {
    after_long_help_text(help_color())
}

/// Build the `--help` trailer, coloring section headers when `color` is set.
fn after_long_help_text(color: bool) -> String {
    use crate::help::header;
    let mut s = String::new();
    s.push_str(&header("EXAMPLES:", color));
    s.push_str(concat!(
        "\n  naque --url postgres://user@host/mydb     Connect directly by URL",
        "\n  naque myproj                              Launch the 'myproj' profile",
        "\n  DATABASE_URL=postgres://... naque         Use the DATABASE_URL env var",
        "\n  naque myproj --mode readonly              Profile in read-only mode",
        "\n\n",
    ));
    s.push_str(&header("CONNECTION PRECEDENCE (first match wins):", color));
    s.push_str(concat!(
        "\n  1. --url",
        "\n  2. active profile (positional arg, naque.toml `project`, or central default)",
        "\n  3. DATABASE_URL",
        "\n\n",
    ));
    s.push_str(&header("CONFIG FILES:", color));
    s.push_str(concat!(
        "\n  ./naque.toml                              local project profiles & settings",
        "\n  ~/.naque/                                 central profiles, config & secrets",
    ));
    s
}

/// Arguments accepted by `naque`.
#[derive(clap::Parser, Debug)]
#[command(
    name = "naque",
    about = "Agentic AI query tool over databases",
    long_about = LONG_ABOUT,
    styles = STYLES,
    after_long_help = after_long_help()
)]
pub struct Args {
    /// Profile name to launch (overrides naque.toml `project` / central default).
    pub profile: Option<String>,

    /// Explicit connection string (overrides profile resolution).
    #[arg(long)]
    pub url: Option<String>,

    /// Permission mode: strict | default | readonly | wildcard.
    #[arg(long)]
    pub mode: Option<String>,

    /// Disable the always-on catastrophic guard (--yolo).
    #[arg(long = "no-guard")]
    pub no_guard: bool,

    /// Force no color output.
    #[arg(long = "no-color")]
    pub no_color: bool,

    /// AI provider override (claude | openai | gemini | hf | ollama).
    #[arg(long)]
    pub provider: Option<String>,

    /// Model name override (e.g. "claude-opus-4-8", "zai-org/GLM-5.2").
    #[arg(long)]
    pub model: Option<String>,
}

impl Args {
    /// True when no connection-related argument was supplied.
    ///
    /// Used to decide whether a missing connection should be presented as
    /// friendly first-run guidance (bare launch) or as an error.
    pub fn is_bare(&self) -> bool {
        self.profile.is_none()
            && self.url.is_none()
            && self.mode.is_none()
            && self.provider.is_none()
            && self.model.is_none()
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn full_args_parse_correctly() {
        let args = Args::try_parse_from(["naque", "prod", "--mode", "readonly", "--no-guard", "--no-color"])
            .expect("parse failed");
        assert_eq!(args.profile.as_deref(), Some("prod"));
        assert_eq!(args.mode.as_deref(), Some("readonly"));
        assert!(args.no_guard);
        assert!(args.no_color);
        assert!(args.url.is_none());
    }

    #[test]
    fn empty_args_all_none_or_false() {
        let args = Args::try_parse_from(["naque"]).expect("parse failed");
        assert!(args.profile.is_none());
        assert!(args.url.is_none());
        assert!(args.mode.is_none());
        assert!(!args.no_guard);
        assert!(!args.no_color);
    }

    #[test]
    fn url_arg_parsed() {
        let args = Args::try_parse_from(["naque", "--url", "postgres://localhost/mydb"]).unwrap();
        assert_eq!(args.url.as_deref(), Some("postgres://localhost/mydb"));
    }

    #[test]
    fn bare_when_no_connection_args() {
        assert!(Args::try_parse_from(["naque"]).unwrap().is_bare());
        // Cosmetic flags don't count as connection intent.
        assert!(Args::try_parse_from(["naque", "--no-color"]).unwrap().is_bare());
    }

    #[test]
    fn not_bare_when_connection_args_present() {
        assert!(!Args::try_parse_from(["naque", "prod"]).unwrap().is_bare());
        assert!(!Args::try_parse_from(["naque", "--url", "postgres://x/y"]).unwrap().is_bare());
        assert!(!Args::try_parse_from(["naque", "--provider", "hf"]).unwrap().is_bare());
    }

    #[test]
    fn long_help_shows_examples_and_precedence() {
        use clap::CommandFactory;
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("EXAMPLES"));
        assert!(help.contains("PRECEDENCE"));
        assert!(help.contains("DATABASE_URL"));
    }

    #[test]
    fn after_long_help_text_plain_has_no_ansi() {
        let t = after_long_help_text(false);
        assert!(t.contains("EXAMPLES:"));
        assert!(t.contains("CONNECTION PRECEDENCE"));
        assert!(t.contains("CONFIG FILES:"));
        assert!(t.contains("DATABASE_URL"));
        assert!(!t.contains('\x1b'));
    }

    #[test]
    fn after_long_help_text_colors_section_headers() {
        let t = after_long_help_text(true);
        assert!(t.contains("\x1b[1;36mEXAMPLES:"));
        assert!(t.contains("\x1b[1;36mCONFIG FILES:"));
    }

    #[test]
    fn provider_and_model_args_parsed() {
        let args = Args::try_parse_from(["naque", "--provider", "hf", "--model", "zai-org/GLM-5.2:together"])
            .expect("parse failed");
        assert_eq!(args.provider.as_deref(), Some("hf"));
        assert_eq!(args.model.as_deref(), Some("zai-org/GLM-5.2:together"));
    }
}
