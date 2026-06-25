//! Command-line argument parser for the `naque` binary.

/// Arguments accepted by `naque`.
#[derive(clap::Parser, Debug)]
#[command(name = "naque", about = "Agentic AI query tool over databases")]
pub struct Args {
    /// Profile name to launch (overrides naque.toml `project` / central default).
    pub profile: Option<String>,

    /// Explicit connection string (overrides profile resolution).
    #[arg(long)]
    pub url: Option<String>,

    /// Permission mode: strict | default | readonly | wildcard.
    #[arg(long)]
    pub mode: Option<String>,

    /// Disable the always-on catastrophic guard (--yolo).
    #[arg(long = "no-guard")]
    pub no_guard: bool,

    /// Force no color output.
    #[arg(long = "no-color")]
    pub no_color: bool,

    /// AI provider override (claude | openai | gemini | hf | ollama).
    #[arg(long)]
    pub provider: Option<String>,

    /// Model name override (e.g. "claude-opus-4-8", "zai-org/GLM-5.2").
    #[arg(long)]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn full_args_parse_correctly() {
        let args = Args::try_parse_from([
            "naque",
            "prod",
            "--mode",
            "readonly",
            "--no-guard",
            "--no-color",
        ])
        .expect("parse failed");
        assert_eq!(args.profile.as_deref(), Some("prod"));
        assert_eq!(args.mode.as_deref(), Some("readonly"));
        assert!(args.no_guard);
        assert!(args.no_color);
        assert!(args.url.is_none());
    }

    #[test]
    fn empty_args_all_none_or_false() {
        let args = Args::try_parse_from(["naque"]).expect("parse failed");
        assert!(args.profile.is_none());
        assert!(args.url.is_none());
        assert!(args.mode.is_none());
        assert!(!args.no_guard);
        assert!(!args.no_color);
    }

    #[test]
    fn url_arg_parsed() {
        let args = Args::try_parse_from(["naque", "--url", "postgres://localhost/mydb"]).unwrap();
        assert_eq!(args.url.as_deref(), Some("postgres://localhost/mydb"));
    }

    #[test]
    fn provider_and_model_args_parsed() {
        let args = Args::try_parse_from([
            "naque",
            "--provider",
            "hf",
            "--model",
            "zai-org/GLM-5.2:together",
        ])
        .expect("parse failed");
        assert_eq!(args.provider.as_deref(), Some("hf"));
        assert_eq!(args.model.as_deref(), Some("zai-org/GLM-5.2:together"));
    }
}
