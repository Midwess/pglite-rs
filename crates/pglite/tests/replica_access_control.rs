#![cfg(feature = "replica")]

use std::time::{Duration, Instant};

use futures::executor::block_on;
use pglite::{PGlite, Replica, ReplicaConfig};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

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

#[test]
fn replica_access_control() {
    let Ok(host) = std::env::var("PGLITE_REPLICA_UPSTREAM_HOST") else {
        eprintln!("skipping access-control integration test: PGLITE_REPLICA_UPSTREAM_HOST not set");
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

    up.batch_execute("DROP PUBLICATION IF EXISTS pglite_ac_pub")
        .unwrap();
    let _ = up.execute(
        "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = 'pglite_ac_slot'",
        &[],
    );
    up.batch_execute(
        "DROP TABLE IF EXISTS public.ac_secure;
         DROP ROLE IF EXISTS pglite_ac_app;
         CREATE ROLE pglite_ac_app NOLOGIN;
         CREATE TABLE public.ac_secure (id int PRIMARY KEY, owner text, secret text);
         INSERT INTO public.ac_secure VALUES (1, 'alice', 'a-secret'), (2, 'bob', 'b-secret');
         GRANT SELECT ON public.ac_secure TO pglite_ac_app;
         ALTER TABLE public.ac_secure ENABLE ROW LEVEL SECURITY;
         CREATE POLICY ac_owner_only ON public.ac_secure FOR SELECT TO pglite_ac_app \
           USING (owner = current_setting('request.jwt.claims', true)::json->>'owner');
         CREATE PUBLICATION pglite_ac_pub FOR TABLE public.ac_secure;",
    )
    .unwrap();

    let db = block_on(PGlite::open_temp()).unwrap();
    let config = ReplicaConfig {
        host: host.clone(),
        port,
        user,
        password,
        database,
        publication: "pglite_ac_pub".into(),
        slot_name: "pglite_ac_slot".into(),
        read_timeout: Duration::from_secs(2),
        role_poll_interval: Duration::from_secs(1),
        ..Default::default()
    };

    let replica = block_on(Replica::start(db.clone(), config)).expect("replica start");

    let policies = block_on(db.query(
        "SELECT count(*)::int FROM pg_policies WHERE schemaname = 'public' AND tablename = 'ac_secure'",
        &[],
    ))
    .unwrap();
    assert_eq!(policies[0].get::<i32>(0).unwrap(), 1, "policy replicated");

    let role = block_on(db.query(
        "SELECT rolcanlogin::int, rolbypassrls::int FROM pg_roles WHERE rolname = 'pglite_ac_app'",
        &[],
    ))
    .unwrap();
    assert_eq!(role.len(), 1, "app role replicated");
    assert_eq!(
        role[0].get::<i32>(0).unwrap(),
        0,
        "replicated role is NOLOGIN"
    );
    assert_eq!(
        role[0].get::<i32>(1).unwrap(),
        0,
        "replicated role is NOBYPASSRLS"
    );

    let as_super = block_on(db.query("SELECT count(*)::int FROM ac_secure", &[])).unwrap();
    assert_eq!(
        as_super[0].get::<i32>(0).unwrap(),
        2,
        "superuser path bypasses RLS and sees every row"
    );

    let alice = block_on(db.query_as(
        "pglite_ac_app",
        Some("{\"owner\":\"alice\"}"),
        "SELECT id, secret FROM ac_secure ORDER BY id",
        &[],
    ))
    .unwrap();
    assert_eq!(alice.len(), 1, "alice sees only her own row");
    assert_eq!(alice[0].get::<i32>(0).unwrap(), 1);
    assert_eq!(alice[0].get::<&str>(1).unwrap(), "a-secret");

    let bob = block_on(db.query_as(
        "pglite_ac_app",
        Some("{\"owner\":\"bob\"}"),
        "SELECT id FROM ac_secure",
        &[],
    ))
    .unwrap();
    assert_eq!(bob.len(), 1, "bob sees only his own row");
    assert_eq!(bob[0].get::<i32>(0).unwrap(), 2);

    assert!(
        block_on(replica.security_version()).unwrap() >= 1,
        "security version advanced after backfill reconcile"
    );

    let leak = block_on(db.query("SELECT count(*)::int FROM ac_secure", &[])).unwrap();
    assert_eq!(
        leak[0].get::<i32>(0).unwrap(),
        2,
        "query_as role/claims do not leak into a later plain query"
    );

    assert!(
        block_on(db.query_as("pglite_no_such_role", None, "SELECT 1", &[])).is_err(),
        "query_as fails closed for an unknown role (no superuser fallback)"
    );

    up.batch_execute(
        "REVOKE SELECT ON public.ac_secure FROM pglite_ac_app; \
         DROP POLICY IF EXISTS ac_owner_only ON public.ac_secure; \
         DROP ROLE IF EXISTS pglite_ac_app;",
    )
    .unwrap();
    let revoked = wait_until(Duration::from_secs(10), || {
        block_on(db.query_as(
            "pglite_ac_app",
            Some("{\"owner\":\"alice\"}"),
            "SELECT id FROM ac_secure",
            &[],
        ))
        .is_err()
    });
    assert!(
        revoked,
        "after upstream drops the role+grant, the role poll revokes the lingering local grant (Issue-1 deny-by-default convergence)"
    );

    replica.stop();
    up.batch_execute(
        "DROP PUBLICATION IF EXISTS pglite_ac_pub; \
         DROP TABLE IF EXISTS public.ac_secure; \
         DROP ROLE IF EXISTS pglite_ac_app;",
    )
    .ok();
}
