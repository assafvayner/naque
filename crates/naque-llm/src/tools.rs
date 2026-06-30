use serde_json::json;

use crate::ToolDef;

/// Returns the standard set of tools the agent may call.
///
/// Each tool corresponds to a database/schema action that the binary's
/// executor implementation will dispatch at runtime.
pub fn standard_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "inspect_table".to_string(),
            description: "Return the full schema for one table: columns (with types and nullability), \
                          defaults, primary key, foreign keys, indexes, and the row count. The row count \
                          may be an estimate (PostgreSQL: pg_class.reltuples; SQLite: sqlite_stat1) and \
                          can be stale on large tables. Accepts a bare or schema-qualified name (e.g. \
                          'orders' or 'public.orders'); spell the identifier exactly as it appears in \
                          the catalog (PostgreSQL folds unquoted identifiers to lowercase). Errors when \
                          the name is missing, ambiguous across schemas, refers to a view/materialized \
                          view, or the connection lacks privilege to read it. \
                          Prefer this over issuing your own queries against information_schema or \
                          sqlite_master; call it only when you need detail the appended schema catalog \
                          does not already provide."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Bare or schema-qualified table name, matching the catalog's exact spelling and case."
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "sample_table".to_string(),
            description: "Return up to `limit` arbitrary rows from a table as a human-readable text \
                          table (for the agent's own orientation — not intended for verbatim display \
                          to the user). Row order is unordered and not a statistical sample. All \
                          columns are returned; wide text/JSON/BLOB/bytea cells may be truncated. \
                          Sampling is always read-only and never triggers an approval prompt. \
                          Use to disambiguate enum-like columns, inspect free-text formats, or see \
                          real values before writing filters. For specific projections, joins, or \
                          filters, use `run_query` with `SELECT ... LIMIT` instead."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Bare or schema-qualified table name, matching the catalog's exact spelling and case."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum rows to return. Defaults to 10; clamped to a maximum of 50. Use a small value (≤10) for orientation.",
                        "default": 10,
                        "minimum": 1
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "explain".to_string(),
            description: "Return the query plan for a single SQL statement WITHOUT executing it \
                          (PostgreSQL: `EXPLAIN`; SQLite: `EXPLAIN QUERY PLAN`). Safe to call on \
                          writes and DDL — no side effects, no real timings (this is not \
                          `EXPLAIN ANALYZE`). The two engines produce very different output shapes \
                          (PostgreSQL: a planner tree with cost estimates; SQLite: a flat list of \
                          query-plan steps). Use to verify index usage, check join order, or \
                          sanity-check a query before running it against a large or unfamiliar table. \
                          Returns the engine's parser/planner error verbatim for invalid SQL; \
                          read the error before retrying."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "A single SQL statement to explain. Parameter placeholders are not supported here."
                    }
                },
                "required": ["sql"]
            }),
        },
        ToolDef {
            name: "run_query".to_string(),
            description: "Execute an arbitrary SQL statement, INCLUDING writes and DDL (SELECT, \
                          INSERT, UPDATE, DELETE, CREATE, ALTER, DROP, …). Every statement passes \
                          through the application's permission gate, which deterministically \
                          auto-runs the statement, prompts the user for approval, or rejects it \
                          based on the active permission mode. Submit the statement the user \
                          asked for as-is — do not rewrite a write into a read or pre-emptively \
                          refuse to avoid the gate; a gate rejection is a normal outcome, not a \
                          failure. Returns: rows for SELECT, the number of affected rows for DML, \
                          and a success or notice for DDL. The response is a labelled envelope: \
                          the first line is one of `auto_executed`, `rejected`, or `error`, and \
                          the remaining lines are the body. For `auto_executed`, the body is the \
                          rendered result table. For `rejected`, the body is a `reason: …` line \
                          describing why the gate declined (e.g. user rejected the prompt). For \
                          `error`, the body is a `message: …` line with the database or parser \
                          error. Branch on the first line when reporting to the user, so that \
                          'the user rejected this' is not conflated with 'the query failed'. \
                          Submit one statement per call (no semicolon-separated batches); \
                          explicit transaction control (BEGIN/COMMIT) is not supported here. \
                          Parameter binding is not supported — inline literals."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "A single SQL statement to execute."
                    },
                    "byte_count_columns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of result-column names whose values are integer byte counts. The TUI renders a human-readable size (e.g. '4.5 GB') next to the raw integer for each named column. Match the result-column name exactly as it appears in the SELECT list, including any `AS` alias and the original case. Set ONLY when selecting a true byte-count expression such as `pg_total_relation_size(...)`, `length(blob_col)`, or `octet_length(text_col)`. Do NOT set for IDs, row counts, durations, timestamps, percentages, or for `bytea`/`BLOB` columns themselves. Omit or pass an empty list when no result column is a byte count.",
                        "default": []
                    }
                },
                "required": ["sql"]
            }),
        },
        ToolDef {
            name: "read_file".to_string(),
            description: "Read a UTF-8 text file from the local filesystem (e.g. SQL the user wrote, schema \
                          definitions, ORM models, or docs that explain the database). Use this to gather \
                          context the user points you to — not to browse arbitrarily. Reads are gated by a \
                          filesystem permission separate from the SQL permission mode: a path is allowed \
                          only if it matches the user's configured `read_paths` globs or an in-session \
                          grant. A path outside that set prompts the user for approval (interactive) or is \
                          rejected (non-interactive); rejection is a normal outcome, not a failure. \
                          Symlinks and `..` are resolved before the check, so paths cannot escape the \
                          allowed roots. The response is a labelled envelope: the first line is `ok`, \
                          `denied`, or `error`, and the remaining lines are the body (file contents, a \
                          reason, or a message). Optionally request a line window with `offset`/`limit`."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file. Relative paths resolve against the project directory; '~' expands to the home directory."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-based line number to start reading from. Defaults to the start of the file.",
                        "minimum": 1
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to return from `offset`. Defaults to the whole file.",
                        "minimum": 1
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "list_directory".to_string(),
            description: "List filesystem entries under a directory, or the files matching a glob pattern \
                          (e.g. 'migrations/**/*.sql'). Use this to discover what context is available \
                          before reading specific files. Subject to the same filesystem permission as \
                          `read_file`: only entries within the allowed `read_paths` globs (or an \
                          in-session grant) are returned, and listing a path outside that set prompts for \
                          approval (interactive) or is rejected (non-interactive). The response is a \
                          labelled envelope whose first line is `ok`, `denied`, or `error`. A plain \
                          directory path lists its immediate entries; a path containing glob metacharacters \
                          (*, ?, [) is expanded and the matching paths are returned."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A directory path or a glob pattern. Relative paths resolve against the project directory; '~' expands to the home directory."
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "web_fetch".to_string(),
            description: "Fetch a single http(s) URL and return its content as text — HTML is converted to \
                          Markdown; JSON, plain text, XML, CSV and similar are returned as-is; binary types \
                          (images, PDFs, archives) are refused. Use it to read documentation, a GitHub file \
                          (prefer a raw URL), or reference material the user links to. Only fetches a URL \
                          you pass explicitly; it does not search the web. Requests to loopback, private, \
                          or link-local hosts are blocked, and large bodies are truncated. The response is \
                          a labelled envelope whose first line is `ok`, `disabled`, or `error`; on `ok` the \
                          body is the fetched text. Web access can be turned off in config (`web_access`), \
                          in which case the first line is `disabled`."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "An absolute http or https URL to fetch."
                    }
                },
                "required": ["url"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_query_advertises_byte_count_columns() {
        let tools = standard_tools();
        let run_query = tools.iter().find(|t| t.name == "run_query").expect("run_query tool");
        let props = run_query.input_schema.get("properties").expect("properties");
        let bc = props.get("byte_count_columns").expect("byte_count_columns present");
        assert_eq!(bc.get("type").and_then(|v| v.as_str()), Some("array"));
        assert_eq!(bc.get("items").and_then(|i| i.get("type")).and_then(|v| v.as_str()), Some("string"));
    }

    #[test]
    fn run_query_is_not_advertised_as_read_only() {
        let tools = standard_tools();
        let run_query = tools.iter().find(|t| t.name == "run_query").expect("run_query tool");
        let desc = run_query.description.to_ascii_lowercase();
        assert!(
            !desc.contains("read-only") && !desc.contains("read only"),
            "run_query must not be labeled read-only (the agent then refuses writes): {}",
            run_query.description
        );
        assert!(
            desc.contains("insert") || desc.contains("modify"),
            "run_query should make clear it can execute writes: {}",
            run_query.description
        );
    }

    #[test]
    fn run_query_names_gate_outcomes() {
        let tools = standard_tools();
        let run_query = tools.iter().find(|t| t.name == "run_query").expect("run_query tool");
        let desc = &run_query.description;
        // Only the outcomes actually emitted by the executor envelope are advertised;
        // `awaiting_approval` is never observable on the tool-result surface because
        // the approval prompt is resolved synchronously before `run_query` returns.
        for outcome in ["auto_executed", "rejected", "error"] {
            assert!(desc.contains(outcome), "run_query description must name gate outcome '{outcome}': {desc}");
        }
        assert!(
            !desc.contains("awaiting_approval"),
            "run_query must not advertise an outcome it never emits: {desc}"
        );
    }

    #[test]
    fn explain_clarifies_no_side_effects() {
        let tools = standard_tools();
        let explain = tools.iter().find(|t| t.name == "explain").expect("explain tool");
        let desc = explain.description.to_ascii_lowercase();
        assert!(
            desc.contains("without executing") || desc.contains("does not execute") || desc.contains("no side effects"),
            "explain description must state that the statement is not executed: {}",
            explain.description
        );
    }

    #[test]
    fn sample_table_documents_arbitrary_order() {
        let tools = standard_tools();
        let sample = tools.iter().find(|t| t.name == "sample_table").expect("sample_table tool");
        let desc = sample.description.to_ascii_lowercase();
        assert!(
            desc.contains("unordered") || desc.contains("arbitrary"),
            "sample_table description must state that row order is arbitrary: {}",
            sample.description
        );
    }
}
