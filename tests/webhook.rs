use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use hmac::{Hmac, Mac};
use kiln::{
    build_app, config::ServerSettings, execution::callback_token, github::run_marker, AgentJob,
    CheckRunRequest, CheckRunUpdate, DisabledJobLauncher, GitHubClient, GitHubContext, GitHubError,
    IssueComment, JobLaunchError, JobLaunchResult, JobLauncher, RepoPermission, RuntimeConfig,
    Settings,
};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
use tokio::time::{sleep, Duration};
use tower::ServiceExt;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct MockGitHubClient {
    inner: Arc<Mutex<MockState>>,
}

struct MockState {
    permission: RepoPermission,
    head_sha: String,
    existing_runs: HashSet<String>,
    comments: Vec<IssueComment>,
    checks: Vec<CheckRunRequest>,
    check_updates: Vec<CheckRunUpdate>,
    next_comment_id: u64,
    fail_next_comment: bool,
}

#[derive(Clone)]
struct RecordingLauncher {
    jobs: Arc<Mutex<Vec<AgentJob>>>,
    fail: bool,
    terminal: bool,
}

impl RecordingLauncher {
    fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            fail: false,
            terminal: true,
        }
    }

    fn nonterminal() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            fail: false,
            terminal: false,
        }
    }

    fn failing() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            fail: true,
            terminal: true,
        }
    }

    fn jobs(&self) -> Vec<AgentJob> {
        self.jobs.lock().unwrap().clone()
    }
}

#[async_trait]
impl JobLauncher for RecordingLauncher {
    async fn launch(&self, job: AgentJob) -> Result<JobLaunchResult, JobLaunchError> {
        self.jobs.lock().unwrap().push(job);

        if self.fail {
            return Err(JobLaunchError::Disabled);
        }

        Ok(JobLaunchResult {
            status: "recorded".to_string(),
            external_id: Some("job-recorded".to_string()),
            terminal: self.terminal,
        })
    }
}

impl MockGitHubClient {
    fn new(permission: RepoPermission) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockState {
                permission,
                head_sha: "abc123".to_string(),
                existing_runs: HashSet::new(),
                comments: Vec::new(),
                checks: Vec::new(),
                check_updates: Vec::new(),
                next_comment_id: 1,
                fail_next_comment: false,
            })),
        }
    }

    fn comments(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .comments
            .iter()
            .map(|comment| comment.body.clone())
            .collect()
    }

    fn checks(&self) -> Vec<CheckRunRequest> {
        self.inner.lock().unwrap().checks.clone()
    }

    fn check_updates(&self) -> Vec<CheckRunUpdate> {
        self.inner.lock().unwrap().check_updates.clone()
    }

    fn add_comment(&self, body: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_comment_id;
        inner.next_comment_id += 1;
        inner.comments.push(IssueComment {
            id,
            body: body.into(),
            trusted: false,
        });
    }

    fn fail_next_comment(&self) {
        self.inner.lock().unwrap().fail_next_comment = true;
    }

    fn set_head_sha(&self, head_sha: impl Into<String>) {
        self.inner.lock().unwrap().head_sha = head_sha.into();
    }
}

#[async_trait]
impl GitHubClient for MockGitHubClient {
    async fn user_permission(
        &self,
        _ctx: &GitHubContext,
        _username: &str,
    ) -> Result<RepoPermission, GitHubError> {
        Ok(self.inner.lock().unwrap().permission.clone())
    }

    async fn pull_request_head_sha(&self, _ctx: &GitHubContext) -> Result<String, GitHubError> {
        Ok(self.inner.lock().unwrap().head_sha.clone())
    }

