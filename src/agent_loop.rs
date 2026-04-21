use serde_json::{json, Value};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

use crate::models::GithubConnection;

/// Background loop that fetches new commits and generates AI summaries.
pub async fn start_agent_loop(pool: PgPool, http_client: reqwest::Client) {
    tracing::info!("[Agent] Background loop started");

    loop {
        if let Err(e) = tick(&pool, &http_client).await {
            tracing::error!("[Agent] Loop tick error: {e}");
        }

        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// Run the agent for a single project (triggered manually).
pub async fn run_for_project(
    pool: &PgPool,
    http: &reqwest::Client,
    project_id: Uuid,
) -> Result<(), String> {
    let openai_key = get_setting(pool, "OPENAI_API_KEY").await;
    if openai_key.is_none() {
        log(pool, project_id, "warn", "OPENAI_API_KEY not configured — commits will be fetched but not summarized").await;
    }

    let conn = sqlx::query_as::<_, GithubConnection>(
        "SELECT * FROM github_connections WHERE project_id = $1 AND owner != '' AND repo != ''",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("DB error: {e}"))?
    .ok_or_else(|| "No linked repository".to_string())?;

    log(
        pool,
        project_id,
        "info",
        &format!("Running agent for {}/{}", conn.owner, conn.repo),
    )
    .await;

    let result = process_connection(pool, http, &conn, openai_key.as_deref()).await;
    match result {
        Ok(count) => {
            log(
                pool,
                project_id,
                "info",
                &format!("Done — {count} new commit(s) processed"),
            )
            .await;
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            log(pool, project_id, "error", &format!("Agent error: {msg}")).await;
            Err(msg)
        }
    }
}

async fn tick(pool: &PgPool, http: &reqwest::Client) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let openai_key = get_setting(pool, "OPENAI_API_KEY").await;
    if openai_key.is_none() {
        tracing::debug!("[Agent] OPENAI_API_KEY not configured, skipping");
        return Ok(());
    }

    let connections = sqlx::query_as::<_, GithubConnection>(
        "SELECT * FROM github_connections WHERE owner != '' AND repo != ''",
    )
    .fetch_all(pool)
    .await?;

    for conn in connections {
        if let Err(e) = process_connection(pool, http, &conn, openai_key.as_deref()).await {
            tracing::warn!(
                "[Agent] Error processing {}/{}: {e}",
                conn.owner,
                conn.repo
            );
            log(pool, conn.project_id, "error", &format!("Agent error: {e}")).await;
        }
    }

    Ok(())
}

/// Process a single connection. Returns the number of new commits processed.
async fn process_connection(
    pool: &PgPool,
    http: &reqwest::Client,
    conn: &GithubConnection,
    openai_key: Option<&str>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    // Fetch commits from the last 48 hours
    let since = (chrono::Utc::now() - chrono::Duration::hours(48))
        .to_rfc3339();
    let url = format!(
        "https://api.github.com/repos/{}/{}/commits?sha={}&since={}&per_page=100",
        conn.owner, conn.repo, conn.default_branch, since
    );

    let resp = http
        .get(&url)
        .header("Authorization", format!("Bearer {}", conn.access_token))
        .header("User-Agent", "starflask-agent")
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let reason = if status == reqwest::StatusCode::UNAUTHORIZED {
            "token may be revoked"
        } else if status == reqwest::StatusCode::FORBIDDEN {
            "rate limited or forbidden"
        } else {
            "unexpected error"
        };
        return Err(format!("GitHub {status}: {reason}").into());
    }

    let commits: Vec<Value> = resp.json().await?;
    let mut new_count = 0usize;

    for commit_data in &commits {
        let sha = match commit_data["sha"].as_str() {
            Some(s) => s,
            None => continue,
        };

        // Check if already exists
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM commit_summaries WHERE project_id = $1 AND sha = $2)",
        )
        .bind(conn.project_id)
        .bind(sha)
        .fetch_one(pool)
        .await
        .unwrap_or(true);

        if exists {
            continue;
        }

        // Fetch full commit data
        let full_url = format!(
            "https://api.github.com/repos/{}/{}/commits/{}",
            conn.owner, conn.repo, sha
        );
        let full_resp = http
            .get(&full_url)
            .header("Authorization", format!("Bearer {}", conn.access_token))
            .header("User-Agent", "starflask-agent")
            .send()
            .await?;

        if !full_resp.status().is_success() {
            tracing::warn!("[Agent] GitHub {} fetching commit {sha}", full_resp.status());
            continue;
        }

        let full_data: Value = full_resp.json().await?;

        let author = full_data["commit"]["author"]["name"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let message = full_data["commit"]["message"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let committed_at = full_data["commit"]["author"]["date"]
            .as_str()
            .and_then(|d| d.parse::<chrono::DateTime<chrono::Utc>>().ok());

        let files_changed: Option<Value> = full_data["files"].as_array().map(|files| {
            json!(files
                .iter()
                .map(|f| {
                    json!({
                        "filename": f["filename"],
                        "status": f["status"],
                        "additions": f["additions"],
                        "deletions": f["deletions"],
                    })
                })
                .collect::<Vec<_>>())
        });

        // Insert with summary = NULL
        sqlx::query(
            "INSERT INTO commit_summaries (project_id, github_connection_id, sha, author, message, committed_at, files_changed, raw_data) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (project_id, sha) DO NOTHING",
        )
        .bind(conn.project_id)
        .bind(conn.id)
        .bind(sha)
        .bind(&author)
        .bind(&message)
        .bind(committed_at)
        .bind(&files_changed)
        .bind(&full_data)
        .execute(pool)
        .await?;

        new_count += 1;

        // Generate AI summary if key is available
        if let Some(key) = openai_key {
            let summary = generate_summary(http, key, &message, &files_changed).await;
            if let Some(summary_text) = summary {
                sqlx::query(
                    "UPDATE commit_summaries SET summary = $1 WHERE project_id = $2 AND sha = $3",
                )
                .bind(&summary_text)
                .bind(conn.project_id)
                .bind(sha)
                .execute(pool)
                .await?;
            }
        }
    }

    // Update last_checked_sha
    if let Some(first) = commits.first() {
        if let Some(sha) = first["sha"].as_str() {
            sqlx::query("UPDATE github_connections SET last_checked_sha = $1, updated_at = now() WHERE id = $2")
                .bind(sha)
                .bind(conn.id)
                .execute(pool)
                .await?;
        }
    }

    Ok(new_count)
}

