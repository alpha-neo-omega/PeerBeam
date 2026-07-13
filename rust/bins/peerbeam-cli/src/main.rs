//! PeerBeam CLI entry point.

use clap::Parser;

use peerbeam_cli::cli::Cli;
use peerbeam_cli::commands;
use peerbeam_cli::output::Ctx;

#[tokio::main]
async fn main() {
    std::process::exit(run().await);
}

async fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // clap prints help/usage; its exit code is 0 for --help/--version.
            let _ = e.print();
            return e.exit_code();
        }
    };

    let g = &cli.global;
    let ctx = Ctx::new(g.json, g.no_color, g.verbose, g.quiet, g.yes);
    if ctx.verbose > 0 {
        eprintln!(
            "{}",
            ctx.dim(&format!("peerbeam v{}", env!("CARGO_PKG_VERSION")))
        );
    }
    let cfg_override = g.config.clone();

    match commands::dispatch(cli.command, &ctx, cfg_override).await {
        Ok(()) => 0,
        Err(e) => {
            ctx.error(&e);
            e.code()
        }
    }
}
