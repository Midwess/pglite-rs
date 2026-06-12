use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = pglite::PGlite::open_temp().await?;
    let uri = db.unix_uri().await?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&uri)
        .await?;

    sqlx::raw_sql(
        "CREATE TABLE events (id serial PRIMARY KEY, payload jsonb);
         CREATE INDEX events_payload_gin ON events USING GIN (payload);
         INSERT INTO events (payload) VALUES
           ('{\"kind\":\"click\",\"user\":{\"id\":1,\"name\":\"alice\"},\"tags\":[\"a\",\"b\"]}'),
           ('{\"kind\":\"view\",\"user\":{\"id\":2,\"name\":\"bob\"},\"tags\":[\"b\"]}'),
           ('{\"kind\":\"click\",\"user\":{\"id\":1,\"name\":\"alice\"},\"tags\":[\"c\"]}')",
    )
    .execute(&pool)
    .await?;

    let rows = sqlx::query(
        "SELECT payload->'user'->>'name' FROM events WHERE payload->>'kind' = $1 ORDER BY id",
    )
    .bind("click")
    .fetch_all(&pool)
    .await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].try_get::<&str, _>(0)?, "alice");
    println!("PASS jsonb path extraction + filtering");

    let rows = sqlx::query(
        "SELECT id FROM events WHERE payload @> '{\"tags\":[\"b\"]}'::jsonb ORDER BY id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(rows.len(), 2);
    println!("PASS jsonb containment via GIN-indexable @>");

    let row = sqlx::query(
        "SELECT jsonb_agg(DISTINCT payload->>'kind' ORDER BY payload->>'kind')::text FROM events",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(row.try_get::<&str, _>(0)?, "[\"click\", \"view\"]");
    println!("PASS jsonb aggregation");

    sqlx::query(
        "UPDATE events SET payload = jsonb_set(payload, '{user,name}', '\"carol\"') WHERE id = 2",
    )
    .execute(&pool)
    .await?;
    let row = sqlx::query("SELECT payload->'user'->>'name' FROM events WHERE id = 2")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<&str, _>(0)?, "carol");
    println!("PASS jsonb_set in-place update");

    pool.close().await;
    db.close().await?;
    println!("jsonb: all checks passed via SQLx");
    Ok(())
}
