use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;

        db.exec(
            "CREATE TABLE docs (id serial PRIMARY KEY, body text,
                                tsv tsvector GENERATED ALWAYS AS (to_tsvector('english', body)) STORED);
             CREATE INDEX docs_tsv ON docs USING GIN (tsv);
             INSERT INTO docs (body) VALUES
               ('PostgreSQL is a powerful, open source object-relational database'),
               ('Rust is a language empowering everyone to build reliable software'),
               ('Embedding databases in applications simplifies deployment')",
        )
        .await?;

        let rows = db
            .query(
                "SELECT id FROM docs WHERE tsv @@ plainto_tsquery('english', $1)",
                &[&"databases"],
            )
            .await?;
        assert_eq!(rows.len(), 2);
        println!("PASS stemmed full-text match (database/databases via snowball)");

        let rows = db
            .query(
                "SELECT id, ts_rank(tsv, query)::float8 AS rank
                 FROM docs, plainto_tsquery('english', 'reliable software') query
                 WHERE tsv @@ query ORDER BY rank DESC",
                &[],
            )
            .await?;
        assert_eq!(rows[0].get::<i32>(0)?, 2);
        println!("PASS ts_rank relevance ordering");

        let rows = db
            .query(
                "SELECT ts_headline('english', body, plainto_tsquery('english', 'rust'), 'StartSel=<b>, StopSel=</b>')
                 FROM docs WHERE id = 2",
                &[],
            )
            .await?;
        assert!(rows[0].get::<&str>(0)?.contains("<b>Rust</b>"));
        println!("PASS ts_headline highlighting");

        db.close().await?;
        println!("fulltext: all checks passed");
        Ok(())
    })
}