    async fn check_run_exists(
        &self,
        _ctx: &GitHubContext,
        _head_sha: &str,
        external_id: &str,
    ) -> Result<bool, GitHubError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .checks
            .iter()
            .any(|check| check.external_id == external_id))
    }

    async fn issue_comments(&self, _ctx: &GitHubContext) -> Result<Vec<IssueComment>, GitHubError> {
        Ok(self.inner.lock().unwrap().comments.clone())
    }

    async fn create_issue_comment(
        &self,
        _ctx: &GitHubContext,
        body: &str,
    ) -> Result<IssueComment, GitHubError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_next_comment {
            inner.fail_next_comment = false;
            return Err(GitHubError::Api {
                status: StatusCode::BAD_GATEWAY,
                body: "mock comment failure".to_string(),
            });
        }
        if let Some(run_id) = extract_run_id(body) {
            inner.existing_runs.insert(run_id);
        }
        let id = inner.next_comment_id;
        inner.next_comment_id += 1;
        inner.comments.push(IssueComment {
            id,
            body: body.to_string(),
            trusted: true,
        });
        Ok(IssueComment {
            id,
            body: body.to_string(),
            trusted: true,
        })
    }

    async fn create_check_run(
        &self,
        _ctx: &GitHubContext,
        request: CheckRunRequest,
    ) -> Result<(), GitHubError> {
        self.inner.lock().unwrap().checks.push(request);
        Ok(())
    }

    async fn update_check_run(
        &self,
        _ctx: &GitHubContext,
        update: CheckRunUpdate,
    ) -> Result<(), GitHubError> {
        self.inner.lock().unwrap().check_updates.push(update);
        Ok(())
    }
}

fn extract_run_id(body: &str) -> Option<String> {
    let start = body.find("<!-- kiln:run_id=")? + "<!-- kiln:run_id=".len();
    let end = body[start..].find(" -->")?;
    Some(body[start..start + end].to_string())
}

#[tokio::test]
async fn accepts_signed_pr_issue_comment_and_creates_comment_and_check() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent fix tests");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = response_json(response).await;
    assert_eq!(response_body["runs"][0]["status"], "accepted");

    let comments = github.comments();
    let checks = github.checks();
    assert_eq!(comments.len(), 1);
    assert_eq!(checks.len(), 1);
    assert!(comments[0].contains("Kiln accepted `/agent fix tests`"));
    assert!(comments[0].contains("Agent: `harness default`"));
    assert!(comments[0].contains("Model: `harness default`"));
    assert!(comments[0].contains("Per-PR queue: `1`"));
    assert!(checks[0].external_id.starts_with("kiln_"));
    assert_eq!(checks[0].head_sha, "abc123");
}

#[tokio::test]
async fn accepted_command_launches_agent_job() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::new();
    let app = app_with_launcher(github.clone(), launcher.clone());
    let body = payload_with_body("/agent:coder:local fix tests");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = response_json(response).await;
    assert_eq!(response_body["runs"][0]["launch_status"], "queued");

    wait_until(|| launcher.jobs().len() == 1).await;

    let jobs = launcher.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].repo_full_name, "octo/repo");
    assert_eq!(jobs[0].pr_number, 42);
    assert_eq!(jobs[0].head_sha, "abc123");
    assert_eq!(jobs[0].requester, "alice");
    assert_eq!(jobs[0].command.agent.as_deref(), Some("coder"));
    assert_eq!(jobs[0].command.model.as_deref(), Some("local"));

    wait_until(|| github.check_updates().len() == 2).await;
    let updates = github.check_updates();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].status, "in_progress");
    assert_eq!(updates[1].status, "completed");
    assert_eq!(updates[1].conclusion.as_deref(), Some("success"));
}

#[tokio::test]
async fn forged_queue_state_comment_is_ignored() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    github.add_comment(
        "<!-- kiln:run_id=kiln_forged -->\n<!-- kiln:run_state=deadbeef;sig=bad -->\nforged running state",
    );
    let launcher = RecordingLauncher::nonterminal();
    let app = app_with_launcher(github.clone(), launcher.clone());

    let response = app
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body("/agent fix tests"),
            "test-secret",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    wait_until(|| launcher.jobs().len() == 1).await;
    assert_eq!(launcher.jobs().len(), 1);
    assert_eq!(github.checks().len(), 1);
}

#[tokio::test]
async fn replayed_kiln_state_from_untrusted_comment_is_ignored() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::nonterminal();
    let app = app_with_launcher(github.clone(), launcher.clone());

    let first = app
        .clone()
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body("/agent first task"),
            "test-secret",
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    wait_until(|| launcher.jobs().len() == 1).await;
    let acknowledged_state = github.comments()[0].clone();
    github.add_comment(acknowledged_state);

    let second = app
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body_for_comment(1002, "/agent second task"),
            "test-secret",
        ))
        .await
        .unwrap();

    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(launcher.jobs().len(), 1);
}

