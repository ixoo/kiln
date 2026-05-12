#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUser {
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequest {
    pub provider: String,
    pub repository: String,
    pub number: u64,
    pub head_sha: String,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Queued,
    Running,
    Analyzing,
    Editing,
    Testing,
    Pushing,
    Completed,
    Failed,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Analyzing => "analyzing",
            Self::Editing => "editing",
            Self::Testing => "testing",
            Self::Pushing => "pushing",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeComment {
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderToken {
    pub installation_id: u64,
}
