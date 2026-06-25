use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("database error: {0}")]
    Db(#[from] naque_db::DbError),
}