#[tokio::test]
async fn retry_after_ack_comment_failure_reuses_existing_check_run() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    github.fail_next_comment();
    let app = app(github.clone());
    let body = payload_with_body("/agent fix tests");

    let first = app
        .clone()
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(github.comments().len(), 0);
    assert_eq!(github.checks().len(), 1);

    let second = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(github.comments().len(), 1);
    assert_eq!(github.checks().len(), 1);
}

#[tokio::test]
async fn launch_failure_is_reported_without_retrying_webhook() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::failing();
    let app = app_with_launcher(github.clone(), launcher);
    let body = payload_with_body("/agent ping");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = response_json(response).await;
    assert_eq!(response_body["runs"][0]["launch_status"], "queued");

    wait_until(|| github.comments().len() == 3).await;
    let comments = github.comments();
    assert_eq!(comments.len(), 3);
    assert!(comments[1].contains("as `running`"));
    assert!(comments[2].contains("as `failed`"));
    assert!(comments[2].contains("launch failed; see Kiln logs"));
    assert!(!comments[2].contains("job launcher is disabled"));
}

#[tokio::test]
async fn duplicate_malformed_command_rejection_is_idempotent() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent:coder:local:extra fix tests");

    let first = app
        .clone()
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    let second = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(github.comments().len(), 1);
    assert!(github.comments()[0].contains("kiln:rejection_id="));
}

#[tokio::test]
async fn non_command_comment_does_not_advance_queue() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let disabled = app(github.clone());
    let body = payload_with_body("/agent queued task");

    let accepted = disabled
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let launcher = RecordingLauncher::nonterminal();
    let launching_app = app_with_launcher(github, launcher.clone());
    let ignored = launching_app
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body_for_comment(1002, "plain comment"),
            "test-secret",
        ))
        .await
        .unwrap();

    assert_eq!(ignored.status(), StatusCode::OK);
    sleep(Duration::from_millis(100)).await;
    assert_eq!(launcher.jobs().len(), 0);
}

#[tokio::test]
async fn queued_run_fails_instead_of_launching_when_pr_head_changes() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let disabled = app(github.clone());
    let body = payload_with_body("/agent queued task");

    let accepted = disabled
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    github.set_head_sha("def456");
    let launcher = RecordingLauncher::nonterminal();
    let launching_app = app_with_launcher(github.clone(), launcher.clone());
    let duplicate = launching_app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(duplicate.status(), StatusCode::OK);
    wait_until(|| {
        github
            .comments()
            .iter()
            .any(|comment| comment.contains("PR head changed before launch"))
    })
    .await;
    assert_eq!(launcher.jobs().len(), 0);
    assert!(github
        .check_updates()
        .iter()
        .any(|update| update.conclusion.as_deref() == Some("failure")));
}

#[tokio::test]
async fn previous_state_secret_preserves_duplicate_detection_during_rotation() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let old_app = app_with_state_secrets(
        github.clone(),
        DisabledJobLauncher,
        "old-state-secret",
        Vec::new(),
    );
    let body = payload_with_body("/agent rotate safely");

    let first = old_app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let new_app = app_with_state_secrets(
        github.clone(),
        DisabledJobLauncher,
        "new-state-secret",
        vec!["old-state-secret".to_string()],
    );
    let second = new_app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(github.comments().len(), 1);
    assert_eq!(github.checks().len(), 1);
}

#[tokio::test]
async fn launch_lease_prevents_two_workers_from_launching_same_queued_run() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let disabled = app(github.clone());
    let body = payload_with_body("/agent one launch only");

    let accepted = disabled
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let first_launcher = RecordingLauncher::nonterminal();
    let second_launcher = RecordingLauncher::nonterminal();
    let first_app = app_with_launcher(github.clone(), first_launcher.clone());
    let second_app = app_with_launcher(github, second_launcher.clone());

    let (first, second) = tokio::join!(
        first_app.oneshot(signed_request("issue_comment", &body, "test-secret")),
        second_app.oneshot(signed_request("issue_comment", &body, "test-secret")),
    );

    assert_eq!(first.unwrap().status(), StatusCode::OK);
    assert_eq!(second.unwrap().status(), StatusCode::OK);
    wait_until(|| first_launcher.jobs().len() + second_launcher.jobs().len() == 1).await;
}

#[tokio::test]
async fn duplicate_delivery_is_idempotent() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent fix tests");

    let first = app
        .clone()
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    assert_eq!(github.comments().len(), 1);
    assert_eq!(github.checks().len(), 1);
}

