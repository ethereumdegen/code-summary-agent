pub mod agent_loop;
pub mod migrations;
pub mod models;
pub mod routes;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::response::Response;
use sqlx::PgPool;

/// Trait the host app implements to provide user identity to agent routes.
pub trait AgentUser: Send + Sync + 'static {
    fn user_id(&self) -> &str;
    fn is_admin(&self) -> bool;
}

pub struct CodeSummaryAgentConfig {
    pub pool: PgPool,
    pub http_client: reqwest::Client,
}

pub struct CodeSummaryAgent {
    pool: PgPool,
    http_client: reqwest::Client,
}

impl CodeSummaryAgent {
    pub async fn new(config: CodeSummaryAgentConfig) -> Self {
        Self {
            pool: config.pool,
            http_client: config.http_client,
        }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
        migrations::run_migrations(&self.pool).await
    }

    /// Returns an Axum router with all agent routes.
    /// S = host AppState, U = user extractor implementing AgentUser.
    pub fn router<S, U>(&self) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
        U: AgentUser + FromRequestParts<S> + Send + 'static,
        <U as FromRequestParts<S>>::Rejection: Into<Response>,
    {
        let state = routes::AgentState {
            pool: self.pool.clone(),
            http_client: self.http_client.clone(),
        };

        routes::router::<S, U>().layer(axum::Extension(state))
    }

    /// Starts the background agent loop (call via tokio::spawn).
    pub async fn start_agent_loop(pool: PgPool, http_client: reqwest::Client) {
        agent_loop::start_agent_loop(pool, http_client).await;
    }
}
