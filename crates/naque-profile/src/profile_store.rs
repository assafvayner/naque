//! The per-profile directory model: a `Profile` (project/schema) owning many
//! named connection `environments`, plus `Store` I/O for `~/.naque/profiles/`.

use std::collections::BTreeMap;

use crate::{ConnectionSpec, NaqueConfig};

/// A project/schema profile: shared schema + context, with one or more named
/// connection environments (e.g. `prod`, `dev`). The profile name is the
/// directory name, not stored inside the struct.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    /// Default environment to use when none is specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_environment: Option<String>,

    /// Per-profile config overrides (provider/model/mode/row_cap/...).
    #[serde(default)]
    pub config: NaqueConfig,

    /// Named connection environments.
    #[serde(default)]
    pub environments: BTreeMap<String, ConnectionSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_toml_roundtrip() {
        let toml_str = r#"
default_environment = "dev"

[config]
mode = "readonly"

[environments.prod]
engine = "postgres"
host = "prod.db"
user = "analyst"
password_env = "PROD_PW"

[environments.dev]
engine = "sqlite"
path = "/tmp/dev.db"
"#;
        let p: Profile = toml::from_str(toml_str).unwrap();
        assert_eq!(p.default_environment.as_deref(), Some("dev"));
        assert_eq!(p.config.mode.as_deref(), Some("readonly"));
        assert_eq!(p.environments.len(), 2);
        assert_eq!(p.environments["prod"].password_env.as_deref(), Some("PROD_PW"));
        let back: Profile = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }
}
