use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;

        db.exec(
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
        .await?;

        let rows = db
            .query("SELECT name FROM people WHERE feeling = 'happy'", &[])
            .await?;
        assert_eq!(rows[0].get::<&str>(0)?, "alice");
        println!("PASS enum type");

        let rows = db
            .query("SELECT name FROM people WHERE $1 = ANY(scores)", &[&95i32])
            .await?;
        assert_eq!(rows[0].get::<&str>(0)?, "alice");
        println!("PASS integer arrays + ANY");

        let rows = db
            .query(
                "SELECT unnest(scores)::int FROM people WHERE name = 'bob' ORDER BY 1",
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<i32>(0)?, 60);
        println!("PASS unnest");

        let err = db
            .exec("INSERT INTO people (name, age) VALUES ('carl', -5)")
            .await
            .unwrap_err();
        match err {
            pglite::Error::Database { sqlstate, .. } => assert_eq!(sqlstate, "23514"),
            other => panic!("expected check violation, got {other:?}"),
        }
        println!("PASS domain CHECK constraint rejects invalid value");

        let rows = db
            .query(
                "SELECT name FROM people WHERE span && '[8,9)'::int4range ORDER BY name",
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 2);
        println!("PASS range type overlap");

        let rows = db
            .query("SELECT count(DISTINCT id) FROM people", &[])
            .await?;
        assert_eq!(rows[0].get::<i64>(0)?, 2);
        println!("PASS uuid generation (gen_random_uuid)");

        db.close().await?;
        println!("rich_types: all checks passed");
        Ok(())
    })
}
