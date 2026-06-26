mod cache;
mod error;
mod introspect;
mod model;
mod render;

pub use cache::{cached_fingerprint, load_schema, save_schema};
pub use error::SchemaError;
pub use introspect::{current_fingerprint, introspect};
pub use model::{ColumnInfo, DocEntry, ForeignKey, IndexInfo, SchemaModel, TableInfo};
pub use render::schema_markdown;

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    // ── helpers ────────────────────────────────────────────────────────────────

    fn make_model() -> SchemaModel {
        SchemaModel {
            engine: "postgres".to_owned(),
            tables: vec![
                TableInfo {
                    schema: Some("public".to_owned()),
                    name: "users".to_owned(),
                    columns: vec![
                        ColumnInfo {
                            name: "id".to_owned(),
                            data_type: "bigint".to_owned(),
                            nullable: false,
                            default: Some("nextval('users_id_seq')".to_owned()),
                            primary_key: true,
                        },
                        ColumnInfo {
                            name: "email".to_owned(),
                            data_type: "text".to_owned(),
                            nullable: false,
                            default: None,
                            primary_key: false,
                        },
                    ],
                    foreign_keys: vec![],
                    indexes: vec![IndexInfo {
                        name: "users_email_idx".to_owned(),
                        columns: vec!["email".to_owned()],
                        unique: true,
                    }],
                    description: None,
                },
                TableInfo {
                    schema: Some("public".to_owned()),
                    name: "orders".to_owned(),
                    columns: vec![
                        ColumnInfo {
                            name: "id".to_owned(),
                            data_type: "bigint".to_owned(),
                            nullable: false,
                            default: None,
                            primary_key: true,
                        },
                        ColumnInfo {
                            name: "user_id".to_owned(),
                            data_type: "bigint".to_owned(),
                            nullable: false,
                            default: None,
                            primary_key: false,
                        },
                        ColumnInfo {
                            name: "total".to_owned(),
                            data_type: "numeric".to_owned(),
                            nullable: true,
                            default: None,
                            primary_key: false,
                        },
                    ],
                    foreign_keys: vec![ForeignKey {
                        columns: vec!["user_id".to_owned()],
                        ref_table: "users".to_owned(),
                        ref_columns: vec!["id".to_owned()],
                    }],
                    indexes: vec![IndexInfo {
                        name: "orders_user_id_idx".to_owned(),
                        columns: vec!["user_id".to_owned()],
                        unique: false,
                    }],
                    description: Some("Customer purchase records".to_owned()),
                },
            ],
            docs: vec![],
        }
    }

    // ── 1. Serde round-trip ────────────────────────────────────────────────────

    #[test]
    fn serde_round_trip() {
        let original = make_model();
        let json = serde_json::to_string(&original).expect("serialize");
        let recovered: SchemaModel = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, recovered);
    }

    // ── 2. fingerprint ─────────────────────────────────────────────────────────

    #[test]
    fn fingerprint_stable() {
        let m = make_model();
        let fp1 = m.fingerprint();
        let fp2 = m.fingerprint();
        assert_eq!(fp1, fp2, "fingerprint must be deterministic");
        assert_eq!(fp1.len(), 64, "should be 256-bit hex");
    }

    #[test]
    fn fingerprint_unchanged_by_docs_and_description() {
        let mut m = make_model();
        let fp_before = m.fingerprint();

        // Add a doc.
        m.ingest_docs(vec![DocEntry {
            path: "README.md".to_owned(),
            content: "Some documentation about orders and users.".to_owned(),
        }]);
        assert_eq!(m.fingerprint(), fp_before, "docs must not affect fingerprint");

        // Change a description.
        m.tables[0].description = Some("Now has a description".to_owned());
        assert_eq!(m.fingerprint(), fp_before, "description must not affect fingerprint");
    }

    #[test]
    fn fingerprint_changed_by_new_column() {
        let m = make_model();
        let fp_before = m.fingerprint();

        let mut m2 = m.clone();
        m2.tables[0].columns.push(ColumnInfo {
            name: "created_at".to_owned(),
            data_type: "timestamptz".to_owned(),
            nullable: true,
            default: None,
            primary_key: false,
        });
        assert_ne!(m2.fingerprint(), fp_before, "adding a column must change fingerprint");
    }

    #[test]
    fn fingerprint_changed_by_type_change() {
        let m = make_model();
        let fp_before = m.fingerprint();

        let mut m2 = m.clone();
        m2.tables[0].columns[1].data_type = "varchar(255)".to_owned();
        assert_ne!(m2.fingerprint(), fp_before, "changing column type must change fingerprint");
    }

    #[test]
    fn fingerprint_changed_by_default_change() {
        let m = make_model();
        let fp_before = m.fingerprint();

        // users.email has default None; set a default.
        let mut m2 = m.clone();
        m2.tables[0].columns[1].default = Some("'unknown@example.com'".to_owned());
        assert_ne!(m2.fingerprint(), fp_before, "changing a column default must change fingerprint");

        // Also: changing an existing default value must change the fingerprint.
        let mut m3 = m.clone();
        m3.tables[0].columns[0].default = Some("nextval('other_seq')".to_owned());
        assert_ne!(m3.fingerprint(), fp_before, "altering an existing default must change fingerprint");
    }

    #[test]
    fn fingerprint_order_insensitive() {
        let m = make_model();
        let fp = m.fingerprint();

        // Reverse table order.
        let mut m2 = m.clone();
        m2.tables.reverse();
        assert_eq!(m2.fingerprint(), fp, "reversing table order must not change fingerprint");

        // Reverse column order in users table.
        let mut m3 = m.clone();
        m3.tables[0].columns.reverse();
        assert_eq!(m3.fingerprint(), fp, "reversing column order must not change fingerprint");
    }

    // ── 3. compact_catalog ─────────────────────────────────────────────────────

    #[test]
    fn compact_catalog_lists_both_tables() {
        let m = make_model();
        let catalog = m.compact_catalog();
        assert!(catalog.contains("users"), "catalog must mention 'users': {catalog}");
        assert!(catalog.contains("orders"), "catalog must mention 'orders': {catalog}");
        // Should not dump full column detail (i.e., no per-column type lines).
        assert!(!catalog.contains("nextval("), "catalog must not include column defaults: {catalog}");
    }

    #[test]
    fn compact_catalog_is_terse() {
        let m = make_model();
        let catalog = m.compact_catalog();
        // Terse: one non-empty line per table.
        let lines: Vec<&str> = catalog.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "one line per table, got:\n{catalog}");
    }

    #[test]
    fn compact_catalog_includes_description() {
        let m = make_model();
        let catalog = m.compact_catalog();
        // orders has a description.
        assert!(catalog.contains("Customer purchase records"), "catalog must include order description: {catalog}");
    }

    #[test]
    fn compact_catalog_truncates_multibyte_description_without_panic() {
        let mut m = make_model();
        // A non-ASCII description well over 80 chars; naive byte slicing at
        // [..80] would panic mid-character.
        let long_desc = "café ".repeat(40); // each "café " is 6 bytes, 5 chars
        m.tables[0].description = Some(long_desc);

        // Must not panic.
        let catalog = m.compact_catalog();
        assert!(catalog.contains('…'), "long description should be truncated with an ellipsis: {catalog}");
    }

    // ── 4. describe_table ──────────────────────────────────────────────────────

    #[test]
    fn describe_table_found_by_qualified_name() {
        let m = make_model();
        let detail = m.describe_table("public.orders").expect("should find orders");
        // Contains column names and types.
        assert!(detail.contains("user_id"), "should list user_id column");
        assert!(detail.contains("bigint"), "should list bigint type");
        // Contains FK.
        assert!(detail.contains("users"), "should mention FK to users");
        // Contains PK marker.
        assert!(detail.contains("[PK]"), "should mark primary key");
    }

    #[test]
    fn describe_table_found_by_bare_name() {
        let m = make_model();
        let detail = m.describe_table("orders").expect("bare name lookup");
        assert!(detail.contains("user_id"));
    }

    #[test]
    fn describe_table_none_for_missing() {
        let m = make_model();
        assert!(m.describe_table("nonexistent").is_none());
    }

    // ── 5. ingest_docs ─────────────────────────────────────────────────────────

    #[test]
    fn ingest_docs_adds_entries() {
        let mut m = make_model();
        assert_eq!(m.docs.len(), 0);
        let fp_before = m.fingerprint();

        m.ingest_docs(vec![
            DocEntry {
                path: "docs/orders.md".to_owned(),
                content: "The orders table holds all orders.".to_owned(),
            },
            DocEntry {
                path: "docs/users.md".to_owned(),
                content: "The users table holds account information.".to_owned(),
            },
        ]);

        assert_eq!(m.docs.len(), 2);
        assert_eq!(m.fingerprint(), fp_before, "fingerprint unchanged by docs");
    }

    // ── 6. save_schema / load_schema / cached_fingerprint ─────────────────────

    #[test]
    fn cache_round_trip() {
        let dir = tempdir().expect("tempdir");
        let m = make_model();

        save_schema(dir.path(), &m).expect("save");
        let loaded = load_schema(dir.path()).expect("load").expect("should be Some");
        assert_eq!(m, loaded);

        let fp = cached_fingerprint(dir.path()).expect("fingerprint file");
        assert_eq!(fp, m.fingerprint());
    }

    #[test]
    fn load_schema_empty_dir_returns_none() {
        let dir = tempdir().expect("tempdir");
        let result = load_schema(dir.path()).expect("no I/O error");
        assert!(result.is_none());
    }

    #[test]
    fn cached_fingerprint_absent_without_save() {
        let dir = tempdir().expect("tempdir");
        assert!(cached_fingerprint(dir.path()).is_none());
    }

    #[test]
    fn save_creates_cache_dir_if_needed() {
        let dir = tempdir().expect("tempdir");
        let cache_dir = dir.path().join("nested").join("cache");
        let m = make_model();
        save_schema(&cache_dir, &m).expect("save should create nested dirs");
        let loaded = load_schema(&cache_dir).expect("load").expect("Some");
        assert_eq!(m, loaded);
    }
}
