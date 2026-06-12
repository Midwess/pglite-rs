use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;

        db.exec(
            "CREATE TABLE events (id serial PRIMARY KEY, payload jsonb);
             CREATE INDEX events_payload_gin ON events USING GIN (payload);
             INSERT INTO events (payload) VALUES
               ('{\"kind\":\"click\",\"user\":{\"id\":1,\"name\":\"alice\"},\"tags\":[\"a\",\"b\"]}'),
               ('{\"kind\":\"view\",\"user\":{\"id\":2,\"name\":\"bob\"},\"tags\":[\"b\"]}'),
               ('{\"kind\":\"click\",\"user\":{\"id\":1,\"name\":\"alice\"},\"tags\":[\"c\"]}')",
        )
        .await?;

        let rows = db
            .query("SELECT payload->'user'->>'name' FROM events WHERE payload->>'kind' = $1 ORDER BY id", &[&"click"])
            .await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<&str>(0)?, "alice");
        println!("PASS jsonb path extraction + filtering");

        let rows = db
            .query(
                "SELECT count(*) FROM events WHERE payload @> '{\"tags\":[\"b\"]}'",
                &[],
            )
            .await?;
        assert_eq!(rows[0].get::<i64>(0)?, 2);
        println!("PASS jsonb containment via GIN-indexed @>");

        let rows = db
            .query(
                "SELECT jsonb_agg(DISTINCT payload->>'kind' ORDER BY payload->>'kind')::text FROM events",
                &[],
            )
            .await?;
        assert_eq!(rows[0].get::<&str>(0)?, "[\"click\", \"view\"]");
        println!("PASS jsonb_agg aggregation");

        let rows = db
            .query(
                "SELECT key, count(*) FROM events, jsonb_each(payload) GROUP BY key ORDER BY key",
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 3);
        println!("PASS jsonb_each lateral expansion");

        db.close().await?;
        println!("jsonb: all checks passed");
        Ok(())
    })
}
