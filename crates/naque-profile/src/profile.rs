//! Profile types: connection specs and per-profile bodies.

use std::collections::BTreeMap;

use crate::NaqueConfig;

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

    /// Plaintext password, used at the user's own risk. Lowest-priority password
    /// source (keyring and env var win). Never written by `/save`; only honored
    /// if the user puts it here themselves.
    pub password: Option<String>,

    /// File-system path to a SQLite database file.
    pub path: Option<String>,

    /// Additional driver-specific query parameters (e.g. `sslmode=require`).
    pub params: Option<BTreeMap<String, String>>,
}

/// Structural identity of a database connection, used to recognize that two
/// connection strings/specs point at the same database. The password is
/// deliberately excluded — identity is engine + location + database + user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnId {
    Postgres {
        host: String,
        port: u16,
        dbname: String,
        user: String,
    },
    Sqlite {
        path: String,
    },
}

/// Parse a connection URL into a [`ConnId`], or `None` if it can't be parsed
/// into a recognizable identity. The password (if any) is intentionally
/// ignored — identity is engine + host + port + database + user.
///
/// Note: `url::Url::host_str()` returns IPv6 hosts in their bracketed form
/// (e.g. `[::1]`), so a hand-authored component profile with a bracketless
/// IPv6 `host` (`"::1"`) will not match a URL-derived id (`"[::1]"`).
pub fn url_conn_id(url: &str) -> Option<ConnId> {
    let scheme = url.split(':').next()?;
    match scheme {
        "postgres" | "postgresql" => {
            let parsed = url::Url::parse(url).ok()?;
            let host = parsed.host_str()?;
            if host.is_empty() {
                return None;
            }
            let dbname = parsed.path().trim_start_matches('/');
            if dbname.is_empty() {
                return None;
            }
            let user = parsed.username();
            if user.is_empty() {
                return None;
            }
            let user = urlencoding::decode(user).ok()?.into_owned();
            Some(ConnId::Postgres {
                host: host.to_ascii_lowercase(),
                port: parsed.port().unwrap_or(5432),
                dbname: dbname.to_string(),
                user,
            })
        },
        "sqlite" => {
            // Path is normalized for IDENTITY-MATCHING ONLY (lossy: the leading
            // slash / absoluteness is not preserved); never used to open a
            // connection.
            let path = url.strip_prefix("sqlite:")?.trim_start_matches('/');
            Some(ConnId::Sqlite { path: path.to_string() })
        },
        _ => None,
    }
}

impl ConnectionSpec {
    /// Structural identity of this spec, used to recognize that it points at the
    /// same database as a given connection URL. The password is excluded.
    pub fn conn_id(&self) -> Option<ConnId> {
        if let Some(u) = &self.url {
            return url_conn_id(u);
        }
        match self.engine {
            Some(ProfileEngine::Postgres) | None => {
                let host = self.host.as_deref()?;
                let dbname = self.dbname.clone()?;
                let user = self.user.clone()?;
                Some(ConnId::Postgres {
                    host: host.to_ascii_lowercase(),
                    port: self.port.unwrap_or(5432),
                    dbname,
                    user,
                })
            },
            Some(ProfileEngine::Sqlite) => self.path.clone().map(|path| ConnId::Sqlite { path }),
        }
    }

