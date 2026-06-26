//! Config/profile resolution with precedence and credential resolution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::NaqueConfig;
use crate::discovery::find_naque_toml;
use crate::error::ConfigError;
use crate::file::NaqueFile;
use crate::profile::{ConnectionSpec, ProfileBody, ProfileEngine};
use crate::secrets::Secrets;
use crate::store::Store;

/// CLI / caller overrides that win over everything else.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// Force a specific profile name.
    pub profile: Option<String>,
    /// Force a specific connection URL (skips profile resolution entirely).
    pub url: Option<String>,
    /// Config fields that win over central + local config.
    pub config: NaqueConfig,
}

/// The fully-resolved view of config, profiles, and connection info.
///
/// # Security
///
/// `connection_url` may contain a cleartext password in its authority section.
/// The manual [`std::fmt::Debug`] implementation redacts `connection_url` so
/// the URL is never accidentally logged via `{:?}`. Do not log or print
/// `connection_url` directly.
#[derive(Clone)]
pub struct Resolved {
    /// Merged configuration (default → central → local → overrides).
    pub config: NaqueConfig,
    /// Union of central and local profiles (local wins on name collision).
    pub profiles: BTreeMap<String, ProfileBody>,
    /// The active profile name (after applying precedence rules).
    pub active_profile: Option<String>,
    /// The resolved connection URL, if any.
    ///
    /// # Security
    ///
    /// This may contain a cleartext password. Never log it directly.
    pub connection_url: Option<String>,
    /// Parent directory of the discovered `naque.toml`, if found.
    pub local_toml_dir: Option<PathBuf>,
    /// Non-fatal warnings accumulated during resolution.
    pub warnings: Vec<String>,
}

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact `connection_url` so its cleartext password never leaks into
        // logs via `{:?}`. Preserve the `Some`/`None` shape for diagnostics.
        let connection_url = self.connection_url.as_ref().map(|_| "<redacted>".to_string());
        f.debug_struct("Resolved")
            .field("config", &self.config)
            .field("profiles", &self.profiles)
            .field("active_profile", &self.active_profile)
            .field("connection_url", &connection_url)
            .field("local_toml_dir", &self.local_toml_dir)
            .field("warnings", &self.warnings)
            .finish()
    }
}

/// Resolve the full configuration from all sources.
///
/// Precedence (later wins per field):
/// 1. `NaqueConfig::default()`
/// 2. Central `[config]`
/// 3. Local `naque.toml` `[config]`
/// 4. `overrides.config`
///
/// Profile union: central ∪ local; on name collision local wins and a warning
/// is pushed.
///
/// Active profile: `overrides.profile` → local `project` → central
/// `default_profile` → `None`.
///
/// Connection URL: `overrides.url` → active profile's `resolve_url` →
/// `secrets.env("DATABASE_URL")` → `None`.
pub fn resolve(
    store: &Store,
    start_dir: &Path,
    overrides: &Overrides,
    secrets: &dyn Secrets,
) -> Result<Resolved, ConfigError> {
    let mut warnings = Vec::new();

    // Load central (~/.naque/config.toml + profiles.toml merged).
    let central = store.load_central()?;

    // Discover and load local naque.toml.
    let (local, local_toml_dir) = match find_naque_toml(start_dir) {
        Some(toml_path) => {
            let dir = toml_path.parent().map(PathBuf::from);
            let raw = std::fs::read_to_string(&toml_path).map_err(ConfigError::io)?;
            let file: NaqueFile =
                toml::from_str(&raw).map_err(|e| ConfigError::parse(toml_path.display().to_string(), e))?;
            (file, dir)
        },
        None => (NaqueFile::default(), None),
    };

    // -------------------------------------------------------------------------
    // Profile union: central first, then local (local wins on collision)
    // -------------------------------------------------------------------------
    let mut profiles: BTreeMap<String, ProfileBody> = central.profiles.clone().unwrap_or_default();

    for (name, body) in local.profiles.clone().unwrap_or_default() {
        if profiles.contains_key(&name) {
            warnings.push(format!("profile '{}' from naque.toml overrides central profile", name));
        }
        profiles.insert(name, body);
    }

    // -------------------------------------------------------------------------
    // Active profile precedence
    // -------------------------------------------------------------------------
    let active_profile = overrides
        .profile
        .clone()
        .or_else(|| local.project.clone())
        .or_else(|| central.default_profile.clone());

    // -------------------------------------------------------------------------
    // Config precedence: default → central → local → active profile → overrides
    //
    // The active profile's inline `[config]` keys (e.g. `model`, `provider`)
    // win over the global/local `[config]` but lose to CLI overrides. When no
    // profile is active, or it has no inline config, this layer is a no-op.
    // -------------------------------------------------------------------------
    let profile_config = active_profile
        .as_ref()
        .and_then(|name| profiles.get(name))
        .map(|body| body.config.clone())
        .unwrap_or_default();

    let mut config = NaqueConfig::default()
        .merge(central.config.clone().unwrap_or_default())
        .merge(local.config.clone().unwrap_or_default())
        .merge(profile_config)
        .merge(overrides.config.clone());

    // If no provider was set anywhere, detect one from the environment by
    // common API-key variables. This only fills an absent provider — an
    // explicit config/profile/CLI provider always wins.
    if config.provider.is_none() {
        config.provider = detect_provider(secrets);
    }

    // -------------------------------------------------------------------------
    // Connection URL precedence
    // -------------------------------------------------------------------------
    let connection_url = if let Some(url) = &overrides.url {
        Some(url.clone())
    } else if let Some(name) = &active_profile {
        if let Some(body) = profiles.get(name) {
            Some(body.connection.resolve_url(secrets)?)
        } else {
            warnings.push(format!("active profile '{}' not found in profiles; falling back to DATABASE_URL", name));
            secrets.env("DATABASE_URL")
        }
    } else {
        secrets.env("DATABASE_URL")
    };

    Ok(Resolved {
        config,
        profiles,
        active_profile,
        connection_url,
        local_toml_dir,
        warnings,
    })
}

