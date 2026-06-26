//! Wires CLI args → profile resolution → App + Theme.

use anyhow::{Context, anyhow};
use naque::App;
use naque_core::PermissionMode;
use naque_db::Database;
use naque_llm::{Agent, AgentConfig, ClaudeProvider, GeminiProvider, HfProvider, OllamaProvider, OpenAIProvider};
use naque_profile::{NaqueConfig, Overrides, Profile, Store, SystemSecrets};
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

    // New per-profile-directory model: when the active profile exists as a
    // directory profile, take its chosen environment's connection + per-profile
    // config. Falls back to the resolved (flat/--url/DATABASE_URL) connection.
    let mut active_env: Option<String> = None;
    let mut active_connection: Option<naque_profile::ConnectionSpec> = None;
    let mut connection_url = resolved.connection_url.clone();
    let mut dir_profile_config = naque_profile::NaqueConfig::default();
    if let Some(profile_name) = resolved.active_profile.clone()
        && let Some(profile) = store.load_profile(&profile_name)?
    {
        dir_profile_config = profile.config.clone();
        if let Some(env) = pick_environment(&profile, args.env.as_deref()) {
            let spec = profile.environments[&env].clone();
            connection_url = Some(spec.resolve_url(&SystemSecrets).context("resolve environment URL")?);
            active_env = Some(env);
            active_connection = Some(spec);
        }
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
        system_preamble: SYSTEM_PREAMBLE.to_string(),
    };

    // 9. Build agent.
    let agent = Agent::new(provider, agent_config);

    // 10. Profile name for the status bar.
    let profile_name = resolved.active_profile.as_deref().unwrap_or("(none)").to_string();

    // 11. Construct App (schema loaded lazily via /learn).
    let mut app = App::new(db, agent, mode, profile_name, catastrophic_guard, row_cap);
    app.set_active_profile(store.clone(), resolved.active_profile.clone(), active_env, active_connection);
    if let Some(p) = &resolved.active_profile {
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
}
