use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "mdsync",
    about = "Sync Markdown notes from git repositories into Google Drive",
    version
)]
struct Cli {
    /// Increase log verbosity. Repeat for more (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check the config file for errors without touching the network.
    Validate(ConfigArgs),

    /// Clone the configured repositories, render the documents, and publish them.
    Sync(SyncArgs),

    /// One-time interactive flow to mint a Google OAuth refresh token.
    Auth(AuthArgs),
}

#[derive(Debug, Args)]
struct ConfigArgs {
    /// Path to the config file.
    #[arg(short, long, default_value = "repos.yml")]
    config: PathBuf,
}

#[derive(Debug, Args)]
struct SyncArgs {
    #[command(flatten)]
    config: ConfigArgs,

    /// Render documents to disk and skip Google Drive entirely.
    #[arg(long)]
    dry_run: bool,

    /// Directory for rendered Markdown. Always written, dry run or not.
    #[arg(long, default_value = "out")]
    out: PathBuf,

    /// Restrict the run to these repositories, by config name. Repeatable.
    #[arg(long)]
    only: Vec<String>,
}

#[derive(Debug, Args)]
struct AuthArgs {
    /// OAuth client ID. Falls back to $GOOGLE_CLIENT_ID.
    #[arg(long, env = "GOOGLE_CLIENT_ID")]
    client_id: String,

    /// OAuth client secret. Falls back to $GOOGLE_CLIENT_SECRET.
    #[arg(long, env = "GOOGLE_CLIENT_SECRET")]
    client_secret: String,

    /// Port for the loopback redirect listener. 0 picks a free one.
    #[arg(long, default_value_t = 0)]
    port: u16,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Validate(_) => anyhow::bail!("`validate` is not implemented yet"),
        Command::Sync(_) => anyhow::bail!("`sync` is not implemented yet"),
        Command::Auth(_) => anyhow::bail!("`auth` is not implemented yet"),
    }
}

/// `-v` and `-vv` raise the default level; RUST_LOG always wins if set, so CI
/// can turn on debug logging without changing the workflow's command line.
fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default)),
        )
        .with_target(false)
        .without_time()
        .init();
}
