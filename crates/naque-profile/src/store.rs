//! `Store`: the central `~/.naque/` directory.

use std::path::{Path, PathBuf};

use crate::{ConfigError, NaqueFile};

/// A handle to the naque home directory (default `~/.naque/`).
///
/// The directory need not exist when `Store::open` is called; call
/// `ensure_dirs` before writing.
pub struct Store {
    home: PathBuf,
}

impl Store {
    /// Create a store handle rooted at `home`. The directory need not exist.
    pub fn open(home: impl Into<PathBuf>) -> Store {
        Store { home: home.into() }
    }

    /// Resolve the default naque home directory (`~/.naque`).
    /// Returns `None` if the home directory cannot be determined.
    pub fn default_home() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".naque"))
    }

    /// Path to the global config file: `<home>/config.toml`.
    pub fn config_path(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    /// Path to the profiles file: `<home>/profiles.toml`.
    pub fn profiles_path(&self) -> PathBuf {
        self.home.join("profiles.toml")
    }

    /// Path to the cache directory: `<home>/cache`.
    pub fn cache_dir(&self) -> PathBuf {
        self.home.join("cache")
    }

    /// Create `<home>/` and `<home>/cache/` if they do not already exist.
    pub fn ensure_dirs(&self) -> Result<(), ConfigError> {
        std::fs::create_dir_all(&self.home).map_err(ConfigError::io)?;
        std::fs::create_dir_all(self.cache_dir()).map_err(ConfigError::io)?;
        Ok(())
    }

    /// Load and merge `config.toml` and `profiles.toml` into a single
    /// `NaqueFile`. Missing files are treated as empty (not an error).
    pub fn load_central(&self) -> Result<NaqueFile, ConfigError> {
        let config = load_optional(&self.config_path())?;
        let profiles = load_optional(&self.profiles_path())?;
        Ok(config.merge(profiles))
    }
}

/// Parse a TOML file into `NaqueFile`. If the file does not exist, return an
/// empty `NaqueFile`. Any other I/O error or parse error is propagated.
fn load_optional(path: &Path) -> Result<NaqueFile, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(NaqueFile::default()),
        Err(e) => return Err(ConfigError::io(e)),
    };
    toml::from_str(&raw).map_err(|e| ConfigError::parse(path.display().to_string(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn store_paths_are_relative_to_home() {
        let home = PathBuf::from("/tmp/naque-test-home");
        let store = Store::open(&home);
        assert_eq!(store.config_path(), home.join("config.toml"));
        assert_eq!(store.profiles_path(), home.join("profiles.toml"));
        assert_eq!(store.cache_dir(), home.join("cache"));
    }

    #[test]
    fn ensure_dirs_creates_home_and_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("dot-naque");
        let store = Store::open(&home);
        assert!(!home.exists());
        store.ensure_dirs().unwrap();
        assert!(home.is_dir());
        assert!(home.join("cache").is_dir());
    }

    #[test]
    fn load_central_missing_files_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("no-such-dir"));
        let file = store.load_central().unwrap();
        assert_eq!(file, NaqueFile::default());
    }

    #[test]
    fn load_central_merges_config_and_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("naque");
        std::fs::create_dir_all(&home).unwrap();

        // Write config.toml
        std::fs::write(
            home.join("config.toml"),
            r#"
default_profile = "staging"

[config]
mode = "readonly"
row_cap = 250
"#,
        )
        .unwrap();

        // Write profiles.toml
        std::fs::write(
            home.join("profiles.toml"),
            r#"
[profiles.staging]
engine = "postgres"
host = "staging.db.internal"
user = "naque"
password_env = "STAGING_DB_PW"
"#,
        )
        .unwrap();

        let store = Store::open(&home);
        let merged = store.load_central().unwrap();

        // Fields from config.toml
        assert_eq!(merged.default_profile.as_deref(), Some("staging"));
        let cfg = merged.config.as_ref().unwrap();
        assert_eq!(cfg.mode.as_deref(), Some("readonly"));
        assert_eq!(cfg.row_cap, Some(250));

        // Fields from profiles.toml
        let profiles = merged.profiles.as_ref().unwrap();
        let staging = profiles.get("staging").unwrap();
        assert_eq!(staging.connection.host.as_deref(), Some("staging.db.internal"));
        assert_eq!(staging.connection.user.as_deref(), Some("naque"));
        assert_eq!(staging.connection.password_env.as_deref(), Some("STAGING_DB_PW"));
    }
}