#[tokio::test]
async fn concurrent_duplicate_delivery_is_idempotent() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent fix tests");

    let (first, second) = tokio::join!(
        app.clone()
            .oneshot(signed_request("issue_comment", &body, "test-secret")),
        app.oneshot(signed_request("issue_comment", &body, "test-secret")),
    );

    assert_eq!(first.unwrap().status(), StatusCode::OK);
    assert_eq!(second.unwrap().status(), StatusCode::OK);
    assert_eq!(github.comments().len(), 1);
    assert_eq!(github.checks().len(), 1);
}

#[tokio::test]
async fn multiple_commands_are_queued_per_pr_in_order() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body =
        payload_with_body("/agent first task\nplain text\n/agent:reviewer:local review this");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let comments = github.comments();
    let checks = github.checks();
    assert_eq!(comments.len(), 2);
    assert_eq!(checks.len(), 2);
    assert!(comments[0].contains("Per-PR queue: `1`"));
    assert!(comments[1].contains("Per-PR queue: `2`"));
    assert!(comments[1].contains("Agent: `reviewer`"));
    assert!(comments[1].contains("Model: `local`"));
    assert!(checks[0].summary.contains("queue position: 1"));
    assert!(checks[1].summary.contains("queue position: 2"));
}

#[tokio::test]
async fn multiple_commands_launch_in_comment_order_with_fast_terminal_launcher() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::new();
    let app = app_with_launcher(github.clone(), launcher.clone());
    let body = payload_with_body("/agent first task\n/agent second task");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    wait_until(|| launcher.jobs().len() == 2).await;

    let jobs = launcher.jobs();
    assert_eq!(jobs[0].command.raw, "/agent first task");
    assert_eq!(jobs[0].queue_position, 1);
    assert_eq!(jobs[1].command.raw, "/agent second task");
    assert_eq!(jobs[1].queue_position, 2);

    let comments = github.comments();
    assert!(comments[0].contains("Per-PR queue: `1`"));
    assert!(comments[1].contains("Per-PR queue: `2`"));
}

#[tokio::test]
async fn second_command_stays_queued_when_pr_has_running_agent() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::nonterminal();
    let app = app_with_launcher(github.clone(), launcher.clone());

    let first = app
        .clone()
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body("/agent first task"),
            "test-secret",
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    wait_until(|| launcher.jobs().len() == 1).await;

    let second = app
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body_for_comment(1002, "/agent second task"),
            "test-secret",
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    let response_body = response_json(second).await;
    assert_eq!(response_body["runs"][0]["launch_status"], "queued");
    assert_eq!(response_body["runs"][0]["queue_position"], 2);

    let jobs = launcher.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].command.raw, "/agent first task");
    assert!(github
        .comments()
        .iter()
        .any(|comment| comment.contains("as `running`")));
}

#[tokio::test]
async fn stale_running_run_is_failed_and_queue_advances() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::nonterminal();
    let mut settings = settings();
    settings.execution.stale_run_seconds = 1;
    let app = app_with_launcher_and_settings(github.clone(), launcher.clone(), settings);

    let first = app
        .clone()
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body("/agent first task"),
            "test-secret",
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    wait_until(|| launcher.jobs().len() == 1).await;

    sleep(Duration::from_millis(1100)).await;

    let second = app
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body_for_comment(1002, "/agent second task"),
            "test-secret",
        ))
        .await
        .unwrap();

    assert_eq!(second.status(), StatusCode::OK);
    wait_until(|| launcher.jobs().len() == 2).await;
    assert!(github
        .comments()
        .iter()
        .any(|comment| comment.contains("run exceeded stale timeout")));
    assert!(github
        .check_updates()
        .iter()
        .any(|update| update.conclusion.as_deref() == Some("failure")));
}

#[tokio::test]
async fn stale_running_run_is_failed_without_another_webhook() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::nonterminal();
    let mut settings = settings();
    settings.execution.stale_run_seconds = 1;
    let app = app_with_launcher_and_settings(github.clone(), launcher.clone(), settings);

    let response = app
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body("/agent eventually stale"),
            "test-secret",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    wait_until(|| launcher.jobs().len() == 1).await;

    wait_until(|| {
        github
            .comments()
            .iter()
            .any(|comment| comment.contains("run exceeded stale timeout"))
    })
    .await;
    assert!(github
        .check_updates()
        .iter()
        .any(|update| update.conclusion.as_deref() == Some("failure")));
}

