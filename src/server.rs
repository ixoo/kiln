use crate::{
    command::{extract_commands, AgentCommand, CommandLine},
    config::RuntimeConfig,
    execution::{callback_token, queue_key, AgentJob, JobLauncher, PerPrQueue},
    github::{
        run_marker, CheckRunRequest, CheckRunUpdate, GitHubClient, GitHubContext, IssueComment,
    },
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
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

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
        .route("/callbacks/agent", post(agent_callback))
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

async fn agent_callback(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<AgentCallbackPayload>,
) -> Response {
    let Some(callback_key) = state.config.agent_callback_secret.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(CallbackResponse::error(
                "agent callbacks are not configured",
            )),
        )
            .into_response();
    };

    let provided_token = headers
        .get("x-kiln-callback-token")
        .and_then(|value| value.to_str().ok());
    let expected_token = callback_token(
        callback_key,
        &payload.run_id,
        &payload.repo_full_name,
        payload.pr_number,
        payload.installation_id,
    );
    if !constant_time_token_eq(provided_token, &expected_token) {
        warn!(run_id = %payload.run_id, "agent callback authentication failed");
        return (
            StatusCode::UNAUTHORIZED,
            Json(CallbackResponse::error("unauthorized")),
        )
            .into_response();
    }

    if payload.repo_full_name != format!("{}/{}", payload.owner, payload.repo) {
        return (
            StatusCode::BAD_REQUEST,
            Json(CallbackResponse::error(
                "owner, repo, and repo_full_name do not match",
            )),
        )
            .into_response();
    }

    let ctx = GitHubContext {
        owner: payload.owner.clone(),
        repo: payload.repo.clone(),
        repo_full_name: payload.repo_full_name.clone(),
        pr_number: payload.pr_number,
        installation_id: payload.installation_id,
    };

    match handle_agent_callback(state.clone(), &ctx, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(CallbackError::RunNotFound(run_id)) => (
            StatusCode::NOT_FOUND,
            Json(CallbackResponse::error(format!(
                "run `{run_id}` was not found"
            ))),
        )
            .into_response(),
        Err(CallbackError::InvalidRunStatus { run_id, status }) => (
            StatusCode::CONFLICT,
            Json(CallbackResponse::error(format!(
                "run `{run_id}` is `{status}` and cannot accept a completion callback"
            ))),
        )
            .into_response(),
        Err(CallbackError::GitHub(error)) => {
            warn!(%error, "failed to handle agent callback");
            (
                StatusCode::BAD_GATEWAY,
                Json(CallbackResponse::error("github api error")),
            )
                .into_response()
        }
    }
}

