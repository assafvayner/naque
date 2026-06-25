use crate::ToolDef;
use serde_json::json;

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
            description: "Execute a read-only SQL query and return the results as formatted text."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The SQL query to run."
                    }
                },
                "required": ["sql"]
            }),
        },
    ]
}
