//! Wires CLI args → profile resolution → App + Theme.

use anyhow::{anyhow, Context};
use naque_core::PermissionMode;
use naque_db::Database;
use naque_llm::{Agent, AgentConfig, ClaudeProvider, HfProvider, OllamaProvider, OpenAIProvider};
use naque_profile::{NaqueConfig, Overrides, Store, SystemSecrets};
use naque_tui::Theme;

use naque::App;

use crate::cli::Args;

/// System prompt injected into every agent turn.
pub const SYSTEM_PREAMBLE: &str = "\
You are a careful SQL assistant connected to a relational database. \
You have four tools: inspect_table (schema of a single table), \
sample_table (a few rows for orientation), explain (EXPLAIN plan), \
and run_query (execute SQL and return results). \
A compact schema catalog is appended to each user message — consult it before \
deciding which tables or columns exist. \
Write correct SQL for the target engine. Prefer read-only exploration. \
After running a query, reply with a concise natural-language answer. \
Do not over-plan; act directly on the user's question.";

/// Build an [`App`] and [`Theme`] from the parsed CLI arguments.
///
/// This is async because it needs to connect to the database.
pub async fn build_app(args: &Args) -> anyhow::Result<(App, Theme)> {
    // 1. Open the central store.
    let store = Store::open(Store::default_home().unwrap_or_else(|| ".naque".into()));

    // 2. Build CLI overrides.
    let overrides = Overrides {
        profile: args.profile.clone(),
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

    // 4. Require a connection URL.
    let url = resolved.connection_url.ok_or_else(|| {
        anyhow!(
            "no database connection configured — set a profile, add naque.toml, \
             use --url, or set DATABASE_URL"
        )
    })?;

    // 5. Connect to the database.
    let db = Database::connect(&url)
        .await
        .context("database connection failed")?;

    // 6. Permission mode.
    let mode = match resolved.config.mode.as_deref() {
        Some(s) => s
            .parse::<PermissionMode>()
            .map_err(|e| anyhow!("invalid permission mode {:?}: {}", s, e))?,
        None => PermissionMode::Default,
    };
    let catastrophic_guard = !args.no_guard;
    let row_cap = resolved.config.row_cap.unwrap_or(1000) as usize;

    // 7. Build provider.
    let provider: Box<dyn naque_llm::LlmProvider> = match resolved.config.provider.as_deref() {
        Some("openai") => {
            let p =
                OpenAIProvider::from_env().map_err(|e| anyhow!("OpenAI provider error: {e}"))?;
            Box::new(p)
        }
        Some("ollama") => Box::new(OllamaProvider::new(None)),
        Some("hf") | Some("huggingface") => {
            let p = HfProvider::from_env().map_err(|e| anyhow!("HF provider error: {e}"))?;
            Box::new(p)
        }
        // "claude" / "anthropic" / None → Claude
        _ => {
            let p =
                ClaudeProvider::from_env().map_err(|e| anyhow!("Claude provider error: {e}"))?;
            Box::new(p)
        }
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
        system_preamble: SYSTEM_PREAMBLE.to_string(),
    };

    // 9. Build agent.
    let agent = Agent::new(provider, agent_config);

    // 10. Profile name for the status bar.
    let profile_name = resolved
        .active_profile
        .as_deref()
        .unwrap_or("(none)")
        .to_string();

    // 11. Construct App (schema loaded lazily via /learn).
    let app = App::new(db, agent, mode, profile_name, catastrophic_guard, row_cap);

    // 12. Theme.
    let theme = if args.no_color {
        Theme::new(false)
    } else {
        Theme::detect()
    };

    Ok((app, theme))
}

/// Return a sensible default model name for a given provider string.
fn default_model_for_provider(provider: Option<&str>) -> String {
    match provider {
        Some("openai") => "gpt-4o".to_string(),
        Some("ollama") => "llama3".to_string(),
        Some("hf") | Some("huggingface") => "zai-org/GLM-5.2:together".to_string(),
        // claude / anthropic / None
        _ => "claude-opus-4-8".to_string(),
    }
}