async fn handle_agent_callback(
    state: Arc<AppState>,
    ctx: &GitHubContext,
    payload: AgentCallbackPayload,
) -> Result<CallbackResponse, CallbackError> {
    let queue_key = queue_key(&ctx.repo_full_name, ctx.pr_number);
    let queue_lock = state.queue.lock_for(&queue_key).await;
    let queue_guard = queue_lock.lock().await;

    let outcome = async {
        let comments = state.github.issue_comments(ctx).await?;
        let records = queue_records(&comments, &state.config);
        let Some(record) = records
            .into_iter()
            .find(|record| record.job.run_id == payload.run_id)
        else {
            return Err(CallbackError::RunNotFound(payload.run_id));
        };

        let terminal_status = payload.status.queue_status();
        if matches!(record.status, QueueStatus::Completed | QueueStatus::Failed) {
            return Ok(CallbackResponse::ok(
                record.job.run_id,
                record.status,
                Vec::new(),
            ));
        }

        if record.status != QueueStatus::Running {
            return Err(CallbackError::InvalidRunStatus {
                run_id: record.job.run_id,
                status: record.status.as_str().to_string(),
            });
        }

        let public_detail = public_callback_detail(payload.detail.as_deref());

        state
            .github
            .create_issue_comment(
                ctx,
                &run_status_comment(
                    &record.job,
                    terminal_status,
                    public_detail.as_deref(),
                    &state.config.state_secret,
                ),
            )
            .await?;
        update_check_run_for_status(
            &state,
            ctx,
            &record.job,
            terminal_status,
            public_detail.as_deref(),
        )
        .await?;
        Ok(CallbackResponse::ok(
            record.job.run_id,
            terminal_status,
            Vec::new(),
        ))
    }
    .await;

    drop(queue_guard);
    state.queue.release_if_idle(&queue_key, &queue_lock).await;

    if outcome.is_ok() && state.launcher.should_launch() {
        schedule_advance_queue(state.clone(), ctx.clone());
    }

    outcome
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

    let ctx = GitHubContext {
        owner: payload.repository.owner.login.clone(),
        repo: payload.repository.name.clone(),
        repo_full_name: payload.repository.full_name.clone(),
        pr_number: payload.issue.number,
        installation_id: installation.id,
    };

    let commands = extract_commands(&payload.comment.body);
    if commands.is_empty() {
        return Ok(WebhookResponse::ignored(
            "comment has no line-start /agent command",
        ));
    }

    let mut permission = None;
    let mut head_sha = None;
    let queue_key = queue_key(&ctx.repo_full_name, ctx.pr_number);
    let queue_lock = state.queue.lock_for(&queue_key).await;
    let queue_guard = queue_lock.lock().await;
    let mut schedule_queue = false;

    let result = async {
        let mut outcomes = Vec::new();
        for command_line in commands {
            let (outcome, command_schedule_queue) = handle_command_line(
                state.clone(),
                &payload,
                &ctx,
                &command_line,
                &mut permission,
                &mut head_sha,
            )
            .await?;
            schedule_queue |= command_schedule_queue;
            outcomes.push(outcome);
        }

        Ok::<_, crate::github::GitHubError>(WebhookResponse {
            status: "ok".to_string(),
            ignored: false,
            reason: None,
            runs: outcomes,
        })
    }
    .await;

    drop(queue_guard);
    state.queue.release_if_idle(&queue_key, &queue_lock).await;

    if result.is_ok() && schedule_queue {
        schedule_advance_queue(state.clone(), ctx.clone());
    }

    result
}

async fn handle_command_line(
    state: Arc<AppState>,
    payload: &IssueCommentPayload,
    ctx: &GitHubContext,
    command_line: &CommandLine,
    permission: &mut Option<crate::github::RepoPermission>,
    head_sha: &mut Option<String>,
) -> Result<(RunOutcome, bool), crate::github::GitHubError> {
    let mut schedule_queue = false;

    let comments = state.github.issue_comments(ctx).await?;
    let rejection_id = rejection_id(ctx, payload.comment.id, command_line);

    if permission.is_none() {
        *permission = Some(
            state
                .github
                .user_permission(ctx, &payload.sender.login)
                .await?,
        );
    }

    if !permission
        .as_ref()
        .expect("permission was fetched")
        .can_invoke_agent()
    {
        let reason = "requester must have write, maintain, or admin permission";
        create_rejection_once(
            state.as_ref(),
            ctx,
            &comments,
            &rejection_id,
            command_line,
            reason,
        )
        .await?;
        return Ok((RunOutcome::rejected(command_line, reason), false));
    }

    let command = match &command_line.parsed {
        Ok(command) => command,
        Err(reason) => {
            create_rejection_once(
                state.as_ref(),
                ctx,
                &comments,
                &rejection_id,
                command_line,
                reason,
            )
            .await?;
            return Ok((RunOutcome::rejected(command_line, reason), false));
        }
    };

    let run_id = run_id(ctx, payload.comment.id, command);
    let records = queue_records(&comments, &state.config);
    if let Some(existing) = records.iter().find(|record| record.job.run_id == run_id) {
        info!(%run_id, "skipping duplicate agent command");
        ensure_check_run(&state, ctx, &existing.job, existing.job.queue_position).await?;
        if state.launcher.should_launch() {
            schedule_queue = true;
        }
        return Ok((
            RunOutcome::duplicate(command, run_id, existing.job.queue_position),
            schedule_queue,
        ));
    }

    match state.policy.evaluate_invocation(
        permission.as_ref().expect("permission was fetched"),
        command,
    ) {
        PolicyDecision::Allow => {}
        PolicyDecision::Deny(reason) => {
            create_rejection_once(
                state.as_ref(),
                ctx,
                &comments,
                &rejection_id,
                command_line,
                &reason,
            )
            .await?;
            return Ok((RunOutcome::rejected_command(command, &reason), false));
        }
    }

    if head_sha.is_none() {
        *head_sha = Some(state.github.pull_request_head_sha(ctx).await?);
    }

    let queue_position = records
        .iter()
        .filter(|record| !record.status.is_terminal())
        .count()
        + 1;
    let head_sha = head_sha
        .as_ref()
        .expect("head sha was just fetched")
        .clone();
    let job = AgentJob::new(
        &run_id,
        ctx,
        head_sha.clone(),
        &payload.sender.login,
        command,
        queue_position,
    );
    ensure_check_run(&state, ctx, &job, queue_position).await?;

    let acknowledgement = acknowledgement_comment(
        command,
        &run_id,
        queue_position,
        &job,
        &state.config.state_secret,
    );
    state
        .github
        .create_issue_comment(ctx, &acknowledgement)
        .await?;

    if state.launcher.should_launch() {
        schedule_queue = true;
    }

    Ok((
        RunOutcome::accepted(command, run_id, queue_position, "queued".to_string()),
        schedule_queue,
    ))
}

