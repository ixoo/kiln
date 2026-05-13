use crate::{
    command::{extract_commands, AgentCommand, CommandLine},
    config::RuntimeConfig,
    execution::{queue_key, AgentJob, JobLauncher, PerPrQueue},
    github::{run_marker, CheckRunRequest, GitHubClient, GitHubContext},
    policy::{PolicyDecision, PolicyEngine},
    signature::verify_github_signature,
};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    config: Arc<RuntimeConfig>,
    github: Arc<dyn GitHubClient>,
    launcher: Arc<dyn JobLauncher>,
    queue: Arc<PerPrQueue>,
    policy: PolicyEngine,
}

pub fn build_app(
    config: RuntimeConfig,
    github: Arc<dyn GitHubClient>,
    launcher: Arc<dyn JobLauncher>,
) -> Router {
    let state = Arc::new(AppState {
        config: Arc::new(config),
        github,
        launcher,
        queue: Arc::new(PerPrQueue::default()),
        policy: PolicyEngine,
    });

    Router::new()
        .route("/healthz", get(healthz))
        .route("/webhooks/github", post(github_webhook))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn github_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok());

    if !verify_github_signature(&state.config.webhook_secret, signature, &body) {
        warn!("github webhook signature verification failed");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    let event = headers
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    if event != "issue_comment" {
        return ignored(format!("unsupported event `{event}`"));
    }

    let payload = match serde_json::from_slice::<IssueCommentPayload>(&body) {
        Ok(payload) => payload,
        Err(error) => {
            warn!(%error, "failed to parse github issue_comment payload");
            return (
                StatusCode::BAD_REQUEST,
                Json(WebhookResponse::error("invalid payload")),
            )
                .into_response();
        }
    };

    match handle_issue_comment(state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => {
            warn!(%error, "failed to handle github webhook");
            (
                StatusCode::BAD_GATEWAY,
                Json(WebhookResponse::error("github api error")),
            )
                .into_response()
        }
    }
}

async fn handle_issue_comment(
    state: Arc<AppState>,
    payload: IssueCommentPayload,
) -> Result<WebhookResponse, crate::github::GitHubError> {
    if payload.action.as_deref() != Some("created") {
        return Ok(WebhookResponse::ignored(
            "issue comment action is not created",
        ));
    }

    if payload.issue.pull_request.is_none() {
        return Ok(WebhookResponse::ignored(
            "issue comment is not on a pull request",
        ));
    }

    let Some(installation) = payload.installation.as_ref() else {
        return Ok(WebhookResponse::ignored(
            "webhook payload has no installation",
        ));
    };

    let commands = extract_commands(&payload.comment.body);
    if commands.is_empty() {
        return Ok(WebhookResponse::ignored(
            "comment has no line-start /agent command",
        ));
    }

    let ctx = GitHubContext {
        owner: payload.repository.owner.login.clone(),
        repo: payload.repository.name.clone(),
        repo_full_name: payload.repository.full_name.clone(),
        pr_number: payload.issue.number,
        installation_id: installation.id,
    };

    let mut outcomes = Vec::new();
    let mut permission = None;
    let mut head_sha = None;

    for command_line in commands {
        let outcome = handle_command_line(
            &state,
            &payload,
            &ctx,
            &command_line,
            &mut permission,
            &mut head_sha,
        )
        .await?;
        outcomes.push(outcome);
    }

    Ok(WebhookResponse {
        status: "ok".to_string(),
        ignored: false,
        reason: None,
        runs: outcomes,
    })
}

async fn handle_command_line(
    state: &AppState,
    payload: &IssueCommentPayload,
    ctx: &GitHubContext,
    command_line: &CommandLine,
    permission: &mut Option<crate::github::RepoPermission>,
    head_sha: &mut Option<String>,
) -> Result<RunOutcome, crate::github::GitHubError> {
    let command = match &command_line.parsed {
        Ok(command) => command,
        Err(reason) => {
            let body = rejection_comment(command_line.line_number, &command_line.raw, reason);
            state.github.create_issue_comment(ctx, &body).await?;
            return Ok(RunOutcome::rejected(command_line, reason));
        }
    };

    let queue_key = queue_key(&ctx.repo_full_name, ctx.pr_number);
    let queue_lock = state.queue.lock_for(&queue_key).await;
    let queue_guard = queue_lock.lock().await;

    let outcome = async {
        let run_id = run_id(ctx, payload.comment.id, command);
        if state.github.run_exists(ctx, &run_id).await? {
            info!(%run_id, "skipping duplicate agent command");
            return Ok(RunOutcome::duplicate(command, run_id));
        }

        if permission.is_none() {
            *permission = Some(
                state
                    .github
                    .user_permission(ctx, &payload.sender.login)
                    .await?,
            );
        }

        match state.policy.evaluate_invocation(
            permission.as_ref().expect("permission was fetched"),
            command,
        ) {
            PolicyDecision::Allow => {}
            PolicyDecision::Deny(reason) => {
                let body = rejection_comment(command.line_number, &command.raw, &reason);
                state.github.create_issue_comment(ctx, &body).await?;
                return Ok(RunOutcome::rejected_command(command, &reason));
            }
        }

        if head_sha.is_none() {
            *head_sha = Some(state.github.pull_request_head_sha(ctx).await?);
        }

        let queue_position = command.command_index + 1;
        let head_sha = head_sha
            .as_ref()
            .expect("head sha was just fetched")
            .clone();
        let acknowledgement = acknowledgement_comment(command, &run_id, queue_position);
        state
            .github
            .create_issue_comment(ctx, &acknowledgement)
            .await?;

        let check = CheckRunRequest {
            name: format!("kiln/{} ({run_id})", agent_label(command)),
            head_sha: head_sha.clone(),
            external_id: run_id.clone(),
            summary: format!(
                "Queued `{}` with agent `{}` and model `{}`. Per-PR queue position: {}.",
                command.raw,
                agent_label(command),
                model_label(command),
                queue_position
            ),
        };
        state.github.create_check_run(ctx, check).await?;

        let job = AgentJob::new(
            &run_id,
            ctx,
            head_sha,
            &payload.sender.login,
            command,
            queue_position,
        );
        let launch_status = match state.launcher.launch(job).await {
            Ok(result) => result.status,
            Err(error) => {
                warn!(%run_id, %error, "failed to launch agent job");
                let body = launch_failure_comment(command, &run_id, &error.to_string());
                state.github.create_issue_comment(ctx, &body).await?;
                "launch-failed".to_string()
            }
        };

        Ok(RunOutcome::accepted(
            command,
            run_id,
            queue_position,
            launch_status,
        ))
    }
    .await;

    drop(queue_guard);
    state.queue.release_if_idle(&queue_key, &queue_lock).await;

    outcome
}

