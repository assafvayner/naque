//! Integration tests for naque-db against a real Postgres instance.
//!
//! Each test reads `NAQUE_TEST_PG_URL` from the environment. When the variable
//! is unset the test prints a skip message and returns immediately, so the
//! suite stays green in CI environments without a Postgres container.
//!
//! Set the variable before running to exercise the tests:
//!
//! ```text
//! export NAQUE_TEST_PG_URL=postgres://naque:naque@localhost:55432/naque
//! cargo test -p naque-db --test postgres_integration
//! ```

use naque_db::{Database, DbError};

/// Return the PG URL or skip.
fn pg_url() -> Option<String> {
    std::env::var("NAQUE_TEST_PG_URL").ok()
}

// ---------------------------------------------------------------------------
// Test 1: connect, create table, insert, fetch — columns + NULLs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_fetch_columns_and_nulls() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: NAQUE_TEST_PG_URL not set");
        return;
    };

    let mut db = Database::connect(&url).await.expect("connect");

    // Idempotent setup.
    db.execute("DROP TABLE IF EXISTS naque_test_fetch_nulls")
        .await
        .expect("DROP TABLE IF EXISTS");
    db.execute("CREATE TABLE naque_test_fetch_nulls (id INT4, name TEXT, score FLOAT8)")
        .await
        .expect("CREATE TABLE");
    db.execute("INSERT INTO naque_test_fetch_nulls VALUES (1, 'alpha', 1.5), (2, NULL, NULL)")
        .await
        .expect("INSERT");

    let result = db
        .fetch("SELECT id, name, score FROM naque_test_fetch_nulls ORDER BY id")
        .await
        .expect("SELECT");

    // Columns
    assert_eq!(result.columns.len(), 3, "expected 3 columns");
    assert_eq!(result.columns[0].name, "id");
    assert_eq!(result.columns[1].name, "name");
    assert_eq!(result.columns[2].name, "score");

    // Rows
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0][0], Some("1".to_string()));
    assert_eq!(result.rows[0][1], Some("alpha".to_string()));
    assert_eq!(result.rows[0][2], Some("1.5".to_string()));
    assert_eq!(result.rows[1][0], Some("2".to_string()));
    assert_eq!(result.rows[1][1], None, "name NULL => None");
    assert_eq!(result.rows[1][2], None, "score NULL => None");

    // Cleanup.
    db.execute("DROP TABLE IF EXISTS naque_test_fetch_nulls")
        .await
        .expect("cleanup DROP");
}

// ---------------------------------------------------------------------------
// Test 2: read-only enforcement — INSERT on readonly connection must error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_readonly_enforcement() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: NAQUE_TEST_PG_URL not set");
        return;
    };

    let mut db = Database::connect(&url).await.expect("connect");

    // Create the table via primary so the missing-table error can't masquerade
    // as the write-rejection we're looking for.
    db.execute("DROP TABLE IF EXISTS naque_test_ro_enforcement")
        .await
        .expect("DROP TABLE IF EXISTS");
    db.execute("CREATE TABLE naque_test_ro_enforcement (id INT4)")
        .await
        .expect("CREATE TABLE");

    // A SELECT on the readonly connection must succeed.
    let ok = db
        .fetch_readonly("SELECT count(*) FROM naque_test_ro_enforcement")
        .await
        .expect("readonly SELECT should succeed");
    assert_eq!(ok.rows.len(), 1);

    // A write on the readonly connection must be rejected by Postgres.
    let err = db.execute_readonly("INSERT INTO naque_test_ro_enforcement VALUES (99)").await;
    assert!(err.is_err(), "INSERT on readonly connection must return Err, got: {err:?}");
    match err.unwrap_err() {
        DbError::Query(_) => {}, // Postgres rejected the write — expected
        other => panic!("expected DbError::Query (write rejection), got: {other:?}"),
    }

    // Cleanup via primary.
    db.execute("DROP TABLE IF EXISTS naque_test_ro_enforcement")
        .await
        .expect("cleanup DROP");
}

// ---------------------------------------------------------------------------
// Test 3: session persistence and reset via reconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_session_persistence_and_reconnect_reset() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: NAQUE_TEST_PG_URL not set");
        return;
    };

    let mut db = Database::connect(&url).await.expect("connect");

    // Set a non-default GUC on the primary connection.
    db.execute("SET statement_timeout = '12345ms'")
        .await
        .expect("SET statement_timeout");

    // First fetch — same connection, session state should persist.
    let r1 = db.fetch("SHOW statement_timeout").await.expect("SHOW after SET");
    assert_eq!(r1.rows.len(), 1);
    let val1 = r1.rows[0][0].as_deref().expect("SHOW statement_timeout returned NULL");
    assert!(val1.contains("12345"), "expected session GUC to be set to 12345ms, got: {val1}");

    // Second fetch on same connection — still the same session value.
    let r2 = db.fetch("SHOW statement_timeout").await.expect("SHOW second call");
    assert_eq!(r2.rows[0][0], r1.rows[0][0], "session value must persist across calls");

    // Reconnect drops and recreates the connection — GUC reverts to default.
    db.reconnect().await.expect("reconnect");

    let r3 = db.fetch("SHOW statement_timeout").await.expect("SHOW after reconnect");
    assert_eq!(r3.rows.len(), 1);
    let val3 = r3.rows[0][0]
        .as_deref()
        .expect("SHOW statement_timeout returned NULL after reconnect");
    assert!(!val3.contains("12345"), "statement_timeout should have reverted after reconnect, got: {val3}");
}

