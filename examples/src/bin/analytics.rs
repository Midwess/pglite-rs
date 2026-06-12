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
        "CREATE TABLE sales (region text, month int, amount numeric);
         INSERT INTO sales VALUES
           ('north', 1, 100), ('north', 2, 150), ('north', 3, 120),
           ('south', 1, 80),  ('south', 2, 90),  ('south', 3, 200)",
    )
    .execute(&pool)
    .await?;

    let rows = sqlx::query(
        "SELECT region, month,
                sum(amount) OVER (PARTITION BY region ORDER BY month)::int AS running,
                rank() OVER (ORDER BY amount DESC)::int AS overall_rank
         FROM sales ORDER BY region, month",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[2].try_get::<i32, _>(2)?, 370);
    println!("PASS window functions (running sum, rank)");

    let row = sqlx::query(
        "WITH RECURSIVE fib(n, a, b) AS (
           SELECT 1, 0::bigint, 1::bigint
           UNION ALL
           SELECT n + 1, b, a + b FROM fib WHERE n < 10
         ) SELECT a FROM fib ORDER BY n DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(row.try_get::<i64, _>(0)?, 34);
    println!("PASS recursive CTE");

    let rows = sqlx::query(
        "SELECT s.region, top.amount::int
         FROM (SELECT DISTINCT region FROM sales) s,
         LATERAL (SELECT amount FROM sales WHERE region = s.region ORDER BY amount DESC LIMIT 1) top
         ORDER BY s.region",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(rows[0].try_get::<i32, _>(1)?, 150);
    assert_eq!(rows[1].try_get::<i32, _>(1)?, 200);
    println!("PASS LATERAL subquery");

    let rows = sqlx::query(
        "SELECT region, sum(amount)::int FROM sales GROUP BY ROLLUP(region) ORDER BY region NULLS LAST",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].try_get::<i32, _>(1)?, 740);
    println!("PASS GROUPING SETS / ROLLUP");

    pool.close().await;
    db.close().await?;
    println!("analytics: all checks passed via SQLx");
    Ok(())
}
