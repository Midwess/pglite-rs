use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;

        db.exec(
            "CREATE TABLE accounts (id int PRIMARY KEY, balance numeric NOT NULL);
             CREATE TABLE audit (id serial, account int, delta numeric, at timestamptz DEFAULT now());
             INSERT INTO accounts VALUES (1, 100), (2, 50)",
        )
        .await?;

        db.exec(
            "CREATE FUNCTION transfer(src int, dst int, amount numeric) RETURNS void AS $$
             BEGIN
               UPDATE accounts SET balance = balance - amount WHERE id = src;
               UPDATE accounts SET balance = balance + amount WHERE id = dst;
               IF (SELECT balance FROM accounts WHERE id = src) < 0 THEN
                 RAISE EXCEPTION 'insufficient funds on %', src;
               END IF;
             END;
             $$ LANGUAGE plpgsql",
        )
        .await?;

        db.exec(
            "CREATE FUNCTION audit_balance() RETURNS trigger AS $$
             BEGIN
               INSERT INTO audit (account, delta) VALUES (NEW.id, NEW.balance - OLD.balance);
               RETURN NEW;
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER accounts_audit AFTER UPDATE ON accounts
             FOR EACH ROW EXECUTE FUNCTION audit_balance()",
        )
        .await?;

        db.exec("SELECT transfer(1, 2, 30)").await?;
        let rows = db
            .query("SELECT balance::int FROM accounts ORDER BY id", &[])
            .await?;
        assert_eq!(rows[0].get::<i32>(0)?, 70);
        assert_eq!(rows[1].get::<i32>(0)?, 80);
        println!("PASS plpgsql function with control flow");

        let rows = db.query("SELECT count(*) FROM audit", &[]).await?;
        assert_eq!(rows[0].get::<i64>(0)?, 2);
        println!("PASS row-level trigger wrote audit entries");

        let err = db.exec("SELECT transfer(1, 2, 1000)").await.unwrap_err();
        match err {
            pglite::Error::Database {
                sqlstate, message, ..
            } => {
                assert_eq!(sqlstate, "P0001");
                assert!(message.contains("insufficient funds"));
            }
            other => panic!("expected raised exception, got {other:?}"),
        }
        let rows = db
            .query("SELECT balance::int FROM accounts WHERE id = 1", &[])
            .await?;
        assert_eq!(rows[0].get::<i32>(0)?, 70);
        println!("PASS RAISE EXCEPTION rolled back the failed transfer");

        db.close().await?;
        println!("plpgsql: all checks passed");
        Ok(())
    })
}