// ---------------------------------------------------------------------------
// Test 4: type stringification — int4, int8, float8, text, bool, timestamptz,
//          uuid, jsonb all render to sensible non-empty strings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_type_stringification() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: NAQUE_TEST_PG_URL not set");
        return;
    };

    let mut db = Database::connect(&url).await.expect("connect");

    db.execute("DROP TABLE IF EXISTS naque_test_types")
        .await
        .expect("DROP TABLE IF EXISTS");
    db.execute(
        "CREATE TABLE naque_test_types (
            i4    INT4,
            i8    INT8,
            f8    FLOAT8,
            t     TEXT,
            b     BOOLEAN,
            ts    TIMESTAMPTZ,
            uid   UUID,
            j     JSONB
        )",
    )
    .await
    .expect("CREATE TABLE");

    db.execute(
        "INSERT INTO naque_test_types VALUES (
            42,
            9000000000,
            3.14,
            'hello',
            TRUE,
            '2024-01-15 12:34:56+00'::TIMESTAMPTZ,
            'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::UUID,
            '{\"key\": \"value\"}'::JSONB
        )",
    )
    .await
    .expect("INSERT");

    let result = db
        .fetch("SELECT i4, i8, f8, t, b, ts, uid, j FROM naque_test_types")
        .await
        .expect("SELECT");

    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];

    // INT4
    let i4 = row[0].as_deref().expect("i4 should be Some");
    assert_eq!(i4, "42");

    // INT8
    let i8 = row[1].as_deref().expect("i8 should be Some");
    assert_eq!(i8, "9000000000");

    // FLOAT8
    let f8 = row[2].as_deref().expect("f8 should be Some");
    assert!(f8.contains("3.14"), "f8 should contain 3.14, got: {f8}");

    // TEXT
    let t = row[3].as_deref().expect("t should be Some");
    assert_eq!(t, "hello");

    // BOOL
    let b = row[4].as_deref().expect("b should be Some");
    assert!(!b.is_empty(), "bool should render to non-empty string");

    // TIMESTAMPTZ — just check it's non-empty and contains date fragments
    let ts = row[5].as_deref().expect("ts should be Some");
    assert!(ts.contains("2024") || ts.contains("15"), "timestamptz should contain date info, got: {ts}");

    // UUID
    let uid = row[6].as_deref().expect("uid should be Some");
    assert!(uid.contains("a0eebc99"), "uuid should contain expected fragment, got: {uid}");

    // JSONB
    let j = row[7].as_deref().expect("j should be Some");
    assert!(j.contains("key") || j.contains("value"), "jsonb should contain key/value content, got: {j}");

    // Cleanup.
    db.execute("DROP TABLE IF EXISTS naque_test_types").await.expect("cleanup DROP");
}

// ---------------------------------------------------------------------------
// Test: render common types that previously showed <unrenderable:...>
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_renders_common_types() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: NAQUE_TEST_PG_URL not set");
        return;
    };

    let mut db = Database::connect(&url).await.expect("connect");

    let result = db
        .fetch(
            "SELECT \
             INTERVAL '1 year 2 months 3 days 4 hours 5 minutes 6 seconds' AS iv, \
             '$123.45'::money AS amount, \
             int4range(1, 10) AS r, \
             ARRAY[1,2,3]::int4[] AS arr, \
             ARRAY['a', NULL, 'c']::text[] AS sarr, \
             ARRAY['192.168.1.0/24','10.0.0.0/8']::cidr[] AS netarr, \
             '192.168.1.0/24'::cidr AS net, \
             '08:00:2b:01:02:03'::macaddr AS mac, \
             B'101'::bit(3) AS bits, \
             12345::oid AS the_oid",
        )
        .await
        .expect("SELECT common types");

    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    let cell = |name: &str| -> String {
        let idx = result.columns.iter().position(|c| c.name == name).expect("column present");
        row[idx].clone().expect("non-null cell")
    };

    assert_eq!(cell("iv"), "1 year 2 mons 3 days 04:05:06");
    assert_eq!(cell("amount"), "123.45");
    assert_eq!(cell("r"), "[1,10)");
    assert_eq!(cell("arr"), "{1,2,3}");
    assert_eq!(cell("sarr"), "{a,NULL,c}");
    assert_eq!(cell("netarr"), "{192.168.1.0/24,10.0.0.0/8}");
    assert_eq!(cell("net"), "192.168.1.0/24");
    assert_eq!(cell("mac"), "08:00:2b:01:02:03");
    assert_eq!(cell("bits"), "101");
    assert_eq!(cell("the_oid"), "12345");

    // No cell should be an unrenderable placeholder.
    for (c, v) in result.columns.iter().zip(row.iter()) {
        if let Some(s) = v {
            assert!(!s.starts_with("<unrenderable"), "column {} rendered as {s}", c.name);
        }
    }
}
