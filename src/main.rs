use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use mdsync::config::Config;
use mdsync::drive::auth::{self, OauthClient};
use mdsync::sync;
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    // Printed rather than returned from main: a config typo should read as one
    // line of explanation, not a backtrace (RUST_BACKTRACE is on in the dev shell).
    if let Err(err) = run(cli.command) {
        eprintln!("error: {err:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Validate(args) => validate(&args),
        Command::Sync(args) => run_sync(&args),
        Command::Auth(args) => mint_token(&args),
    }
}

fn validate(args: &ConfigArgs) -> Result<()> {
    let config = Config::load(&args.config)
        .with_context(|| format!("validating {}", args.config.display()))?;

    println!("{} is valid.", args.config.display());
    println!(
        "  {} repositories -> Drive folder {}",
        config.repos.len(),
        config.drive.folder_id
    );
    for repo in &config.repos {
        println!(
            "  - {:<24} {}{}",
            repo.name,
            repo.url,
            if repo.private { " (private)" } else { "" }
        );
    }

    Ok(())
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

fn run_sync(args: &SyncArgs) -> Result<()> {
    let config = Config::load(&args.config.config)
        .with_context(|| format!("loading {}", args.config.config.display()))?;

    let options = sync::Options {
        dry_run: args.dry_run,
        out: args.out.clone(),
        only: args.only.clone(),
    };

    let documents = sync::build(&config, &options)?;

    let written = sync::write_local(&documents, &options.out)?;
    for path in &written {
        println!("wrote {}", path.display());
    }

    if options.dry_run {
        println!(
            "dry run: {} document(s), nothing published",
            documents.len()
        );
        return Ok(());
    }

    sync::publish(&config, &documents, &options)
}

fn mint_token(args: &AuthArgs) -> Result<()> {
    let client = OauthClient::new(args.client_id.clone(), args.client_secret.clone())?;
    let refresh_token = client.mint_refresh_token(args.port)?;

    println!(
        "\nRefresh token minted. Store it as the {} repository secret:\n",
        auth::REFRESH_TOKEN_VAR
    );
    println!("{refresh_token}\n");
    println!(
        "It does not expire on a schedule, but it will stop working if the app's consent \n\
         screen is left in Testing status, so set it to In production."
    );
    Ok(())
}
