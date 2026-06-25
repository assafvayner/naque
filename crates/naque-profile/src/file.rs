//! `NaqueFile`: unified parse type for all three naque config files.

use std::collections::BTreeMap;

use crate::{NaqueConfig, ProfileBody};

/// One parse type for all three files:
/// - `~/.naque/config.toml` (global settings + `default_profile`)
/// - `~/.naque/profiles.toml` (named connection profiles)
/// - `./naque.toml` (project-local override: active `project` profile)
///
/// Fields absent in a file simply deserialize as `None`, so the same type
/// is safe to use regardless of which file is being read.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NaqueFile {
    /// `naque.toml` only: the name of the profile active in this directory.
    pub project: Option<String>,

    /// Central `config.toml`: the profile to use when no project override
    /// or CLI flag names one.
    pub default_profile: Option<String>,

    /// `[config]` table: global/overridable settings.
    pub config: Option<NaqueConfig>,

    /// `[profiles.<name>]` tables: named connection profiles.
    pub profiles: Option<BTreeMap<String, ProfileBody>>,
}

impl NaqueFile {
    /// Merge two `NaqueFile`s: `other` fields win when `Some`; `None` in
    /// `other` does not clobber `self`. Config sections are deep-merged via
    /// `NaqueConfig::merge`.
    pub fn merge(self, other: NaqueFile) -> NaqueFile {
        let config = match (self.config, other.config) {
            (Some(base), Some(over)) => Some(base.merge(over)),
            (Some(base), None) => Some(base),
            (None, over) => over,
        };
        let profiles = match (self.profiles, other.profiles) {
            (Some(mut base), Some(over)) => {
                base.extend(over);
                Some(base)
            }
            (Some(base), None) => Some(base),
            (None, over) => over,
        };
        NaqueFile {
            project: other.project.or(self.project),
            default_profile: other.default_profile.or(self.default_profile),
            config,
            profiles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConnectionSpec, ProfileEngine};

    const REPRESENTATIVE_TOML: &str = r#"
project = "acme"

[config]
mode = "readonly"
row_cap = 500

[profiles.prod]
engine = "postgres"
host = "db.example.com"
user = "analyst"
password_env = "PROD_DB_PASSWORD"
docs = ["Production database — read-only analyst role.", "VPN required."]

[profiles.local]
engine = "sqlite"
path = "/tmp/local.db"
password_keyring = "naque-local"
"#;

    #[test]
    fn parse_representative_naque_toml() {
        let file: NaqueFile = toml::from_str(REPRESENTATIVE_TOML).unwrap();

        // top-level project
        assert_eq!(file.project.as_deref(), Some("acme"));

        // [config]
        let cfg = file.config.as_ref().unwrap();
        assert_eq!(cfg.mode.as_deref(), Some("readonly"));
        assert_eq!(cfg.row_cap, Some(500));

        // profiles map
        let profiles = file.profiles.as_ref().unwrap();
        assert_eq!(profiles.len(), 2);

        // prod profile
        let prod = profiles.get("prod").unwrap();
        assert_eq!(prod.connection.engine, Some(ProfileEngine::Postgres));
        assert_eq!(prod.connection.host.as_deref(), Some("db.example.com"));
        assert_eq!(prod.connection.user.as_deref(), Some("analyst"));
        assert_eq!(
            prod.connection.password_env.as_deref(),
            Some("PROD_DB_PASSWORD")
        );
        let docs = prod.docs.as_ref().unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0], "Production database — read-only analyst role.");
        assert_eq!(docs[1], "VPN required.");

        // local profile
        let local = profiles.get("local").unwrap();
        assert_eq!(local.connection.engine, Some(ProfileEngine::Sqlite));
        assert_eq!(local.connection.path.as_deref(), Some("/tmp/local.db"));
        assert_eq!(
            local.connection.password_keyring.as_deref(),
            Some("naque-local")
        );
    }

    #[test]
    fn merge_combines_disjoint_files() {
        let central = NaqueFile {
            default_profile: Some("prod".into()),
            config: Some(NaqueConfig {
                mode: Some("default".into()),
                row_cap: Some(100),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profiles_file = NaqueFile {
            profiles: Some({
                let mut m = BTreeMap::new();
                m.insert(
                    "prod".into(),
                    ProfileBody {
                        connection: ConnectionSpec {
                            engine: Some(ProfileEngine::Postgres),
                            host: Some("db.prod".into()),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
                m
            }),
            ..Default::default()
        };

        let merged = central.merge(profiles_file);
        assert_eq!(merged.default_profile.as_deref(), Some("prod"));
        assert_eq!(merged.config.as_ref().unwrap().row_cap, Some(100));
        assert!(merged.profiles.as_ref().unwrap().contains_key("prod"));
    }
}
