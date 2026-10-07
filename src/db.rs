//! Postgres pool and migrations. Postgres is the source of truth for the whole
//! shop; see AGENTS.md.

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::config::DatabaseConfig;

/// Opens the connection pool. Fails fast if the database is unreachable at startup.
pub async fn connect(cfg: &DatabaseConfig) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&cfg.url)
        .await
}

/// Applies pending migrations, which are embedded in the binary at compile time.
/// sqlx holds a Postgres advisory lock while migrating, so many instances can
/// start at once without racing each other.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}