    /// Parse a connection URL into a component `ConnectionSpec`, capturing host /
    /// port / dbname / user (and any inline password — which callers must treat
    /// as a secret and which `/save` strips). Returns `None` if the URL can't be
    /// parsed into a usable spec. The captured spec's identity (`conn_id`) equals
    /// `url_conn_id(url)`.
    ///
    /// URL query parameters are NOT captured: a `?password=...`-style param in
    /// the URL is dropped on capture (not persisted, not leaked, just not
    /// carried into the saved spec). The sqlite path is normalized as in
    /// [`url_conn_id`] (lossy — identity-matching only).
    ///
    /// # Security
    ///
    /// The returned spec's `password` field may hold a cleartext password lifted
    /// from the URL. Treat it as a secret: never log it, and rely on `/save` to
    /// strip it before persisting.
    pub fn from_url(url: &str) -> Option<ConnectionSpec> {
        let scheme = url.split(':').next()?;
        match scheme {
            "postgres" | "postgresql" => {
                let parsed = url::Url::parse(url).ok()?;
                let host = parsed.host_str()?;
                if host.is_empty() {
                    return None;
                }
                let dbname = parsed.path().trim_start_matches('/');
                if dbname.is_empty() {
                    return None;
                }
                let user = parsed.username();
                if user.is_empty() {
                    return None;
                }
                let user = urlencoding::decode(user).ok()?.into_owned();
                let password = parsed
                    .password()
                    .and_then(|p| urlencoding::decode(p).ok().map(|c| c.into_owned()));
                Some(ConnectionSpec {
                    engine: Some(ProfileEngine::Postgres),
                    host: Some(host.to_string()),
                    port: parsed.port(),
                    dbname: Some(dbname.to_string()),
                    user: Some(user),
                    password,
                    ..Default::default()
                })
            },
            "sqlite" => {
                let path = url.strip_prefix("sqlite:")?.trim_start_matches('/');
                Some(ConnectionSpec {
                    engine: Some(ProfileEngine::Sqlite),
                    path: Some(path.to_string()),
                    ..Default::default()
                })
            },
            _ => None,
        }
    }
}

