//! Registry of slash commands, shared by `/help` and the autocomplete popup.
//!
//! Keeping the command list in one place means the help text and the
//! suggestion popup never drift from each other. The actual command behavior
//! lives in the app layer; this module only describes the commands.

/// A user-facing slash command (entered with a leading `/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommand {
    /// Command word without the leading slash, e.g. `"mode"`.
    pub name: &'static str,
    /// Argument hint shown after the name, e.g. `"<mode>"`; empty if none.
    pub args: &'static str,
    /// One-line description shown in help and the suggestion popup.
    pub help: &'static str,
}

impl SlashCommand {
    /// The command head as displayed: `/name` or `/name <args>`.
    pub fn head(&self) -> String {
        if self.args.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.args)
        }
    }
}

/// All slash commands, in display order.
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        args: "",
        help: "show this help",
    },
    SlashCommand {
        name: "mode",
        args: "<mode>",
        help: "set permission mode: default | readonly | strict | wildcard",
    },
    SlashCommand {
        name: "learn",
        args: "",
        help: "introspect the database schema",
    },
    SlashCommand {
        name: "clear",
        args: "",
        help: "clear the chat window and the agent's conversation memory",
    },
    SlashCommand {
        name: "cost",
        args: "",
        help: "show token usage so far",
    },
    SlashCommand {
        name: "export",
        args: "<csv|json>",
        help: "export the last result",
    },
    SlashCommand {
        name: "profile",
        args: "",
        help: "switch profile + environment (picker)",
    },
    SlashCommand {
        name: "env",
        args: "",
        help: "switch environment within the current profile",
    },
    SlashCommand {
        name: "save",
        args: "[profile] [env]",
        help: "save current connection, schema & context",
    },
    SlashCommand {
        name: "context",
        args: "[note]",
        help: "show the context doc, or append a note",
    },
    SlashCommand {
        name: "quit",
        args: "",
        help: "exit naque",
    },
    SlashCommand {
        name: "exit",
        args: "",
        help: "exit naque",
    },
];

/// Commands whose name starts with `prefix` (case-insensitive), in registry order.
///
/// `prefix` is the text the user has typed after the leading `/` (no slash).
pub fn matching(prefix: &str) -> Vec<&'static SlashCommand> {
    let prefix = prefix.to_ascii_lowercase();
    SLASH_COMMANDS.iter().filter(|c| c.name.starts_with(&prefix)).collect()
}

/// Render the full help text shown by `/help`.
pub fn help_text() -> String {
    let mut out = String::from(
        "naque — ask your database in natural language\n\
         \n\
         Input:\n\
         \u{20} text…      natural-language question (the agent writes & runs SQL)\n\
         \u{20} !<sql>     run raw SQL directly\n\
         \u{20} \\<cmd>     database command: \\dt, \\d <table>, \\reset\n\
         \u{20} /<cmd>     slash command (below)\n\
         \n\
         Slash commands:\n",
    );
    let width = SLASH_COMMANDS.iter().map(|c| c.head().len()).max().unwrap_or(0);
    for c in SLASH_COMMANDS {
        out.push_str(&format!("  {:<width$}  {}\n", c.head(), c.help, width = width));
    }
    out.push_str(
        "\n\
         Keys:\n\
         \u{20} ←/→ move cursor   Home/End or Ctrl+A/Ctrl+E line start/end   Tab complete\n\
         \u{20} PgUp/PgDn scroll  Ctrl+End back to input   Ctrl+C cancel / quit\n\
         \u{20} Ctrl+↑/↓ select tool step   Ctrl+R expand/collapse the SQL\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_empty_prefix_returns_all() {
        assert_eq!(matching("").len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn matching_filters_by_prefix() {
        let names: Vec<&str> = matching("c").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["clear", "cost", "context"]);
    }

    #[test]
    fn registry_includes_profile_commands() {
        let names: Vec<&str> = SLASH_COMMANDS.iter().map(|c| c.name).collect();
        for expected in ["profile", "env", "save", "context"] {
            assert!(names.contains(&expected), "registry missing /{expected}");
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        let names: Vec<&str> = matching("MO").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["mode"]);
    }

    #[test]
    fn matching_unknown_prefix_is_empty() {
        assert!(matching("zzz").is_empty());
    }

    #[test]
    fn head_formats_args() {
        let mode = SLASH_COMMANDS.iter().find(|c| c.name == "mode").unwrap();
        assert_eq!(mode.head(), "/mode <mode>");
        let help = SLASH_COMMANDS.iter().find(|c| c.name == "help").unwrap();
        assert_eq!(help.head(), "/help");
    }

    #[test]
    fn help_text_lists_every_command() {
        let text = help_text();
        for c in SLASH_COMMANDS {
            assert!(text.contains(&c.head()), "help should list {}", c.head());
        }
        assert!(text.contains("Slash commands:"));
    }

    #[test]
    fn help_text_documents_step_keys() {
        let help = help_text();
        assert!(help.contains("select tool step"), "help must document step navigation:\n{help}");
        assert!(help.contains("expand/collapse"), "help must document the expand toggle:\n{help}");
    }
}
