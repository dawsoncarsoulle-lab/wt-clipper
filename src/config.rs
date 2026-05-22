use std::{fs, path::Path, time::Duration};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(alias = "warthunder")]
    pub war_thunder: WarThunderConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WarThunderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub player_name: Option<String>,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }
}

impl WarThunderConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms.clamp(250, 500))
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
}

fn default_base_url() -> String {
    "http://127.0.0.1:8111".to_owned()
}

fn default_poll_interval_ms() -> u64 {
    300
}

fn default_request_timeout_ms() -> u64 {
    500
}
