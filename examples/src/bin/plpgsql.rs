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
        "CREATE TABLE accounts (id int PRIMARY KEY, balance numeric NOT NULL);
         CREATE TABLE audit (id serial, account int, delta numeric, at timestamptz DEFAULT now());
         INSERT INTO accounts VALUES (1, 100), (2, 50)",
    )
    .execute(&pool)
    .await?;

    sqlx::raw_sql(
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
    .execute(&pool)
    .await?;

    sqlx::raw_sql(
        "CREATE FUNCTION audit_balance() RETURNS trigger AS $$
         BEGIN
           INSERT INTO audit (account, delta) VALUES (NEW.id, NEW.balance - OLD.balance);
           RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER accounts_audit AFTER UPDATE ON accounts
         FOR EACH ROW EXECUTE FUNCTION audit_balance()",
    )
    .execute(&pool)
    .await?;

    sqlx::query("SELECT transfer(1, 2, 30)")
        .execute(&pool)
        .await?;
    let rows = sqlx::query("SELECT balance::int FROM accounts ORDER BY id")
        .fetch_all(&pool)
        .await?;
    assert_eq!(rows[0].try_get::<i32, _>(0)?, 70);
    assert_eq!(rows[1].try_get::<i32, _>(0)?, 80);
    println!("PASS plpgsql function with control flow");

    let row = sqlx::query("SELECT count(*) FROM audit")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<i64, _>(0)?, 2);
    println!("PASS row-level trigger wrote audit entries");

    let err = sqlx::query("SELECT transfer(1, 2, 1000)")
        .execute(&pool)
        .await
        .unwrap_err();
    match err {
        sqlx::Error::Database(db_err) => {
            assert_eq!(db_err.code().as_deref(), Some("P0001"));
            assert!(db_err.message().contains("insufficient funds"));
        }
        other => panic!("expected raised exception, got {other:?}"),
    }
    let row = sqlx::query("SELECT balance::int FROM accounts WHERE id = 1")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<i32, _>(0)?, 70);
    println!("PASS RAISE EXCEPTION rolled back the failed transfer");

    pool.close().await;
    db.close().await?;
    println!("plpgsql: all checks passed via SQLx");
    Ok(())
}
