//! The four permission modes that gate query execution.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// Human approves every query, including the agent's introspection.
    Strict,
    /// Introspection runs freely; primary queries are gated.
    #[default]
    Default,
    /// Reads auto-approved (under DB-level read-only); writes prompt.
    ReadOnly,
    /// Everything auto-approved (catastrophic guard still fires).
    Wildcard,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown permission mode: {0} (expected strict|default|readonly|wildcard)")]
pub struct ParsePermissionModeError(pub String);

impl FromStr for PermissionMode {
    type Err = ParsePermissionModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(Self::Strict),
            "default" => Ok(Self::Default),
            "readonly" | "read-only" | "ro" => Ok(Self::ReadOnly),
            "wildcard" | "wild" => Ok(Self::Wildcard),
            other => Err(ParsePermissionModeError(other.to_string())),
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Strict => "strict",
            Self::Default => "default",
            Self::ReadOnly => "readonly",
            Self::Wildcard => "wildcard",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_and_aliases() {
        assert_eq!("strict".parse(), Ok(PermissionMode::Strict));
        assert_eq!("DEFAULT".parse(), Ok(PermissionMode::Default));
        assert_eq!("read-only".parse(), Ok(PermissionMode::ReadOnly));
        assert_eq!("wild".parse(), Ok(PermissionMode::Wildcard));
    }

    #[test]
    fn rejects_unknown() {
        let err = "nope".parse::<PermissionMode>().unwrap_err();
        assert_eq!(err, ParsePermissionModeError("nope".to_string()));
    }

    #[test]
    fn display_roundtrips_to_parse() {
        for m in [
            PermissionMode::Strict,
            PermissionMode::Default,
            PermissionMode::ReadOnly,
            PermissionMode::Wildcard,
        ] {
            assert_eq!(m.to_string().parse(), Ok(m));
        }
    }

    #[test]
    fn default_is_default_mode() {
        assert_eq!(PermissionMode::default(), PermissionMode::Default);
    }
}
