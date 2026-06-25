use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TableInfo {
    pub schema: Option<String>,
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    pub indexes: Vec<IndexInfo>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocEntry {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SchemaModel {
    pub engine: String,
    pub tables: Vec<TableInfo>,
    pub docs: Vec<DocEntry>,
}

impl SchemaModel {
    /// Stable SHA-256 hex over normalized structural metadata only
    /// (engine + tables/columns/types/nullable/defaults/pk/fk/indexes). Docs and
    /// descriptions are excluded. Column defaults are included because a default
    /// change is a real schema change for drift detection. Order-insensitive:
    /// tables sorted by (schema, name), columns by name, FK columns/ref_columns
    /// in declaration order (FK list sorted by ref_table then columns), indexes
    /// sorted by name.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.engine.as_bytes());
        hasher.update(b"\x00");

        // Collect and sort tables by (schema, name).
        let mut tables: Vec<&TableInfo> = self.tables.iter().collect();
        tables.sort_by(|a, b| {
            a.schema
                .as_deref()
                .unwrap_or("")
                .cmp(b.schema.as_deref().unwrap_or(""))
                .then(a.name.cmp(&b.name))
        });

        for table in tables {
            // schema\x00name\x00
            hasher.update(table.schema.as_deref().unwrap_or("").as_bytes());
            hasher.update(b"\x00");
            hasher.update(table.name.as_bytes());
            hasher.update(b"\x00");

            // Columns sorted by name.
            let mut cols: Vec<&ColumnInfo> = table.columns.iter().collect();
            cols.sort_by(|a, b| a.name.cmp(&b.name));
            for col in cols {
                hasher.update(col.name.as_bytes());
                hasher.update(b"\x01");
                hasher.update(col.data_type.as_bytes());
                hasher.update(b"\x01");
                hasher.update(if col.nullable { b"1" } else { b"0" });
                hasher.update(b"\x01");
                hasher.update(if col.primary_key { b"1" } else { b"0" });
                hasher.update(b"\x01");
                hasher.update(col.default.as_deref().unwrap_or("").as_bytes());
                hasher.update(b"\x00");
            }

            // Foreign keys sorted by (ref_table, joined columns).
            let mut fks: Vec<&ForeignKey> = table.foreign_keys.iter().collect();
            fks.sort_by(|a, b| {
                a.ref_table
                    .cmp(&b.ref_table)
                    .then(a.columns.join(",").cmp(&b.columns.join(",")))
            });
            for fk in fks {
                hasher.update(fk.columns.join(",").as_bytes());
                hasher.update(b"\x01");
                hasher.update(fk.ref_table.as_bytes());
                hasher.update(b"\x01");
                hasher.update(fk.ref_columns.join(",").as_bytes());
                hasher.update(b"\x00");
            }

            // Indexes sorted by name.
            let mut idxs: Vec<&IndexInfo> = table.indexes.iter().collect();
            idxs.sort_by(|a, b| a.name.cmp(&b.name));
            for idx in idxs {
                hasher.update(idx.name.as_bytes());
                hasher.update(b"\x01");
                hasher.update(idx.columns.join(",").as_bytes());
                hasher.update(b"\x01");
                hasher.update(if idx.unique { b"1" } else { b"0" });
                hasher.update(b"\x00");
            }

            // Table separator.
            hasher.update(b"\xff");
        }

        hex::encode_lower(hasher.finalize())
    }

    /// Compact catalog for agent context: one line per table.
    /// Format: `schema.table (N cols)[: description]`
    pub fn compact_catalog(&self) -> String {
        let mut lines = Vec::with_capacity(self.tables.len());
        let mut tables: Vec<&TableInfo> = self.tables.iter().collect();
        tables.sort_by(|a, b| {
            a.schema
                .as_deref()
                .unwrap_or("")
                .cmp(b.schema.as_deref().unwrap_or(""))
                .then(a.name.cmp(&b.name))
        });
        for table in tables {
            let qualified = match &table.schema {
                Some(s) => format!("{}.{}", s, table.name),
                None => table.name.clone(),
            };
            let ncols = table.columns.len();
            let desc = table
                .description
                .as_deref()
                .map(|d| {
                    let trimmed = d.trim();
                    // Truncate at a char boundary (byte slicing panics mid-char).
                    match trimmed.char_indices().nth(80) {
                        Some((i, _)) => format!("{}…", &trimmed[..i]),
                        None => trimmed.to_owned(),
                    }
                })
                .or_else(|| {
                    // Look for a doc that mentions the table name.
                    self.docs.iter().find_map(|doc| {
                        if doc.content.contains(&table.name) {
                            let summary = doc.content.lines().find(|l| l.contains(&table.name))?.trim().to_owned();
                            if summary.len() > 80 {
                                Some(format!("{}…", &summary[..80]))
                            } else {
                                Some(summary)
                            }
                        } else {
                            None
                        }
                    })
                });
            let line = match desc {
                Some(d) => format!("{} ({} cols): {}", qualified, ncols, d),
                None => format!("{} ({} cols)", qualified, ncols),
            };
            lines.push(line);
        }
        lines.join("\n")
    }

    /// Full detail for one table (columns, FKs, indexes), or None if not found.
    /// Accepts bare name or schema-qualified "schema.table".
    pub fn describe_table(&self, name: &str) -> Option<String> {
        let table = self.tables.iter().find(|t| {
            let qualified = match &t.schema {
                Some(s) => format!("{}.{}", s, t.name),
                None => t.name.clone(),
            };
            qualified == name || t.name == name
        })?;

        let mut out = String::new();

        let qualified = match &table.schema {
            Some(s) => format!("{}.{}", s, table.name),
            None => table.name.clone(),
        };
        out.push_str(&format!("Table: {}\n", qualified));

        if let Some(desc) = &table.description {
            out.push_str(&format!("Description: {}\n", desc.trim()));
        }

        out.push_str("Columns:\n");
        for col in &table.columns {
            let pk_flag = if col.primary_key { " [PK]" } else { "" };
            let null_flag = if col.nullable { "" } else { " NOT NULL" };
            let default_str = col.default.as_deref().map(|d| format!(" DEFAULT {}", d)).unwrap_or_default();
            out.push_str(&format!("  {}: {}{}{}{}\n", col.name, col.data_type, pk_flag, null_flag, default_str));
        }

        if !table.foreign_keys.is_empty() {
            out.push_str("Foreign Keys:\n");
            for fk in &table.foreign_keys {
                out.push_str(&format!(
                    "  ({}) -> {}.({}) \n",
                    fk.columns.join(", "),
                    fk.ref_table,
                    fk.ref_columns.join(", ")
                ));
            }
        }

        if !table.indexes.is_empty() {
            out.push_str("Indexes:\n");
            for idx in &table.indexes {
                let unique = if idx.unique { "UNIQUE " } else { "" };
                out.push_str(&format!("  {} {}({})\n", idx.name, unique, idx.columns.join(", ")));
            }
        }

        Some(out)
    }

    /// Attach doc entries (raw file contents). No LLM summarization.
    pub fn ingest_docs(&mut self, docs: Vec<DocEntry>) {
        self.docs.extend(docs);
    }
}

// Internal helper for lowercase hex encoding without adding a hex crate dep.
mod hex {
    const CHARS: &[u8] = b"0123456789abcdef";
    pub fn encode_lower(bytes: impl AsRef<[u8]>) -> String {
        let bytes = bytes.as_ref();
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(CHARS[(b >> 4) as usize] as char);
            s.push(CHARS[(b & 0xf) as usize] as char);
        }
        s
    }
}