#[tokio::test]
async fn callback_rejects_mismatched_repository_fields() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github);
    let run_id = "kiln_missing";
    let mut payload = agent_callback_payload(run_id, "completed");
    payload["repo_full_name"] = json!("octo/other");

    let response = app
        .oneshot(agent_callback_request(
            payload,
            &callback_token("callback-secret", run_id, "octo/other", 42, 999),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn agent_callback_completes_running_run_and_launches_next_queued_run() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::nonterminal();
    let app = app_with_launcher(github.clone(), launcher.clone());

    let first = app
        .clone()
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body("/agent first task"),
            "test-secret",
        ))
        .await
        .unwrap();
    let first_body = response_json(first).await;
    let first_run_id = first_body["runs"][0]["run_id"].as_str().unwrap();
    wait_until(|| launcher.jobs().len() == 1).await;

    let second = app
        .clone()
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body_for_comment(1002, "/agent second task"),
            "test-secret",
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(launcher.jobs().len(), 1);

    let callback = app
        .oneshot(agent_callback_request(
            agent_callback_payload(first_run_id, "completed"),
            &test_callback_token(first_run_id),
        ))
        .await
        .unwrap();

    assert_eq!(callback.status(), StatusCode::OK);
    let callback_body = response_json(callback).await;
    assert_eq!(callback_body["run_status"], "completed");
    assert!(callback_body["launched"]
        .as_array()
        .is_none_or(Vec::is_empty));

    wait_until(|| launcher.jobs().len() == 2).await;
    let jobs = launcher.jobs();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[1].command.raw, "/agent second task");
    assert!(github
        .comments()
        .iter()
        .any(|comment| comment.contains("as `completed`")));
}

#[tokio::test]
async fn agent_callback_detail_is_not_echoed_to_public_comments() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let launcher = RecordingLauncher::nonterminal();
    let app = app_with_launcher(github.clone(), launcher.clone());

    let first = app
        .clone()
        .oneshot(signed_request(
            "issue_comment",
            &payload_with_body("/agent protect details"),
            "test-secret",
        ))
        .await
        .unwrap();
    let first_body = response_json(first).await;
    let run_id = first_body["runs"][0]["run_id"].as_str().unwrap();
    wait_until(|| launcher.jobs().len() == 1).await;

    let mut payload = agent_callback_payload(run_id, "completed");
    payload["detail"] = json!("secret-token-from-agent");
    let callback = app
        .oneshot(agent_callback_request(
            payload,
            &test_callback_token(run_id),
        ))
        .await
        .unwrap();

    assert_eq!(callback.status(), StatusCode::OK);
    assert!(!github
        .comments()
        .iter()
        .any(|comment| comment.contains("secret-token-from-agent")));
    assert!(github
        .comments()
        .iter()
        .any(|comment| comment.contains("agent provided completion detail")));
}

#[tokio::test]
async fn agent_callback_requires_secret() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github);

    let response = app
        .oneshot(agent_callback_request(
            agent_callback_payload("kiln_missing", "completed"),
            "wrong-token",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_agent_and_model_are_preserved_as_opaque_metadata() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent:any-harness:any-model run this");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(github.comments().len(), 1);
    assert_eq!(github.checks().len(), 1);
    assert!(github.comments()[0].contains("Agent: `any-harness`"));
    assert!(github.comments()[0].contains("Model: `any-model`"));
}

#[tokio::test]
async fn malformed_command_gets_rejection_comment_without_check() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent:coder:local:extra fix tests");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(github.comments().len(), 1);
    assert_eq!(github.checks().len(), 0);
    assert!(github.comments()[0].contains("could not accept command"));
}

#[tokio::test]
async fn ignores_non_command_agent_prefix_words() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agentic please review\n/agents please review");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = response_json(response).await;
    assert_eq!(response_body["status"], "ignored");
    assert_eq!(github.comments().len(), 0);
    assert_eq!(github.checks().len(), 0);
}

