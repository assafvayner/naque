//! CLI argument parser for the `naque-gui` binary.
//!
//! Mirrors the terminal binary's connection/override knobs so a GUI launch can
//! skip the in-app picker (`naque-gui --url …`, `naque-gui myproj --env dev`).
//! Cosmetic-only flags like `--no-color` are TUI-specific and intentionally
//! absent.

use clap::Parser;
use naque::setup::LaunchConfig;

/// Arguments accepted by `naque-gui`.
#[derive(Parser, Debug)]
#[command(
    name = "naque-gui",
    about = "GUI for naque — natural-language SQL over databases, with a hard read-only safety boundary"
)]
pub struct Args {
    /// Profile to launch (same as --profile), e.g. `naque-gui myproj`.
    #[arg(value_name = "PROFILE")]
    pub profile_arg: Option<String>,

    /// Profile to launch (overrides naque.toml `project` / central default).
    #[arg(long = "profile", value_name = "NAME", conflicts_with = "profile_arg")]
    pub profile_flag: Option<String>,

    /// Environment within the profile to connect to (prod/dev/test).
    #[arg(long)]
    pub env: Option<String>,

    /// Explicit connection string (overrides profile resolution).
    #[arg(long)]
    pub url: Option<String>,

    /// Permission mode: strict | default | readonly | wildcard.
    #[arg(long)]
    pub mode: Option<String>,

    /// Disable the always-on catastrophic guard.
    #[arg(long = "no-guard")]
    pub no_guard: bool,

    /// AI provider override (claude | openai | gemini | hf | ollama).
    #[arg(long)]
    pub provider: Option<String>,

    /// Model name override (e.g. "claude-opus-4-8", "zai-org/GLM-5.2").
    #[arg(long)]
    pub model: Option<String>,
}

impl Args {
    fn profile(&self) -> Option<&str> {
        self.profile_flag.as_deref().or(self.profile_arg.as_deref())
    }

    /// Build the shared [`LaunchConfig`] consumed by `naque::setup::build_app`.
    pub fn launch_config(&self) -> LaunchConfig {
        LaunchConfig {
            profile: self.profile().map(str::to_string),
            env: self.env.clone(),
            url: self.url.clone(),
            mode: self.mode.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            no_guard: self.no_guard,
        }
    }
}
