#![cfg(feature = "replica")]

use std::time::{Duration, Instant};

use futures::executor::block_on;
use pglite::{PGlite, Replica, ReplicaConfig, RowChange};

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[test]
fn replica_end_to_end() {
    let Ok(host) = std::env::var("PGLITE_REPLICA_UPSTREAM_HOST") else {
        eprintln!("skipping replica integration test: PGLITE_REPLICA_UPSTREAM_HOST not set");
        return;
    };
    let port: u16 = env_or("PGLITE_REPLICA_UPSTREAM_PORT", "5432")
        .parse()
        .unwrap();
    let user = env_or("PGLITE_REPLICA_UPSTREAM_USER", "postgres");
    let password = env_or("PGLITE_REPLICA_UPSTREAM_PASSWORD", "postgres");
    let database = env_or("PGLITE_REPLICA_UPSTREAM_DB", "postgres");

    let mut up = postgres::Client::connect(
        &format!("host={host} port={port} user={user} password={password} dbname={database}"),
        postgres::NoTls,
    )
    .unwrap();

    up.batch_execute("DROP PUBLICATION IF EXISTS pglite_test_pub")
        .unwrap();
    let _ = up.execute(
        "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = 'pglite_test_slot'",
        &[],
    );
    up.batch_execute(
        "DROP TABLE IF EXISTS public.repl_todos;
         DROP TABLE IF EXISTS public.repl_typed;
         DROP TABLE IF EXISTS public.repl_nopk;
         DROP TABLE IF EXISTS public.repl_new;
         DROP TYPE IF EXISTS public.repl_addr CASCADE;
         DROP TYPE IF EXISTS public.repl_num CASCADE;
         DROP DOMAIN IF EXISTS public.repl_pos CASCADE;
         CREATE TABLE public.repl_todos (id int PRIMARY KEY, title text);
         INSERT INTO public.repl_todos VALUES (1, 'one'), (2, 'two');
         CREATE TYPE public.repl_addr AS (street text, zip int);
         CREATE DOMAIN public.repl_pos AS int CHECK (VALUE > 0);
         CREATE TYPE public.repl_num AS RANGE (subtype = numeric);
         CREATE TABLE public.repl_typed (id int PRIMARY KEY, home public.repl_addr, qty public.repl_pos, span public.repl_num);
         INSERT INTO public.repl_typed VALUES (1, ROW('Main St', 90210), 5, public.repl_num(1, 9));
         CREATE TABLE public.repl_nopk (a int, b text);
         ALTER TABLE public.repl_nopk REPLICA IDENTITY FULL;
         INSERT INTO public.repl_nopk VALUES (1, 'x'), (2, 'y');
         CREATE PUBLICATION pglite_test_pub FOR TABLE public.repl_todos, public.repl_typed, public.repl_nopk;",
    )
    .unwrap();

    up.batch_execute(&format!(
        "CREATE OR REPLACE FUNCTION public.pglite_emit_ddl() RETURNS event_trigger \
         LANGUAGE plpgsql AS $fn$ BEGIN PERFORM pg_logical_emit_message(true, '{}', ''); END $fn$; \
         DROP EVENT TRIGGER IF EXISTS pglite_ddl_watch; \
         CREATE EVENT TRIGGER pglite_ddl_watch ON ddl_command_end EXECUTE FUNCTION public.pglite_emit_ddl();",
        pglite::DDL_SIGNAL_PREFIX
    ))
    .unwrap();

    let db = block_on(PGlite::open_temp()).unwrap();
    let config = ReplicaConfig {
        host: host.clone(),
        port,
        user,
        password,
        database,
        publication: "pglite_test_pub".into(),
        slot_name: "pglite_test_slot".into(),
        read_timeout: Duration::from_secs(2),
        ..Default::default()
    };

    let replica = block_on(Replica::start(db.clone(), config.clone())).expect("replica start");

    let rows = block_on(db.query("SELECT id, title FROM repl_todos ORDER BY id", &[])).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get::<&str>(1).unwrap(), "one");
    assert_eq!(rows[1].get::<&str>(1).unwrap(), "two");
    let backfill_watermark = replica.watermark();
    assert!(backfill_watermark.0 > 0);

    let typed = block_on(db.query(
        "SELECT id, (home).zip, qty, lower(span)::text FROM repl_typed ORDER BY id",
        &[],
    ))
    .unwrap();
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].get::<i32>(1).unwrap(), 90210);
    assert_eq!(typed[0].get::<i32>(2).unwrap(), 5);
    assert_eq!(typed[0].get::<&str>(3).unwrap(), "1");
    let nopk = block_on(db.query("SELECT count(*) FROM repl_nopk", &[])).unwrap();
    assert_eq!(nopk[0].get::<i64>(0).unwrap(), 2);

    let events = replica.subscribe();
    up.execute("INSERT INTO public.repl_todos VALUES (3, 'three')", &[])
        .unwrap();
    up.execute(
        "UPDATE public.repl_todos SET title = 'uno' WHERE id = 1",
        &[],
    )
    .unwrap();
    up.execute("DELETE FROM public.repl_todos WHERE id = 2", &[])
        .unwrap();

    let first = events.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(matches!(first.changes[0], RowChange::Insert { .. }));
    match &first.changes[0] {
        RowChange::Insert { schema, table, row } => {
            assert_eq!(schema, "public");
            assert_eq!(table, "repl_todos");
            assert!(row.contains(&("id".to_string(), Some("3".to_string()))));
            assert!(row.contains(&("title".to_string(), Some("three".to_string()))));
        }
        other => panic!("unexpected change: {other:?}"),
    }
    let second = events.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(matches!(second.changes[0], RowChange::Update { .. }));
    let third = events.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(matches!(third.changes[0], RowChange::Delete { .. }));
    assert!(first.end_lsn < second.end_lsn && second.end_lsn < third.end_lsn);

    let rows = block_on(db.query("SELECT id, title FROM repl_todos ORDER BY id", &[])).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get::<&str>(1).unwrap(), "uno");
    assert_eq!(rows[1].get::<i32>(0).unwrap(), 3);
    assert!(replica.watermark() > backfill_watermark);

    let stop_started = Instant::now();
    replica.stop();
    assert!(wait_until(Duration::from_secs(10), || replica.is_stopped()));
    assert!(stop_started.elapsed() < Duration::from_secs(8));
    assert!(!replica.is_halted());

    up.execute("INSERT INTO public.repl_todos VALUES (4, 'four')", &[])
        .unwrap();

    let replica2 = block_on(Replica::start(db.clone(), config.clone())).unwrap();
    let events2 = replica2.subscribe();
    let resumed = events2.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(matches!(resumed.changes[0], RowChange::Insert { .. }));

    let rows = block_on(db.query("SELECT id FROM repl_todos ORDER BY id", &[])).unwrap();
    let ids: Vec<i32> = rows.iter().map(|r| r.get::<i32>(0).unwrap()).collect();
    assert_eq!(ids, vec![1, 3, 4]);

    up.batch_execute("ALTER TABLE public.repl_todos ADD COLUMN extra text")
        .unwrap();
    up.execute("INSERT INTO public.repl_todos VALUES (5, 'five', 'x')", &[])
        .unwrap();

    let added = events2.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(matches!(added.changes[0], RowChange::Insert { .. }));
    assert!(!replica2.is_halted());

    let rows = block_on(db.query("SELECT id, extra FROM repl_todos ORDER BY id", &[])).unwrap();
    let ids: Vec<i32> = rows.iter().map(|r| r.get::<i32>(0).unwrap()).collect();
    assert_eq!(ids, vec![1, 3, 4, 5]);
    let row5 = rows.iter().find(|r| r.get::<i32>(0).unwrap() == 5).unwrap();
    assert_eq!(row5.get::<&str>(1).unwrap(), "x");

    replica2.stop();
    assert!(wait_until(Duration::from_secs(10), || replica2.is_stopped()));
    assert!(!replica2.is_halted());

    block_on(Replica::decommission(&db, &config)).unwrap();
    let slots = up
        .query(
            "SELECT slot_name FROM pg_replication_slots WHERE slot_name = 'pglite_test_slot'",
            &[],
        )
        .unwrap();
    assert!(slots.is_empty());
    let meta = block_on(db.query("SELECT count(*) FROM _pglite_replica", &[])).unwrap();
    assert_eq!(meta[0].get::<i64>(0).unwrap(), 0);
    block_on(Replica::decommission(&db, &config)).unwrap();

    let replica3 = block_on(Replica::start(db.clone(), config.clone())).unwrap();
    let rows = block_on(db.query("SELECT id, extra FROM repl_todos ORDER BY id", &[])).unwrap();
    let ids: Vec<i32> = rows.iter().map(|r| r.get::<i32>(0).unwrap()).collect();
    assert_eq!(ids, vec![1, 3, 4, 5]);

    let events3 = replica3.subscribe();

    let mut walsender: Option<i32> = None;
    assert!(wait_until(Duration::from_secs(10), || {
        walsender = up
            .query(
                "SELECT active_pid FROM pg_replication_slots WHERE slot_name = 'pglite_test_slot' AND active_pid IS NOT NULL",
                &[],
            )
            .ok()
            .and_then(|rows| rows.first().map(|row| row.get(0)));
        walsender.is_some()
    }));
    up.query("SELECT pg_terminate_backend($1)", &[&walsender.unwrap()])
        .unwrap();

    up.execute("INSERT INTO public.repl_todos VALUES (6, 'six', NULL)", &[])
        .unwrap();
    let after_kill = events3.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(matches!(after_kill.changes[0], RowChange::Insert { .. }));
    assert!(!replica3.is_halted());
    let rows = block_on(db.query("SELECT count(*) FROM repl_todos WHERE id = 6", &[])).unwrap();
    assert_eq!(rows[0].get::<i64>(0).unwrap(), 1);

    let stamp = |db: &pglite::PGlite| -> String {
        block_on(db.query("SELECT updated_at::text FROM _pglite_replica", &[])).unwrap()[0]
            .get::<&str>(0)
            .unwrap()
            .to_string()
    };
    let before_churn = stamp(&db);
    up.batch_execute(
        "DROP TABLE IF EXISTS public.side; CREATE TABLE public.side (id int PRIMARY KEY);",
    )
    .unwrap();
    for i in 0..5i32 {
        up.execute("INSERT INTO public.side VALUES ($1)", &[&i])
            .unwrap();
    }
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(stamp(&db), before_churn);
    assert!(!replica3.is_halted());

    up.execute(
        "INSERT INTO public.repl_todos VALUES (7, 'seven', NULL)",
        &[],
    )
    .unwrap();
    let after_churn = events3.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(matches!(after_churn.changes[0], RowChange::Insert { .. }));
    assert_ne!(stamp(&db), before_churn);

    up.batch_execute(
        "ALTER TABLE public.repl_todos DROP COLUMN extra;
         CREATE TABLE public.repl_new (id int PRIMARY KEY, v text);
         ALTER PUBLICATION pglite_test_pub ADD TABLE public.repl_new;",
    )
    .unwrap();
    up.execute("INSERT INTO public.repl_new VALUES (1, 'alpha')", &[])
        .unwrap();

    assert!(wait_until(Duration::from_secs(30), || {
        block_on(db.query("SELECT v FROM repl_new WHERE id = 1", &[]))
            .ok()
            .and_then(|rows| rows.first().map(|r| r.get::<&str>(0).unwrap().to_string()))
            == Some("alpha".to_string())
    }));
    assert!(!replica3.is_halted());

    up.execute("INSERT INTO public.repl_todos VALUES (8, 'eight')", &[])
        .unwrap();
    let got8 = wait_until(Duration::from_secs(30), || {
        replica3.is_halted()
            || block_on(db.query("SELECT count(*) FROM repl_todos WHERE id = 8", &[]))
                .map(|rows| rows[0].get::<i64>(0).unwrap() == 1)
                .unwrap_or(false)
    });
    if replica3.is_halted() {
        panic!("replica3 halted: {:?}", replica3.halt_reason());
    }
    assert!(got8);
    let extra_cols = block_on(db.query(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_name = 'repl_todos' AND column_name = 'extra'",
        &[],
    ))
    .unwrap();
    assert_eq!(extra_cols[0].get::<i64>(0).unwrap(), 0);
    assert!(!replica3.is_halted());

    replica3.stop();
    assert!(wait_until(Duration::from_secs(10), || replica3.is_stopped()));
    assert!(!replica3.is_halted());

    block_on(Replica::decommission(&db, &config)).unwrap();
    let _ = up.batch_execute(
        "DROP EVENT TRIGGER IF EXISTS pglite_ddl_watch; \
         DROP FUNCTION IF EXISTS public.pglite_emit_ddl(); \
         DROP PUBLICATION IF EXISTS pglite_test_pub; \
         DROP TABLE IF EXISTS public.repl_todos; \
         DROP TABLE IF EXISTS public.repl_typed; \
         DROP TABLE IF EXISTS public.repl_nopk; \
         DROP TABLE IF EXISTS public.repl_new; \
         DROP TABLE IF EXISTS public.side; \
         DROP TYPE IF EXISTS public.repl_addr CASCADE; \
         DROP TYPE IF EXISTS public.repl_num CASCADE; \
         DROP DOMAIN IF EXISTS public.repl_pos CASCADE;",
    );

    block_on(db.close()).unwrap();
}
