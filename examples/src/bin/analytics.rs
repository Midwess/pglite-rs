use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;

        db.exec(
            "CREATE TABLE sales (region text, month int, amount numeric);
             INSERT INTO sales VALUES
               ('north', 1, 100), ('north', 2, 150), ('north', 3, 120),
               ('south', 1, 80),  ('south', 2, 90),  ('south', 3, 200)",
        )
        .await?;

        let rows = db
            .query(
                "SELECT region, month,
                        sum(amount) OVER (PARTITION BY region ORDER BY month)::int AS running,
                        rank() OVER (ORDER BY amount DESC)::int AS overall_rank
                 FROM sales ORDER BY region, month",
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[2].get::<i32>(2)?, 370);
        println!("PASS window functions (running sum, rank)");

        let rows = db
            .query(
                "WITH RECURSIVE fib(n, a, b) AS (
                   SELECT 1, 0::bigint, 1::bigint
                   UNION ALL
                   SELECT n + 1, b, a + b FROM fib WHERE n < 10
                 ) SELECT a FROM fib ORDER BY n DESC LIMIT 1",
                &[],
            )
            .await?;
        assert_eq!(rows[0].get::<i64>(0)?, 34);
        println!("PASS recursive CTE");

        let rows = db
            .query(
                "SELECT s.region, top.amount::int
                 FROM (SELECT DISTINCT region FROM sales) s,
                 LATERAL (SELECT amount FROM sales WHERE region = s.region ORDER BY amount DESC LIMIT 1) top
                 ORDER BY s.region",
                &[],
            )
            .await?;
        assert_eq!(rows[0].get::<i32>(1)?, 150);
        assert_eq!(rows[1].get::<i32>(1)?, 200);
        println!("PASS LATERAL subquery");

        let rows = db
            .query(
                "SELECT region, sum(amount)::int FROM sales GROUP BY ROLLUP(region) ORDER BY region NULLS LAST",
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].get::<i32>(1)?, 740);
        println!("PASS GROUPING SETS / ROLLUP");

        db.close().await?;
        println!("analytics: all checks passed");
        Ok(())
    })
}
