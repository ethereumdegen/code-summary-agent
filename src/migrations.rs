use sqlx::PgPool;

pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    let sql = include_str!("../migrations/001_init.sql");
    sqlx::raw_sql(sql).execute(pool).await?;
    tracing::info!("[CodeSummaryAgent] Migration complete");
    Ok(())
}