async fn advance_queue(
    state: &AppState,
    ctx: &GitHubContext,
) -> Result<Vec<QueueLaunch>, crate::github::GitHubError> {
    let mut launches = Vec::new();

    loop {
        let comments = state.github.issue_comments(ctx).await?;
        let records = queue_records(&comments, &state.config);

        if let Some(running) = records
            .iter()
            .find(|record| record.status == QueueStatus::Running)
        {
            if !running.is_stale(state.config.settings.execution.stale_run_seconds) {
                return Ok(launches);
            }

            state
                .github
                .create_issue_comment(
                    ctx,
                    &run_status_comment(
                        &running.job,
                        QueueStatus::Failed,
                        Some("run exceeded stale timeout"),
                        &state.config.state_secret,
                    ),
                )
                .await?;
            update_check_run_for_status(
                state,
                ctx,
                &running.job,
                QueueStatus::Failed,
                Some("run exceeded stale timeout"),
            )
            .await?;
            continue;
        }

        let Some(next) = records
            .into_iter()
            .filter(|record| record.status == QueueStatus::Queued)
            .min_by_key(|record| (record.first_seen, record.job.command.command_index))
        else {
            return Ok(launches);
        };

        let job = next.job;
        let current_head_sha = state.github.pull_request_head_sha(ctx).await?;
        if current_head_sha != job.head_sha {
            let detail =
                "PR head changed before launch; request a new /agent run for the latest commit";
            state
                .github
                .create_issue_comment(
                    ctx,
                    &run_status_comment(
                        &job,
                        QueueStatus::Failed,
                        Some(detail),
                        &state.config.state_secret,
                    ),
                )
                .await?;
            update_check_run_for_status(state, ctx, &job, QueueStatus::Failed, Some(detail))
                .await?;
            continue;
        }

        let lease_id = launch_lease_id(&job.run_id);
        state
            .github
            .create_issue_comment(
                ctx,
                &run_status_comment_with_lease(
                    &job,
                    QueueStatus::Running,
                    None,
                    Some(&lease_id),
                    &state.config.state_secret,
                ),
            )
            .await?;

        let comments = state.github.issue_comments(ctx).await?;
        if winning_running_lease(&comments, &state.config, &job.run_id).as_deref()
            != Some(lease_id.as_str())
        {
            info!(run_id = %job.run_id, "another kiln worker won the launch lease");
            return Ok(launches);
        }

        update_check_run_for_status(state, ctx, &job, QueueStatus::Running, None).await?;

        match state.launcher.launch(job.clone()).await {
            Ok(result) => {
                launches.push(QueueLaunch {
                    run_id: job.run_id.clone(),
                    status: result.status.clone(),
                    terminal: result.terminal,
                });

                if result.terminal {
                    let public_detail = public_launch_detail(&result.status);
                    state
                        .github
                        .create_issue_comment(
                            ctx,
                            &run_status_comment(
                                &job,
                                QueueStatus::Completed,
                                Some(&public_detail),
                                &state.config.state_secret,
                            ),
                        )
                        .await?;
                    update_check_run_for_status(
                        state,
                        ctx,
                        &job,
                        QueueStatus::Completed,
                        Some(&public_detail),
                    )
                    .await?;
                    continue;
                }

                return Ok(launches);
            }
            Err(error) => {
                warn!(run_id = %job.run_id, %error, "failed to launch agent job");
                let public_detail = "launch failed; see Kiln logs";
                launches.push(QueueLaunch {
                    run_id: job.run_id.clone(),
                    status: "launch-failed".to_string(),
                    terminal: true,
                });
                state
                    .github
                    .create_issue_comment(
                        ctx,
                        &run_status_comment(
                            &job,
                            QueueStatus::Failed,
                            Some(public_detail),
                            &state.config.state_secret,
                        ),
                    )
                    .await?;
                update_check_run_for_status(
                    state,
                    ctx,
                    &job,
                    QueueStatus::Failed,
                    Some(public_detail),
                )
                .await?;
            }
        }
    }
}

