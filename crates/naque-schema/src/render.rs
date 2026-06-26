//! Render a `SchemaModel` to a compact Markdown outline for the context doc.
//!
//! Catalog granularity: one bullet per table with its columns (type, key,
//! nullability) and FK references. Deliberately terse — per-table depth stays
//! behind the `inspect_table` tool, not dumped every turn.

use crate::SchemaModel;

/// Markdown outline of the schema: a `### <table>` per table with a bullet per
/// column and a foreign-keys line. Tables sorted by (schema, name).
pub fn schema_markdown(model: &SchemaModel) -> String {
    let mut tables: Vec<&crate::TableInfo> = model.tables.iter().collect();
    tables.sort_by(|a, b| {
        a.schema
            .as_deref()
            .unwrap_or("")
            .cmp(b.schema.as_deref().unwrap_or(""))
            .then(a.name.cmp(&b.name))
    });

    let mut out = String::new();
    for table in tables {
        let qualified = match &table.schema {
            Some(s) => format!("{}.{}", s, table.name),
            None => table.name.clone(),
        };
        out.push_str(&format!("### {qualified}\n"));
        if let Some(desc) = &table.description {
            out.push_str(&format!("{}\n", desc.trim()));
        }
        for col in &table.columns {
            let mut tags = Vec::new();
            if col.primary_key {
                tags.push("PK".to_string());
            }
            if !col.nullable {
                tags.push("NOT NULL".to_string());
            }
            if let Some(fk) = table.foreign_keys.iter().find(|fk| fk.columns.contains(&col.name)) {
                tags.push(format!("FK→{}.{}", fk.ref_table, fk.ref_columns.join(",")));
            }
            let suffix = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };
            out.push_str(&format!("- {} {}{}\n", col.name, col.data_type, suffix));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnInfo, ForeignKey, TableInfo};

    fn model() -> SchemaModel {
        SchemaModel {
            engine: "postgres".into(),
            tables: vec![TableInfo {
                schema: Some("public".into()),
                name: "orders".into(),
                columns: vec![
                    ColumnInfo {
                        name: "id".into(),
                        data_type: "bigint".into(),
                        nullable: false,
                        default: None,
                        primary_key: true,
                    },
                    ColumnInfo {
                        name: "user_id".into(),
                        data_type: "bigint".into(),
                        nullable: false,
                        default: None,
                        primary_key: false,
                    },
                ],
                foreign_keys: vec![ForeignKey {
                    columns: vec!["user_id".into()],
                    ref_table: "users".into(),
                    ref_columns: vec!["id".into()],
                }],
                indexes: vec![],
                description: Some("Customer orders".into()),
            }],
            docs: vec![],
        }
    }

    #[test]
    fn renders_table_columns_and_fk() {
        let md = schema_markdown(&model());
        assert!(md.contains("### public.orders"));
        assert!(md.contains("Customer orders"));
        assert!(md.contains("- id bigint [PK, NOT NULL]"));
        assert!(md.contains("FK→users.id"));
    }

    #[test]
    fn empty_model_is_empty_string() {
        assert_eq!(schema_markdown(&SchemaModel::default()), "");
    }
}