fn ignored(reason: String) -> Response {
    (StatusCode::OK, Json(WebhookResponse::ignored(reason))).into_response()
}

fn run_id(ctx: &GitHubContext, comment_id: u64, command: &AgentCommand) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ctx.repo_full_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(ctx.pr_number.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(comment_id.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(command.command_index.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(command.raw.as_bytes());

    let digest = hex::encode(hasher.finalize());
    format!("kiln_{}", &digest[..16])
}

fn acknowledgement_comment(command: &AgentCommand, run_id: &str, queue_position: usize) -> String {
    format!(
        "{}\nKiln accepted `{}`.\n\nRun: `{}`\nAgent: `{}`\nModel: `{}`\nStatus: `queued`\nPer-PR queue: `{}`",
        run_marker(run_id),
        command.raw,
        run_id,
        agent_label(command),
        model_label(command),
        queue_position
    )
}

fn agent_label(command: &AgentCommand) -> &str {
    command.agent.as_deref().unwrap_or("harness default")
}

fn model_label(command: &AgentCommand) -> &str {
    command.model.as_deref().unwrap_or("harness default")
}

fn rejection_comment(line_number: usize, raw: &str, reason: &str) -> String {
    format!(
        "Kiln could not accept command on line {}: `{}`.\n\nReason: {}.",
        line_number, raw, reason
    )
}

fn launch_failure_comment(command: &AgentCommand, run_id: &str, reason: &str) -> String {
    format!(
        "{}\nKiln accepted `{}` but failed to launch the agent job.\n\nRun: `{}`\nReason: {}.",
        run_marker(run_id),
        command.raw,
        run_id,
        reason
    )
}

#[derive(Debug, Serialize)]
struct WebhookResponse {
    status: String,
    ignored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    runs: Vec<RunOutcome>,
}

impl WebhookResponse {
    fn ignored(reason: impl Into<String>) -> Self {
        Self {
            status: "ignored".to_string(),
            ignored: true,
            reason: Some(reason.into()),
            runs: Vec::new(),
        }
    }

    fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            ignored: false,
            reason: Some(reason.into()),
            runs: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
struct RunOutcome {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    command: String,
    line_number: usize,
    queue_position: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    launch_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl RunOutcome {
    fn accepted(
        command: &AgentCommand,
        run_id: String,
        queue_position: usize,
        launch_status: String,
    ) -> Self {
        Self {
            status: "accepted".to_string(),
            run_id: Some(run_id),
            command: command.raw.clone(),
            line_number: command.line_number,
            queue_position,
            launch_status: Some(launch_status),
            reason: None,
        }
    }

    fn duplicate(command: &AgentCommand, run_id: String) -> Self {
        Self {
            status: "duplicate".to_string(),
            run_id: Some(run_id),
            command: command.raw.clone(),
            line_number: command.line_number,
            queue_position: command.command_index + 1,
            launch_status: None,
            reason: Some("run already exists".to_string()),
        }
    }

    fn rejected(command_line: &CommandLine, reason: &str) -> Self {
        Self {
            status: "rejected".to_string(),
            run_id: None,
            command: command_line.raw.clone(),
            line_number: command_line.line_number,
            queue_position: command_line.command_index + 1,
            launch_status: None,
            reason: Some(reason.to_string()),
        }
    }

    fn rejected_command(command: &AgentCommand, reason: &str) -> Self {
        Self {
            status: "rejected".to_string(),
            run_id: None,
            command: command.raw.clone(),
            line_number: command.line_number,
            queue_position: command.command_index + 1,
            launch_status: None,
            reason: Some(reason.to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct IssueCommentPayload {
    action: Option<String>,
    repository: RepositoryPayload,
    issue: IssuePayload,
    comment: CommentPayload,
    sender: SenderPayload,
    installation: Option<InstallationPayload>,
}

#[derive(Debug, Deserialize)]
struct RepositoryPayload {
    full_name: String,
    name: String,
    owner: OwnerPayload,
}

#[derive(Debug, Deserialize)]
struct OwnerPayload {
    login: String,
}

#[derive(Debug, Deserialize)]
struct IssuePayload {
    number: u64,
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CommentPayload {
    id: u64,
    body: String,
}

#[derive(Debug, Deserialize)]
struct SenderPayload {
    login: String,
}

#[derive(Debug, Deserialize)]
struct InstallationPayload {
    id: u64,
}
