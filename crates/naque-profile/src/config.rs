//! Global, overridable naque settings.

/// Global settings that can be set in `~/.naque/config.toml` or overridden
/// per project in `./naque.toml`. All fields are `Option` so `merge()` can
/// layer a higher-priority config on top of a lower-priority one without
/// clobbering fields that are absent in the override.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NaqueConfig {
    /// The permission mode (parsed to `PermissionMode` at use-site; stored as
    /// a plain string here so the config layer stays dependency-light and
    /// validation can be deferred to the caller).
    pub mode: Option<String>,

    /// AI provider identifier (e.g. "anthropic", "openai").
    pub provider: Option<String>,

    /// Model name or identifier.
    pub model: Option<String>,

    /// Maximum number of rows returned per query.
    pub row_cap: Option<u64>,

    /// Maximum agent iterations before giving up.
    pub max_iterations: Option<u32>,

    /// Output format: "table" | "csv" | "json".
    pub output_format: Option<String>,
}

impl NaqueConfig {
    /// Merge two configs: fields set in `other` win over `self`.
    /// A `None` in `other` does **not** clobber a value in `self`.
    pub fn merge(self, other: NaqueConfig) -> NaqueConfig {
        NaqueConfig {
            mode: other.mode.or(self.mode),
            provider: other.provider.or(self.provider),
            model: other.model.or(self.model),
            row_cap: other.row_cap.or(self.row_cap),
            max_iterations: other.max_iterations.or(self.max_iterations),
            output_format: other.output_format.or(self.output_format),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_other_wins_and_none_does_not_clobber() {
        let base = NaqueConfig {
            mode: Some("default".into()),
            row_cap: Some(100),
            ..Default::default()
        };
        let override_cfg = NaqueConfig {
            mode: Some("strict".into()),
            model: Some("claude-opus-4".into()),
            ..Default::default()
        };
        let merged = base.merge(override_cfg);
        assert_eq!(merged.mode.as_deref(), Some("strict")); // override wins
        assert_eq!(merged.model.as_deref(), Some("claude-opus-4")); // override provides
        assert_eq!(merged.row_cap, Some(100)); // base preserved
        assert_eq!(merged.provider, None); // absent in both
    }
}
