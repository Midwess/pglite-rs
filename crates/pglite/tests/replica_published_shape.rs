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
fn replica_published_shape_and_rejections() {
    let db = block_on(PGlite::open_temp()).unwrap();

    block_on(db.query(
        "CREATE TABLE nn (id int PRIMARY KEY, req text NOT NULL, opt text)",
        &[],
    ))
    .unwrap();
    let rows = block_on(db.query(
        "SELECT a.attname::text, a.attnotnull::text, a.attnotnull::int::text \
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
         WHERE c.relname = 'nn' AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attnum",
        &[],
    ))
    .unwrap();
    let col = |name: &str, idx: usize| {
        rows.iter()
            .find(|r| r.get::<&str>(0).unwrap() == name)
            .unwrap()
            .get::<&str>(idx)
            .unwrap()
            .to_string()
    };
    assert_eq!(col("req", 1), "true");
    assert_eq!(col("req", 2), "1");
    assert_eq!(col("opt", 2), "0");

    let Ok(host) = std::env::var("PGLITE_REPLICA_UPSTREAM_HOST") else {
        eprintln!("skipping upstream replica scenarios: PGLITE_REPLICA_UPSTREAM_HOST not set");
        block_on(db.close()).unwrap();
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

    let version: i32 = up
        .query_one("SELECT current_setting('server_version_num')::int", &[])
        .unwrap()
        .get(0);

    if version >= 150000 {
        up.batch_execute("DROP PUBLICATION IF EXISTS pglite_shape_pub")
            .unwrap();
        let _ = up.execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = 'pglite_shape_slot'",
            &[],
        );
        up.batch_execute(
            "DROP TABLE IF EXISTS public.shape;
             CREATE TABLE public.shape (
                id int PRIMARY KEY,
                a int NOT NULL,
                g int GENERATED ALWAYS AS (a * 2) STORED,
                secret text
             );
             INSERT INTO public.shape (id, a, secret) VALUES (1, 10, 's1'), (2, 20, 's2');
             CREATE PUBLICATION pglite_shape_pub FOR TABLE public.shape (id, a);",
        )
        .unwrap();

        let config = ReplicaConfig {
            host: host.clone(),
            port,
            user: user.clone(),
            password: password.clone(),
            database: database.clone(),
            publication: "pglite_shape_pub".into(),
            slot_name: "pglite_shape_slot".into(),
            read_timeout: Duration::from_secs(2),
            ..Default::default()
        };
        let replica =
            block_on(Replica::start(db.clone(), config.clone())).expect("shape replica start");

        let rows = block_on(db.query("SELECT id, a FROM shape ORDER BY id", &[])).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<i32>(1).unwrap(), 10);
        assert!(block_on(db.query("SELECT g FROM shape", &[])).is_err());
        assert!(block_on(db.query("SELECT secret FROM shape", &[])).is_err());
        let nn = block_on(db.query(
            "SELECT a.attnotnull::int::text FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             WHERE c.relname = 'shape' AND a.attname = 'a'",
            &[],
        ))
        .unwrap();
        assert_eq!(
            nn[0].get::<&str>(0).unwrap(),
            "1",
            "NOT NULL must be preserved on the replica column"
        );

        let events = replica.subscribe();
        up.execute(
            "INSERT INTO public.shape (id, a, secret) VALUES (3, 30, 's3')",
            &[],
        )
        .unwrap();
        let first = events.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(matches!(first.changes[0], RowChange::Insert { .. }));
        assert!(!replica.is_halted());
        let rows = block_on(db.query("SELECT id, a FROM shape ORDER BY id", &[])).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].get::<i32>(1).unwrap(), 30);

        replica.stop();
        assert!(wait_until(Duration::from_secs(10), || replica.is_stopped()));
        assert!(!replica.is_halted());
        block_on(Replica::decommission(&db, &config)).unwrap();
        let _ = up.batch_execute(
            "DROP PUBLICATION IF EXISTS pglite_shape_pub; DROP TABLE IF EXISTS public.shape;",
        );

        up.batch_execute("DROP PUBLICATION IF EXISTS pglite_filt_pub")
            .unwrap();
        let _ = up.execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = 'pglite_filt_slot'",
            &[],
        );
        up.batch_execute(
            "DROP TABLE IF EXISTS public.filt;
             CREATE TABLE public.filt (id int PRIMARY KEY, v text);
             CREATE INDEX filt_v_idx ON public.filt (v);
             INSERT INTO public.filt VALUES (1, 'a'), (2, 'b'), (3, 'c');
             CREATE PUBLICATION pglite_filt_pub FOR TABLE public.filt WHERE (id >= 2);",
        )
        .unwrap();

        let filt_config = ReplicaConfig {
            host: host.clone(),
            port,
            user: user.clone(),
            password: password.clone(),
            database: database.clone(),
            publication: "pglite_filt_pub".into(),
            slot_name: "pglite_filt_slot".into(),
            read_timeout: Duration::from_secs(2),
            ..Default::default()
        };
        let filt_replica =
            block_on(Replica::start(db.clone(), filt_config.clone())).expect("filt replica start");

        let ids = block_on(db.query("SELECT id FROM filt ORDER BY id", &[])).unwrap();
        let ids: Vec<i32> = ids.iter().map(|r| r.get::<i32>(0).unwrap()).collect();
        assert_eq!(
            ids,
            vec![2, 3],
            "publication row filter not applied to backfill"
        );

        let idx = block_on(db.query(
            "SELECT indexname::text FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'filt'",
            &[],
        ))
        .unwrap();
        let names: Vec<String> = idx
            .iter()
            .map(|r| r.get::<&str>(0).unwrap().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "filt_v_idx"),
            "secondary index not replicated: {names:?}"
        );

        filt_replica.stop();
        assert!(wait_until(Duration::from_secs(10), || filt_replica.is_stopped()));
        block_on(Replica::decommission(&db, &filt_config)).unwrap();
        let _ = up.batch_execute(
            "DROP PUBLICATION IF EXISTS pglite_filt_pub; DROP TABLE IF EXISTS public.filt;",
        );
    }

    let reject_config = ReplicaConfig {
        host: host.clone(),
        port,
        user,
        password,
        database,
        publication: "pglite_reject_pub".into(),
        slot_name: "pglite_reject_slot".into(),
        read_timeout: Duration::from_secs(2),
        ..Default::default()
    };
    let reset = |up: &mut postgres::Client| {
        up.batch_execute("DROP PUBLICATION IF EXISTS pglite_reject_pub")
            .unwrap();
        let _ = up.execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = 'pglite_reject_slot'",
            &[],
        );
        up.batch_execute("DROP TABLE IF EXISTS public.reject_t")
            .unwrap();
    };

    reset(&mut up);
    up.batch_execute(
        "CREATE TABLE public.reject_t (id int PRIMARY KEY, v text);
         ALTER TABLE public.reject_t REPLICA IDENTITY NOTHING;
         CREATE PUBLICATION pglite_reject_pub FOR TABLE public.reject_t;",
    )
    .unwrap();
    let err = block_on(Replica::start(db.clone(), reject_config.clone()))
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        err.contains("REPLICA IDENTITY NOTHING"),
        "RI NOTHING: {err}"
    );
    block_on(Replica::decommission(&db, &reject_config)).unwrap();

    reset(&mut up);
    up.batch_execute(
        "CREATE TABLE public.reject_t (id int PRIMARY KEY, v text);
         CREATE PUBLICATION pglite_reject_pub FOR TABLE public.reject_t WITH (publish = 'insert');",
    )
    .unwrap();
    let err = block_on(Replica::start(db.clone(), reject_config.clone()))
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(err.contains("does not publish"), "insert-only: {err}");
    block_on(Replica::decommission(&db, &reject_config)).unwrap();

    reset(&mut up);
    block_on(db.close()).unwrap();
}
