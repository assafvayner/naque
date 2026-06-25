//! Integration tests for naque-db using SQLite only.
//!
//! Each test that needs shared state between primary and readonly connections
//! uses a temp-file database (so both connections see the same on-disk data).

use naque_db::{Database, DbError, Engine};
use tempfile::NamedTempFile;

fn tempfile_url() -> (NamedTempFile, String) {
    let f = NamedTempFile::new().expect("create tempfile");
    let path = f.path().to_string_lossy().to_string();
    let url = format!("sqlite://{path}");
    (f, url)
}

// ---------------------------------------------------------------------------
// Test 1: Engine::from_url mapping (unit test, no DB)
// ---------------------------------------------------------------------------

#[test]
fn engine_from_url_mapping() {
    assert_eq!(Engine::from_url("postgres://localhost/db").unwrap(), Engine::Postgres);
    assert_eq!(Engine::from_url("postgresql://user:pass@host/db").unwrap(), Engine::Postgres,);
    assert_eq!(Engine::from_url("sqlite::memory:").unwrap(), Engine::Sqlite);
    assert_eq!(Engine::from_url("sqlite://./foo.db").unwrap(), Engine::Sqlite);
    assert_eq!(Engine::from_url("file:///tmp/foo.db").unwrap(), Engine::Sqlite);

    assert!(Engine::from_url("mysql://localhost/db").is_err());
    assert!(Engine::from_url("garbage").is_err());
    assert!(Engine::from_url("").is_err());
}

// ---------------------------------------------------------------------------
// Test 2: connect + execute DDL + DML; rows_affected == 2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_rows_affected() {
    let (_f, url) = tempfile_url();
    let mut db = Database::connect(&url).await.expect("connect");

    db.execute("CREATE TABLE t(id INTEGER, name TEXT, score REAL)")
        .await
        .expect("CREATE TABLE");

    let affected = db
        .execute("INSERT INTO t VALUES (1,'a',1.5),(2,NULL,NULL)")
        .await
        .expect("INSERT");

    assert_eq!(affected, 2, "expected 2 rows affected");
}

// ---------------------------------------------------------------------------
// Test 3: fetch returns correct columns and cell values (including NULLs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_columns_and_nulls() {
    let (_f, url) = tempfile_url();
    let mut db = Database::connect(&url).await.expect("connect");

    db.execute("CREATE TABLE t(id INTEGER, name TEXT, score REAL)")
        .await
        .expect("CREATE TABLE");
    db.execute("INSERT INTO t VALUES (1,'a',1.5),(2,NULL,NULL)")
        .await
        .expect("INSERT");

    let result = db.fetch("SELECT id, name, score FROM t ORDER BY id").await.expect("SELECT");

    // Columns
    assert_eq!(result.columns.len(), 3);
    assert_eq!(result.columns[0].name, "id");
    assert_eq!(result.columns[1].name, "name");
    assert_eq!(result.columns[2].name, "score");

    // Row 0
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0][0], Some("1".to_string()));
    assert_eq!(result.rows[0][1], Some("a".to_string()));
    assert_eq!(result.rows[0][2], Some("1.5".to_string()));

    // Row 1: name and score are NULL
    assert_eq!(result.rows[1][0], Some("2".to_string()));
    assert_eq!(result.rows[1][1], None, "name should be NULL");
    assert_eq!(result.rows[1][2], None, "score should be NULL");
}

// ---------------------------------------------------------------------------
// Test 4: read-only enforcement — SELECT succeeds, INSERT errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn readonly_enforcement() {
    let (_f, url) = tempfile_url();
    let mut db = Database::connect(&url).await.expect("connect");

    // Prepare data via primary connection.
    db.execute("CREATE TABLE t(id INTEGER, name TEXT, score REAL)")
        .await
        .expect("CREATE TABLE");
    db.execute("INSERT INTO t VALUES (1,'a',1.5),(2,NULL,NULL)")
        .await
        .expect("INSERT");

    // Read on readonly connection must succeed.
    let result = db
        .fetch_readonly("SELECT count(*) FROM t")
        .await
        .expect("readonly SELECT should succeed");
    assert_eq!(result.rows.len(), 1);

    // Write on readonly connection must be rejected by SQLite.
    let err = db.execute_readonly("INSERT INTO t VALUES (3,'c',3.0)").await;
    assert!(err.is_err(), "INSERT on readonly connection must return Err, got: {err:?}");
    match err.unwrap_err() {
        DbError::Query(_) => {}, // expected
        other => panic!("expected DbError::Query, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 5: reconnect resets session state (temp table gone after reconnect)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_resets_session() {
    let (_f, url) = tempfile_url();
    let mut db = Database::connect(&url).await.expect("connect");

    // Create a temp table on the primary connection.
    db.execute("CREATE TEMP TABLE tmp(x INTEGER)").await.expect("CREATE TEMP TABLE");

    // Verify it exists.
    db.fetch("SELECT * FROM tmp")
        .await
        .expect("temp table should be accessible before reconnect");

    // Reconnect resets the session — the temp table lives in the old connection.
    db.reconnect().await.expect("reconnect");

    // After reconnect the temp table should be gone.
    let err = db.fetch("SELECT * FROM tmp").await;
    assert!(err.is_err(), "SELECT on dropped temp table should error after reconnect, got: {err:?}");
}