#[tokio::test]
async fn unauthorized_requester_gets_rejection_comment_without_check() {
    let github = MockGitHubClient::new(RepoPermission::Read);
    let app = app(github.clone());
    let body = payload_with_body("/agent fix tests");

    let response = app
        .oneshot(signed_request("issue_comment", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(github.comments().len(), 1);
    assert_eq!(github.checks().len(), 0);
    assert!(github.comments()[0].contains("write, maintain, or admin"));
}

#[tokio::test]
async fn ignored_events_return_200_without_github_calls() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent fix tests");

    let response = app
        .oneshot(signed_request("ping", &body, "test-secret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = response_json(response).await;
    assert_eq!(response_body["status"], "ignored");
    assert_eq!(github.comments().len(), 0);
    assert_eq!(github.checks().len(), 0);
}

#[tokio::test]
async fn invalid_signature_returns_401() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github.clone());
    let body = payload_with_body("/agent fix tests");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .header("x-github-event", "issue_comment")
                .header("x-hub-signature-256", "sha256=bad")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(github.comments().len(), 0);
    assert_eq!(github.checks().len(), 0);
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let github = MockGitHubClient::new(RepoPermission::Write);
    let app = app(github);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

fn app(github: MockGitHubClient) -> axum::Router {
    app_with_launcher(github, DisabledJobLauncher)
}

fn app_with_launcher(
    github: MockGitHubClient,
    launcher: impl JobLauncher + 'static,
) -> axum::Router {
    app_with_launcher_and_settings(github, launcher, settings())
}

fn app_with_launcher_and_settings(
    github: MockGitHubClient,
    launcher: impl JobLauncher + 'static,
    settings: Settings,
) -> axum::Router {
    app_with_launcher_settings_and_state_secrets(
        github,
        launcher,
        settings,
        "state-secret",
        Vec::new(),
    )
}

fn app_with_state_secrets(
    github: MockGitHubClient,
    launcher: impl JobLauncher + 'static,
    state_secret: &str,
    previous_state_secrets: Vec<String>,
) -> axum::Router {
    app_with_launcher_settings_and_state_secrets(
        github,
        launcher,
        settings(),
        state_secret,
        previous_state_secrets,
    )
}

fn app_with_launcher_settings_and_state_secrets(
    github: MockGitHubClient,
    launcher: impl JobLauncher + 'static,
    settings: Settings,
    state_secret: &str,
    previous_state_secrets: Vec<String>,
) -> axum::Router {
    build_app(
        RuntimeConfig {
            settings,
            webhook_secret: "test-secret".to_string(),
            agent_callback_secret: Some("callback-secret".to_string()),
            state_secret: state_secret.to_string(),
            previous_state_secrets,
        },
        Arc::new(github),
        Arc::new(launcher),
    )
}

fn settings() -> Settings {
    Settings {
        server: ServerSettings {
            bind_address: "127.0.0.1:3000".to_string(),
        },
        execution: Default::default(),
    }
}

fn payload_with_body(comment_body: &str) -> Value {
    payload_with_body_for_comment(1001, comment_body)
}

fn payload_with_body_for_comment(comment_id: u64, comment_body: &str) -> Value {
    json!({
        "action": "created",
        "repository": {
            "full_name": "octo/repo",
            "name": "repo",
            "owner": { "login": "octo" }
        },
        "issue": {
            "number": 42,
            "pull_request": { "url": "https://api.github.com/repos/octo/repo/pulls/42" }
        },
        "comment": {
            "id": comment_id,
            "body": comment_body
        },
        "sender": { "login": "alice" },
        "installation": { "id": 999 }
    })
}

fn signed_request(event: &str, body: &Value, secret: &str) -> Request<Body> {
    let body = body.to_string();
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

    Request::builder()
        .method("POST")
        .uri("/webhooks/github")
        .header("x-github-event", event)
        .header("x-hub-signature-256", signature)
        .body(Body::from(body))
        .unwrap()
}

fn agent_callback_payload(run_id: &str, status: &str) -> Value {
    json!({
        "run_id": run_id,
        "status": status,
        "owner": "octo",
        "repo": "repo",
        "repo_full_name": "octo/repo",
        "pr_number": 42,
        "installation_id": 999,
        "detail": "agent finished"
    })
}

fn agent_callback_request(body: Value, secret: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/callbacks/agent")
        .header("content-type", "application/json")
        .header("x-kiln-callback-token", secret)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn test_callback_token(run_id: &str) -> String {
    callback_token("callback-secret", run_id, "octo/repo", 42, 999)
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..250 {
        if predicate() {
            return;
        }

        sleep(Duration::from_millis(20)).await;
    }

    assert!(predicate());
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn run_marker_is_stable_metadata() {
    assert_eq!(run_marker("kiln_test"), "<!-- kiln:run_id=kiln_test -->");
}
