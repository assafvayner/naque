use naque_db::Database;
use naque_schema::{current_fingerprint, introspect};
use tempfile::NamedTempFile;

/// Create a temp-file SQLite DB, connect, run DDL, and return the Database.
async fn open_sqlite_db() -> (Database, NamedTempFile) {
    let file = NamedTempFile::new().expect("tempfile");
    let path = file.path().to_str().expect("utf-8 path").to_owned();
    let url = format!("sqlite://{path}");
    let db = Database::connect(&url).await.expect("connect sqlite");
    (db, file)
}

#[tokio::test]
async fn test_sqlite_introspect_tables_and_columns() {
    let (mut db, _file) = open_sqlite_db().await;

    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .expect("create users");
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), total REAL)",
    )
    .await
    .expect("create orders");

    let model = introspect(&mut db).await.expect("introspect");

    assert_eq!(model.engine, "sqlite");

    let table_names: Vec<&str> = model.tables.iter().map(|t| t.name.as_str()).collect();
    assert!(
        table_names.contains(&"users"),
        "should contain users, got: {table_names:?}"
    );
    assert!(
        table_names.contains(&"orders"),
        "should contain orders, got: {table_names:?}"
    );

    // users.id is PK
    let users = model
        .tables
        .iter()
        .find(|t| t.name == "users")
        .expect("users table");
    let id_col = users
        .columns
        .iter()
        .find(|c| c.name == "id")
        .expect("id column");
    assert!(id_col.primary_key, "users.id should be PK");

    // users.name is NOT NULL
    let name_col = users
        .columns
        .iter()
        .find(|c| c.name == "name")
        .expect("name column");
    assert!(!name_col.nullable, "users.name should be NOT NULL");

    // orders has FK to users
    let orders = model
        .tables
        .iter()
        .find(|t| t.name == "orders")
        .expect("orders table");
    assert!(
        !orders.foreign_keys.is_empty(),
        "orders should have foreign keys"
    );
    let fk = orders
        .foreign_keys
        .iter()
        .find(|fk| fk.ref_table == "users")
        .expect("FK to users");
    assert_eq!(fk.columns, vec!["user_id"], "FK from column");
    assert_eq!(fk.ref_columns, vec!["id"], "FK to column");
}

#[tokio::test]
async fn test_sqlite_fingerprint_changes_after_alter() {
    let (mut db, _file) = open_sqlite_db().await;

    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .expect("create users");
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), total REAL)",
    )
    .await
    .expect("create orders");

    let fp_before = current_fingerprint(&mut db)
        .await
        .expect("fingerprint before");

    // SQLite doesn't support adding columns via ALTER easily on all versions,
    // but this should be fine for testing.
    db.execute("ALTER TABLE users ADD COLUMN email TEXT")
        .await
        .expect("alter table");

    let fp_after = current_fingerprint(&mut db)
        .await
        .expect("fingerprint after");

    assert_ne!(
        fp_before, fp_after,
        "fingerprint should change after ALTER TABLE ADD COLUMN"
    );
}

#[tokio::test]
async fn test_sqlite_composite_foreign_key() {
    let (mut db, _file) = open_sqlite_db().await;

    db.execute("CREATE TABLE parent (a INTEGER, b INTEGER, PRIMARY KEY (a, b))")
        .await
        .expect("create parent");
    db.execute(
        "CREATE TABLE child (x INTEGER, y INTEGER, FOREIGN KEY (x, y) REFERENCES parent(a, b))",
    )
    .await
    .expect("create child");

    let model = introspect(&mut db).await.expect("introspect");

    let child = model
        .tables
        .iter()
        .find(|t| t.name == "child")
        .expect("child table");

    assert_eq!(
        child.foreign_keys.len(),
        1,
        "child should have exactly one FK, got: {:?}",
        child.foreign_keys
    );
    let fk = &child.foreign_keys[0];
    assert_eq!(fk.columns, vec!["x", "y"], "composite FK source columns");
    assert_eq!(fk.ref_table, "parent", "composite FK ref table");
    assert_eq!(
        fk.ref_columns,
        vec!["a", "b"],
        "composite FK referenced columns"
    );
}
