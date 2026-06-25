//! Error types for naque-profile.

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(String),

    #[error("toml parse error in {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },

    #[error("{0}")]
    Other(String),
}

impl ConfigError {
    pub(crate) fn io(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }

    pub(crate) fn parse(path: impl Into<String>, source: toml::de::Error) -> Self {
        Self::Parse {
            path: path.into(),
            source,
        }
    }
}
