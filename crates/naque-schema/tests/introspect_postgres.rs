use std::sync::OnceLock;

use naque_db::Database;
use naque_schema::{current_fingerprint, introspect};
use tokio::sync::Mutex;

const PG_URL_ENV: &str = "NAQUE_TEST_PG_URL";

/// Serializes the Postgres tests. They share one database, and `cargo test`
/// runs tests in parallel; concurrent DDL (CREATE/DROP/ALTER) across separate
/// connections invalidates cached relation OIDs and yields spurious
/// "could not open relation with OID" errors. Holding this lock for the
/// duration of each test guarantees a single in-flight DDL session at a time.
fn pg_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Returns the PG URL or prints a skip message and returns None.
fn pg_url() -> Option<String> {
    match std::env::var(PG_URL_ENV) {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            eprintln!(
                "SKIP: {PG_URL_ENV} not set — skipping Postgres introspection tests. \
                 Set it to postgres://user:pass@host/db to run."
            );
            None
        },
    }
}

#[tokio::test]
async fn test_postgres_introspect_tables_and_fk() {
    let Some(url) = pg_url() else { return };
    let _guard = pg_lock().lock().await;

    let mut db = Database::connect(&url).await.expect("connect postgres");

    // Clean up first, then create test tables.
    db.execute("DROP TABLE IF EXISTS nq_orders CASCADE")
        .await
        .expect("drop nq_orders");
    db.execute("DROP TABLE IF EXISTS nq_users CASCADE")
        .await
        .expect("drop nq_users");

    db.execute(
        "CREATE TABLE nq_users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL
        )",
    )
    .await
    .expect("create nq_users");

    db.execute(
        "CREATE TABLE nq_orders (
            id SERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES nq_users(id),
            total NUMERIC
        )",
    )
    .await
    .expect("create nq_orders");

    let model = introspect(&mut db).await.expect("introspect");

    assert_eq!(model.engine, "postgres");

    let nq_users = model
        .tables
        .iter()
        .find(|t| t.name == "nq_users")
        .expect("nq_users table not found in introspection");

    let nq_orders = model
        .tables
        .iter()
        .find(|t| t.name == "nq_orders")
        .expect("nq_orders table not found in introspection");

    // Schema should be "public"
    assert_eq!(nq_users.schema.as_deref(), Some("public"), "nq_users schema");
    assert_eq!(nq_orders.schema.as_deref(), Some("public"), "nq_orders schema");

    // nq_users.id should be PK
    let id_col = nq_users.columns.iter().find(|c| c.name == "id").expect("nq_users.id column");
    assert!(id_col.primary_key, "nq_users.id should be PK");

    // nq_orders should have FK to nq_users
    assert!(!nq_orders.foreign_keys.is_empty(), "nq_orders should have foreign keys");
    let fk = nq_orders
        .foreign_keys
        .iter()
        .find(|fk| fk.ref_table == "nq_users")
        .expect("FK from nq_orders to nq_users");
    assert_eq!(fk.columns, vec!["user_id"], "FK from column");
    assert_eq!(fk.ref_columns, vec!["id"], "FK to column");

    // Cleanup
    db.execute("DROP TABLE IF EXISTS nq_orders CASCADE").await.ok();
    db.execute("DROP TABLE IF EXISTS nq_users CASCADE").await.ok();
}

#[tokio::test]
async fn test_postgres_fingerprint_changes_after_alter() {
    let Some(url) = pg_url() else { return };
    let _guard = pg_lock().lock().await;

    let mut db = Database::connect(&url).await.expect("connect postgres");

    db.execute("DROP TABLE IF EXISTS nq_fp_test CASCADE")
        .await
        .expect("drop nq_fp_test");
    db.execute("CREATE TABLE nq_fp_test (id SERIAL PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .expect("create nq_fp_test");

    let fp_before = current_fingerprint(&mut db).await.expect("fingerprint before");

    db.execute("ALTER TABLE nq_fp_test ADD COLUMN email TEXT")
        .await
        .expect("alter table");

    let fp_after = current_fingerprint(&mut db).await.expect("fingerprint after");

    assert_ne!(fp_before, fp_after, "fingerprint should change after ALTER TABLE ADD COLUMN");

    // Cleanup
    db.execute("DROP TABLE IF EXISTS nq_fp_test CASCADE").await.ok();
}

#[tokio::test]
async fn test_postgres_composite_foreign_key() {
    let Some(url) = pg_url() else { return };
    let _guard = pg_lock().lock().await;

    let mut db = Database::connect(&url).await.expect("connect postgres");

    db.execute("DROP TABLE IF EXISTS nq_child CASCADE")
        .await
        .expect("drop nq_child");
    db.execute("DROP TABLE IF EXISTS nq_parent CASCADE")
        .await
        .expect("drop nq_parent");

    db.execute("CREATE TABLE nq_parent (a INT, b INT, PRIMARY KEY (a, b))")
        .await
        .expect("create nq_parent");
    db.execute("CREATE TABLE nq_child (x INT, y INT, FOREIGN KEY (x, y) REFERENCES nq_parent(a, b))")
        .await
        .expect("create nq_child");

    let model = introspect(&mut db).await.expect("introspect");

    let child = model
        .tables
        .iter()
        .find(|t| t.name == "nq_child")
        .expect("nq_child table not found in introspection");

    assert_eq!(
        child.foreign_keys.len(),
        1,
        "nq_child should have exactly one FK (no N^2 duplication), got: {:?}",
        child.foreign_keys
    );
    let fk = &child.foreign_keys[0];
    assert_eq!(fk.columns, vec!["x", "y"], "composite FK source columns (no duplicates)");
    assert_eq!(fk.ref_table, "nq_parent", "composite FK ref table");
    assert_eq!(fk.ref_columns, vec!["a", "b"], "composite FK referenced columns (no duplicates)");

    // Cleanup
    db.execute("DROP TABLE IF EXISTS nq_child CASCADE").await.ok();
    db.execute("DROP TABLE IF EXISTS nq_parent CASCADE").await.ok();
}
