mod config;
mod db;
mod digest;
mod llm;
mod metrics;
mod segments;
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
    Sync,
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
    db::seed_gym_config(&pool).await?;

    match cli.command {
        Command::Sync => {
            sync::run(&pool, &config).await?;
        }
        Command::Serve => {
            web::serve(pool, config).await?;
        }
        Command::ImportSegments => {
            segments::import_starred(&pool, &config).await?;
        }
    }

    Ok(())
}