async fn ensure_check_run(
    state: &AppState,
    ctx: &GitHubContext,
    job: &AgentJob,
    queue_position: usize,
) -> Result<(), crate::github::GitHubError> {
    if state
        .github
        .check_run_exists(ctx, &job.head_sha, &job.run_id)
        .await?
    {
        return Ok(());
    }

    let check = CheckRunRequest {
        name: format!("kiln/{} ({})", agent_label(&job.command), job.run_id),
        head_sha: job.head_sha.clone(),
        external_id: job.run_id.clone(),
        summary: format!(
            "Queued {} with agent {} and model {}. Per-PR queue position: {}.",
            markdown_inline_code(&job.command.raw),
            markdown_inline_code(agent_label(&job.command)),
            markdown_inline_code(model_label(&job.command)),
            queue_position
        ),
    };
    state.github.create_check_run(ctx, check).await
}

async fn update_check_run_for_status(
    state: &AppState,
    ctx: &GitHubContext,
    job: &AgentJob,
    status: QueueStatus,
    detail: Option<&str>,
) -> Result<(), crate::github::GitHubError> {
    let (check_status, conclusion) = match status {
        QueueStatus::Queued => ("queued", None),
        QueueStatus::Running => ("in_progress", None),
        QueueStatus::Completed => ("completed", Some("success")),
        QueueStatus::Failed => ("completed", Some("failure")),
    };
    let detail = detail
        .map(|detail| format!(" Detail: {}.", safe_markdown_text(detail)))
        .unwrap_or_default();

    state
        .github
        .update_check_run(
            ctx,
            CheckRunUpdate {
                external_id: job.run_id.clone(),
                head_sha: job.head_sha.clone(),
                status: check_status.to_string(),
                conclusion: conclusion.map(str::to_string),
                summary: format!(
                    "Kiln marked run `{}` as `{}` for {}.{}",
                    job.run_id,
                    status.as_str(),
                    markdown_inline_code(&job.command.raw),
                    detail
                ),
            },
        )
        .await
}

fn ignored(reason: String) -> Response {
    (StatusCode::OK, Json(WebhookResponse::ignored(reason))).into_response()
}

fn schedule_advance_queue(state: Arc<AppState>, ctx: GitHubContext) {
    tokio::spawn(async move {
        let queue_key = queue_key(&ctx.repo_full_name, ctx.pr_number);
        let queue_lock = state.queue.lock_for(&queue_key).await;
        let queue_guard = queue_lock.lock().await;
        let outcome = advance_queue(&state, &ctx).await;
        drop(queue_guard);
        state.queue.release_if_idle(&queue_key, &queue_lock).await;

        match outcome {
            Ok(launches) if launches.iter().any(|launch| !launch.terminal) => {
                schedule_stale_wakeup(state.clone(), ctx.clone());
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%error, repo = %ctx.repo_full_name, pr = ctx.pr_number, "failed to advance queue");
            }
        }
    });
}