async fn generate_summary(
    http: &reqwest::Client,
    openai_key: &str,
    message: &str,
    files_changed: &Option<Value>,
) -> Option<String> {
    let files_desc = files_changed
        .as_ref()
        .map(|f| serde_json::to_string_pretty(f).unwrap_or_default())
        .unwrap_or_else(|| "No file data".into());

    let prompt = format!(
        "Summarize this git commit in 2-3 sentences for a project status report. \
         Focus on what changed and why it matters.\n\n\
         Commit message: {message}\n\n\
         Files changed:\n{files_desc}"
    );

    let body = json!({
        "model": "gpt-4o-mini",
        "messages": [
            { "role": "system", "content": "You are a concise code reviewer. Summarize commits for non-technical stakeholders." },
            { "role": "user", "content": prompt },
        ],
        "max_tokens": 200,
    });

    let resp = http
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {openai_key}"))
        .json(&body)
        .send()
        .await
        .ok()?;

    let json: Value = resp.json().await.ok()?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
}

async fn get_setting(pool: &PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM app_settings WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()?
}

async fn log(pool: &PgPool, project_id: Uuid, level: &str, message: &str) {
    tracing::info!("[Agent] [{level}] {message}");
    let _ = sqlx::query(
        "INSERT INTO agent_logs (project_id, level, message) VALUES ($1, $2, $3)",
    )
    .bind(project_id)
    .bind(level)
    .bind(message)
    .execute(pool)
    .await;
}
