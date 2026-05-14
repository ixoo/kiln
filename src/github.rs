use async_trait::async_trait;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::Arc,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::Mutex;

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

#[derive(Debug, Clone)]
pub struct CheckRunUpdate {
    pub external_id: String,
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueComment {
    pub id: u64,
    pub body: String,
    pub trusted: bool,
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
    #[error("failed to parse github timestamp: {0}")]
    ParseTimestamp(#[from] time::error::Parse),
}

#[async_trait]
pub trait GitHubClient: Send + Sync {
    async fn user_permission(
        &self,
        ctx: &GitHubContext,
        username: &str,
    ) -> Result<RepoPermission, GitHubError>;

    async fn pull_request_head_sha(&self, ctx: &GitHubContext) -> Result<String, GitHubError>;

    async fn check_run_exists(
        &self,
        ctx: &GitHubContext,
        head_sha: &str,
        external_id: &str,
    ) -> Result<bool, GitHubError>;

    async fn issue_comments(&self, ctx: &GitHubContext) -> Result<Vec<IssueComment>, GitHubError>;

    async fn create_issue_comment(
        &self,
        ctx: &GitHubContext,
        body: &str,
    ) -> Result<IssueComment, GitHubError>;

    async fn create_check_run(
        &self,
        ctx: &GitHubContext,
        request: CheckRunRequest,
    ) -> Result<(), GitHubError>;

    async fn update_check_run(
        &self,
        ctx: &GitHubContext,
        update: CheckRunUpdate,
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
    token_cache: Arc<Mutex<HashMap<u64, CachedInstallationToken>>>,
}

impl RealGitHubClient {
    pub fn new(app_id: u64, private_key_path: impl AsRef<Path>) -> Result<Self, GitHubError> {
        let pem = fs::read(private_key_path)?;
        let private_key = EncodingKey::from_rsa_pem(&pem)?;

        Ok(Self {
            app_id,
            private_key,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()?,
            api_base_url: "https://api.github.com".to_string(),
            token_cache: Arc::new(Mutex::new(HashMap::new())),
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
        let now = unix_now()?;
        if let Some(token) = self.token_cache.lock().await.get(&installation_id) {
            if token.expires_at_unix > now + 60 {
                return Ok(token.token.clone());
            }
        }

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
        let token_value = token.token;
        let expires_at_unix = OffsetDateTime::parse(&token.expires_at, &Rfc3339)?
            .unix_timestamp()
            .try_into()
            .unwrap_or(now);
        self.token_cache.lock().await.insert(
            installation_id,
            CachedInstallationToken {
                token: token_value.clone(),
                expires_at_unix,
            },
        );
        Ok(token_value)
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

    async fn patch_with_installation_token<T: Serialize + ?Sized>(
        &self,
        ctx: &GitHubContext,
        path: &str,
        body: &T,
    ) -> Result<reqwest::Response, GitHubError> {
        let token = self.installation_token(ctx.installation_id).await?;
        let response = self
            .http
            .patch(format!("{}{}", self.api_base_url, path))
            .header(USER_AGENT, "kiln")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .json(body)
            .send()
            .await?;

        ensure_success(response).await
    }

    async fn find_check_run_id(
        &self,
        ctx: &GitHubContext,
        head_sha: &str,
        external_id: &str,
    ) -> Result<Option<u64>, GitHubError> {
        let mut page = 1;
        loop {
            let path = format!(
                "/repos/{}/{}/commits/{}/check-runs?per_page=100&page={page}",
                ctx.owner, ctx.repo, head_sha
            );
            let response = self.get_with_installation_token(ctx, &path).await?;
            let response = response.json::<CheckRunsResponse>().await?;

            if let Some(check) = response
                .check_runs
                .iter()
                .find(|check| check.external_id.as_deref() == Some(external_id))
            {
                return Ok(Some(check.id));
            }

            if response.check_runs.len() < 100 {
                return Ok(None);
            }

            page += 1;
        }
    }

    async fn create_check_run_with_status(
        &self,
        ctx: &GitHubContext,
        request: CheckRunPost<'_>,
    ) -> Result<(), GitHubError> {
        let path = format!("/repos/{}/{}/check-runs", ctx.owner, ctx.repo);
        let body = CreateCheckRunBody {
            name: request.name,
            head_sha: request.head_sha,
            status: request.status,
            conclusion: request.conclusion,
            external_id: request.external_id,
            output: CheckRunOutput {
                title: request.name,
                summary: request.summary,
            },
        };
        self.post_with_installation_token(ctx, &path, &body).await?;
        Ok(())
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
        let token = self.installation_token(ctx.installation_id).await?;
        let response = self
            .http
            .get(format!("{}{}", self.api_base_url, path))
            .header(USER_AGENT, "kiln")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(RepoPermission::Read);
        }
        let response = ensure_success(response).await?;
        let permission = response.json::<PermissionResponse>().await?;
        Ok(RepoPermission::from_github(permission.permission))
    }

    async fn pull_request_head_sha(&self, ctx: &GitHubContext) -> Result<String, GitHubError> {
        let path = format!("/repos/{}/{}/pulls/{}", ctx.owner, ctx.repo, ctx.pr_number);
        let response = self.get_with_installation_token(ctx, &path).await?;
        let pull = response.json::<PullRequestResponse>().await?;
        Ok(pull.head.sha)
    }

    async fn check_run_exists(
        &self,
        ctx: &GitHubContext,
        head_sha: &str,
        external_id: &str,
    ) -> Result<bool, GitHubError> {
        Ok(self
            .find_check_run_id(ctx, head_sha, external_id)
            .await?
            .is_some())
    }

    async fn issue_comments(&self, ctx: &GitHubContext) -> Result<Vec<IssueComment>, GitHubError> {
        let mut all_comments = Vec::new();

        let mut page = 1;
        loop {
            let path = format!(
                "/repos/{}/{}/issues/{}/comments?per_page=100&page={page}",
                ctx.owner, ctx.repo, ctx.pr_number
            );
            let response = self.get_with_installation_token(ctx, &path).await?;
            let comments = response.json::<Vec<IssueCommentResponse>>().await?;

            all_comments.extend(comments.iter().map(|comment| {
                IssueComment {
                    id: comment.id,
                    body: comment.body.clone(),
                    trusted: comment
                        .performed_via_github_app
                        .as_ref()
                        .is_some_and(|app| app.id == self.app_id),
                }
            }));

            if comments.len() < 100 {
                return Ok(all_comments);
            }

            page += 1;
        }
    }

    async fn create_issue_comment(
        &self,
        ctx: &GitHubContext,
        body: &str,
    ) -> Result<IssueComment, GitHubError> {
        let path = format!(
            "/repos/{}/{}/issues/{}/comments",
            ctx.owner, ctx.repo, ctx.pr_number
        );
        let response = self
            .post_with_installation_token(ctx, &path, &CreateCommentBody { body })
            .await?;
        let comment = response.json::<IssueCommentResponse>().await?;
        Ok(IssueComment {
            id: comment.id,
            body: comment.body,
            trusted: comment
                .performed_via_github_app
                .as_ref()
                .is_some_and(|app| app.id == self.app_id),
        })
    }

    async fn create_check_run(
        &self,
        ctx: &GitHubContext,
        request: CheckRunRequest,
    ) -> Result<(), GitHubError> {
        self.create_check_run_with_status(
            ctx,
            CheckRunPost {
                name: &request.name,
                head_sha: &request.head_sha,
                external_id: &request.external_id,
                status: "queued",
                conclusion: None,
                summary: &request.summary,
            },
        )
        .await
    }

    async fn update_check_run(
        &self,
        ctx: &GitHubContext,
        update: CheckRunUpdate,
    ) -> Result<(), GitHubError> {
        let Some(check_run_id) = self
            .find_check_run_id(ctx, &update.head_sha, &update.external_id)
            .await?
        else {
            return self
                .create_check_run_with_status(
                    ctx,
                    CheckRunPost {
                        name: &format!("kiln/recovered ({})", update.external_id),
                        head_sha: &update.head_sha,
                        external_id: &update.external_id,
                        status: &update.status,
                        conclusion: update.conclusion.as_deref(),
                        summary: &update.summary,
                    },
                )
                .await;
        };

        let path = format!(
            "/repos/{}/{}/check-runs/{check_run_id}",
            ctx.owner, ctx.repo
        );
        let body = UpdateCheckRunBody {
            status: &update.status,
            conclusion: update.conclusion.as_deref(),
            output: CheckRunOutput {
                title: "Kiln agent run",
                summary: &update.summary,
            },
        };
        self.patch_with_installation_token(ctx, &path, &body)
            .await?;
        Ok(())
    }
}

fn unix_now() -> Result<u64, GitHubError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GitHubError::Clock)?
        .as_secs())
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
    expires_at: String,
}

#[derive(Debug, Clone)]
struct CachedInstallationToken {
    token: String,
    expires_at_unix: u64,
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
    id: u64,
    body: String,
    #[serde(default)]
    performed_via_github_app: Option<GitHubAppResponse>,
}

#[derive(Debug, Deserialize)]
struct GitHubAppResponse {
    id: u64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    conclusion: Option<&'a str>,
    external_id: &'a str,
    output: CheckRunOutput<'a>,
}

struct CheckRunPost<'a> {
    name: &'a str,
    head_sha: &'a str,
    external_id: &'a str,
    status: &'a str,
    conclusion: Option<&'a str>,
    summary: &'a str,
}

#[derive(Debug, Serialize)]
struct UpdateCheckRunBody<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    conclusion: Option<&'a str>,
    output: CheckRunOutput<'a>,
}

#[derive(Debug, Serialize)]
struct CheckRunOutput<'a> {
    title: &'a str,
    summary: &'a str,
}

#[derive(Debug, Deserialize)]
struct CheckRunsResponse {
    check_runs: Vec<CheckRunResponse>,
}

#[derive(Debug, Deserialize)]
struct CheckRunResponse {
    id: u64,
    external_id: Option<String>,
}
