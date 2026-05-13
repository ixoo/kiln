use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub server: ServerSettings,
    #[serde(default)]
    pub execution: ExecutionSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    pub bind_address: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionSettings {
    #[serde(default = "default_execution_mode")]
    pub mode: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default = "default_job_image")]
    pub job_image: String,
    #[serde(default = "default_runtime_image")]
    pub default_runtime_image: String,
    #[serde(default)]
    pub service_account_name: Option<String>,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            mode: default_execution_mode(),
            namespace: default_namespace(),
            job_image: default_job_image(),
            default_runtime_image: default_runtime_image(),
            service_account_name: None,
        }
    }
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
    #[error("unsupported execution mode `{0}`; expected `disabled` or `kubectl`")]
    InvalidExecutionMode(String),
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)?;
        let settings = toml::from_str::<Settings>(&raw)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.execution.validate()
    }
}

impl ExecutionSettings {
    pub fn validate(&self) -> Result<(), ConfigError> {
        match self.mode.as_str() {
            "disabled" | "kubectl" => Ok(()),
            other => Err(ConfigError::InvalidExecutionMode(other.to_string())),
        }
    }
}

fn default_execution_mode() -> String {
    "disabled".to_string()
}

fn default_namespace() -> String {
    "default".to_string()
}

fn default_job_image() -> String {
    "ghcr.io/ixoo/kiln-agent:latest".to_string()
}

fn default_runtime_image() -> String {
    "ghcr.io/devcontainers/base:ubuntu".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_execution_mode() {
        let settings = Settings {
            server: ServerSettings {
                bind_address: "127.0.0.1:3000".to_string(),
            },
            execution: ExecutionSettings {
                mode: "kubeclt".to_string(),
                ..Default::default()
            },
        };

        assert!(matches!(
            settings.validate(),
            Err(ConfigError::InvalidExecutionMode(mode)) if mode == "kubeclt"
        ));
    }
}
