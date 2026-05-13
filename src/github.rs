use async_trait::async_trait;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoPermission {
    Admin,
    Maintain,
    Write,
    Triage,
    Read,
    Unknown(String),
}

impl RepoPermission {
    pub fn from_github(value: impl Into<String>) -> Self {
        match value.into().as_str() {
            "admin" => Self::Admin,
            "maintain" => Self::Maintain,
            "write" => Self::Write,
            "triage" => Self::Triage,
            "read" => Self::Read,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn can_invoke_agent(&self) -> bool {
        matches!(self, Self::Admin | Self::Maintain | Self::Write)
    }
}

#[derive(Debug, Clone)]
pub struct GitHubContext {
    pub owner: String,
    pub repo: String,
    pub repo_full_name: String,
    pub pr_number: u64,
    pub installation_id: u64,
}

#[derive(Debug, Clone)]
pub struct CheckRunRequest {
    pub name: String,
    pub head_sha: String,
    pub external_id: String,
    pub summary: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error("github request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("github api returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("failed to read github app private key: {0}")]
    ReadPrivateKey(#[from] std::io::Error),
    #[error("failed to create github app jwt: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("system clock is before unix epoch")]
    Clock,
}

#[async_trait]
pub trait GitHubClient: Send + Sync {
    async fn user_permission(
        &self,
        ctx: &GitHubContext,
        username: &str,
    ) -> Result<RepoPermission, GitHubError>;

    async fn pull_request_head_sha(&self, ctx: &GitHubContext) -> Result<String, GitHubError>;

    async fn run_exists(&self, ctx: &GitHubContext, run_id: &str) -> Result<bool, GitHubError>;

    async fn create_issue_comment(
        &self,
        ctx: &GitHubContext,
        body: &str,
    ) -> Result<(), GitHubError>;

    async fn create_check_run(
        &self,
        ctx: &GitHubContext,
        request: CheckRunRequest,
    ) -> Result<(), GitHubError>;
}

pub fn run_marker(run_id: &str) -> String {
    format!("<!-- kiln:run_id={run_id} -->")
}

pub struct RealGitHubClient {
    app_id: u64,
    private_key: EncodingKey,
    http: reqwest::Client,
    api_base_url: String,
}

impl RealGitHubClient {
    pub fn new(app_id: u64, private_key_path: impl AsRef<Path>) -> Result<Self, GitHubError> {
        let pem = fs::read(private_key_path)?;
        let private_key = EncodingKey::from_rsa_pem(&pem)?;

        Ok(Self {
            app_id,
            private_key,
            http: reqwest::Client::new(),
            api_base_url: "https://api.github.com".to_string(),
        })
    }

    fn app_jwt(&self) -> Result<String, GitHubError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| GitHubError::Clock)?
            .as_secs();

        let claims = AppClaims {
            iat: now.saturating_sub(60),
            exp: now + 540,
            iss: self.app_id.to_string(),
        };

        Ok(encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &self.private_key,
        )?)
    }

    async fn installation_token(&self, installation_id: u64) -> Result<String, GitHubError> {
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.api_base_url
        );
        let jwt = self.app_jwt()?;

        let response = self
            .http
            .post(url)
            .header(USER_AGENT, "kiln")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {jwt}"))
            .send()
            .await?;

        let response = ensure_success(response).await?;
        let token = response.json::<InstallationToken>().await?;
        Ok(token.token)
    }

    async fn get_with_installation_token(
        &self,
        ctx: &GitHubContext,
        path: &str,
    ) -> Result<reqwest::Response, GitHubError> {
        let token = self.installation_token(ctx.installation_id).await?;
        let response = self
            .http
            .get(format!("{}{}", self.api_base_url, path))
            .header(USER_AGENT, "kiln")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await?;

        ensure_success(response).await
    }

    async fn post_with_installation_token<T: Serialize + ?Sized>(
        &self,
        ctx: &GitHubContext,
        path: &str,
        body: &T,
    ) -> Result<reqwest::Response, GitHubError> {
        let token = self.installation_token(ctx.installation_id).await?;
        let response = self
            .http
            .post(format!("{}{}", self.api_base_url, path))
            .header(USER_AGENT, "kiln")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .json(body)
            .send()
            .await?;

        ensure_success(response).await
    }
}

