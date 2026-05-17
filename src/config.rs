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
    #[serde(default)]
    pub job_env_from_secret: Option<String>,
    #[serde(default)]
    pub local_command: Vec<String>,
    #[serde(default)]
    pub callback_url: Option<String>,
    #[serde(skip)]
    pub callback_secret: Option<String>,
    #[serde(default = "default_launch_timeout_seconds")]
    pub launch_timeout_seconds: u64,
    #[serde(default = "default_stale_run_seconds")]
    pub stale_run_seconds: u64,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            mode: default_execution_mode(),
            namespace: default_namespace(),
            job_image: default_job_image(),
            default_runtime_image: default_runtime_image(),
            service_account_name: None,
            job_env_from_secret: None,
            local_command: Vec::new(),
            callback_url: None,
            callback_secret: None,
            launch_timeout_seconds: default_launch_timeout_seconds(),
            stale_run_seconds: default_stale_run_seconds(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub settings: Settings,
    pub webhook_secret: String,
    pub agent_callback_secret: Option<String>,
    pub state_secret: String,
    pub previous_state_secrets: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unsupported execution mode `{0}`; expected `disabled`, `local`, or `kubectl`")]
    InvalidExecutionMode(String),
    #[error("execution mode `local` requires at least one `local_command` entry")]
    MissingLocalCommand,
    #[error("execution mode `kubectl` requires `callback_url`")]
    MissingCallbackUrl,
    #[error("execution mode `kubectl` requires `KILN_AGENT_CALLBACK_SECRET`")]
    MissingCallbackSecret,
    #[error("execution timeout values must be greater than zero")]
    InvalidTimeout,
    #[error("{0} must be at least 16 characters and must not use example placeholder values")]
    InvalidSecret(&'static str),
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)?;
        Ok(toml::from_str::<Settings>(&raw)?)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.execution.validate()
    }
}

impl ExecutionSettings {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.launch_timeout_seconds == 0 || self.stale_run_seconds == 0 {
            return Err(ConfigError::InvalidTimeout);
        }

        match self.mode.as_str() {
            "disabled" => Ok(()),
            "kubectl" if self.callback_url.is_none() => Err(ConfigError::MissingCallbackUrl),
            "kubectl" if self.callback_secret.is_none() => Err(ConfigError::MissingCallbackSecret),
            "kubectl" => Ok(()),
            "local" if self.local_command.is_empty() => Err(ConfigError::MissingLocalCommand),
            "local" => Ok(()),
            other => Err(ConfigError::InvalidExecutionMode(other.to_string())),
        }
    }
}

pub fn validate_runtime_secrets(
    webhook_secret: &str,
    state_secret: &str,
    agent_callback_secret: Option<&str>,
) -> Result<(), ConfigError> {
    validate_secret("KILN_GITHUB_WEBHOOK_SECRET", webhook_secret)?;
    validate_secret("KILN_STATE_SECRET", state_secret)?;
    if let Some(secret) = agent_callback_secret {
        validate_secret("KILN_AGENT_CALLBACK_SECRET", secret)?;
    }
    Ok(())
}

fn validate_secret(name: &'static str, value: &str) -> Result<(), ConfigError> {
    let value = value.trim();
    if value.len() < 16 || value.contains("change-me") {
        Err(ConfigError::InvalidSecret(name))
    } else {
        Ok(())
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

fn default_launch_timeout_seconds() -> u64 {
    300
}

fn default_stale_run_seconds() -> u64 {
    3600
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

    #[test]
    fn validates_local_execution_mode() {
        let mut execution = ExecutionSettings {
            mode: "local".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            execution.validate(),
            Err(ConfigError::MissingLocalCommand)
        ));

        execution.local_command = vec!["kiln-agent".to_string()];
        assert!(execution.validate().is_ok());
    }

    #[test]
    fn validates_kubectl_callback_configuration() {
        let mut execution = ExecutionSettings {
            mode: "kubectl".to_string(),
            ..Default::default()
        };

        assert!(matches!(
            execution.validate(),
            Err(ConfigError::MissingCallbackUrl)
        ));

        execution.callback_url = Some("https://kiln.example.com/callbacks/agent".to_string());
        assert!(matches!(
            execution.validate(),
            Err(ConfigError::MissingCallbackSecret)
        ));

        execution.callback_secret = Some("secret".to_string());
        assert!(execution.validate().is_ok());
    }

    #[test]
    fn rejects_empty_short_and_placeholder_runtime_secrets() {
        assert!(matches!(
            validate_runtime_secrets("change-me", "long-enough-state-secret", None),
            Err(ConfigError::InvalidSecret("KILN_GITHUB_WEBHOOK_SECRET"))
        ));
        assert!(matches!(
            validate_runtime_secrets("long-enough-webhook-secret", "short", None),
            Err(ConfigError::InvalidSecret("KILN_STATE_SECRET"))
        ));
        assert!(matches!(
            validate_runtime_secrets(
                "long-enough-webhook-secret",
                "long-enough-state-secret",
                Some("change-me-too")
            ),
            Err(ConfigError::InvalidSecret("KILN_AGENT_CALLBACK_SECRET"))
        ));
    }
}
