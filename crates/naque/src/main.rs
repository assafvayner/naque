//! `naque` binary entry point.

mod cli;
mod setup;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    let (app, theme) = runtime.block_on(setup::build_app(&args))?;
    naque::ui::run(app, theme, &runtime)
}
