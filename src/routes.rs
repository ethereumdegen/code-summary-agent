use axum::{
    Extension,
    Router,
    extract::{FromRequestParts, Path},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::AgentUser;
use crate::models::{CommitSummary, GithubConnection};

#[derive(Clone)]
pub struct AgentState {
    pub pool: PgPool,
    pub http_client: reqwest::Client,
}

/// Build the router for the code-summary-agent.
pub fn router<S, U>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    U: AgentUser + FromRequestParts<S> + Send + 'static,
    <U as FromRequestParts<S>>::Rejection: Into<Response>,
{
    Router::new()
        .route("/projects/{id}/github/auth-url", get(get_auth_url::<S, U>))
        .route("/github/callback", get(github_callback::<S, U>))
        .route("/projects/{id}/github", get(get_connection::<S, U>))
        .route("/projects/{id}/github", delete(disconnect_github::<S, U>))
        .route("/projects/{id}/github/repos", get(list_repos::<S, U>))
        .route("/projects/{id}/github/link-repo", post(link_repo::<S, U>))
        .route("/projects/{id}/summaries", get(list_summaries::<S, U>))
        .route("/projects/{id}/trigger", post(trigger_run::<S, U>))
        .route("/projects/{id}/logs", get(list_logs::<S, U>))
}

/// Helper to load a setting from app_settings table.
async fn get_setting(pool: &PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM app_settings WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()?
}

// --- Route handlers ---

async fn get_auth_url<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let client_id = get_setting(&agent.pool, "GITHUB_CLIENT_ID")
        .await
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let frontend_url = get_setting(&agent.pool, "FRONTEND_URL")
        .await
        .unwrap_or_else(|| "https://starflaskdigital.com".into());

    let redirect_uri = format!("{}/api/agent/github/callback", frontend_url.trim_end_matches('/'));
    let state = project_id.to_string();

    let url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=repo&state={}",
        client_id,
        urlencoded(&redirect_uri),
        state,
    );

    Ok(Json(json!({ "url": url })))
}

async fn github_callback<S, U>(
    user: U,
    axum::extract::Query(params): axum::extract::Query<CallbackParams>,
    Extension(agent): Extension<AgentState>,
) -> Result<Response, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let code = params.code.ok_or(StatusCode::BAD_REQUEST)?;
    let project_id: Uuid = params.state.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let client_id = get_setting(&agent.pool, "GITHUB_CLIENT_ID")
        .await
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let client_secret = get_setting(&agent.pool, "GITHUB_CLIENT_SECRET")
        .await
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Exchange code for access token
    let token_resp = agent
        .http_client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .json(&json!({
            "client_id": client_id,
            "client_secret": client_secret,
            "code": code,
        }))
        .send()
        .await
        .map_err(|e| {
            tracing::error!("[Agent] GitHub token exchange failed: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    let token_json: Value = token_resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let access_token = token_json["access_token"]
        .as_str()
        .ok_or(StatusCode::BAD_GATEWAY)?
        .to_string();

    // Upsert github_connections
    sqlx::query(
        "INSERT INTO github_connections (project_id, access_token) VALUES ($1, $2) \
         ON CONFLICT (project_id) DO UPDATE SET access_token = $2, updated_at = now()",
    )
    .bind(project_id)
    .bind(&access_token)
    .execute(&agent.pool)
    .await
    .map_err(|e| {
        tracing::error!("[Agent] DB insert github_connections: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let frontend_url = get_setting(&agent.pool, "FRONTEND_URL")
        .await
        .unwrap_or_else(|| "https://starflaskdigital.com".into());

    let redirect = format!(
        "{}/dashboard/projects/{}?tab=overview&github=connected",
        frontend_url.trim_end_matches('/'),
        project_id
    );
    Ok(Redirect::temporary(&redirect).into_response())
}

async fn get_connection<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let conn = sqlx::query_as::<_, GithubConnection>(
        "SELECT * FROM github_connections WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&agent.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match conn {
        Some(c) => Ok(Json(json!({
            "connected": true,
            "owner": c.owner,
            "repo": c.repo,
            "linked": !c.owner.is_empty() && !c.repo.is_empty(),
        }))),
        None => Ok(Json(json!({ "connected": false }))),
    }
}

async fn disconnect_github<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    sqlx::query("DELETE FROM github_connections WHERE project_id = $1")
        .bind(project_id)
        .execute(&agent.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "success": true })))
}

