use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use hmac::{Hmac, Mac};
use kiln::{
    build_app, config::ServerSettings, github::run_marker, CheckRunRequest, GitHubClient,
    GitHubContext, GitHubError, RepoPermission, RuntimeConfig, Settings,
};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
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
    comments: Vec<String>,
    checks: Vec<CheckRunRequest>,
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
            })),
        }
    }

    fn comments(&self) -> Vec<String> {
        self.inner.lock().unwrap().comments.clone()
    }

    fn checks(&self) -> Vec<CheckRunRequest> {
        self.inner.lock().unwrap().checks.clone()
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

    async fn run_exists(&self, _ctx: &GitHubContext, run_id: &str) -> Result<bool, GitHubError> {
        Ok(self.inner.lock().unwrap().existing_runs.contains(run_id))
    }

    async fn create_issue_comment(
        &self,
        _ctx: &GitHubContext,
        body: &str,
    ) -> Result<(), GitHubError> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(run_id) = extract_run_id(body) {
            inner.existing_runs.insert(run_id);
        }
        inner.comments.push(body.to_string());
        Ok(())
    }

    async fn create_check_run(
        &self,
        _ctx: &GitHubContext,
        request: CheckRunRequest,
    ) -> Result<(), GitHubError> {
        self.inner.lock().unwrap().checks.push(request);
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
    build_app(
        RuntimeConfig {
            settings: settings(),
            webhook_secret: "test-secret".to_string(),
        },
        Arc::new(github),
    )
}

fn settings() -> Settings {
    Settings {
        server: ServerSettings {
            bind_address: "127.0.0.1:3000".to_string(),
        },
    }
}

fn payload_with_body(comment_body: &str) -> Value {
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
            "id": 1001,
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

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn run_marker_is_stable_metadata() {
    assert_eq!(run_marker("kiln_test"), "<!-- kiln:run_id=kiln_test -->");
}
