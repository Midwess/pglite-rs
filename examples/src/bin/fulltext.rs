use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = pglite::PGlite::open_temp().await?;
    let gateway = db.serve_unix_socket().await?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(gateway.uri())
        .await?;

    sqlx::raw_sql(
        "CREATE TABLE docs (id serial PRIMARY KEY, body text,
                            tsv tsvector GENERATED ALWAYS AS (to_tsvector('english', body)) STORED);
         CREATE INDEX docs_tsv ON docs USING GIN (tsv);
         INSERT INTO docs (body) VALUES
           ('PostgreSQL is a powerful, open source object-relational database'),
           ('Rust is a language empowering everyone to build reliable software'),
           ('Embedding databases in applications simplifies deployment')",
    )
    .execute(&pool)
    .await?;

    let rows = sqlx::query("SELECT id FROM docs WHERE tsv @@ plainto_tsquery('english', $1)")
        .bind("databases")
        .fetch_all(&pool)
        .await?;
    assert_eq!(rows.len(), 2);
    println!("PASS stemmed full-text match (database/databases via snowball)");

    let rows = sqlx::query(
        "SELECT id, ts_rank(tsv, query)::float8 AS rank
         FROM docs, plainto_tsquery('english', 'reliable software') query
         WHERE tsv @@ query ORDER BY rank DESC",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(rows[0].try_get::<i32, _>(0)?, 2);
    println!("PASS ts_rank relevance ordering");

    let row = sqlx::query(
        "SELECT ts_headline('english', body, plainto_tsquery('english', 'rust'), 'StartSel=<b>, StopSel=</b>')
         FROM docs WHERE id = 2",
    )
    .fetch_one(&pool)
    .await?;
    assert!(row.try_get::<String, _>(0)?.contains("<b>Rust</b>"));
    println!("PASS ts_headline highlighting");

    pool.close().await;
    db.close().await?;
    println!("fulltext: all checks passed via SQLx");
    Ok(())
}
