/// All errors produced by the `naque-db` crate.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("unsupported connection url: {0}")]
    UnsupportedUrl(String),
    #[error("connection error: {0}")]
    Connect(String),
    #[error("query error: {0}")]
    Query(String),
}