impl ConnectionSpec {
    /// Resolve this spec to a connection URL string.
    ///
    /// If `url` is set, it is returned after `${VAR}` interpolation via
    /// `secrets.env`. Otherwise the URL is assembled from components:
    ///
    /// - `postgres`: `postgres://user[:pass]@host[:port]/dbname[?params]` (user, password, and query params are
    ///   percent-encoded)
    /// - `sqlite`: `sqlite://<path>`
    ///
    /// Returns an error if required fields are missing, a referenced secret is
    /// not found, or a `${VAR}` interpolation references an unset variable.
    ///
    /// # Security
    ///
    /// The returned URL may contain a cleartext password in its authority
    /// section. Treat it as a secret: never log or print it.
    pub fn resolve_url(&self, secrets: &dyn Secrets) -> Result<String, ConfigError> {
        if let Some(url) = &self.url {
            return interpolate_env(url, secrets);
        }

        match self.engine {
            Some(ProfileEngine::Postgres) | None => self.build_postgres_url(secrets),
            Some(ProfileEngine::Sqlite) => self.build_sqlite_url(),
        }
    }

    fn build_postgres_url(&self, secrets: &dyn Secrets) -> Result<String, ConfigError> {
        let host = self
            .host
            .as_deref()
            .ok_or_else(|| ConfigError::Other("postgres profile missing 'host'".into()))?;
        let user = self
            .user
            .as_deref()
            .ok_or_else(|| ConfigError::Other("postgres profile missing 'user'".into()))?;
        let dbname = self
            .dbname
            .as_deref()
            .ok_or_else(|| ConfigError::Other("postgres profile missing 'dbname'".into()))?;

        // Resolve password: keyring → env → inline plaintext (lowest priority).
        // A configured-but-missing keyring or env source is an error; inline
        // is a silent fallback used only when neither keyring nor env is
        // configured.
        let password = if let Some(account) = &self.password_keyring {
            let val = secrets.keyring(account).ok_or_else(|| {
                ConfigError::Other(format!("postgres profile: keyring account '{}' not found", account))
            })?;
            Some(val)
        } else if let Some(env_var) = &self.password_env {
            let val = secrets.env(env_var).ok_or_else(|| {
                ConfigError::Other(format!("postgres profile: password_env '{}' is not set", env_var))
            })?;
            Some(val)
        } else {
            self.password.clone()
        };

        let port = self.port.unwrap_or(5432);

        // Build user[info] part. User and password are percent-encoded so that
        // characters like `@`, `/`, and `:` cannot break out of the authority.
        let user_enc = urlencoding::encode(user);
        let userinfo = if let Some(pw) = password {
            format!("{}:{}", user_enc, urlencoding::encode(&pw))
        } else {
            user_enc.into_owned()
        };

        // Build query string from params; each key and value is percent-encoded.
        let query = if let Some(params) = &self.params {
            if params.is_empty() {
                String::new()
            } else {
                let qs: String = params
                    .iter()
                    .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
                    .collect::<Vec<_>>()
                    .join("&");
                format!("?{}", qs)
            }
        } else {
            String::new()
        };

        Ok(format!("postgres://{}@{}:{}/{}{}", userinfo, host, port, dbname, query))
    }

