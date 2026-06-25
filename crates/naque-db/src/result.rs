/// A single column descriptor returned in a query result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

/// The result of executing a SQL statement against a database.
///
/// Row-returning queries populate `columns` and `rows`.
/// Non-row-returning statements (INSERT, UPDATE, DELETE, DDL) populate
/// `rows_affected`.
///
/// # Known limitation — empty result sets
///
/// When a row-returning query returns **zero rows**, `columns` will be empty.
/// sqlx's dynamic (non-typed) API cannot recover column metadata without at
/// least one decoded row. Callers must not rely on `columns` to inspect the
/// schema of an empty result set; use a schema-introspection query (e.g.
/// `information_schema.columns`) instead.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    /// Stringified cell values; `None` represents SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    pub rows_affected: Option<u64>,
}
