//! The per-profile directory model: a `Profile` (project/schema) owning many
//! named connection `environments`, plus `Store` I/O for `~/.naque/profiles/`.

use std::collections::BTreeMap;

use crate::{ConfigError, ConnectionSpec, NaqueConfig, Store, url_conn_id};

impl Store {
    /// Names of all profiles (subdirectories of `profiles/` containing a
    /// `profile.toml`), sorted. Missing `profiles/` dir → empty list.
    pub fn list_profiles(&self) -> Result<Vec<String>, ConfigError> {
        let dir = self.profiles_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ConfigError::io(e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(ConfigError::io)?;
            if entry.path().join("profile.toml").is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Load `<home>/profiles/<name>/profile.toml`. `Ok(None)` if absent.
    pub fn load_profile(&self, name: &str) -> Result<Option<Profile>, ConfigError> {
        let path = self.profile_dir(name).join("profile.toml");
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ConfigError::io(e)),
        };
        let profile = toml::from_str(&raw).map_err(|e| ConfigError::parse(path.display().to_string(), e))?;
        Ok(Some(profile))
    }

    /// Write `<home>/profiles/<name>/profile.toml`, creating the dir.
    pub fn save_profile(&self, name: &str, profile: &Profile) -> Result<(), ConfigError> {
        let dir = self.profile_dir(name);
        std::fs::create_dir_all(&dir).map_err(ConfigError::io)?;
        let toml_str = toml::to_string_pretty(profile).map_err(|e| ConfigError::Other(e.to_string()))?;
        std::fs::write(dir.join("profile.toml"), toml_str).map_err(ConfigError::io)?;
        Ok(())
    }
}

/// Find a saved directory profile + environment whose connection structurally
/// matches `url` (engine+host+port+dbname+user, password ignored). Scans
/// profiles and their environments in deterministic (sorted) order; returns the
/// first match as `(profile_name, env_name)`, or `None`.
pub fn match_profile_by_url(store: &Store, url: &str) -> Option<(String, String)> {
    let target = url_conn_id(url)?;
    for name in store.list_profiles().unwrap_or_default() {
        let Ok(Some(profile)) = store.load_profile(&name) else {
            continue;
        };
        for (env_name, spec) in &profile.environments {
            if spec.conn_id().as_ref() == Some(&target) {
                return Some((name, env_name.clone()));
            }
        }
    }
    None
}

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
    use crate::Store;

    #[test]
    fn save_then_load_profile_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path());
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "dev".to_string(),
            ConnectionSpec {
                path: Some("/tmp/x.db".into()),
                engine: Some(crate::ProfileEngine::Sqlite),
                ..Default::default()
            },
        );
        let profile = Profile {
            default_environment: Some("dev".into()),
            config: NaqueConfig::default(),
            environments: env,
        };
        store.save_profile("shop", &profile).unwrap();
        let loaded = store.load_profile("shop").unwrap().unwrap();
        assert_eq!(loaded, profile);
    }

    #[test]
    fn list_profiles_returns_sorted_names() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path());
        store.save_profile("zeta", &Profile::default()).unwrap();
        store.save_profile("alpha", &Profile::default()).unwrap();
        assert_eq!(store.list_profiles().unwrap(), vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[test]
    fn load_missing_profile_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path());
        assert!(store.load_profile("nope").unwrap().is_none());
    }

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

    #[test]
    fn match_profile_by_url_matches_ignoring_password_and_default_port() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path());
        let mut env = BTreeMap::new();
        env.insert(
            "prod".to_string(),
            ConnectionSpec {
                engine: Some(crate::ProfileEngine::Postgres),
                host: Some("db.host".into()),
                dbname: Some("shop".into()),
                user: Some("analyst".into()),
                ..Default::default()
            },
        );
        let profile = Profile {
            environments: env,
            ..Default::default()
        };
        store.save_profile("shop", &profile).unwrap();

        // Password and an explicit default port don't prevent the match.
        assert_eq!(
            match_profile_by_url(&store, "postgres://analyst:somepw@db.host:5432/shop"),
            Some(("shop".to_string(), "prod".to_string()))
        );
        // Different database → no match.
        assert!(match_profile_by_url(&store, "postgres://analyst@db.host/other").is_none());
    }
}
