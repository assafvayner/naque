//! High-level [`Database`] handle with primary + read-only connections.

use sqlx::postgres::PgConnection;
use sqlx::sqlite::SqliteConnection;
use sqlx::Connection;

use crate::conn::Conn;
use crate::engine::Engine;
use crate::error::DbError;
use crate::result::QueryResult;

/// A database handle holding a primary connection and a read-only connection.
///
/// Both connections are single [`sqlx`] connection objects (not pools), so
/// session state (SET, PRAGMA, search_path) persists across calls on each
/// connection.
pub struct Database {
    url: String,
    engine: Engine,
    primary: Conn,
    readonly: Conn,
}

impl Database {
    /// Connect to the database at `url`, open both the primary and read-only
    /// connections, and enforce read-only mode on the read-only connection.
    pub async fn connect(url: &str) -> Result<Database, DbError> {
        let engine = Engine::from_url(url)?;
        let primary = open_conn(engine, url).await?;
        let mut readonly = open_conn(engine, url).await?;
        enforce_readonly(&mut readonly, engine).await?;
        Ok(Database {
            url: url.to_string(),
            engine,
            primary,
            readonly,
        })
    }

    /// Return the [`Engine`] variant for this connection.
    pub fn engine(&self) -> Engine {
        self.engine
    }

    /// Execute a row-returning query on the **primary** connection.
    pub async fn fetch(&mut self, sql: &str) -> Result<QueryResult, DbError> {
        self.primary.fetch(sql).await
    }

    /// Execute a row-returning query on the **read-only** connection.
    pub async fn fetch_readonly(&mut self, sql: &str) -> Result<QueryResult, DbError> {
        self.readonly.fetch(sql).await
    }

    /// Execute a non-row-returning statement on the **primary** connection and
    /// return the number of rows affected.
    pub async fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        self.primary.execute(sql).await
    }

    /// Attempt to execute a statement on the **read-only** connection.
    ///
    /// Any write will be rejected by the database engine itself:
    /// - SQLite: `PRAGMA query_only = ON` causes the driver to return an error.
    /// - Postgres: `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` causes the server to reject writes.
    pub async fn execute_readonly(&mut self, sql: &str) -> Result<u64, DbError> {
        self.readonly.execute(sql).await
    }

    /// Drop and reopen both connections, re-applying read-only enforcement on
    /// the read-only connection. This resets any session state (temp tables,
    /// SET variables, PRAGMAs) that was established on either connection.
    pub async fn reconnect(&mut self) -> Result<(), DbError> {
        self.primary = open_conn(self.engine, &self.url).await?;
        let mut readonly = open_conn(self.engine, &self.url).await?;
        enforce_readonly(&mut readonly, self.engine).await?;
        self.readonly = readonly;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

async fn open_conn(engine: Engine, url: &str) -> Result<Conn, DbError> {
    match engine {
        Engine::Postgres => {
            let conn = PgConnection::connect(url).await.map_err(|e| DbError::Connect(e.to_string()))?;
            Ok(Conn::Pg(conn))
        },
        Engine::Sqlite => {
            let conn = SqliteConnection::connect(url)
                .await
                .map_err(|e| DbError::Connect(e.to_string()))?;
            Ok(Conn::Sqlite(conn))
        },
    }
}

async fn enforce_readonly(conn: &mut Conn, engine: Engine) -> Result<(), DbError> {
    let sql = match engine {
        Engine::Sqlite => "PRAGMA query_only = ON;",
        Engine::Postgres => "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY;",
    };
    conn.execute(sql).await?;
    Ok(())
}
