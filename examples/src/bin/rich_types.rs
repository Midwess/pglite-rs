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
        "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
         CREATE DOMAIN positive_int AS int CHECK (VALUE > 0);
         CREATE TABLE people (
           id uuid DEFAULT gen_random_uuid() PRIMARY KEY,
           name text NOT NULL,
           feeling mood DEFAULT 'ok',
           scores int[],
           age positive_int,
           span int4range
         );
         INSERT INTO people (name, feeling, scores, age, span) VALUES
           ('alice', 'happy', '{90,85,95}', 30, '[1,10)'),
           ('bob', 'sad', '{60,70}', 40, '[5,15)')",
    )
    .execute(&pool)
    .await?;

    let rows = sqlx::query("SELECT name FROM people WHERE feeling = 'happy'")
        .fetch_all(&pool)
        .await?;
    assert_eq!(rows[0].try_get::<&str, _>(0)?, "alice");
    println!("PASS enum type");

    let rows = sqlx::query("SELECT name FROM people WHERE $1 = ANY(scores)")
        .bind(95i32)
        .fetch_all(&pool)
        .await?;
    assert_eq!(rows[0].try_get::<&str, _>(0)?, "alice");
    println!("PASS integer arrays + ANY");

    let rows = sqlx::query("SELECT unnest(scores)::int FROM people WHERE name = 'bob' ORDER BY 1")
        .fetch_all(&pool)
        .await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].try_get::<i32, _>(0)?, 60);
    println!("PASS unnest");

    let err = sqlx::query("INSERT INTO people (name, age) VALUES ('carl', -5)")
        .execute(&pool)
        .await
        .unwrap_err();
    match err {
        sqlx::Error::Database(db_err) => assert_eq!(db_err.code().as_deref(), Some("23514")),
        other => panic!("expected check violation, got {other:?}"),
    }
    println!("PASS domain CHECK constraint rejects invalid value");

    let rows =
        sqlx::query("SELECT name FROM people WHERE span && '[8,9)'::int4range ORDER BY name")
            .fetch_all(&pool)
            .await?;
    assert_eq!(rows.len(), 2);
    println!("PASS range type overlap");

    let row = sqlx::query("SELECT count(DISTINCT id) FROM people")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<i64, _>(0)?, 2);
    println!("PASS uuid generation (gen_random_uuid)");

    pool.close().await;
    db.close().await?;
    println!("rich_types: all checks passed via SQLx");
    Ok(())
}
