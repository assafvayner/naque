//! Grammar router: classify raw input lines into their interaction class.

/// The four input classes at the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Bare text — a natural-language request.
    NaturalLanguage(String),
    /// `!`-prefixed raw SQL (prefix stripped, trimmed).
    RawSql(String),
    /// `\`-prefixed live-DB / psql-style command (prefix stripped, trimmed).
    DbCommand(String),
    /// `/`-prefixed naque tool command (prefix stripped, trimmed).
    ToolCommand(String),
    /// Empty / whitespace-only input.
    Empty,
}

/// Route a raw input line to its class. Leading whitespace is trimmed before
/// checking the prefix; the remainder after the prefix is also trimmed.
pub fn route_input(line: &str) -> Input {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Input::Empty;
    }
    let mut chars = trimmed.chars();
    match chars.next() {
        Some('!') => Input::RawSql(chars.as_str().trim().to_string()),
        Some('\\') => Input::DbCommand(chars.as_str().trim().to_string()),
        Some('/') => Input::ToolCommand(chars.as_str().trim().to_string()),
        _ => Input::NaturalLanguage(trimmed.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_empty() {
        assert_eq!(route_input(""), Input::Empty);
    }

    #[test]
    fn whitespace_only_is_empty() {
        assert_eq!(route_input("   \t  "), Input::Empty);
    }

    #[test]
    fn raw_sql_prefix() {
        assert_eq!(route_input("!SELECT 1"), Input::RawSql("SELECT 1".to_string()));
    }

    #[test]
    fn raw_sql_with_surrounding_spaces() {
        assert_eq!(route_input("  !  SELECT 1  "), Input::RawSql("SELECT 1".to_string()));
    }

    #[test]
    fn db_command_prefix() {
        assert_eq!(route_input("\\dt"), Input::DbCommand("dt".to_string()));
    }

    #[test]
    fn db_command_with_surrounding_spaces() {
        assert_eq!(route_input("  \\d users  "), Input::DbCommand("d users".to_string()));
    }

    #[test]
    fn tool_command_prefix() {
        assert_eq!(route_input("/help"), Input::ToolCommand("help".to_string()));
    }

    #[test]
    fn natural_language_line() {
        assert_eq!(route_input("show me all users"), Input::NaturalLanguage("show me all users".to_string()));
    }

    #[test]
    fn line_containing_prefix_char_but_not_starting_with_it_is_nl() {
        // The '!' is not the first char — should be NaturalLanguage.
        assert_eq!(route_input("hello! world"), Input::NaturalLanguage("hello! world".to_string()));
    }

    #[test]
    fn line_containing_slash_not_leading_is_nl() {
        assert_eq!(route_input("path/to/something"), Input::NaturalLanguage("path/to/something".to_string()));
    }

    #[test]
    fn line_containing_backslash_not_leading_is_nl() {
        assert_eq!(route_input("C:\\Users\\foo"), Input::NaturalLanguage("C:\\Users\\foo".to_string()));
    }

    #[test]
    fn prefix_only_yields_empty_payload() {
        assert_eq!(route_input("!"), Input::RawSql("".to_string()));
        assert_eq!(route_input("\\"), Input::DbCommand("".to_string()));
        assert_eq!(route_input("/"), Input::ToolCommand("".to_string()));
    }

    #[test]
    fn leading_whitespace_then_prefix() {
        assert_eq!(route_input("   /status"), Input::ToolCommand("status".to_string()));
    }
}
