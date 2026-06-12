#[cfg(feature = "replica")]
fn main() {
    use std::time::{Duration, Instant};

    use futures::executor::block_on;
    use pglite::{Lsn, PGlite, Replica, ReplicaConfig, RowChange};

    fn env_or(key: &str, default: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| default.to_string())
    }

    fn change_kind(c: &RowChange) -> &'static str {
        match c {
            RowChange::Insert { .. } => "insert",
            RowChange::Update { .. } => "update",
            RowChange::Delete { .. } => "delete",
            RowChange::Truncate { .. } => "truncate",
        }
    }

    let host = env_or("PGLITE_REPLICA_UPSTREAM_HOST", "127.0.0.1");
    let port: u16 = env_or("PGLITE_REPLICA_UPSTREAM_PORT", "5433")
        .parse()
        .unwrap();
    let user = env_or("PGLITE_REPLICA_UPSTREAM_USER", "postgres");
    let password = env_or("PGLITE_REPLICA_UPSTREAM_PASSWORD", "postgres");
    let database = env_or("PGLITE_REPLICA_UPSTREAM_DB", "postgres");

    let mut up = match postgres::Client::connect(
        &format!("host={host} port={port} user={user} password={password} dbname={database}"),
        postgres::NoTls,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot reach upstream postgres at {host}:{port}: {e}");
            eprintln!("start one with: ./examples/run-replica-demo.sh (or docker run -d -p 5433:5432 -e POSTGRES_PASSWORD=postgres postgres:16 -c wal_level=logical)");
            std::process::exit(1);
        }
    };

    println!("== upstream: seeding schema and data ==");
    up.batch_execute("DROP PUBLICATION IF EXISTS demo_pub")
        .unwrap();
    let _ = up.execute(
        "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = 'demo_slot'",
        &[],
    );
    up.batch_execute(
        "SET TIME ZONE 'UTC';
         DROP TABLE IF EXISTS public.orders, public.customers, public.archive;
         CREATE TABLE public.customers (
             id int PRIMARY KEY,
             name text NOT NULL,
             joined timestamptz NOT NULL DEFAULT now()
         );
         CREATE TABLE public.orders (
             id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
             customer_id int NOT NULL REFERENCES public.customers(id),
             amount numeric(12,2) NOT NULL,
             tags text[],
             meta jsonb NOT NULL DEFAULT '{}',
             created_at timestamptz NOT NULL DEFAULT now()
         );
         CREATE TABLE public.archive (id int PRIMARY KEY, label text);
         INSERT INTO public.customers (id, name) VALUES (1,'ada'), (2,'grace'), (3,'edsger');
         INSERT INTO public.orders (customer_id, amount, tags, meta)
             SELECT (g % 3) + 1,
                    (g * 7)::numeric / 100,
                    ARRAY['tag' || (g % 5), 'a,b\"c'],
                    jsonb_build_object('seq', g)
             FROM generate_series(1, 200) g;
         INSERT INTO public.archive VALUES (1, 'old'), (2, 'older');
         CREATE PUBLICATION demo_pub FOR TABLE public.customers, public.orders, public.archive;",
    )
    .unwrap();
    println!("   3 customers, 200 orders, 2 archive rows published as demo_pub");

    println!("== replica: backfill ==");
    let db = block_on(PGlite::open_temp()).unwrap();
    block_on(db.exec("SET TIME ZONE 'UTC'")).unwrap();
    let config = ReplicaConfig {
        host,
        port,
        user,
        password,
        database,
        publication: "demo_pub".into(),
        slot_name: "demo_slot".into(),
        read_timeout: Duration::from_secs(2),
        ..Default::default()
    };
    let t0 = Instant::now();
    let replica = block_on(Replica::start(db.clone(), config)).expect("replica start");
    println!(
        "   backfill done in {:?}, watermark {}",
        t0.elapsed(),
        replica.watermark()
    );
    for tbl in ["customers", "orders", "archive"] {
        let rows = block_on(db.query(&format!("SELECT count(*) FROM {tbl}"), &[])).unwrap();
        println!("   {tbl}: {} rows", rows[0].get::<i64>(0).unwrap());
    }

    let events = replica.subscribe();
    let expect_event = |label: &str, expected_changes: usize| {
        let ev = events
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
        let mut counts: Vec<String> = Vec::new();
        for kind in ["insert", "update", "delete", "truncate"] {
            let n = ev.changes.iter().filter(|c| change_kind(c) == kind).count();
            if n > 0 {
                counts.push(format!("{n} {kind}"));
            }
        }
        println!(
            "   event xid={} end_lsn={} -> {} ({label})",
            ev.xid,
            ev.end_lsn,
            counts.join(", ")
        );
        assert_eq!(
            ev.changes.len(),
            expected_changes,
            "{label}: expected {expected_changes} changes, got {}",
            ev.changes.len()
        );
        ev
    };

    println!("== live sync: one upstream transaction = one replica transaction ==");
    let mut txn = up.transaction().unwrap();
    txn.execute(
        "INSERT INTO public.customers (id, name) VALUES (4, 'o''brien \"quoted\"')",
        &[],
    )
    .unwrap();
    txn.execute(
        "INSERT INTO public.orders (customer_id, amount, tags, meta)
         SELECT 4, 9.99, ARRAY['new'], jsonb_build_object('batch', true) FROM generate_series(1, 5)",
        &[],
    )
    .unwrap();
    txn.commit().unwrap();
    let ev = expect_event("atomic insert batch", 6);
    assert!(matches!(ev.changes[0], RowChange::Insert { .. }));

    let updated = up
        .execute(
            "UPDATE public.orders SET amount = round(amount * 11, 2) / 10
             WHERE id IN (SELECT id FROM public.orders ORDER BY id LIMIT 50)",
            &[],
        )
        .unwrap();
    expect_event("bulk price update", updated as usize);

    let deleted = up
        .execute("DELETE FROM public.orders WHERE customer_id = 1", &[])
        .unwrap();
    expect_event("delete customer 1 orders", deleted as usize);

    up.execute(
        "UPDATE public.customers SET name = 'O''Brien \\ \"final\" 💡' WHERE id = 4",
        &[],
    )
    .unwrap();
    expect_event("special characters update", 1);

    up.batch_execute("TRUNCATE public.archive").unwrap();
    let ev = expect_event("truncate archive", 1);
    assert!(matches!(ev.changes[0], RowChange::Truncate { .. }));

    println!("== read-your-writes barrier: wait for upstream LSN on the replica ==");
    let lsn_row = up
        .query_one("SELECT pg_current_wal_lsn()::text", &[])
        .unwrap();
    let target = Lsn::from_pg_str(lsn_row.get::<_, &str>(0)).unwrap();
    let t1 = Instant::now();
    while replica.watermark() < target {
        assert!(t1.elapsed() < Duration::from_secs(30), "barrier timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "   watermark {} >= upstream {} after {:?}",
        replica.watermark(),
        target,
        t1.elapsed()
    );

    println!("== consistency audit: count + md5 over every row, both sides ==");
    for (tbl, pk) in [("customers", "id"), ("orders", "id"), ("archive", "id")] {
        let sql = format!(
            "SELECT count(*)::text, coalesce(md5(string_agg(x, '|')), 'empty')
             FROM (SELECT t::text AS x FROM public.{tbl} t ORDER BY {pk}) s"
        );
        let urow = up.query_one(&sql, &[]).unwrap();
        let (ucount, usum) = (urow.get::<_, &str>(0), urow.get::<_, &str>(1));
        let lrows = block_on(db.query(&sql, &[])).unwrap();
        let (lcount, lsum) = (
            lrows[0].get::<&str>(0).unwrap(),
            lrows[0].get::<&str>(1).unwrap(),
        );
        assert_eq!((ucount, usum), (lcount, lsum), "mismatch on {tbl}");
        println!("   {tbl}: {ucount} rows, md5 {usum} == upstream");
    }

    let note = block_on(db.query("SELECT name FROM customers WHERE id = 4", &[])).unwrap();
    assert_eq!(note[0].get::<&str>(0).unwrap(), "O'Brien \\ \"final\" 💡");
    println!("   unicode/quoting fidelity verified");

    replica.stop();
    let t2 = Instant::now();
    while !replica.is_stopped() {
        assert!(t2.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!replica.is_halted());
    println!("   replica stopped cleanly in {:?}", t2.elapsed());

    let _ = up.execute("SELECT pg_drop_replication_slot('demo_slot')", &[]);
    let _ = up.batch_execute(
        "DROP PUBLICATION IF EXISTS demo_pub; DROP TABLE IF EXISTS public.orders, public.customers, public.archive;",
    );
    block_on(db.close()).unwrap();
    println!("replica_sync: all checks passed");
}

#[cfg(not(feature = "replica"))]
fn main() {
    eprintln!("rebuild with: cargo run -p pglite-examples --features replica --bin replica_sync");
}