#[derive(Serialize, Deserialize)]
struct GithubRepo {
    full_name: String,
    name: String,
    owner: GithubOwner,
    default_branch: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct GithubOwner {
    login: String,
}

async fn list_repos<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let conn = sqlx::query_as::<_, GithubConnection>(
        "SELECT * FROM github_connections WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&agent.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let resp = agent
        .http_client
        .get("https://api.github.com/user/repos?per_page=100&sort=updated")
        .header("Authorization", format!("Bearer {}", conn.access_token))
        .header("User-Agent", "starflask-agent")
        .send()
        .await
        .map_err(|e| {
            tracing::error!("[Agent] GitHub list repos: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let repos: Vec<GithubRepo> = resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;

    let list: Vec<Value> = repos
        .iter()
        .map(|r| {
            json!({
                "full_name": r.full_name,
                "name": r.name,
                "owner": r.owner.login,
                "default_branch": r.default_branch,
            })
        })
        .collect();

    Ok(Json(json!({ "repos": list })))
}

#[derive(Deserialize)]
struct LinkRepoPayload {
    owner: String,
    repo: String,
}

async fn link_repo<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
    Json(payload): Json<LinkRepoPayload>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    // Validate owner/repo names
    if !is_valid_repo_name(&payload.owner) || !is_valid_repo_name(&payload.repo) {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Fetch default branch from GitHub
    let conn = sqlx::query_as::<_, GithubConnection>(
        "SELECT * FROM github_connections WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&agent.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let repo_resp = agent
        .http_client
        .get(format!(
            "https://api.github.com/repos/{}/{}",
            payload.owner, payload.repo
        ))
        .header("Authorization", format!("Bearer {}", conn.access_token))
        .header("User-Agent", "starflask-agent")
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let repo_json: Value = repo_resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let default_branch = repo_json["default_branch"]
        .as_str()
        .unwrap_or("main")
        .to_string();

    sqlx::query(
        "UPDATE github_connections SET owner = $1, repo = $2, default_branch = $3, updated_at = now() WHERE project_id = $4",
    )
    .bind(&payload.owner)
    .bind(&payload.repo)
    .bind(&default_branch)
    .bind(project_id)
    .execute(&agent.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "success": true, "default_branch": default_branch })))
}

async fn list_summaries<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let summaries = sqlx::query_as::<_, CommitSummary>(
        "SELECT * FROM commit_summaries WHERE project_id = $1 ORDER BY committed_at DESC NULLS LAST LIMIT 50",
    )
    .bind(project_id)
    .fetch_all(&agent.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "summaries": summaries })))
}

async fn trigger_run<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let pool = agent.pool.clone();
    let http = agent.http_client.clone();

    // Run in background so the request returns immediately
    tokio::spawn(async move {
        let _ = crate::agent_loop::run_for_project(&pool, &http, project_id).await;
    });

    Ok(Json(json!({ "success": true, "message": "Agent run triggered" })))
}

async fn list_logs<S, U>(
    user: U,
    Path(project_id): Path<Uuid>,
    Extension(agent): Extension<AgentState>,
) -> Result<Json<Value>, StatusCode>
where
    U: AgentUser,
{
    if !user.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let logs = sqlx::query_as::<_, crate::models::AgentLog>(
        "SELECT * FROM agent_logs WHERE project_id = $1 ORDER BY created_at DESC LIMIT 50",
    )
    .bind(project_id)
    .fetch_all(&agent.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "logs": logs })))
}

// --- Helpers ---

#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: String,
}

fn urlencoded(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const ENCODE_SET: &AsciiSet = &CONTROLS
        .add(b' ').add(b':').add(b'/').add(b'?')
        .add(b'#').add(b'[').add(b']').add(b'@')
        .add(b'!').add(b'$').add(b'&').add(b'\'')
        .add(b'(').add(b')').add(b'*').add(b'+')
        .add(b',').add(b';').add(b'=').add(b'%');
    utf8_percent_encode(s, ENCODE_SET).to_string()
}

fn is_valid_repo_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}
