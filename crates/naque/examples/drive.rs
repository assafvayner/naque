//! Non-interactive driver harness for the naque engine.
//!
//! Reads lines from stdin and runs them through App::handle_line with a real
//! LLM provider and real database, printing results in plain text.
//!
//! Usage:
//!   cargo run -p naque --example drive -- \
//!     --url postgres://user:pass@host/db \
//!     [--mode strict|default|readonly|wildcard] \
//!     [--approve yes|no] \
//!     [--model <model-id>] \
//!     [--no-guard] \
//!     [--no-learn]

use std::io::{self, BufRead};

use naque::{App, AutoApprove, AutoReject, TranscriptEntry};
use naque_core::PermissionMode;
use naque_db::Database;
use naque_llm::{Agent, AgentConfig, HfProvider};

const DEFAULT_MODEL: &str = "zai-org/GLM-5.2:together";

const SYSTEM_PREAMBLE: &str = "\
You are a careful SQL assistant connected to a relational database. \
You have four tools: inspect_table (schema of a single table), \
sample_table (a few rows for orientation), explain (EXPLAIN plan), \
and run_query (execute SQL and return results). \
A compact schema catalog is appended to each user message — consult it before \
deciding which tables or columns exist. \
Write correct SQL for the target engine. Prefer read-only exploration. \
After running a query, reply with a concise natural-language answer. \
Do not over-plan; act directly on the user's question.";

// ---------------------------------------------------------------------------
// Arg parsing
// ---------------------------------------------------------------------------

struct Args {
    url: String,
    mode: PermissionMode,
    approve: bool,
    model: String,
    guard: bool,
    learn: bool,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut url: Option<String> = None;
    let mut mode = PermissionMode::Wildcard;
    let mut approve = true;
    let mut model = DEFAULT_MODEL.to_string();
    let mut guard = true;
    let mut learn = true;

    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--url" => {
                i += 1;
                url = Some(raw.get(i).ok_or("--url requires a value")?.clone());
            }
            "--mode" => {
                i += 1;
                let s = raw.get(i).ok_or("--mode requires a value")?;
                mode =
                    s.parse()
                        .map_err(|e: naque_core::permission::ParsePermissionModeError| {
                            format!("invalid --mode: {e}")
                        })?;
            }
            "--approve" => {
                i += 1;
                let s = raw.get(i).ok_or("--approve requires yes|no")?;
                approve = match s.as_str() {
                    "yes" => true,
                    "no" => false,
                    other => return Err(format!("--approve must be yes or no, got {other:?}")),
                };
            }
            "--model" => {
                i += 1;
                model = raw.get(i).ok_or("--model requires a value")?.clone();
            }
            "--no-guard" => {
                guard = false;
            }
            "--no-learn" => {
                learn = false;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }

    let url = url.ok_or("--url is required")?;
    Ok(Args {
        url,
        mode,
        approve,
        model,
        guard,
        learn,
    })
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

fn print_result(result: Option<&naque_db::QueryResult>) {
    let Some(r) = result else {
        return;
    };

    if r.columns.is_empty() {
        if let Some(n) = r.rows_affected {
            println!("  rows affected: {n}");
        }
        return;
    }

    let headers: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
    println!("  columns: {}", headers.join(" | "));
    println!("  row count: {}", r.rows.len());
    for row in r.rows.iter().take(8) {
        let cells: Vec<String> = row
            .iter()
            .map(|v| v.as_deref().unwrap_or("NULL").to_string())
            .collect();
        println!("    {}", cells.join(" | "));
    }
    if r.rows.len() > 8 {
        println!("  ... ({} more rows not shown)", r.rows.len() - 8);
    }
}

fn render_entry(e: &TranscriptEntry) {
    match e {
        TranscriptEntry::User(s) => println!("  User: {s}"),
        TranscriptEntry::Agent(s) => println!("  Agent: {s}"),
        TranscriptEntry::Sql { sql, label } => println!("  SQL[{label}]: {sql}"),
        TranscriptEntry::Info(s) => println!("  Info: {s}"),
        TranscriptEntry::Error(s) => println!("  Error: {s}"),
        TranscriptEntry::Rejected(s) => println!("  Rejected: {s}"),
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!(
                "usage: drive --url <conn> [--mode strict|default|readonly|wildcard] \
                 [--approve yes|no] [--model <id>] [--no-guard] [--no-learn]"
            );
            std::process::exit(1);
        }
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    rt.block_on(async move {
        run(args).await;
    });
}

async fn run(args: Args) {
    // 1. Connect to database.
    println!("connecting to database ...");
    let db = match Database::connect(&args.url).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("database connect failed: {e}");
            std::process::exit(1);
        }
    };
    println!("connected.");

    // 2. Build HF provider.
    let provider = match HfProvider::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("provider error: {e}");
            eprintln!("hint: set HF_TOKEN before running");
            std::process::exit(1);
        }
    };

    // 3. Build agent.
    let config = AgentConfig {
        model: args.model.clone(),
        max_iterations: 8,
        max_tokens: 1024,
        system_preamble: SYSTEM_PREAMBLE.to_string(),
    };
    let agent = Agent::new(Box::new(provider), config);

    // 4. Build App.
    let mut app = App::new(db, agent, args.mode, "shop".to_string(), args.guard, 100);

    println!("mode: {}", args.mode);
    println!("model: {}", args.model);
    println!();

    // 5. Schema learning.
    if args.learn {
        println!("=== /learn ===");
        if args.approve {
            let _ = app.handle_line("/learn", &mut AutoApprove).await;
        } else {
            let _ = app.handle_line("/learn", &mut AutoReject).await;
        }
        // Print what was recorded.
        for e in app.transcript() {
            render_entry(e);
        }
        if let Some(schema) = app.schema() {
            println!("  schema: {} table(s) learned", schema.tables.len());
            let catalog = schema.compact_catalog();
            if !catalog.is_empty() {
                println!("  catalog:\n{catalog}");
            }
        }
        println!();
    }

    // 6. Read lines from stdin.
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.expect("stdin read error");
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        println!("==============================================================");
        println!(">>> {trimmed}");
        println!();

        let before = app.transcript().len();

        if args.approve {
            if let Err(e) = app.handle_line(trimmed, &mut AutoApprove).await {
                eprintln!("handle_line error: {e}");
            }
        } else if let Err(e) = app.handle_line(trimmed, &mut AutoReject).await {
            eprintln!("handle_line error: {e}");
        }

        // Print new transcript entries.
        let entries = app.transcript();
        for e in &entries[before..] {
            render_entry(e);
        }
        println!();

        // Print last result.
        print_result(app.last_result());

        // Print cumulative usage.
        let u = app.usage();
        println!(
            "  usage (cumulative): {} in + {} out = {} total",
            u.input_tokens,
            u.output_tokens,
            u.input_tokens + u.output_tokens
        );
        println!();
    }

    // 7. Final summary.
    println!("==============================================================");
    let u = app.usage();
    println!(
        "TOTAL USAGE: {} input + {} output = {} tokens",
        u.input_tokens,
        u.output_tokens,
        u.input_tokens + u.output_tokens
    );
}
