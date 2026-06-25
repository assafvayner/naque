//! Wires CLI args → profile resolution → App + Theme.

use anyhow::{anyhow, Context};
use naque::App;
use naque_core::PermissionMode;
use naque_db::Database;
use naque_llm::{Agent, AgentConfig, ClaudeProvider, GeminiProvider, HfProvider, OllamaProvider, OpenAIProvider};
use naque_profile::{NaqueConfig, Overrides, Store, SystemSecrets};
use naque_tui::Theme;

use crate::cli::Args;
use crate::help::NoConnection;

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

    // 4. Require a connection URL. The binary renders this into friendly guidance (bare launch) or a formatted error
    //    (see `help`).
    let url = resolved.connection_url.ok_or_else(|| NoConnection { bare: args.is_bare() })?;

    // 5. Connect to the database.
    let db = Database::connect(&url).await.context("database connection failed")?;

    // 6. Permission mode.
    let mode = match resolved.config.mode.as_deref() {
        Some(s) => s
            .parse::<PermissionMode>()
            .map_err(|e| anyhow!("invalid permission mode {:?}: {}", s, e))?,
        None => PermissionMode::Default,
    };
    let catastrophic_guard = !args.no_guard;
    let row_cap = resolved.config.row_cap.unwrap_or(1000) as usize;

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
            return Err(anyhow!("unknown provider {other:?}; expected one of: claude, openai, gemini, hf, ollama"))
        },
        None => {
            return Err(anyhow!(
                "no AI provider configured and no known API key found — set one of \
                 ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY, or HF_TOKEN, \
                 or set `provider` in your config/profile or pass --provider"
            ))
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
        system_preamble: SYSTEM_PREAMBLE.to_string(),
    };

    // 9. Build agent.
    let agent = Agent::new(provider, agent_config);

    // 10. Profile name for the status bar.
    let profile_name = resolved.active_profile.as_deref().unwrap_or("(none)").to_string();

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
}