fn schedule_stale_wakeup(state: Arc<AppState>, ctx: GitHubContext) {
    tokio::spawn(async move {
        let delay = state
            .config
            .settings
            .execution
            .stale_run_seconds
            .saturating_add(1);
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        schedule_advance_queue(state, ctx);
    });
}

async fn create_rejection_once(
    state: &AppState,
    ctx: &GitHubContext,
    comments: &[IssueComment],
    rejection_id: &str,
    command_line: &CommandLine,
    reason: &str,
) -> Result<(), crate::github::GitHubError> {
    let marker = rejection_marker(rejection_id);
    if comments
        .iter()
        .any(|comment| comment.trusted && comment.body.contains(&marker))
    {
        return Ok(());
    }

    let body = rejection_comment(&marker, command_line.line_number, &command_line.raw, reason);
    state.github.create_issue_comment(ctx, &body).await?;
    Ok(())
}

fn rejection_id(ctx: &GitHubContext, comment_id: u64, command_line: &CommandLine) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ctx.repo_full_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(ctx.pr_number.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(comment_id.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(command_line.command_index.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(command_line.raw.as_bytes());

    let digest = hex::encode(hasher.finalize());
    format!("kiln_reject_{}", &digest[..16])
}

fn rejection_marker(rejection_id: &str) -> String {
    format!("<!-- kiln:rejection_id={rejection_id} -->")
}

fn accepted_state_secrets(config: &RuntimeConfig) -> Vec<&str> {
    let mut secrets = Vec::with_capacity(config.previous_state_secrets.len() + 1);
    secrets.push(config.state_secret.as_str());
    secrets.extend(config.previous_state_secrets.iter().map(String::as_str));
    secrets
}

fn launch_lease_id(run_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(std::process::id().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(now_unix_nanos().to_string().as_bytes());

    let digest = hex::encode(hasher.finalize());
    format!("lease_{}", &digest[..16])
}

fn winning_running_lease(
    comments: &[IssueComment],
    config: &RuntimeConfig,
    run_id: &str,
) -> Option<String> {
    comments
        .iter()
        .filter(|comment| comment.trusted)
        .flat_map(|comment| run_states(&comment.body, config))
        .find(|state| state.run_id == run_id && state.status == QueueStatus::Running)
        .and_then(|state| state.lease_id)
}

fn public_launch_detail(_detail: &str) -> String {
    "launch completed".to_string()
}

fn public_callback_detail(detail: Option<&str>) -> Option<String> {
    detail.map(|_| "agent provided completion detail; see agent logs".to_string())
}

fn constant_time_token_eq(provided: Option<&str>, expected: &str) -> bool {
    let Some(provided) = provided else {
        return false;
    };

    let provided = provided.as_bytes();
    let expected = expected.as_bytes();
    let mut diff = provided.len() ^ expected.len();
    for index in 0..provided.len().max(expected.len()) {
        let left = provided.get(index).copied().unwrap_or_default();
        let right = expected.get(index).copied().unwrap_or_default();
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

fn marker_signature(secret: &str, encoded_state: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(b"kiln-run-state-v1");
    mac.update(b"\0");
    mac.update(encoded_state.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn markdown_inline_code(value: &str) -> String {
    let value = safe_markdown_text(value);
    let longest_backtick_run = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let delimiter = "`".repeat(longest_backtick_run + 1);

    if longest_backtick_run == 0 {
        format!("{delimiter}{value}{delimiter}")
    } else {
        format!("{delimiter} {value} {delimiter}")
    }
}

fn safe_markdown_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('@', "&#64;")
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

fn acknowledgement_comment(
    command: &AgentCommand,
    run_id: &str,
    queue_position: usize,
    job: &AgentJob,
    state_secret: &str,
) -> String {
    format!(
        "{}\n{}\nKiln accepted {}.\n\nRun: `{}`\nAgent: {}\nModel: {}\nStatus: `queued`\nPer-PR queue: `{}`",
        run_marker(run_id),
        run_state_marker(job, QueueStatus::Queued, None, state_secret),
        markdown_inline_code(&command.raw),
        run_id,
        markdown_inline_code(agent_label(command)),
        markdown_inline_code(model_label(command)),
        queue_position
    )
}

fn agent_label(command: &AgentCommand) -> &str {
    command.agent.as_deref().unwrap_or("harness default")
}

fn model_label(command: &AgentCommand) -> &str {
    command.model.as_deref().unwrap_or("harness default")
}

fn rejection_comment(marker: &str, line_number: usize, raw: &str, reason: &str) -> String {
    format!(
        "{}\nKiln could not accept command on line {}: {}.\n\nReason: {}.",
        marker,
        line_number,
        markdown_inline_code(raw),
        safe_markdown_text(reason)
    )
}

fn run_status_comment(
    job: &AgentJob,
    status: QueueStatus,
    detail: Option<&str>,
    state_secret: &str,
) -> String {
    run_status_comment_with_lease(job, status, detail, None, state_secret)
}

fn run_status_comment_with_lease(
    job: &AgentJob,
    status: QueueStatus,
    detail: Option<&str>,
    lease_id: Option<&str>,
    state_secret: &str,
) -> String {
    let status_label = status.as_str();
    let detail = detail
        .map(|detail| format!("\nDetail: {}.", safe_markdown_text(detail)))
        .unwrap_or_default();

    format!(
        "{}\n{}\nKiln marked run `{}` as `{}` for {}.{}",
        run_marker(&job.run_id),
        run_state_marker(job, status, lease_id, state_secret),
        job.run_id,
        status_label,
        markdown_inline_code(&job.command.raw),
        detail
    )
}

fn run_state_marker(
    job: &AgentJob,
    status: QueueStatus,
    lease_id: Option<&str>,
    state_secret: &str,
) -> String {
    let state = RunStateMarker {
        run_id: job.run_id.clone(),
        status,
        lease_id: lease_id.map(str::to_string),
        owner: job.owner.clone(),
        repo: job.repo.clone(),
        repo_full_name: job.repo_full_name.clone(),
        pr_number: job.pr_number,
        installation_id: job.installation_id,
        head_sha: job.head_sha.clone(),
        requester: job.requester.clone(),
        command: RunCommandMarker {
            agent: job.command.agent.clone(),
            model: job.command.model.clone(),
            task: job.command.task.clone(),
            raw: job.command.raw.clone(),
            line_number: job.command.line_number,
            command_index: job.command.command_index,
        },
        queue_position: job.queue_position,
        updated_at_unix: now_unix(),
    };
    let encoded = hex::encode(serde_json::to_vec(&state).expect("run state marker serializes"));
    let signature = marker_signature(state_secret, &encoded);
    format!("<!-- kiln:run_state={encoded};sig={signature} -->")
}

fn queue_records(comments: &[IssueComment], config: &RuntimeConfig) -> Vec<QueueRecord> {
    let mut records = HashMap::<String, QueueRecord>::new();

    for (index, comment) in comments.iter().enumerate() {
        if !comment.trusted {
            continue;
        }

        for state in run_states(&comment.body, config) {
            let run_id = state.run_id.clone();
            let status = state.status;
            let updated_at_unix = state.updated_at_unix;
            if records
                .get(&run_id)
                .is_some_and(|record| record.status.is_terminal())
            {
                continue;
            }
            let first_seen = records
                .get(&run_id)
                .map(|record| record.first_seen)
                .unwrap_or(index);
            records.insert(
                run_id,
                QueueRecord {
                    first_seen,
                    status,
                    updated_at_unix,
                    job: state.into_job(),
                },
            );
        }
    }

    records.into_values().collect()
}

fn run_states<'a>(
    body: &'a str,
    config: &'a RuntimeConfig,
) -> impl Iterator<Item = RunStateMarker> + 'a {
    body.lines()
        .filter_map(move |line| parse_run_state_marker(line, config))
}

fn parse_run_state_marker(line: &str, config: &RuntimeConfig) -> Option<RunStateMarker> {
    let marker = line
        .trim()
        .strip_prefix("<!-- kiln:run_state=")?
        .strip_suffix(" -->")?;
    let (encoded, signature) = marker.split_once(";sig=")?;
    if !accepted_state_secrets(config)
        .iter()
        .any(|secret| constant_time_token_eq(Some(signature), &marker_signature(secret, encoded)))
    {
        return None;
    }
    let bytes = hex::decode(encoded).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[derive(Debug, Clone)]
struct QueueLaunch {
    run_id: String,
    status: String,
    terminal: bool,
}

#[derive(Debug, Clone)]
struct QueueRecord {
    first_seen: usize,
    status: QueueStatus,
    updated_at_unix: u64,
    job: AgentJob,
}

impl QueueRecord {
    fn is_stale(&self, stale_after_seconds: u64) -> bool {
        now_unix().saturating_sub(self.updated_at_unix) >= stale_after_seconds
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum QueueStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

impl QueueStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunStateMarker {
    run_id: String,
    status: QueueStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lease_id: Option<String>,
    owner: String,
    repo: String,
    repo_full_name: String,
    pr_number: u64,
    installation_id: u64,
    head_sha: String,
    requester: String,
    command: RunCommandMarker,
    queue_position: usize,
    updated_at_unix: u64,
}

impl RunStateMarker {
    fn into_job(self) -> AgentJob {
        AgentJob {
            run_id: self.run_id,
            owner: self.owner,
            repo: self.repo,
            repo_full_name: self.repo_full_name,
            pr_number: self.pr_number,
            installation_id: self.installation_id,
            head_sha: self.head_sha,
            requester: self.requester,
            command: AgentCommand {
                agent: self.command.agent,
                model: self.command.model,
                task: self.command.task,
                raw: self.command.raw,
                line_number: self.command.line_number,
                command_index: self.command.command_index,
            },
            queue_position: self.queue_position,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunCommandMarker {
    agent: Option<String>,
    model: Option<String>,
    task: String,
    raw: String,
    line_number: usize,
    command_index: usize,
}

#[derive(Debug, Deserialize)]
struct AgentCallbackPayload {
    run_id: String,
    status: AgentCallbackStatus,
    owner: String,
    repo: String,
    repo_full_name: String,
    pr_number: u64,
    installation_id: u64,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum AgentCallbackStatus {
    Completed,
    Failed,
}

impl AgentCallbackStatus {
    fn queue_status(self) -> QueueStatus {
        match self {
            Self::Completed => QueueStatus::Completed,
            Self::Failed => QueueStatus::Failed,
        }
    }
}

#[derive(Debug, Serialize)]
struct CallbackResponse {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    launched: Vec<CallbackLaunch>,
}

impl CallbackResponse {
    fn ok(run_id: String, run_status: QueueStatus, launches: Vec<QueueLaunch>) -> Self {
        Self {
            status: "ok".to_string(),
            reason: None,
            run_id: Some(run_id),
            run_status: Some(run_status.as_str().to_string()),
            launched: launches
                .into_iter()
                .map(|launch| CallbackLaunch {
                    run_id: launch.run_id,
                    status: launch.status,
                })
                .collect(),
        }
    }

    fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            reason: Some(reason.into()),
            run_id: None,
            run_status: None,
            launched: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
struct CallbackLaunch {
    run_id: String,
    status: String,
}

#[derive(Debug, thiserror::Error)]
enum CallbackError {
    #[error("run `{0}` was not found")]
    RunNotFound(String),
    #[error("run `{run_id}` has invalid status `{status}` for callback")]
    InvalidRunStatus { run_id: String, status: String },
    #[error(transparent)]
    GitHub(#[from] crate::github::GitHubError),
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

    fn duplicate(command: &AgentCommand, run_id: String, queue_position: usize) -> Self {
        Self {
            status: "duplicate".to_string(),
            run_id: Some(run_id),
            command: command.raw.clone(),
            line_number: command.line_number,
            queue_position,
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
