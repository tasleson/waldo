use std::path::Path;

use serde::Deserialize;

fn default_min_lock_duration_secs() -> u64 {
    300
}

fn default_cooldown_secs() -> u64 {
    600
}

fn default_online_text() -> String {
    "online".to_string()
}

fn default_offline_text() -> String {
    "offline".to_string()
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub webhook_url: String,
    #[serde(default = "default_min_lock_duration_secs")]
    pub min_lock_duration_secs: u64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    pub display_name: String,
    #[serde(default = "default_online_text")]
    pub online_text: String,
    #[serde(default = "default_offline_text")]
    pub offline_text: String,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config {}: {e}", path.display()))?;
        let config: Config = toml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("failed to parse config {}: {e}", path.display()))?;

        anyhow::ensure!(
            !config.webhook_url.is_empty(),
            "webhook_url must not be empty"
        );
        anyhow::ensure!(
            !config.display_name.is_empty(),
            "display_name must not be empty"
        );
        anyhow::ensure!(
            config.min_lock_duration_secs > 0,
            "min_lock_duration_secs must be > 0"
        );
        anyhow::ensure!(config.cooldown_secs > 0, "cooldown_secs must be > 0");

        Ok(config)
    }
}
