use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;

        db.exec("CREATE TABLE users (id serial PRIMARY KEY, name text NOT NULL)")
            .await?;
        db.exec("INSERT INTO users (name) VALUES ('alice'), ('bob')")
            .await?;

        let rows = db
            .query(
                "SELECT id, name FROM users WHERE id > $1 ORDER BY id",
                &[&0i32],
            )
            .await?;
        for row in &rows {
            let id: i32 = row.get(0)?;
            let name: &str = row.get(1)?;
            println!("{id}: {name}");
        }

        let tx = db.transaction().await?;
        tx.exec("INSERT INTO users (name) VALUES ('carol')").await?;
        tx.rollback().await?;

        let rows = db.query("SELECT count(*) FROM users", &[]).await?;
        let count: i64 = rows[0].get(0)?;
        println!("count after rollback: {count}");

        db.close().await?;
        Ok(())
    })
}
