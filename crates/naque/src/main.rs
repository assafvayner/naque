//! `naque` binary entry point.

mod cli;
mod help;

use std::io::IsTerminal;

use clap::Parser;
use naque::setup::{LaunchConfig, SetupError, build_app};
use naque_tui::Theme;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    let runtime = tokio::runtime::Runtime::new()?;

    let launch = LaunchConfig {
        profile: args.profile().map(str::to_string),
        env: args.env.clone(),
        url: args.url.clone(),
        mode: args.mode.clone(),
        provider: args.provider.clone(),
        model: args.model.clone(),
        no_guard: args.no_guard,
    };

    let app = match runtime.block_on(build_app(&launch)) {
        Ok(app) => app,
        Err(err) => return handle_startup_error(err, &args),
    };

    let theme = if args.no_color {
        Theme::new(false)
    } else {
        Theme::detect()
    };
    naque::ui::run(app, theme, &runtime)
}

/// Render startup failures. A missing connection becomes friendly guidance
/// (bare launch, stdout, exit 0) or a formatted error (stderr, exit 1);
/// anything else propagates to anyhow's default reporting.
fn handle_startup_error(err: anyhow::Error, args: &cli::Args) -> anyhow::Result<()> {
    if !matches!(err.downcast_ref::<SetupError>(), Some(SetupError::NoConnection)) {
        return Err(err);
    }

    let no_color_env = std::env::var_os("NO_COLOR").is_some();
    if args.is_bare() {
        let color = help::color_enabled(args.no_color, no_color_env, std::io::stdout().is_terminal());
        println!("{}", help::render_getting_started(color));
        Ok(())
    } else {
        let color = help::color_enabled(args.no_color, no_color_env, std::io::stderr().is_terminal());
        eprintln!("{}", help::render_no_connection_error(color));
        std::process::exit(1);
    }
}
