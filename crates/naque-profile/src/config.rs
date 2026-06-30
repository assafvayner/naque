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

    /// Glob patterns the agent is allowed to read from the filesystem
    /// (`read_file` / `list_directory`). Unlike the scalar fields, these are
    /// **unioned** across config layers rather than overridden — a global
    /// `~/.naque/config.toml` grant and a per-profile grant accumulate, so a
    /// path is readable if any layer allows it.
    pub read_paths: Option<Vec<String>>,

    /// Whether the agent may use the web (`web_fetch`). Defaults to enabled
    /// when unset (see [`NaqueConfig::web_access_enabled`]).
    pub web_access: Option<bool>,
}

impl NaqueConfig {
    /// Merge two configs: fields set in `other` win over `self`.
    /// A `None` in `other` does **not** clobber a value in `self`.
    ///
    /// `read_paths` is the exception — it is **unioned** (both layers' globs
    /// are kept, in order, deduplicated) rather than overridden.
    pub fn merge(self, other: NaqueConfig) -> NaqueConfig {
        NaqueConfig {
            mode: other.mode.or(self.mode),
            provider: other.provider.or(self.provider),
            model: other.model.or(self.model),
            row_cap: other.row_cap.or(self.row_cap),
            max_iterations: other.max_iterations.or(self.max_iterations),
            output_format: other.output_format.or(self.output_format),
            read_paths: union_read_paths(self.read_paths, other.read_paths),
            web_access: other.web_access.or(self.web_access),
        }
    }

    /// Resolved web-access flag: enabled unless explicitly set to `false`.
    pub fn web_access_enabled(&self) -> bool {
        self.web_access.unwrap_or(true)
    }
}

/// Union two optional glob lists, preserving order and dropping duplicates.
/// Returns `None` only when both inputs are `None`.
fn union_read_paths(base: Option<Vec<String>>, other: Option<Vec<String>>) -> Option<Vec<String>> {
    match (base, other) {
        (None, None) => None,
        (a, b) => {
            let mut out: Vec<String> = Vec::new();
            for p in a.into_iter().flatten().chain(b.into_iter().flatten()) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            Some(out)
        },
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

    #[test]
    fn read_paths_union_accumulates_across_layers() {
        let base = NaqueConfig {
            read_paths: Some(vec!["~/sql/**".into(), "shared/**".into()]),
            ..Default::default()
        };
        let override_cfg = NaqueConfig {
            read_paths: Some(vec!["shared/**".into(), "./docs/**".into()]),
            ..Default::default()
        };
        let merged = base.merge(override_cfg);
        // Union, in order, deduplicated (not overridden).
        assert_eq!(merged.read_paths, Some(vec!["~/sql/**".into(), "shared/**".into(), "./docs/**".into()]));
    }

    #[test]
    fn read_paths_none_in_both_stays_none() {
        let merged = NaqueConfig::default().merge(NaqueConfig::default());
        assert_eq!(merged.read_paths, None);
    }

    #[test]
    fn web_access_defaults_enabled_and_override_wins() {
        assert!(NaqueConfig::default().web_access_enabled());
        let base = NaqueConfig {
            web_access: Some(true),
            ..Default::default()
        };
        let off = NaqueConfig {
            web_access: Some(false),
            ..Default::default()
        };
        assert!(!base.merge(off).web_access_enabled()); // override disables
    }
}
