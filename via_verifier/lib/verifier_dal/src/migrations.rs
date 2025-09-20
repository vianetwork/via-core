use anyhow::Context;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;

/// Runs migrations for the verifier database.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migrator = Migrator::new(path)
        .await
        .context("Failed to create migrator")?;
    migrator
        .run(pool)
        .await
        .context("Failed to run migrations")?;
    Ok(())
} 
