use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;

        db.exec(
            "CREATE TABLE measurements (
               city text, at date, temp int,
               PRIMARY KEY (city, at)
             ) PARTITION BY RANGE (at);
             CREATE TABLE m_2025 PARTITION OF measurements FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
             CREATE TABLE m_2026 PARTITION OF measurements FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
             INSERT INTO measurements VALUES ('oslo', '2025-06-01', 18), ('oslo', '2026-06-01', 21)",
        )
        .await?;
        let rows = db.query("SELECT count(*) FROM m_2026", &[]).await?;
        assert_eq!(rows[0].get::<i64>(0)?, 1);
        println!("PASS declarative range partitioning routes rows");

        db.exec(
            "CREATE TABLE inventory (
               sku text PRIMARY KEY,
               qty int NOT NULL DEFAULT 0,
               price numeric NOT NULL,
               value numeric GENERATED ALWAYS AS (qty * price) STORED
             );
             INSERT INTO inventory (sku, qty, price) VALUES ('apple', 10, 2.5)",
        )
        .await?;
        db.exec(
            "INSERT INTO inventory (sku, qty, price) VALUES ('apple', 5, 2.5)
             ON CONFLICT (sku) DO UPDATE SET qty = inventory.qty + EXCLUDED.qty",
        )
        .await?;
        let rows = db
            .query("SELECT qty, value::float8 FROM inventory", &[])
            .await?;
        assert_eq!(rows[0].get::<i32>(0)?, 15);
        assert_eq!(rows[0].get::<f64>(1)?, 37.5);
        println!("PASS upsert (ON CONFLICT) + generated column");

        db.exec(
            "CREATE MATERIALIZED VIEW expensive AS
               SELECT sku FROM inventory WHERE price > 1;
             REFRESH MATERIALIZED VIEW expensive",
        )
        .await?;
        let rows = db.query("SELECT count(*) FROM expensive", &[]).await?;
        assert_eq!(rows[0].get::<i64>(0)?, 1);
        println!("PASS materialized view + refresh");

        let rows = db
            .query(
                "SELECT relname::text FROM pg_class WHERE relname = 'inventory_pkey'",
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 1);
        println!("PASS system catalogs queryable");

        db.close().await?;
        println!("ddl_power: all checks passed");
        Ok(())
    })
}
