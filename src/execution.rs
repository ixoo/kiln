use crate::{command::AgentCommand, config::ExecutionSettings, github::GitHubContext};
use async_trait::async_trait;
use serde::Serialize;
use std::{
    collections::HashMap,
    io::Write,
    process::{Command, Stdio},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct AgentJob {
    pub run_id: String,
    pub repo_full_name: String,
    pub pr_number: u64,
    pub head_sha: String,
    pub requester: String,
    pub command: AgentCommand,
    pub queue_position: usize,
}

impl AgentJob {
    pub fn new(
        run_id: impl Into<String>,
        ctx: &GitHubContext,
        head_sha: impl Into<String>,
        requester: impl Into<String>,
        command: &AgentCommand,
        queue_position: usize,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            repo_full_name: ctx.repo_full_name.clone(),
            pr_number: ctx.pr_number,
            head_sha: head_sha.into(),
            requester: requester.into(),
            command: command.clone(),
            queue_position,
        }
    }

    pub fn queue_key(&self) -> String {
        format!("{}#{}", self.repo_full_name, self.pr_number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLaunchResult {
    pub status: String,
    pub external_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum JobLaunchError {
    #[error("job launcher is disabled")]
    Disabled,
    #[error("failed to run kubectl: {0}")]
    Io(#[from] std::io::Error),
    #[error("kubectl exited with status {status}: {stderr}")]
    Kubectl { status: String, stderr: String },
    #[error("failed to serialize kubernetes job manifest: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("kubectl launch task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[async_trait]
pub trait JobLauncher: Send + Sync {
    async fn launch(&self, job: AgentJob) -> Result<JobLaunchResult, JobLaunchError>;
}

#[derive(Debug, Default)]
pub struct DisabledJobLauncher;

#[async_trait]
impl JobLauncher for DisabledJobLauncher {
    async fn launch(&self, _job: AgentJob) -> Result<JobLaunchResult, JobLaunchError> {
        Ok(JobLaunchResult {
            status: "launch-disabled".to_string(),
            external_id: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct KubectlJobLauncher {
    settings: ExecutionSettings,
}

impl KubectlJobLauncher {
    pub fn new(settings: ExecutionSettings) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl JobLauncher for KubectlJobLauncher {
    async fn launch(&self, job: AgentJob) -> Result<JobLaunchResult, JobLaunchError> {
        let settings = self.settings.clone();
        tokio::task::spawn_blocking(move || kubectl_launch(settings, job)).await?
    }
}

fn kubectl_launch(
    settings: ExecutionSettings,
    job: AgentJob,
) -> Result<JobLaunchResult, JobLaunchError> {
    let manifest = kubernetes_job_manifest(&settings, &job)?;
    let mut child = Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(manifest.as_bytes())?;

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(JobLaunchError::Kubectl {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(JobLaunchResult {
        status: "launched".to_string(),
        external_id: Some(kubernetes_job_name(&job.run_id)),
    })
}

#[derive(Debug, Default)]
pub struct PerPrQueue {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl PerPrQueue {
    pub async fn lock_for(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

#[derive(Debug, Serialize)]
struct KubernetesJob<'a> {
    #[serde(rename = "apiVersion")]
    api_version: &'a str,
    kind: &'a str,
    metadata: KubernetesMetadata<'a>,
    spec: KubernetesJobSpec<'a>,
}

#[derive(Debug, Serialize)]
struct KubernetesMetadata<'a> {
    name: String,
    namespace: &'a str,
    labels: KubernetesLabels<'a>,
}

#[derive(Debug, Serialize)]
struct KubernetesLabels<'a> {
    app: &'a str,
    #[serde(rename = "kiln.dev/run-id")]
    run_id: &'a str,
}

#[derive(Debug, Serialize)]
struct KubernetesJobSpec<'a> {
    template: KubernetesPodTemplate<'a>,
    #[serde(rename = "backoffLimit")]
    backoff_limit: u8,
}

#[derive(Debug, Serialize)]
struct KubernetesPodTemplate<'a> {
    spec: KubernetesPodSpec<'a>,
}

#[derive(Debug, Serialize)]
struct KubernetesPodSpec<'a> {
    #[serde(rename = "restartPolicy")]
    restart_policy: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "serviceAccountName")]
    service_account_name: Option<&'a str>,
    containers: Vec<KubernetesContainer<'a>>,
}

#[derive(Debug, Serialize)]
struct KubernetesContainer<'a> {
    name: &'a str,
    image: &'a str,
    env: Vec<KubernetesEnvVar>,
}

#[derive(Debug, Serialize)]
struct KubernetesEnvVar {
    name: &'static str,
    value: String,
}

pub fn kubernetes_job_manifest(
    settings: &ExecutionSettings,
    job: &AgentJob,
) -> Result<String, serde_json::Error> {
    let manifest = KubernetesJob {
        api_version: "batch/v1",
        kind: "Job",
        metadata: KubernetesMetadata {
            name: kubernetes_job_name(&job.run_id),
            namespace: &settings.namespace,
            labels: KubernetesLabels {
                app: "kiln-agent",
                run_id: &job.run_id,
            },
        },
        spec: KubernetesJobSpec {
            backoff_limit: 0,
            template: KubernetesPodTemplate {
                spec: KubernetesPodSpec {
                    restart_policy: "Never",
                    service_account_name: settings.service_account_name.as_deref(),
                    containers: vec![KubernetesContainer {
                        name: "agent",
                        image: &settings.job_image,
                        env: job_env(settings, job),
                    }],
                },
            },
        },
    };

    serde_json::to_string_pretty(&manifest)
}

fn job_env(settings: &ExecutionSettings, job: &AgentJob) -> Vec<KubernetesEnvVar> {
    let mut env_vars = vec![
        env_var("KILN_RUN_ID", &job.run_id),
        env_var("KILN_REPOSITORY", &job.repo_full_name),
        env_var("KILN_PR_NUMBER", job.pr_number.to_string()),
        env_var("KILN_HEAD_SHA", &job.head_sha),
        env_var("KILN_REQUESTER", &job.requester),
        env_var("KILN_COMMAND", &job.command.raw),
        env_var("KILN_TASK", &job.command.task),
        env_var("KILN_QUEUE_POSITION", job.queue_position.to_string()),
        env_var(
            "KILN_DEFAULT_RUNTIME_IMAGE",
            &settings.default_runtime_image,
        ),
    ];

    if let Some(agent) = &job.command.agent {
        env_vars.push(env_var("KILN_AGENT", agent));
    }

    if let Some(model) = &job.command.model {
        env_vars.push(env_var("KILN_MODEL", model));
    }

    env_vars
}

fn env_var(name: &'static str, value: impl Into<String>) -> KubernetesEnvVar {
    KubernetesEnvVar {
        name,
        value: value.into(),
    }
}

fn kubernetes_job_name(run_id: &str) -> String {
    run_id.replace('_', "-")
}

pub fn launcher_from_settings(settings: &ExecutionSettings) -> Arc<dyn JobLauncher> {
    match settings.mode.as_str() {
        "kubectl" => Arc::new(KubectlJobLauncher::new(settings.clone())),
        _ => Arc::new(DisabledJobLauncher),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ExecutionSettings {
        ExecutionSettings {
            mode: "disabled".to_string(),
            namespace: "kiln".to_string(),
            job_image: "ghcr.io/ixoo/kiln-agent:latest".to_string(),
            default_runtime_image: "ghcr.io/ixoo/kiln-runtime:latest".to_string(),
            service_account_name: Some("kiln-agent".to_string()),
        }
    }

    fn job() -> AgentJob {
        AgentJob {
            run_id: "kiln_123".to_string(),
            repo_full_name: "octo/repo".to_string(),
            pr_number: 42,
            head_sha: "abc123".to_string(),
            requester: "alice".to_string(),
            command: AgentCommand {
                agent: Some("coder".to_string()),
                model: Some("local".to_string()),
                task: "fix tests".to_string(),
                raw: "/agent:coder:local fix tests".to_string(),
                line_number: 1,
                command_index: 0,
            },
            queue_position: 1,
        }
    }

    #[test]
    fn renders_kubernetes_job_manifest() {
        let manifest = kubernetes_job_manifest(&settings(), &job()).unwrap();

        assert!(manifest.contains("\"kind\": \"Job\""));
        assert!(manifest.contains("\"name\": \"kiln-123\""));
        assert!(manifest.contains("KILN_RUN_ID"));
        assert!(manifest.contains("/agent:coder:local fix tests"));
    }

    #[tokio::test]
    async fn disabled_launcher_is_noop() {
        let result = DisabledJobLauncher.launch(job()).await.unwrap();

        assert_eq!(result.status, "launch-disabled");
        assert_eq!(result.external_id, None);
    }
}
