use crate::error::DbError;

/// The database engine variant inferred from the connection URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Sqlite,
}

impl Engine {
    /// Infer the engine from a connection URL.
    ///
    /// - `postgres://` or `postgresql://` → [`Engine::Postgres`]
    /// - `sqlite:`, `sqlite://`, or `file:` → [`Engine::Sqlite`]
    /// - anything else → [`DbError::UnsupportedUrl`]
    pub fn from_url(url: &str) -> Result<Engine, DbError> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Ok(Engine::Postgres)
        } else if url.starts_with("sqlite:") || url.starts_with("file:") {
            Ok(Engine::Sqlite)
        } else {
            Err(DbError::UnsupportedUrl(url.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_schemes() {
        assert_eq!(Engine::from_url("postgres://localhost/db").unwrap(), Engine::Postgres);
        assert_eq!(Engine::from_url("postgresql://user:pass@host/db").unwrap(), Engine::Postgres);
    }

    #[test]
    fn sqlite_schemes() {
        assert_eq!(Engine::from_url("sqlite::memory:").unwrap(), Engine::Sqlite);
        assert_eq!(Engine::from_url("sqlite://./foo.db").unwrap(), Engine::Sqlite);
        assert_eq!(Engine::from_url("file:///tmp/foo.db").unwrap(), Engine::Sqlite);
    }

    #[test]
    fn unsupported_returns_err() {
        assert!(Engine::from_url("mysql://localhost/db").is_err());
        assert!(Engine::from_url("garbage").is_err());
        assert!(Engine::from_url("").is_err());
    }
}