    fn build_sqlite_url(&self) -> Result<String, ConfigError> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| ConfigError::Other("sqlite profile missing 'path'".into()))?;
        // sqlx expects sqlite://<path>; for absolute paths this produces
        // sqlite:///absolute/path (three slashes total).
        Ok(format!("sqlite://{}", path))
    }
}

/// Detect which AI provider to use from common API-key environment variables.
///
/// Priority (first present wins): Anthropic → OpenAI → Gemini → Hugging Face
/// Inference Providers. Returns the provider identifier string used by the
/// `naque` binary's provider switch, or `None` if no known key is set.
pub fn detect_provider(secrets: &dyn Secrets) -> Option<String> {
    if secrets.env("ANTHROPIC_API_KEY").is_some() {
        Some("claude".to_string())
    } else if secrets.env("OPENAI_API_KEY").is_some() {
        Some("openai".to_string())
    } else if secrets.env("GEMINI_API_KEY").is_some() || secrets.env("GOOGLE_API_KEY").is_some() {
        Some("gemini".to_string())
    } else if secrets.env("HF_TOKEN").is_some() {
        Some("hf".to_string())
    } else {
        None
    }
}

/// Replace `${VAR}` patterns in `s` using `secrets.env`.
///
/// Returns an error if a referenced `${VAR}` resolves to no value (the missing
/// variable is not silently substituted with an empty string). A malformed
/// `${VAR` with no closing brace is emitted verbatim (no error).
fn interpolate_env(s: &str, secrets: &dyn Secrets) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after_brace = &rest[start + 2..];
        if let Some(end) = after_brace.find('}') {
            let var = &after_brace[..end];
            let val = secrets
                .env(var)
                .ok_or_else(|| ConfigError::Other(format!("url interpolation: variable '{}' is not set", var)))?;
            result.push_str(&val);
            rest = &after_brace[end + 1..];
        } else {
            // Malformed interpolation (no closing brace): emit as-is.
            result.push_str("${");
            rest = after_brace;
        }
    }
    result.push_str(rest);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use super::*;

    // -------------------------------------------------------------------------
    // Fake Secrets implementation backed by a HashMap
    // -------------------------------------------------------------------------
    struct FakeSecrets {
        env: HashMap<String, String>,
        keyring: HashMap<String, String>,
    }

    impl FakeSecrets {
        fn new() -> Self {
            FakeSecrets {
                env: HashMap::new(),
                keyring: HashMap::new(),
            }
        }

        fn with_env(mut self, key: &str, val: &str) -> Self {
            self.env.insert(key.to_string(), val.to_string());
            self
        }

        fn with_keyring(mut self, account: &str, val: &str) -> Self {
            self.keyring.insert(account.to_string(), val.to_string());
            self
        }
    }

    impl Secrets for FakeSecrets {
        fn env(&self, var: &str) -> Option<String> {
            self.env.get(var).cloned()
        }

        fn keyring(&self, account: &str) -> Option<String> {
            self.keyring.get(account).cloned()
        }
    }

    // -------------------------------------------------------------------------
    // Helper: build a Store over a temp dir with optional config/profiles files
    // -------------------------------------------------------------------------
    fn make_store(tmp: &tempfile::TempDir, config_toml: Option<&str>, profiles_toml: Option<&str>) -> Store {
        let home = tmp.path().join("naque-home");
        fs::create_dir_all(&home).unwrap();
        if let Some(s) = config_toml {
            fs::write(home.join("config.toml"), s).unwrap();
        }
        if let Some(s) = profiles_toml {
            fs::write(home.join("profiles.toml"), s).unwrap();
        }
        Store::open(home)
    }

    // =========================================================================
    // Test 1: find_naque_toml discovery
    // =========================================================================
    #[test]
    fn discovery_finds_toml_in_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = a.join("b");
        let c = b.join("c");
        fs::create_dir_all(&c).unwrap();
        let toml_path = a.join("naque.toml");
        fs::write(&toml_path, "").unwrap();

        let found = find_naque_toml(&c).unwrap();
        assert_eq!(found, toml_path);
    }

    #[test]
    fn discovery_returns_none_for_tree_with_no_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("x").join("y");
        fs::create_dir_all(&nested).unwrap();
        // No naque.toml anywhere under tmp (temp dirs are ephemeral and isolated)
        assert!(find_naque_toml(&nested).is_none());
    }

    // =========================================================================
    // Test 2: config precedence
    // =========================================================================
    #[test]
    fn config_precedence_central_local_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(
            &tmp,
            Some(
                r#"
[config]
mode = "default"
row_cap = 1000
"#,
            ),
            None,
        );

        // Local naque.toml directory
        let local_dir = tmp.path().join("project");
        fs::create_dir_all(&local_dir).unwrap();
        fs::write(
            local_dir.join("naque.toml"),
            r#"
[config]
mode = "readonly"
"#,
        )
        .unwrap();

        let overrides = Overrides {
            config: NaqueConfig {
                model: Some("m".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        let secrets = FakeSecrets::new();
        let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();

        assert_eq!(resolved.config.mode.as_deref(), Some("readonly")); // local wins
        assert_eq!(resolved.config.row_cap, Some(1000)); // central preserved
        assert_eq!(resolved.config.model.as_deref(), Some("m")); // override wins
    }

    // =========================================================================
    // Test 3: profile union, local wins on collision
    // =========================================================================
    #[test]
    fn profile_union_local_wins_on_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(
            &tmp,
            None,
            Some(
                r#"
[profiles.dev]
engine = "postgres"
host = "central"
user = "u"
dbname = "db"
"#,
            ),
        );

        let local_dir = tmp.path().join("project");
        fs::create_dir_all(&local_dir).unwrap();
        fs::write(
            local_dir.join("naque.toml"),
            r#"
[profiles.dev]
engine = "postgres"
host = "local"
user = "u"
dbname = "db"
"#,
        )
        .unwrap();

        let overrides = Overrides::default();
        let secrets = FakeSecrets::new();
        let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();

        let dev = resolved.profiles.get("dev").unwrap();
        assert_eq!(dev.connection.host.as_deref(), Some("local"));
        assert!(!resolved.warnings.is_empty());
        assert!(resolved.warnings[0].contains("dev"));
    }

    // =========================================================================
    // Test 4: active profile precedence
    // =========================================================================
    #[test]
    fn active_profile_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(&tmp, Some(r#"default_profile = "b""#), None);

        // Case 1: overrides.profile wins
        {
            let local_dir = tmp.path().join("p1");
            fs::create_dir_all(&local_dir).unwrap();
            fs::write(local_dir.join("naque.toml"), "project = \"a\"\n").unwrap();
            let overrides = Overrides {
                profile: Some("c".into()),
                ..Default::default()
            };
            let secrets = FakeSecrets::new();
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.active_profile.as_deref(), Some("c"));
        }

        // Case 2: local project wins over central default_profile
        {
            let local_dir = tmp.path().join("p2");
            fs::create_dir_all(&local_dir).unwrap();
            fs::write(local_dir.join("naque.toml"), "project = \"a\"\n").unwrap();
            let overrides = Overrides::default();
            let secrets = FakeSecrets::new();
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.active_profile.as_deref(), Some("a"));
        }

        // Case 3: no local project → central default_profile
        {
            let local_dir = tmp.path().join("p3");
            fs::create_dir_all(&local_dir).unwrap();
            // no naque.toml here
            let overrides = Overrides::default();
            let secrets = FakeSecrets::new();
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.active_profile.as_deref(), Some("b"));
        }

        // Case 4: no local, no central default → None
        {
            let tmp2 = tempfile::tempdir().unwrap();
            let store2 = make_store(&tmp2, None, None);
            let local_dir = tmp2.path().join("p4");
            fs::create_dir_all(&local_dir).unwrap();
            let overrides = Overrides::default();
            let secrets = FakeSecrets::new();
            let resolved = resolve(&store2, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.active_profile, None);
        }
    }

    // =========================================================================
    // Test 5: resolve_url postgres with password_env
    // =========================================================================
    #[test]
    fn resolve_url_postgres_with_password_env() {
        let spec = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("h".into()),
            port: Some(5432),
            dbname: Some("d".into()),
            user: Some("u".into()),
            password_env: Some("PG_PASS".into()),
            ..Default::default()
        };
        let secrets = FakeSecrets::new().with_env("PG_PASS", "s");
        let url = spec.resolve_url(&secrets).unwrap();
        assert_eq!(url, "postgres://u:s@h:5432/d");
    }

    // =========================================================================
    // Test 6: resolve_url sqlite from path
    // =========================================================================
    #[test]
    fn resolve_url_sqlite_from_path() {
        let spec = ConnectionSpec {
            engine: Some(ProfileEngine::Sqlite),
            path: Some("/tmp/test.db".into()),
            ..Default::default()
        };
        let secrets = FakeSecrets::new();
        let url = spec.resolve_url(&secrets).unwrap();
        assert_eq!(url, "sqlite:///tmp/test.db");
    }

    // =========================================================================
    // Test 7: resolve_url error when password_env missing
    // =========================================================================
    #[test]
    fn resolve_url_postgres_error_when_password_env_missing() {
        let spec = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("h".into()),
            dbname: Some("d".into()),
            user: Some("u".into()),
            password_env: Some("MISSING_VAR".into()),
            ..Default::default()
        };
        let secrets = FakeSecrets::new(); // no env vars
        let result = spec.resolve_url(&secrets);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("MISSING_VAR"));
    }

    // =========================================================================
    // Test 8: connection_url precedence
    // =========================================================================
    #[test]
    fn connection_url_precedence() {
        let tmp = tempfile::tempdir().unwrap();

        // Build store with a profile "mydb"
        let store = make_store(
            &tmp,
            Some(r#"default_profile = "mydb""#),
            Some(
                r#"
[profiles.mydb]
engine = "postgres"
host = "profile-host"
user = "pu"
dbname = "pdb"
"#,
            ),
        );
        let local_dir = tmp.path().join("proj");
        fs::create_dir_all(&local_dir).unwrap();

        // Case 1: overrides.url wins over everything
        {
            let overrides = Overrides {
                url: Some("postgres://override/db".into()),
                ..Default::default()
            };
            let secrets = FakeSecrets::new().with_env("DATABASE_URL", "postgres://env/db");
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.connection_url.as_deref(), Some("postgres://override/db"));
        }

        // Case 2: no override.url → active profile's url
        {
            let overrides = Overrides::default();
            // no password needed (postgres without password_env is allowed if
            // no password_env set — but mydb has no password_env so it builds
            // without password)
            let secrets = FakeSecrets::new().with_env("DATABASE_URL", "postgres://env/db");
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            // Should come from profile
            let url = resolved.connection_url.unwrap();
            assert!(url.starts_with("postgres://pu@profile-host"));
        }

        // Case 3: no profile active → DATABASE_URL env
        {
            let tmp3 = tempfile::tempdir().unwrap();
            let store3 = make_store(&tmp3, None, None);
            let local_dir3 = tmp3.path().join("p");
            fs::create_dir_all(&local_dir3).unwrap();
            let overrides = Overrides::default();
            let secrets = FakeSecrets::new().with_env("DATABASE_URL", "postgres://env/db");
            let resolved = resolve(&store3, &local_dir3, &overrides, &secrets).unwrap();
            assert_eq!(resolved.connection_url.as_deref(), Some("postgres://env/db"));
        }
    }

    // =========================================================================
    // Test 9: end-to-end sqlite connect (tokio async)
    // =========================================================================
    #[tokio::test]
    async fn e2e_sqlite_profile_connects_and_executes() {
        let tmp = tempfile::tempdir().unwrap();
        // Use NamedTempFile so the file exists (sqlx requires it unless create_if_missing).
        let db_file = tempfile::NamedTempFile::new_in(tmp.path()).unwrap();
        let db_path = db_file.path().to_str().unwrap().to_string();

        let store = make_store(
            &tmp,
            Some(r#"default_profile = "testdb""#),
            Some(&format!(
                r#"
[profiles.testdb]
engine = "sqlite"
path = "{}"
"#,
                db_path
            )),
        );

        let local_dir = tmp.path().join("proj");
        fs::create_dir_all(&local_dir).unwrap();

        let overrides = Overrides::default();
        let secrets = FakeSecrets::new();
        let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();

        let url = resolved.connection_url.expect("should have connection_url");
        // URL should be sqlite://<path> (three slashes for absolute paths)
        assert!(url.starts_with("sqlite://"), "url was: {}", url);

        let mut db = naque_db::Database::connect(&url).await.expect("connect failed");
        db.execute("CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY, val TEXT)")
            .await
            .expect("CREATE TABLE failed");
        db.execute("INSERT INTO t (val) VALUES ('hello')").await.expect("INSERT failed");
        let result = db.fetch("SELECT val FROM t").await.expect("SELECT failed");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0].as_deref(), Some("hello"));
    }

    // =========================================================================
    // Test 10: postgres URL percent-encodes special chars in user/password
    // =========================================================================
    #[test]
    fn resolve_url_postgres_percent_encodes_credentials() {
        let spec = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("host".into()),
            port: Some(5432),
            dbname: Some("db".into()),
            user: Some("u".into()),
            password_env: Some("PG_PASS".into()),
            ..Default::default()
        };
        let secrets = FakeSecrets::new().with_env("PG_PASS", "p@ss/word");
        let url = spec.resolve_url(&secrets).unwrap();
        assert_eq!(url, "postgres://u:p%40ss%2Fword@host:5432/db");
        // Encoded special chars present; raw ones absent from the password.
        assert!(url.contains("p%40ss%2Fword"));
        // Host segment intact (the single unencoded `@` separates authority).
        assert!(url.contains("@host:5432/db"));
    }

    // =========================================================================
    // Test 11: warn when active profile is not found in the profiles map
    // =========================================================================
    #[test]
    fn warn_when_active_profile_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(&tmp, Some(r#"default_profile = "ghost""#), None);
        let local_dir = tmp.path().join("proj");
        fs::create_dir_all(&local_dir).unwrap();

        let overrides = Overrides::default();
        let secrets = FakeSecrets::new().with_env("DATABASE_URL", "postgres://env/db");
        let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();

        assert_eq!(resolved.active_profile.as_deref(), Some("ghost"));
        assert_eq!(resolved.connection_url.as_deref(), Some("postgres://env/db"));
        assert!(
            resolved
                .warnings
                .iter()
                .any(|w| w == "active profile 'ghost' not found in profiles; falling back to DATABASE_URL")
        );
    }

    // =========================================================================
    // Test 12: Debug redacts connection_url (never leaks the password)
    // =========================================================================
    #[test]
    fn debug_redacts_connection_url() {
        let resolved = Resolved {
            config: NaqueConfig::default(),
            profiles: BTreeMap::new(),
            active_profile: Some("p".into()),
            connection_url: Some("postgres://u:supersecret@host:5432/db".into()),
            local_toml_dir: None,
            warnings: Vec::new(),
        };
        let dbg = format!("{:?}", resolved);
        assert!(!dbg.contains("supersecret"), "debug leaked password: {dbg}");
        assert!(dbg.contains("<redacted>"));
    }

    // =========================================================================
    // Test 13: ${VAR} interpolation — success and missing-var error
    // =========================================================================
    #[test]
    fn resolve_url_interpolation_success() {
        let spec = ConnectionSpec {
            url: Some("postgres://u:${PW}@host/db".into()),
            ..Default::default()
        };
        let secrets = FakeSecrets::new().with_env("PW", "secret");
        let url = spec.resolve_url(&secrets).unwrap();
        assert_eq!(url, "postgres://u:secret@host/db");
    }

    #[test]
    fn resolve_url_interpolation_missing_var_errors() {
        let spec = ConnectionSpec {
            url: Some("postgres://u:${MISSING}@host/db".into()),
            ..Default::default()
        };
        let secrets = FakeSecrets::new();
        let result = spec.resolve_url(&secrets);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("MISSING"));
    }

    // =========================================================================
    // Test 14: resolve_url postgres with password_keyring (fake keyring)
    // =========================================================================
    // =========================================================================
    // Test 15: active profile's inline config wins over central/local config
    // but loses to CLI overrides
    // =========================================================================
    #[test]
    fn profile_config_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(
            &tmp,
            Some(
                r#"
default_profile = "prod"

[config]
model = "central-model"
mode = "default"
row_cap = 1000
"#,
            ),
            Some(
                r#"
[profiles.prod]
engine = "postgres"
host = "h"
user = "u"
dbname = "d"
model = "profile-model"
mode = "readonly"
"#,
            ),
        );
        let local_dir = tmp.path().join("proj");
        fs::create_dir_all(&local_dir).unwrap();

        // Case 1: profile config overrides central config (no CLI override).
        {
            let overrides = Overrides::default();
            let secrets = FakeSecrets::new();
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.config.model.as_deref(), Some("profile-model"));
            assert_eq!(resolved.config.mode.as_deref(), Some("readonly"));
            // central-only key still flows through
            assert_eq!(resolved.config.row_cap, Some(1000));
        }

        // Case 2: CLI override beats the profile config.
        {
            let overrides = Overrides {
                config: NaqueConfig {
                    model: Some("cli-model".into()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let secrets = FakeSecrets::new();
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.config.model.as_deref(), Some("cli-model"));
            // mode still comes from the profile (no CLI override for it)
            assert_eq!(resolved.config.mode.as_deref(), Some("readonly"));
        }
    }

    // =========================================================================
    // Test 16: provider auto-detection from env keys + priority order
    // =========================================================================
    #[test]
    fn detect_provider_priority() {
        // Anthropic wins over everything else.
        let s = FakeSecrets::new()
            .with_env("ANTHROPIC_API_KEY", "a")
            .with_env("OPENAI_API_KEY", "o")
            .with_env("GEMINI_API_KEY", "g")
            .with_env("HF_TOKEN", "h");
        assert_eq!(detect_provider(&s).as_deref(), Some("claude"));

        // OpenAI next.
        let s = FakeSecrets::new()
            .with_env("OPENAI_API_KEY", "o")
            .with_env("GEMINI_API_KEY", "g")
            .with_env("HF_TOKEN", "h");
        assert_eq!(detect_provider(&s).as_deref(), Some("openai"));

        // Gemini via GEMINI_API_KEY, then GOOGLE_API_KEY.
        let s = FakeSecrets::new().with_env("GEMINI_API_KEY", "g").with_env("HF_TOKEN", "h");
        assert_eq!(detect_provider(&s).as_deref(), Some("gemini"));
        let s = FakeSecrets::new().with_env("GOOGLE_API_KEY", "g").with_env("HF_TOKEN", "h");
        assert_eq!(detect_provider(&s).as_deref(), Some("gemini"));

        // HF_TOKEN last.
        let s = FakeSecrets::new().with_env("HF_TOKEN", "h");
        assert_eq!(detect_provider(&s).as_deref(), Some("hf"));

        // Nothing set → None.
        let s = FakeSecrets::new();
        assert_eq!(detect_provider(&s), None);
    }

    // =========================================================================
    // Test 17: detection only fills an absent provider; explicit config wins
    // =========================================================================
    #[test]
    fn resolve_provider_detection_only_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let local_dir = tmp.path().join("proj");
        fs::create_dir_all(&local_dir).unwrap();

        // No provider configured anywhere → detection fills it from env.
        {
            let store = make_store(&tmp, None, None);
            let overrides = Overrides::default();
            let secrets = FakeSecrets::new().with_env("HF_TOKEN", "x");
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.config.provider.as_deref(), Some("hf"));
        }

        // Explicit central provider wins over a detectable env key.
        {
            let store = make_store(
                &tmp,
                Some(
                    r#"
[config]
provider = "openai"
"#,
                ),
                None,
            );
            let overrides = Overrides::default();
            let secrets = FakeSecrets::new().with_env("ANTHROPIC_API_KEY", "x");
            let resolved = resolve(&store, &local_dir, &overrides, &secrets).unwrap();
            assert_eq!(resolved.config.provider.as_deref(), Some("openai"));
        }
    }

    #[test]
    fn resolve_url_postgres_with_password_keyring() {
        let spec = ConnectionSpec {
            engine: Some(ProfileEngine::Postgres),
            host: Some("host".into()),
            port: Some(5432),
            dbname: Some("db".into()),
            user: Some("u".into()),
            password_keyring: Some("my-account".into()),
            ..Default::default()
        };
        let secrets = FakeSecrets::new().with_keyring("my-account", "kr@pass");
        let url = spec.resolve_url(&secrets).unwrap();
        // Keyring-sourced password is present and percent-encoded.
        assert_eq!(url, "postgres://u:kr%40pass@host:5432/db");
        assert!(url.contains("kr%40pass"));
    }
}
