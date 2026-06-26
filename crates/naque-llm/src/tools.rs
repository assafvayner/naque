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
            description: "Return the column names, types, and row count for a table.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The table name to inspect."
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "sample_table".to_string(),
            description: "Return up to `limit` rows from a table as formatted text.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The table name to sample."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of rows to return.",
                        "default": 10
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "explain".to_string(),
            description: "Run EXPLAIN on a SQL statement and return the query plan.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The SQL statement to explain."
                    }
                },
                "required": ["sql"]
            }),
        },
        ToolDef {
            name: "run_query".to_string(),
            description: "Execute a SQL statement (SELECT, INSERT, UPDATE, DELETE, or DDL) and return \
                          the result rows or the number of rows affected. Statements that modify data \
                          or schema are checked against the session's permission mode and may run \
                          automatically, require user approval, or be rejected — attempt the statement \
                          the user asked for and surface any approval/rejection result."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The SQL statement to run."
                    }
                },
                "required": ["sql"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
