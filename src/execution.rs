use crate::{
    command::AgentCommand,
    config::{ConfigError, ExecutionSettings},
    github::GitHubContext,
};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::{collections::HashMap, process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, sync::Mutex, time::timeout};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct AgentJob {
    pub run_id: String,
    pub owner: String,
    pub repo: String,
    pub repo_full_name: String,
    pub pr_number: u64,
    pub installation_id: u64,
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
            owner: ctx.owner.clone(),
            repo: ctx.repo.clone(),
            repo_full_name: ctx.repo_full_name.clone(),
            pr_number: ctx.pr_number,
            installation_id: ctx.installation_id,
            head_sha: head_sha.into(),
            requester: requester.into(),
            command: command.clone(),
            queue_position,
        }
    }

    pub fn queue_key(&self) -> String {
        queue_key(&self.repo_full_name, self.pr_number)
    }
}

pub fn queue_key(repo_full_name: &str, pr_number: u64) -> String {
    format!("{repo_full_name}#{pr_number}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLaunchResult {
    pub status: String,
    pub external_id: Option<String>,
    pub terminal: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum JobLaunchError {
    #[error("job launcher is disabled")]
    Disabled,
    #[error("failed to run kubectl: {0}")]
    Io(#[from] std::io::Error),
    #[error("kubectl exited with status {status}: {stderr}")]
    Kubectl { status: String, stderr: String },
    #[error("local agent exited with status {status}")]
    Local { status: String },
    #[error("agent launch timed out after {seconds} seconds")]
    Timeout { seconds: u64 },
    #[error("failed to serialize kubernetes job manifest: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("agent launch task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[async_trait]
pub trait JobLauncher: Send + Sync {
    fn should_launch(&self) -> bool {
        true
    }

    async fn launch(&self, job: AgentJob) -> Result<JobLaunchResult, JobLaunchError>;
}

#[derive(Debug, Default)]
pub struct DisabledJobLauncher;

#[async_trait]
impl JobLauncher for DisabledJobLauncher {
    fn should_launch(&self) -> bool {
        false
    }

    async fn launch(&self, _job: AgentJob) -> Result<JobLaunchResult, JobLaunchError> {
        Ok(JobLaunchResult {
            status: "launch-disabled".to_string(),
            external_id: None,
            terminal: true,
        })
    }
}

#[derive(Debug, Clone)]
pub struct LocalJobLauncher {
    settings: ExecutionSettings,
}

impl LocalJobLauncher {
    pub fn new(settings: ExecutionSettings) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl JobLauncher for LocalJobLauncher {
    async fn launch(&self, job: AgentJob) -> Result<JobLaunchResult, JobLaunchError> {
        let settings = self.settings.clone();
        local_launch(settings, job).await
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
        kubectl_launch(settings, job).await
    }
}

async fn kubectl_launch(
    settings: ExecutionSettings,
    job: AgentJob,
) -> Result<JobLaunchResult, JobLaunchError> {
    let manifest = kubernetes_job_manifest(&settings, &job)?;
    let mut command = Command::new("kubectl");
    command
        .args(["apply", "-f", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(manifest.as_bytes()).await?;
    }

    let seconds = settings.launch_timeout_seconds;
    let output = match timeout(Duration::from_secs(seconds), child.wait_with_output()).await {
        Ok(output) => output?,
        Err(_) => return Err(JobLaunchError::Timeout { seconds }),
    };
    if !output.status.success() {
        return Err(JobLaunchError::Kubectl {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(JobLaunchResult {
        status: "launched".to_string(),
        external_id: Some(kubernetes_job_name(&job.run_id)),
        terminal: false,
    })
}

async fn local_launch(
    settings: ExecutionSettings,
    job: AgentJob,
) -> Result<JobLaunchResult, JobLaunchError> {
    let Some((program, args)) = settings.local_command.split_first() else {
        return Err(JobLaunchError::Disabled);
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(job_env_vars(&settings, &job))
        .kill_on_drop(true);
    let mut child = command.spawn()?;

    let seconds = settings.launch_timeout_seconds;
    let status = match timeout(Duration::from_secs(seconds), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(JobLaunchError::Timeout { seconds });
        }
    };

    if !status.success() {
        return Err(JobLaunchError::Local {
            status: status.to_string(),
        });
    }

    Ok(JobLaunchResult {
        status: "local-completed".to_string(),
        external_id: None,
        terminal: true,
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

    pub async fn release_if_idle(&self, key: &str, lock: &Arc<Mutex<()>>) {
        let mut locks = self.locks.lock().await;

        if locks
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, lock) && Arc::strong_count(lock) == 2)
        {
            locks.remove(key);
        }
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
    #[serde(rename = "envFrom", skip_serializing_if = "Vec::is_empty")]
    env_from: Vec<KubernetesEnvFrom<'a>>,
    env: Vec<KubernetesEnvVar>,
}

#[derive(Debug, Serialize)]
struct KubernetesEnvFrom<'a> {
    #[serde(rename = "secretRef")]
    secret_ref: KubernetesSecretRef<'a>,
}

#[derive(Debug, Serialize)]
struct KubernetesSecretRef<'a> {
    name: &'a str,
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
                        env_from: job_env_from(settings),
                        env: job_env(settings, job),
                    }],
                },
            },
        },
    };

    serde_json::to_string_pretty(&manifest)
}

fn job_env(settings: &ExecutionSettings, job: &AgentJob) -> Vec<KubernetesEnvVar> {
    job_env_vars(settings, job)
        .into_iter()
        .map(|(name, value)| env_var(name, value))
        .collect()
}

fn job_env_from(settings: &ExecutionSettings) -> Vec<KubernetesEnvFrom<'_>> {
    settings
        .job_env_from_secret
        .as_deref()
        .map(|secret| {
            vec![KubernetesEnvFrom {
                secret_ref: KubernetesSecretRef { name: secret },
            }]
        })
        .unwrap_or_default()
}

pub fn job_env_vars(settings: &ExecutionSettings, job: &AgentJob) -> Vec<(&'static str, String)> {
    let mut env_vars = vec![
        ("KILN_RUN_ID", job.run_id.clone()),
        ("KILN_REPOSITORY_OWNER", job.owner.clone()),
        ("KILN_REPOSITORY_NAME", job.repo.clone()),
        ("KILN_REPOSITORY", job.repo_full_name.clone()),
        ("KILN_PR_NUMBER", job.pr_number.to_string()),
        (
            "KILN_GITHUB_INSTALLATION_ID",
            job.installation_id.to_string(),
        ),
        ("KILN_HEAD_SHA", job.head_sha.clone()),
        ("KILN_REQUESTER", job.requester.clone()),
        ("KILN_COMMAND", job.command.raw.clone()),
        ("KILN_TASK", job.command.task.clone()),
        ("KILN_QUEUE_POSITION", job.queue_position.to_string()),
        (
            "KILN_DEFAULT_RUNTIME_IMAGE",
            settings.default_runtime_image.clone(),
        ),
    ];

    if let Some(agent) = &job.command.agent {
        env_vars.push(("KILN_AGENT", agent.clone()));
    }

    if let Some(model) = &job.command.model {
        env_vars.push(("KILN_MODEL", model.clone()));
    }

    if let Some(callback_url) = &settings.callback_url {
        env_vars.push(("KILN_CALLBACK_URL", callback_url.clone()));
    }

    if let Some(callback_key) = &settings.callback_secret {
        env_vars.push((
            "KILN_CALLBACK_TOKEN",
            callback_token_for_job(callback_key, job),
        ));
    }

    env_vars
}

pub fn callback_token_for_job(callback_key: &str, job: &AgentJob) -> String {
    callback_token(
        callback_key,
        &job.run_id,
        &job.repo_full_name,
        job.pr_number,
        job.installation_id,
    )
}

pub fn callback_token(
    callback_key: &str,
    run_id: &str,
    repo_full_name: &str,
    pr_number: u64,
    installation_id: u64,
) -> String {
    let mut mac =
        HmacSha256::new_from_slice(callback_key.as_bytes()).expect("HMAC accepts keys of any size");
    mac.update(b"kiln-agent-callback-v1");
    mac.update(b"\0");
    mac.update(run_id.as_bytes());
    mac.update(b"\0");
    mac.update(repo_full_name.as_bytes());
    mac.update(b"\0");
    mac.update(pr_number.to_string().as_bytes());
    mac.update(b"\0");
    mac.update(installation_id.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
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

pub fn launcher_from_settings(
    settings: &ExecutionSettings,
) -> Result<Arc<dyn JobLauncher>, ConfigError> {
    settings.validate()?;

    match settings.mode.as_str() {
        "disabled" => Ok(Arc::new(DisabledJobLauncher)),
        "local" => Ok(Arc::new(LocalJobLauncher::new(settings.clone()))),
        "kubectl" => Ok(Arc::new(KubectlJobLauncher::new(settings.clone()))),
        _ => unreachable!("execution mode was validated"),
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
            job_env_from_secret: None,
            local_command: Vec::new(),
            callback_url: None,
            callback_secret: None,
            launch_timeout_seconds: 300,
            stale_run_seconds: 3600,
        }
    }

    fn job() -> AgentJob {
        AgentJob {
            run_id: "kiln_123".to_string(),
            owner: "octo".to_string(),
            repo: "repo".to_string(),
            repo_full_name: "octo/repo".to_string(),
            pr_number: 42,
            installation_id: 999,
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
        let mut settings = settings();
        settings.callback_url = Some("https://kiln.example.com/callbacks/agent".to_string());
        settings.callback_secret = Some("secret".to_string());
        let manifest = kubernetes_job_manifest(&settings, &job()).unwrap();

        assert!(manifest.contains("\"kind\": \"Job\""));
        assert!(manifest.contains("\"name\": \"kiln-123\""));
        assert!(manifest.contains("KILN_RUN_ID"));
        assert!(manifest.contains("KILN_CALLBACK_URL"));
        assert!(manifest.contains("KILN_CALLBACK_TOKEN"));
        assert!(!manifest.contains("KILN_CALLBACK_SECRET"));
        assert!(manifest.contains("/agent:coder:local fix tests"));
    }

    #[test]
    fn renders_kubernetes_job_env_from_secret() {
        let mut settings = settings();
        settings.job_env_from_secret = Some("kiln-opencode-agent".to_string());
        let manifest = kubernetes_job_manifest(&settings, &job()).unwrap();

        assert!(manifest.contains("\"envFrom\""));
        assert!(manifest.contains("\"secretRef\""));
        assert!(manifest.contains("\"name\": \"kiln-opencode-agent\""));
    }

    #[test]
    fn callback_tokens_are_deterministic_and_run_scoped() {
        let first = callback_token("secret", "kiln_123", "octo/repo", 42, 999);
        let duplicate = callback_token("secret", "kiln_123", "octo/repo", 42, 999);
        let other_run = callback_token("secret", "kiln_456", "octo/repo", 42, 999);

        assert_eq!(first, duplicate);
        assert_ne!(first, other_run);
    }

    #[tokio::test]
    async fn disabled_launcher_is_noop() {
        let result = DisabledJobLauncher.launch(job()).await.unwrap();

        assert_eq!(result.status, "launch-disabled");
        assert_eq!(result.external_id, None);
        assert!(result.terminal);
    }

    #[tokio::test]
    async fn local_launcher_runs_configured_command_with_job_metadata() {
        let mut settings = settings();
        settings.mode = "local".to_string();
        settings.local_command = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "test \"$KILN_RUN_ID\" = kiln_123 && test \"$KILN_AGENT\" = coder && test \"$KILN_MODEL\" = local".to_string(),
        ];

        let result = LocalJobLauncher::new(settings).launch(job()).await.unwrap();

        assert_eq!(result.status, "local-completed");
        assert!(result.terminal);
    }

    #[tokio::test]
    async fn local_launcher_reports_nonzero_exit() {
        let mut settings = settings();
        settings.mode = "local".to_string();
        settings.local_command = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 7".to_string(),
        ];

        let result = LocalJobLauncher::new(settings).launch(job()).await;

        assert!(matches!(result, Err(JobLaunchError::Local { .. })));
    }

    #[tokio::test]
    async fn local_launcher_does_not_inherit_process_environment() {
        std::env::set_var("KILN_AGENT_CALLBACK_SECRET", "root-secret");
        let mut settings = settings();
        settings.mode = "local".to_string();
        settings.local_command = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "test -z \"$KILN_AGENT_CALLBACK_SECRET\"".to_string(),
        ];

        let result = LocalJobLauncher::new(settings).launch(job()).await;
        std::env::remove_var("KILN_AGENT_CALLBACK_SECRET");

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn removes_idle_pr_locks() {
        let queue = PerPrQueue::default();
        let key = "octo/repo#42";
        let lock = queue.lock_for(key).await;

        queue.release_if_idle(key, &lock).await;

        let next_lock = queue.lock_for(key).await;
        assert!(!Arc::ptr_eq(&lock, &next_lock));
    }
}
