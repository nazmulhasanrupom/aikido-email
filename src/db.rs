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
