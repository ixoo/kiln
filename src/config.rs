use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub server: ServerSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    pub bind_address: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub settings: Settings,
    pub webhook_secret: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)?;
        let settings = toml::from_str::<Settings>(&raw)?;
        Ok(settings)
    }
}
