use clickhouse::Client;
use eyre::{Context, ContextCompat, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, clickhouse::Row)]
struct AppliedMigration {
    version: String,
}

pub async fn run_migrations(client: &Client, dir: impl AsRef<Path>) -> Result<()> {
    ensure_base_objects(client).await?;

    let applied = load_applied_versions(client).await?;
    let mut files = collect_sql_files(dir.as_ref())?;
    files.sort();

    for path in files {
        let version = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("invalid migration file name")?
            .to_string();

        if applied.contains(&version) {
            continue;
        }

        let sql = fs::read_to_string(&path)
            .with_context(|| format!("failed to read migration {}", path.display()))?;

        apply_sql_batch(client, &sql)
            .await
            .with_context(|| format!("failed migration {}", version))?;

        record_applied(client, &version)
            .await
            .with_context(|| format!("failed to record migration {}", version))?;
    }

    Ok(())
}

async fn ensure_base_objects(client: &Client) -> Result<()> {
    client
        .query("CREATE DATABASE IF NOT EXISTS logzz")
        .execute()
        .await?;

    client
        .query(
            r#"
            CREATE TABLE IF NOT EXISTS logzz.schema_migrations
            (
                version String,
                applied_at DateTime DEFAULT now()
            )
            ENGINE = MergeTree
            ORDER BY version
            "#,
        )
        .execute()
        .await?;

    Ok(())
}

async fn load_applied_versions(client: &Client) -> Result<std::collections::HashSet<String>> {
    let rows = client
        .query("SELECT version FROM logzz.schema_migrations")
        .fetch_all::<AppliedMigration>()
        .await?;

    Ok(rows.into_iter().map(|r| r.version).collect())
}

fn collect_sql_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("sql") {
            files.push(path);
        }
    }

    Ok(files)
}

async fn apply_sql_batch(client: &Client, sql: &str) -> Result<()> {
    for stmt in split_sql_statements(sql) {
        if stmt.trim().is_empty() {
            continue;
        }

        client.query(&stmt).execute().await?;
    }

    Ok(())
}

async fn record_applied(client: &Client, version: &str) -> Result<()> {
    #[derive(clickhouse::Row, Serialize)]
    struct MigrationRow<'a> {
        version: &'a str,
    }

    let mut insert = client
        .insert::<MigrationRow<'_>>("schema_migrations")
        .await?;
    insert.write(&MigrationRow { version }).await?;
    insert.end().await?;

    Ok(())
}

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;

    for ch in sql.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                current.push(ch);
            }
            ';' if !in_single && !in_double => {
                if !current.trim().is_empty() {
                    out.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }

    out
}
