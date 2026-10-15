mod bests;
mod config;
mod db;
mod digest;
mod importer;
mod llm;
mod metrics;
mod performance;
mod planned;
mod report;
mod route_profile;
mod route_thumb;
mod segments;
mod settings;
mod strava;
mod sync;
mod web;
mod workout;

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
    /// Fetch new Strava activities/streams, run deterministic calcs, generate per-activity bilans.
    /// Triggered daily by a systemd timer.
    Sync {
        /// Walk the whole Strava history (not just activities since the last
        /// sync) and import whatever isn't stored yet — e.g. past runs.
        #[arg(long)]
        full: bool,
    },
    /// Run the web dashboard.
    Serve,
    /// One-time (or re-run as needed) import of starred Strava segments'
    /// coordinates into reference_segments. Starring itself is manual.
    ImportSegments,
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
    db::backfill_route_thumbnails(&pool).await?;
    bests::backfill(&pool).await?;

    match cli.command {
        Command::Sync { full } => {
            if full {
                sync::run_full(&pool).await?;
            } else {
                sync::run(&pool).await?;
            }
        }
        Command::Serve => {
            web::serve(pool).await?;
        }
        Command::ImportSegments => {
            segments::import_starred(&pool).await?;
        }
    }

    Ok(())
}
