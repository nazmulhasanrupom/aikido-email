use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    Ok(PgPoolOptions::new().max_connections(20).acquire_timeout(std::time::Duration::from_secs(10)).connect(url).await?)
}

/// Run the migrations in `./migrations`.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// Like `connect`, but waits for a slow database. Tries every 2 seconds for up to `wait`.
pub async fn connect_retry(url: &str, wait: std::time::Duration) -> anyhow::Result<PgPool> {
    let start = std::time::Instant::now();
    loop {
        match connect(url).await {
            Ok(pool) => return Ok(pool),
            Err(e) if start.elapsed() >= wait => return Err(e.context("the database did not answer in time")),
            Err(e) => {
                tracing::warn!(error = %e, "database not ready, trying again in 2 seconds");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}
