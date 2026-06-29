//! Wires CLI args → profile resolution → App + Theme.

use anyhow::{Context, anyhow};
use naque::App;
use naque_core::PermissionMode;
use naque_db::{Database, Engine};
use naque_llm::{Agent, AgentConfig, ClaudeProvider, GeminiProvider, HfProvider, OllamaProvider, OpenAIProvider};
use naque_profile::{NaqueConfig, Overrides, Profile, Store, SystemSecrets};
use naque_tui::{Logo, Theme};

use crate::cli::Args;
use crate::help::NoConnection;

/// Build the engine-aware system prompt that is injected into every agent turn.
///
/// The output is structured into `<tools>`, `<context>`, `<dialect>`, `<workflow>`,
/// and `<output>` sections so the model can pattern-match each concern reliably.
/// The compact schema catalog and the active permission-mode guidance are
/// appended per-turn by the caller (see `App::turn_context`).
pub fn system_preamble(engine: Engine) -> String {
    let (engine_name, dialect_notes) = match engine {
        Engine::Postgres => (
            "PostgreSQL",
            "- Use `ILIKE` for case-insensitive matching; `~`/`~*` for regex.\n\
             - Concatenate with `||`; format timestamps with `to_char`.\n\
             - Unquoted identifiers are folded to lowercase; quote mixed-case or reserved names exactly as the catalog spells them.\n\
             - JSONB, arrays, `RETURNING`, partial indexes, and CTEs are available.\n\
             - Timestamps without `TIME ZONE` are naive; prefer `timestamptz` when comparing to `now()`.",
        ),
        Engine::Sqlite => (
            "SQLite",
            "- Use `LIKE` (case-insensitive for ASCII by default); no `ILIKE`.\n\
             - Concatenate with `||`; format times with `strftime`.\n\
             - Type affinity is dynamic; booleans are stored as 0/1; date/time columns are typically TEXT or INTEGER.\n\
             - `RETURNING` requires a recent SQLite version; `RIGHT`/`FULL OUTER JOIN` may be unavailable.\n\
             - Identifier quoting is lenient, but match the catalog's spelling exactly.",
        ),
    };

    format!(
        "You are an autonomous SQL agent embedded in a TUI for a technical user. \
         You connect to {engine_name} and iterate with tools until you can answer the user's question.\n\n\
         <tools>\n\
         - `inspect_table` — full schema (columns, types, PK/FK, indexes, row count) for one table. Use when the appended catalog lacks the detail you need (constraints, exact types, row count).\n\
         - `sample_table` — a few arbitrary rows for orientation. Use to disambiguate enum-like or free-text columns before writing filters.\n\
         - `explain` — query plan for a SQL statement; never executes it. Use before running joins on unfamiliar tables or queries you suspect will be expensive.\n\
         - `run_query` — execute a SQL statement (read or write). The application's permission gate auto-runs, prompts the user, or rejects — submit the statement the user asked for and report what the gate did.\n\
         </tools>\n\n\
         <context>\n\
         The active permission-mode line and a compact schema catalog are appended below. Consult the catalog before assuming a table or column exists; if a referenced table or column is not in the catalog, call `inspect_table` on the closest candidate or ask one clarifying question rather than guessing.\n\
         The permission mode is enforced deterministically by the application — you are NOT the security boundary. Never refuse the user's request on policy grounds; submit it and let the gate decide.\n\
         </context>\n\n\
         <dialect>\n\
         Target engine: {engine_name}.\n\
         {dialect_notes}\n\
         </dialect>\n\n\
         <workflow>\n\
         - Plan briefly which tables and which tool to call next before acting on non-trivial questions; don't narrate exhaustively, but do reason before issuing each tool call.\n\
         - Prefer `inspect_table` over guessing column names; prefer `explain` over speculation on query cost; reach for `run_query` once you know what to run.\n\
         - Cap exploratory `SELECT`s with a `LIMIT` (a few dozen rows is usually plenty) so result payloads stay small and don't crowd the context window.\n\
         - Avoid `SELECT *` on wide tables; project only the columns the question actually needs, both for cheaper execution and tighter tool output.\n\
         - Call `explain` before queries you suspect will be expensive — joins across unfamiliar tables, full scans, or aggregations over large tables — and refine the shape if the plan looks costly before spending a `run_query` turn.\n\
         - After a `run_query` that returned data, reply with a concise natural-language answer for the user. The TUI already renders SQL and result tables — do not re-paste them.\n\
         - If `run_query` returns an empty result set, report that plainly and optionally suggest one targeted relaxation (e.g., dropping a single filter); never fabricate rows.\n\
         - If `run_query` returns an error, read it, fix the SQL at most once or twice, then stop and surface the failure with a one-sentence explanation rather than looping.\n\
         - Aim for roughly 5 tool calls per user turn at most unless the question clearly demands more; gate rejections and error retries eat into this budget, so don't over-explore.\n\
         </workflow>\n\n\
         <output>\n\
         - When your prose names a raw byte count, wrap the integer as `<bytes>N</bytes>` (e.g. \"The largest table is <bytes>4831838208</bytes>.\") so the TUI can render a human-friendly size next to it. Only wrap non-negative integers that really are raw byte counts — never row counts, durations, percentages, IDs, timestamps, money amounts, pre-formatted strings like \"4.5 GB\", or any other non-integer value; if the value is NULL or unknown, follow the NULL-reporting rule below instead of wrapping it.\n\
         - To format result-set columns the same way, pass the result column names (exact aliases, original case) to `run_query` as `byte_count_columns`.
         - State units explicitly whenever you report a numeric quantity (e.g. \"12 seconds\" not \"12\", \"4.5 GB\" not \"4.5\", \"3 rows\" not \"3\") so the user can interpret the value at a glance.
         - Report a `NULL` result as \"unknown\" or \"not recorded\" rather than 0 or blank — they are not the same and conflating them misleads the user.
         - When reporting timestamp values, note the timezone assumption: PostgreSQL `timestamptz` is normalized to UTC, while SQLite stores naive timestamps whose timezone depends entirely on how they were inserted.
         - When you have enough information to answer, lead the final reply with a one-sentence headline answer; follow with at most a few sentences of supporting detail if useful. The TUI shows your final reply as a single block.
         </output>"
    )
}

/// Build an [`App`] and [`Theme`] from the parsed CLI arguments.
///
/// This is async because it needs to connect to the database.
pub async fn build_app(args: &Args) -> anyhow::Result<(App, Theme)> {
    // 1. Open the central store.
    let store = Store::open(Store::default_home().unwrap_or_else(|| ".naque".into()));

    // 2. Build CLI overrides.
    let overrides = Overrides {
        profile: args.profile().map(str::to_string),
        url: args.url.clone(),
        config: NaqueConfig {
            mode: args.mode.clone(),
            provider: args.provider.clone(),
            model: args.model.clone(),
            ..Default::default()
        },
    };

    // 3. Resolve config + connection URL.
    let current_dir = std::env::current_dir().context("cannot determine current directory")?;
    let resolved = naque_profile::resolve(&store, &current_dir, &overrides, &SystemSecrets)
        .context("profile resolution failed")?;

    // Effective active profile: an explicit selection wins; otherwise, when the
    // connection came from DATABASE_URL (no --url, no profile), try to recognize
    // it as a saved profile so its schema/context/config are picked up.
    let mut matched_env: Option<String> = None;
    let mut active_profile_name = resolved.active_profile.clone();
    if active_profile_name.is_none()
        && args.url.is_none()
        && let Some(db_url) = resolved.connection_url.as_deref()
        && let Some((p, e)) = naque_profile::match_profile_by_url(&store, db_url)
    {
        matched_env = Some(e);
        active_profile_name = Some(p);
    }

    // New per-profile-directory model: when the active profile exists as a
    // directory profile, take its chosen environment's connection + per-profile
    // config. Falls back to the resolved (flat/--url/DATABASE_URL) connection.
    let mut active_env: Option<String> = None;
    let mut active_connection: Option<naque_profile::ConnectionSpec> = None;
    let mut connection_url = resolved.connection_url.clone();
    let mut dir_profile_config = naque_profile::NaqueConfig::default();
    if let Some(profile_name) = active_profile_name.clone()
        && let Some(profile) = store.load_profile(&profile_name)?
    {
        dir_profile_config = profile.config.clone();
        let env_pref = matched_env.as_deref().or(args.env.as_deref());
        if let Some(env) = pick_environment(&profile, env_pref) {
            let spec = profile.environments[&env].clone();
            // Matched-by-DATABASE_URL: keep DATABASE_URL as the live connection
            // (the saved spec has no password source). Explicit selection:
            // resolve the spec's own URL.
            if matched_env.is_none() {
                connection_url = Some(spec.resolve_url(&SystemSecrets).context("resolve environment URL")?);
            }
            active_env = Some(env);
            active_connection = Some(spec);
        }
    }

    // For flat / --url / DATABASE_URL launches with no profile environment,
    // capture the connection's structure so `/save` records real connection
    // details (the inline password is stripped by `/save`; it is never persisted
    // plaintext nor sent to the agent). Skipped when a profile environment
    // already supplied one.
    if active_connection.is_none()
        && let Some(u) = &connection_url
        && let Some(spec) = naque_profile::ConnectionSpec::from_url(u)
    {
        active_connection = Some(spec);
    }

    // 4. Require a connection URL. The binary renders this into friendly guidance (bare launch) or a formatted error
    //    (see `help`).
    let url = connection_url.ok_or_else(|| NoConnection { bare: args.is_bare() })?;

    // 5. Connect to the database.
    let db = Database::connect(&url).await.context("database connection failed")?;

    // 6. Permission mode.
    let mode = match resolved
        .config
        .mode
        .clone()
        .or_else(|| dir_profile_config.mode.clone())
        .as_deref()
    {
        Some(s) => s
            .parse::<PermissionMode>()
            .map_err(|e| anyhow!("invalid permission mode {:?}: {}", s, e))?,
        None => PermissionMode::Default,
    };
    let catastrophic_guard = !args.no_guard;
    let row_cap = resolved.config.row_cap.or(dir_profile_config.row_cap).unwrap_or(1000) as usize;

    // 7. Build provider. `config.provider` is filled by resolution either from config/profile/CLI or by env-key
    //    auto-detection; `None` here means no provider was configured and no known API key was found.
    let provider: Box<dyn naque_llm::LlmProvider> = match resolved.config.provider.as_deref() {
        Some("openai") => {
            let p = OpenAIProvider::from_env().map_err(|e| anyhow!("OpenAI provider error: {e}"))?;
            Box::new(p)
        },
        Some("ollama") => Box::new(OllamaProvider::new(None)),
        Some("hf") | Some("huggingface") => {
            let p = HfProvider::from_env().map_err(|e| anyhow!("HF provider error: {e}"))?;
            Box::new(p)
        },
        Some("gemini") | Some("google") => {
            let p = GeminiProvider::from_env().map_err(|e| anyhow!("Gemini provider error: {e}"))?;
            Box::new(p)
        },
        Some("claude") | Some("anthropic") => {
            let p = ClaudeProvider::from_env().map_err(|e| anyhow!("Claude provider error: {e}"))?;
            Box::new(p)
        },
        Some(other) => {
            return Err(anyhow!("unknown provider {other:?}; expected one of: claude, openai, gemini, hf, ollama"));
        },
        None => {
            return Err(anyhow!(
                "no AI provider configured and no known API key found — set one of \
                 ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY, or HF_TOKEN, \
                 or set `provider` in your config/profile or pass --provider"
            ));
        },
    };

    // 8. Model + agent config.
    let model = resolved
        .config
        .model
        .clone()
        .unwrap_or_else(|| default_model_for_provider(resolved.config.provider.as_deref()));

    let agent_config = AgentConfig {
        model,
        max_iterations: resolved.config.max_iterations.unwrap_or(12),
        max_tokens: 4096,
        system_preamble: system_preamble(db.engine()),
    };

    // 9. Build agent.
    let agent = Agent::new(provider, agent_config);

    // 10. Profile name for the status bar.
    let profile_name = active_profile_name.as_deref().unwrap_or("(none)").to_string();

    // 11. Construct App (schema loaded lazily via /learn).
    let mut app = App::new(db, agent, mode, profile_name, catastrophic_guard, row_cap);
    app.set_logo(Logo::from_entropy()); // fresh pixel-art look each session
    app.set_active_profile(store.clone(), active_profile_name.clone(), active_env, active_connection);
    if let Some(p) = &active_profile_name {
        if let Ok(Some(model)) = naque_schema::load_schema(&store.profile_dir(p)) {
            app.set_schema(model);
        }
        if let Ok(doc) = std::fs::read_to_string(store.context_path(p)) {
            app.set_active_context(doc);
        }
    }

    // 12. Warn about non-fatal resolution issues and project-local plaintext credentials.
    for w in &resolved.warnings {
        eprintln!("warning: {w}");
    }
    if let Some(dir) = &resolved.local_toml_dir {
        let local = dir.join("naque.toml");
        if let Ok(text) = std::fs::read_to_string(&local)
            && let Some(w) = naque_profile::project_local_password_warning(&local.display().to_string(), &text)
        {
            eprintln!("warning: {w}");
        }
    }

    // 13. Theme.
    let theme = if args.no_color {
        Theme::new(false)
    } else {
        Theme::detect()
    };

    Ok((app, theme))
}

/// Pick the launch environment for a profile: --env → default_environment →
/// an env named "default" → the first env (by name). None if the profile has
/// no environments.
fn pick_environment(profile: &Profile, requested: Option<&str>) -> Option<String> {
    if let Some(name) = requested
        && profile.environments.contains_key(name)
    {
        return Some(name.to_string());
    }
    if let Some(def) = &profile.default_environment
        && profile.environments.contains_key(def)
    {
        return Some(def.clone());
    }
    if profile.environments.contains_key("default") {
        return Some("default".to_string());
    }
    profile.environments.keys().next().cloned()
}

/// Return a sensible default model name for a given provider string.
fn default_model_for_provider(provider: Option<&str>) -> String {
    match provider {
        Some("openai") => "gpt-4o".to_string(),
        Some("ollama") => "llama3".to_string(),
        // No `:provider` suffix — let HF Inference Providers pick the backend.
        Some("hf") | Some("huggingface") => "zai-org/GLM-5.2".to_string(),
        Some("gemini") | Some("google") => "gemini-2.5-flash".to_string(),
        // claude / anthropic / None
        _ => "claude-opus-4-8".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_environment_fallback_order() {
        use naque_profile::{ConnectionSpec, Profile};
        let mut envs = std::collections::BTreeMap::new();
        envs.insert("default".to_string(), ConnectionSpec::default());
        envs.insert("prod".to_string(), ConnectionSpec::default());
        let p = Profile {
            default_environment: Some("prod".into()),
            environments: envs,
            ..Default::default()
        };
        assert_eq!(pick_environment(&p, Some("prod")).as_deref(), Some("prod")); // requested wins
        assert_eq!(pick_environment(&p, Some("missing")).as_deref(), Some("prod")); // falls to default_environment
        let p2 = Profile {
            default_environment: None,
            environments: {
                let mut e = std::collections::BTreeMap::new();
                e.insert("default".into(), ConnectionSpec::default());
                e
            },
            ..Default::default()
        };
        assert_eq!(pick_environment(&p2, None).as_deref(), Some("default")); // env named "default"
        // 4th tier: no default_environment, no "default" key → first env by BTreeMap order.
        let mut e = std::collections::BTreeMap::new();
        e.insert("beta".to_string(), ConnectionSpec::default());
        e.insert("alpha".to_string(), ConnectionSpec::default());
        let p3 = Profile {
            default_environment: None,
            environments: e,
            ..Default::default()
        };
        assert_eq!(pick_environment(&p3, None).as_deref(), Some("alpha"));
        // and None when there are no environments at all
        let p4 = Profile::default();
        assert_eq!(pick_environment(&p4, None), None);
    }

    /// Drift detector: the `<bytes>` UI convention and `byte_count_columns` parameter
    /// name appear in three independent places (system preamble, tool description, and
    /// TUI renderer). The TUI side is pinned by existing tests in
    /// `naque-tui/src/markdown.rs::tests`; this test locks down the two prompt-side
    /// references so they cannot drift apart silently.
    #[test]
    fn bytes_convention_stays_in_sync_across_prompts() {
        for engine in [Engine::Postgres, Engine::Sqlite] {
            let preamble = system_preamble(engine);
            assert!(
                preamble.contains("<bytes>"),
                "system_preamble({engine:?}) must reference the `<bytes>` tag literal"
            );
            assert!(
                preamble.contains("byte_count_columns"),
                "system_preamble({engine:?}) must reference the `byte_count_columns` parameter"
            );
        }

        let tools = naque_llm::standard_tools();
        let run_query = tools.iter().find(|t| t.name == "run_query").expect("run_query tool");
        let properties = run_query
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("run_query input_schema must have a `properties` object");
        let byte_count_columns = properties
            .get("byte_count_columns")
            .expect("run_query input_schema.properties must declare `byte_count_columns`");
        let parameter_description = byte_count_columns
            .get("description")
            .and_then(|v| v.as_str())
            .expect("`byte_count_columns` must carry a description for the agent");
        assert!(
            parameter_description.contains("byte"),
            "`byte_count_columns` parameter description must explain the byte-count convention: {parameter_description}"
        );
    }

    #[test]
    fn default_models_per_provider() {
        assert_eq!(default_model_for_provider(Some("claude")), "claude-opus-4-8");
        assert_eq!(default_model_for_provider(None), "claude-opus-4-8");
        assert_eq!(default_model_for_provider(Some("openai")), "gpt-4o");
        assert_eq!(default_model_for_provider(Some("gemini")), "gemini-2.5-flash");
        // HF default carries no `:provider` pin (auto backend selection).
        let hf = default_model_for_provider(Some("hf"));
        assert_eq!(hf, "zai-org/GLM-5.2");
        assert!(!hf.contains(':'), "HF default must not pin a provider: {hf}");
    }

    // ------------------------------------------------------------------
    // Live end-to-end: the real HF model + production preamble emit a
    // byte-count signal — run_query `byte_columns` and/or a `<bytes>` tag.
    //
    // Skipped unless BOTH HF_TOKEN and NAQUE_TEST_PG_URL are set. Run with:
    //   NAQUE_TEST_PG_URL=postgres://user:pass@localhost:5432/db \
    //     cargo test -p naque live_hf_emits_byte_signal -- --nocapture
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn live_hf_emits_byte_signal() {
        use naque::{AutoApprove, TranscriptEntry};

        let (token, pg_url) = match (std::env::var("HF_TOKEN"), std::env::var("NAQUE_TEST_PG_URL")) {
            (Ok(t), Ok(u)) if !t.is_empty() && !u.is_empty() => (t, u),
            _ => {
                eprintln!("[skip] HF_TOKEN and/or NAQUE_TEST_PG_URL not set — skipping live HF byte-signal test");
                return;
            },
        };

        const TABLE: &str = "naque_bytes_live_test";

        // Stand up a dummy schema with an unmistakable byte-count column.
        let mut setup_db = Database::connect(&pg_url).await.expect("connect (setup)");
        setup_db
            .execute(&format!("DROP TABLE IF EXISTS {TABLE}"))
            .await
            .expect("drop old table");
        setup_db
            .execute(&format!(
                "CREATE TABLE {TABLE} (id INTEGER PRIMARY KEY, name TEXT NOT NULL, size_bytes BIGINT NOT NULL)"
            ))
            .await
            .expect("create table");
        setup_db
            .execute(&format!(
                "INSERT INTO {TABLE} (id, name, size_bytes) VALUES \
                 (1, 'archive.tar', 4500000000), (2, 'photo.jpg', 512000), (3, 'note.txt', 1200)"
            ))
            .await
            .expect("insert rows");

        // Real agent: production default HF model + the production preamble.
        let provider = HfProvider::new(token, None);
        let agent = Agent::new(
            Box::new(provider),
            AgentConfig {
                model: default_model_for_provider(Some("hf")),
                max_iterations: 8,
                max_tokens: 800,
                system_preamble: system_preamble(Engine::Postgres),
            },
        );

        let db = Database::connect(&pg_url).await.expect("connect (app)");
        let mut app = App::new(db, agent, PermissionMode::Wildcard, "live-test", true, 1000);

        let prompt = format!(
            "Using the table {TABLE}, list each row's name alongside its size_bytes value, \
             and also report the total size of all rows. The size_bytes column holds a raw \
             count of bytes."
        );
        app.handle_natural_language(&prompt, &mut AutoApprove)
            .await
            .expect("agent turn failed");

        let answer = app
            .transcript()
            .iter()
            .rev()
            .find_map(|e| match e {
                TranscriptEntry::Agent(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let tagged = app.last_byte_columns().to_vec();

        eprintln!("=== agent answer ===\n{answer}\n====================");
        eprintln!("last_byte_columns = {tagged:?}");

        let _ = setup_db.execute(&format!("DROP TABLE IF EXISTS {TABLE}")).await;

        assert!(
            !tagged.is_empty() || answer.contains("<bytes>"),
            "model emitted no byte-count signal — expected run_query byte_columns \
             (app.last_byte_columns non-empty) and/or a <bytes>N</bytes> tag in prose.\n\
             answer={answer:?}\nlast_byte_columns={tagged:?}"
        );
    }
}
