use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct GithubConnection {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner: String,
    pub repo: String,
    #[serde(skip_serializing)]
    pub access_token: String,
    pub default_branch: String,
    pub last_checked_sha: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CommitSummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub github_connection_id: Uuid,
    pub sha: String,
    pub author: Option<String>,
    pub message: Option<String>,
    pub committed_at: Option<DateTime<Utc>>,
    pub files_changed: Option<serde_json::Value>,
    pub summary: Option<String>,
    pub raw_data: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AgentLog {
    pub id: Uuid,
    pub project_id: Uuid,
    pub level: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}
