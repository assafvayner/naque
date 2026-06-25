//! Profile types: connection specs and per-profile bodies.

use std::collections::BTreeMap;

/// Which database engine a profile targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileEngine {
    Postgres,
    Sqlite,
}

/// How to connect to a database. A full `url` wins over component fields;
/// otherwise the connection string is assembled from `host`, `port`, `dbname`,
/// `user`, and a password source at use-time.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConnectionSpec {
    /// Complete connection URL (e.g. `postgres://user:pass@host/db`). When
    /// present, all other fields are ignored.
    pub url: Option<String>,

    /// Database engine. Inferred from `url` scheme at use-site when absent.
    pub engine: Option<ProfileEngine>,

    /// Hostname or IP address.
    pub host: Option<String>,

    /// TCP port.
    pub port: Option<u16>,

    /// Database (or catalog) name.
    pub dbname: Option<String>,

    /// Database user.
    pub user: Option<String>,

    /// Name of an environment variable that holds the password. Resolved at
    /// connection time — never stored in plaintext.
    pub password_env: Option<String>,

    /// Keyring account name from which to fetch the password. Resolved at
    /// connection time via the system keyring.
    pub password_keyring: Option<String>,

    /// File-system path to a SQLite database file.
    pub path: Option<String>,

    /// Additional driver-specific query parameters (e.g. `sslmode=require`).
    pub params: Option<BTreeMap<String, String>>,
}

/// The body of a `[profiles.<name>]` table. The profile name itself is the
/// map key in `NaqueFile::profiles`; it does not appear inside this struct.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProfileBody {
    /// Connection details. Flattened so connection fields appear at the same
    /// TOML level as `docs` (no `[profiles.name.connection]` nesting).
    #[serde(flatten)]
    pub connection: ConnectionSpec,

    /// Optional inline documentation / notes for this profile.
    pub docs: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_engine_serde_postgres() {
        let toml_str = r#"engine = "postgres""#;
        let spec: ConnectionSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.engine, Some(ProfileEngine::Postgres));
    }

    #[test]
    fn profile_engine_serde_sqlite() {
        let toml_str = r#"engine = "sqlite""#;
        let spec: ConnectionSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.engine, Some(ProfileEngine::Sqlite));
    }

    #[test]
    fn profile_engine_roundtrip() {
        let spec = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("localhost".into()),
            ..Default::default()
        };
        let serialized = toml::to_string(&spec).unwrap();
        let back: ConnectionSpec = toml::from_str(&serialized).unwrap();
        assert_eq!(back, spec);
    }
}