/// The body of a `[profiles.<name>]` table. The profile name itself is the
/// map key in `NaqueFile::profiles`; it does not appear inside this struct.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProfileBody {
    /// Connection details. Flattened so connection fields appear at the same
    /// TOML level as `docs` (no `[profiles.name.connection]` nesting).
    #[serde(flatten)]
    pub connection: ConnectionSpec,

    /// Per-profile settings overriding the global/local `[config]`. Flattened
    /// so keys like `model` and `provider` sit at the profile's top level
    /// (e.g. `model = "claude-opus-4-8"` directly under `[profiles.prod]`).
    /// During resolution these win over central and local config but lose to
    /// CLI overrides.
    #[serde(flatten)]
    pub config: NaqueConfig,

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

    #[test]
    fn connection_spec_accepts_inline_password() {
        let toml_str = r#"
engine = "postgres"
host = "h"
user = "u"
password = "secret"
"#;
        let spec: ConnectionSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.password.as_deref(), Some("secret"));
    }

    #[test]
    fn profile_body_parses_connection_and_inline_config() {
        // Connection fields and per-profile config keys coexist at the top
        // level of the profile body (both flattened).
        let toml_str = r#"
engine = "postgres"
host = "db.example.com"
port = 6543
user = "analyst"
provider = "claude"
model = "claude-opus-4-8"
mode = "readonly"
docs = ["prod read replica"]
"#;
        let body: ProfileBody = toml::from_str(toml_str).unwrap();
        assert_eq!(body.connection.engine, Some(ProfileEngine::Postgres));
        assert_eq!(body.connection.host.as_deref(), Some("db.example.com"));
        assert_eq!(body.connection.port, Some(6543));
        assert_eq!(body.connection.user.as_deref(), Some("analyst"));
        assert_eq!(body.config.provider.as_deref(), Some("claude"));
        assert_eq!(body.config.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(body.config.mode.as_deref(), Some("readonly"));
        assert_eq!(body.docs.as_deref(), Some(&["prod read replica".to_string()][..]));
    }

    #[test]
    fn url_conn_id_postgres_default_port_and_normalization() {
        let a = url_conn_id("postgres://u@h/db").unwrap();
        let b = url_conn_id("postgres://u:pw@h:5432/db").unwrap();
        // Default port normalizes to 5432, and the password is ignored.
        assert_eq!(a, b);
        assert_eq!(
            a,
            ConnId::Postgres {
                host: "h".into(),
                port: 5432,
                dbname: "db".into(),
                user: "u".into(),
            }
        );
        // postgresql scheme and uppercase host lowercased.
        let c = url_conn_id("postgresql://U@H/db").unwrap();
        assert_eq!(
            c,
            ConnId::Postgres {
                host: "h".into(),
                port: 5432,
                dbname: "db".into(),
                user: "U".into(),
            }
        );
    }

    #[test]
    fn url_conn_id_percent_decodes_user() {
        let id = url_conn_id("postgres://user%40corp@h/db").unwrap();
        assert_eq!(
            id,
            ConnId::Postgres {
                host: "h".into(),
                port: 5432,
                dbname: "db".into(),
                user: "user@corp".into(),
            }
        );
    }

    #[test]
    fn url_conn_id_rejects_missing_db_and_unknown_scheme() {
        assert!(url_conn_id("postgres://u@h/").is_none());
        assert!(url_conn_id("mysql://u@h/db").is_none());
    }

    #[test]
    fn url_conn_id_sqlite() {
        let id = url_conn_id("sqlite:///abs/path.db").unwrap();
        assert_eq!(
            id,
            ConnId::Sqlite {
                path: "abs/path.db".into()
            }
        );
    }

    #[test]
    fn conn_id_component_matches_url_based() {
        let component = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("H".into()),
            port: Some(5432),
            dbname: Some("db".into()),
            user: Some("u".into()),
            ..Default::default()
        };
        let url_based = ConnectionSpec {
            url: Some("postgres://u@h:5432/db".into()),
            ..Default::default()
        };
        assert_eq!(component.conn_id(), url_based.conn_id());
    }

    #[test]
    fn conn_id_none_when_required_fields_missing() {
        let no_user = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("h".into()),
            dbname: Some("db".into()),
            ..Default::default()
        };
        assert!(no_user.conn_id().is_none());
        let no_db = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("h".into()),
            user: Some("u".into()),
            ..Default::default()
        };
        assert!(no_db.conn_id().is_none());
    }

    #[test]
    fn from_url_round_trips_identity_and_captures_password() {
        for u in [
            "postgres://analyst@db.host/shop",
            "postgres://analyst:somepw@db.host:5432/shop",
        ] {
            let spec = ConnectionSpec::from_url(u).unwrap();
            assert_eq!(spec.conn_id(), url_conn_id(u));
        }
        let spec = ConnectionSpec::from_url("postgres://analyst:somepw@db.host:5432/shop").unwrap();
        assert_eq!(spec.password.as_deref(), Some("somepw"));
        assert_eq!(spec.engine, Some(ProfileEngine::Postgres));
        assert_eq!(spec.host.as_deref(), Some("db.host"));
        assert_eq!(spec.port, Some(5432));
        assert_eq!(spec.dbname.as_deref(), Some("shop"));
        assert_eq!(spec.user.as_deref(), Some("analyst"));
    }

    #[test]
    fn from_url_sqlite_captures_path() {
        let spec = ConnectionSpec::from_url("sqlite:///abs/path.db").unwrap();
        assert_eq!(spec.engine, Some(ProfileEngine::Sqlite));
        assert_eq!(spec.path.as_deref(), Some("abs/path.db"));
        assert_eq!(spec.conn_id(), url_conn_id("sqlite:///abs/path.db"));
    }

    #[test]
    fn url_conn_id_ipv6_host_kept_bracketed() {
        let id = url_conn_id("postgres://u@[::1]:5432/db").unwrap();
        assert_eq!(
            id,
            ConnId::Postgres {
                host: "[::1]".into(),
                port: 5432,
                dbname: "db".into(),
                user: "u".into(),
            }
        );
        // Both forms store the bracketed host and normalize the default port.
        assert_eq!(
            ConnectionSpec::from_url("postgres://u@[::1]/db").unwrap().conn_id(),
            url_conn_id("postgres://u@[::1]/db")
        );
    }

    #[test]
    fn from_url_no_password_when_absent() {
        assert!(
            ConnectionSpec::from_url("postgres://analyst@db.host/shop")
                .unwrap()
                .password
                .is_none()
        );
    }
}
