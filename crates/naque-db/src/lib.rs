//! Database connection layer for naque.
//!
//! Provides [`Database`], a dual-connection handle (primary + read-only) that
//! enforces read-only mode at the database level on the read-only connection.

mod conn;
mod database;
mod engine;
mod error;
mod result;

pub use database::Database;
pub use engine::Engine;
pub use error::DbError;
pub use result::{Column, QueryResult};
