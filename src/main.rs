mod apikeys;
mod autodetect;
mod bests;
mod config;
mod db;
mod digest;
mod garmin;
mod importer;
mod llm;
mod metrics;
mod performance;
mod planned;
mod power_estimate;
mod report;
mod route_profile;
mod route_thumb;
mod segments;
mod settings;
mod strava;
mod sync;
mod web;
mod workout;
mod zwo;

use clap::{Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(name = "cyclai")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch new activities from the source chosen in /settings (Strava or
    /// Garmin), run deterministic calcs. Triggered daily by a systemd timer.
    Sync {
        /// Walk the whole Strava history (not just activities since the last
        /// sync) and import whatever isn't stored yet — e.g. past runs.
        #[arg(long)]
        full: bool,
    },
    /// Run the web dashboard (UI only, no API) — keep it on a private network.
    Serve {
        /// Address to listen on.
        #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:3000")]
        bind: String,
    },
    /// Run the JSON API only (`/api/v1`), guarded by RS256 bearer tokens — the
    /// process that can be exposed. Keys are managed with `api-key`.
    ServeApi {
        /// Address to listen on.
        #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:3001")]
        bind: String,
    },
    /// Manage the public keys allowed to call the API.
    #[command(subcommand)]
    ApiKey(ApiKeyCommand),
    /// One-time (or re-run as needed) import of starred Strava segments'
    /// coordinates into reference_segments. Starring itself is manual.
    ImportSegments,
}

#[derive(Subcommand)]
enum ApiKeyCommand {
    /// Register a client's RSA public key (PEM file, or `-` for stdin).
    Add {
        /// Key name; clients put it in the JWT `kid` header.
        name: String,
        pem_file: String,
        /// `read` (GET only) or `write` (also POST/DELETE, e.g. planning).
        #[arg(long, default_value = "read")]
        scope: String,
    },
    /// List registered keys.
    List,
    /// Revoke a key (effective on its next request).
    Revoke { name: String },
    /// Mint a short-lived token from a private key — to test the API by hand.
    Token {
        /// Key name registered with `add` (JWT `kid`).
        name: String,
        /// RSA private key (PEM file).
        private_key_file: String,
        /// Validity in seconds (max 300).
        #[arg(long, default_value_t = 60)]
        ttl: i64,
    },
}

async fn api_key_command(pool: &sqlx::SqlitePool, cmd: ApiKeyCommand) -> anyhow::Result<()> {
    match cmd {
        ApiKeyCommand::Add {
            name,
            pem_file,
            scope,
        } => {
            let pem = if pem_file == "-" {
                std::io::read_to_string(std::io::stdin())?
            } else {
                std::fs::read_to_string(&pem_file)?
            };
            apikeys::add(pool, &name, &pem, apikeys::Scope::parse(&scope)?).await?;
            println!("key '{name}' added ({scope})");
        }
        ApiKeyCommand::List => {
            for (name, scope, created, last_used, revoked) in apikeys::list(pool).await? {
                println!(
                    "{name}	{scope}	created {created}	last used {}	{}",
                    last_used.as_deref().unwrap_or("never"),
                    revoked.map_or("active".to_string(), |r| format!("revoked {r}")),
                );
            }
        }
        ApiKeyCommand::Revoke { name } => {
            if apikeys::revoke(pool, &name).await? {
                println!("key '{name}' revoked");
            } else {
                anyhow::bail!("no active key named '{name}'");
            }
        }
        ApiKeyCommand::Token {
            name,
            private_key_file,
            ttl,
        } => {
            let pem = std::fs::read_to_string(&private_key_file)?;
            println!("{}", apikeys::mint_token(&pem, &name, ttl)?);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::seed_athlete_config(&pool, &config).await?;
    db::seed_settings(&pool, &config).await?;
    db::seed_sync_state(&pool, &config).await?;

    match cli.command {
        Command::Sync { full } => {
            if full {
                sync::run_full(&pool).await?;
            } else {
                sync::run(&pool).await?;
            }
        }
        Command::Serve { bind } => {
            web::serve(pool, &bind).await?;
        }
        Command::ServeApi { bind } => {
            web::serve_api(pool, &bind).await?;
        }
        Command::ApiKey(cmd) => {
            api_key_command(&pool, cmd).await?;
        }
        Command::ImportSegments => {
            segments::import_starred(&pool).await?;
        }
    }

    Ok(())
}
