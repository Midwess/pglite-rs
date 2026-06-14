use std::io::{self, Write};

use tokio_postgres::{Client, Config, NoTls};

use crate::di::UpstreamConfig;
use crate::error::CacheError;

pub async fn prepare(upstream: &UpstreamConfig) -> Result<(), CacheError> {
    let publication = &upstream.publication;
    if !is_identifier(publication) {
        return Err(CacheError::Config(format!(
            "invalid publication name `{publication}` (use letters, digits, underscore)"
        )));
    }

    println!("cache-server init");
    println!(
        "Target: postgres {}@{}:{}/{}",
        upstream.user, upstream.host, upstream.port, upstream.database
    );
    println!("Planned changes to your Postgres:");
    println!("  1. if needed, ALTER SYSTEM SET wal_level=logical (+ max_wal_senders/max_replication_slots >= 10) — needs a Postgres restart to take effect");
    println!(
        "  2. CREATE PUBLICATION \"{publication}\" FOR ALL TABLES (only if it does not exist)"
    );
    println!("The replication slot is created automatically when the server starts.");
    print!("Proceed? [y/N] ");
    io::stdout().flush().ok();

    if !confirm() {
        println!("aborted; no changes made.");
        return Ok(());
    }

    let client = connect(upstream).await?;
    apply(&client, publication).await
}

pub async fn preflight(upstream: &UpstreamConfig) -> Result<(), CacheError> {
    let client = connect(upstream).await.map_err(|e| {
        CacheError::Config(format!(
            "cannot reach upstream Postgres at {}:{} ({e}); check --pg-host/--pg-port/--pg-user/--pg-password",
            upstream.host, upstream.port
        ))
    })?;

    let wal_level = setting(&client, "wal_level").await?;
    if wal_level != "logical" {
        return Err(CacheError::Config(format!(
            "upstream wal_level is '{wal_level}', must be 'logical'. Run `cache-server init` then RESTART Postgres for it to take effect."
        )));
    }

    for param in ["max_wal_senders", "max_replication_slots"] {
        let value: i64 = setting(&client, param).await?.parse().unwrap_or(0);
        if value < 1 {
            return Err(CacheError::Config(format!(
                "upstream {param} is {value}, must be >= 1. Run `cache-server init` then RESTART Postgres."
            )));
        }
    }

    let exists: bool = client
        .query_one(
            "select exists(select 1 from pg_publication where pubname = $1)",
            &[&upstream.publication],
        )
        .await?
        .get(0);
    if !exists {
        return Err(CacheError::Config(format!(
            "upstream publication '{}' does not exist. Run `cache-server init` to create it.",
            upstream.publication
        )));
    }

    Ok(())
}

async fn connect(upstream: &UpstreamConfig) -> Result<Client, CacheError> {
    let (client, connection) = Config::new()
        .host(&upstream.host)
        .port(upstream.port)
        .user(&upstream.user)
        .password(&upstream.password)
        .dbname(&upstream.database)
        .connect(NoTls)
        .await?;
    tokio::spawn(connection);
    Ok(client)
}

async fn apply(client: &Client, publication: &str) -> Result<(), CacheError> {
    let mut needs_restart = false;

    let wal_level = setting(client, "wal_level").await?;
    if wal_level == "logical" {
        println!("  ✓ wal_level = logical");
    } else if alter_system(client, "wal_level", "logical").await {
        println!("  ✓ set wal_level = logical (was '{wal_level}')");
        needs_restart = true;
    }

    for param in ["max_wal_senders", "max_replication_slots"] {
        let current: i64 = setting(client, param).await?.parse().unwrap_or(0);
        if current < 10 && alter_system(client, param, "10").await {
            println!("  ✓ set {param} = 10 (was {current})");
            needs_restart = true;
        }
    }

    let exists: bool = client
        .query_one(
            "select exists(select 1 from pg_publication where pubname = $1)",
            &[&publication],
        )
        .await?
        .get(0);
    if exists {
        println!("  ✓ publication \"{publication}\" already exists");
    } else {
        client
            .batch_execute(&format!(
                "CREATE PUBLICATION \"{publication}\" FOR ALL TABLES"
            ))
            .await?;
        println!("  ✓ created publication \"{publication}\" FOR ALL TABLES");
    }

    if needs_restart {
        println!();
        println!(
            "⚠ restart Postgres for the changed settings to take effect (they are restart-only):"
        );
        println!("    docker compose restart <postgres-service>   # or: pg_ctl restart");
    }
    println!("init complete — after any restart, start the server with `cache-server serve`.");
    Ok(())
}

async fn setting(client: &Client, name: &str) -> Result<String, CacheError> {
    Ok(client
        .query_one("select current_setting($1)", &[&name])
        .await?
        .get(0))
}

async fn alter_system(client: &Client, param: &str, value: &str) -> bool {
    match client
        .batch_execute(&format!("ALTER SYSTEM SET {param} = '{value}'"))
        .await
    {
        Ok(_) => true,
        Err(error) => {
            println!("  ⚠ could not set {param} = {value} ({error}); set it manually (needs superuser): ALTER SYSTEM SET {param} = '{value}'; then restart");
            false
        }
    }
}

fn confirm() -> bool {
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