#[async_trait]
impl GitHubClient for RealGitHubClient {
    async fn user_permission(
        &self,
        ctx: &GitHubContext,
        username: &str,
    ) -> Result<RepoPermission, GitHubError> {
        let path = format!(
            "/repos/{}/{}/collaborators/{username}/permission",
            ctx.owner, ctx.repo
        );
        let response = self.get_with_installation_token(ctx, &path).await?;
        let permission = response.json::<PermissionResponse>().await?;
        Ok(RepoPermission::from_github(permission.permission))
    }

    async fn pull_request_head_sha(&self, ctx: &GitHubContext) -> Result<String, GitHubError> {
        let path = format!("/repos/{}/{}/pulls/{}", ctx.owner, ctx.repo, ctx.pr_number);
        let response = self.get_with_installation_token(ctx, &path).await?;
        let pull = response.json::<PullRequestResponse>().await?;
        Ok(pull.head.sha)
    }

    async fn run_exists(&self, ctx: &GitHubContext, run_id: &str) -> Result<bool, GitHubError> {
        let marker = run_marker(run_id);

        let mut page = 1;
        loop {
            let path = format!(
                "/repos/{}/{}/issues/{}/comments?per_page=100&page={page}",
                ctx.owner, ctx.repo, ctx.pr_number
            );
            let response = self.get_with_installation_token(ctx, &path).await?;
            let comments = response.json::<Vec<IssueCommentResponse>>().await?;

            if comments
                .iter()
                .any(|comment| comment.body.contains(&marker))
            {
                return Ok(true);
            }

            if comments.len() < 100 {
                return Ok(false);
            }

            page += 1;
        }
    }

    async fn create_issue_comment(
        &self,
        ctx: &GitHubContext,
        body: &str,
    ) -> Result<(), GitHubError> {
        let path = format!(
            "/repos/{}/{}/issues/{}/comments",
            ctx.owner, ctx.repo, ctx.pr_number
        );
        self.post_with_installation_token(ctx, &path, &CreateCommentBody { body })
            .await?;
        Ok(())
    }

    async fn create_check_run(
        &self,
        ctx: &GitHubContext,
        request: CheckRunRequest,
    ) -> Result<(), GitHubError> {
        let path = format!("/repos/{}/{}/check-runs", ctx.owner, ctx.repo);
        let body = CreateCheckRunBody {
            name: &request.name,
            head_sha: &request.head_sha,
            status: "queued",
            external_id: &request.external_id,
            output: CheckRunOutput {
                title: &request.name,
                summary: &request.summary,
            },
        };
        self.post_with_installation_token(ctx, &path, &body).await?;
        Ok(())
    }
}

async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, GitHubError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable>".to_string());
    Err(GitHubError::Api { status, body })
}

#[derive(Debug, Serialize)]
struct AppClaims {
    iat: u64,
    exp: u64,
    iss: String,
}

#[derive(Debug, Deserialize)]
struct InstallationToken {
    token: String,
}

#[derive(Debug, Deserialize)]
struct PermissionResponse {
    permission: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestResponse {
    head: PullRequestHead,
}

#[derive(Debug, Deserialize)]
struct PullRequestHead {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct IssueCommentResponse {
    body: String,
}

#[derive(Debug, Serialize)]
struct CreateCommentBody<'a> {
    body: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateCheckRunBody<'a> {
    name: &'a str,
    head_sha: &'a str,
    status: &'a str,
    external_id: &'a str,
    output: CheckRunOutput<'a>,
}

#[derive(Debug, Serialize)]
struct CheckRunOutput<'a> {
    title: &'a str,
    summary: &'a str,
}
