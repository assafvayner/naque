//! `naque` binary entry point.

mod cli;
mod help;
mod setup;

use std::io::IsTerminal;

use clap::Parser;
use help::NoConnection;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    let runtime = tokio::runtime::Runtime::new()?;

    let (app, theme) = match runtime.block_on(setup::build_app(&args)) {
        Ok(built) => built,
        Err(err) => return handle_startup_error(err, &args),
    };

    naque::ui::run(app, theme, &runtime)
}

/// Render startup failures. A missing connection becomes friendly guidance
/// (bare launch, stdout, exit 0) or a formatted error (stderr, exit 1);
/// anything else propagates to anyhow's default reporting.
fn handle_startup_error(err: anyhow::Error, args: &cli::Args) -> anyhow::Result<()> {
    let Some(no_conn) = err.downcast_ref::<NoConnection>() else {
        return Err(err);
    };

    let no_color_env = std::env::var_os("NO_COLOR").is_some();
    if no_conn.bare {
        let color = help::color_enabled(args.no_color, no_color_env, std::io::stdout().is_terminal());
        println!("{}", help::render_getting_started(color));
        Ok(())
    } else {
        let color = help::color_enabled(args.no_color, no_color_env, std::io::stderr().is_terminal());
        eprintln!("{}", help::render_no_connection_error(color));
        std::process::exit(1);
    }
}
