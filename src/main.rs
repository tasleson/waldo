// SPDX-License-Identifier: MIT
mod config;
mod dbus;
mod persist;
mod state;
mod webhook;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use crate::config::Config;
use crate::persist::StateFile;
use crate::state::Monitor;
use crate::webhook::WebhookClient;

#[derive(Parser)]
#[command(name = "waldo", about = "Screen lock/unlock webhook notifier")]
struct Cli {
    /// Path to TOML config file
    #[arg(short, long, default_value = "~/.config/waldo/config.toml")]
    config: String,
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let rest = path.strip_prefix("~/").unwrap_or("");
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "waldo=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config_path = expand_tilde(&cli.config);

    let config = Config::load(&config_path)?;
    tracing::info!("Loaded config from {}", config_path.display());
    let config = Arc::new(config);

    let conn = zbus::Connection::system().await?;
    tracing::info!("Connected to system D-Bus");

    let session_path = dbus::discover_session(&conn).await?;
    tracing::info!("Monitoring session at {session_path}");

    let session = dbus::session_proxy(&conn, session_path).await?;
    let webhook = WebhookClient::new(&config);
    let state_path = StateFile::default_path();
    tracing::info!("Using state file {}", state_path.display());
    let state_file = StateFile::load(state_path);
    let mut monitor = Monitor::new(config, config_path, webhook, state_file);

    let locked = match session.locked_hint().await {
        Ok(locked) => locked,
        Err(e) => {
            tracing::warn!("Failed to read initial LockedHint, assuming unlocked: {e}");
            false
        }
    };
    monitor.startup(locked).await;

    monitor.run(&session).await
}
