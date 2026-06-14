use std::collections::{HashMap, HashSet};

use pglite::{CommittedTransaction, Lsn, RowChange};

use serde_json::json;

use crate::classify::ReadClassifier;
use crate::diff::{diff, keyed_map, Delta};
use crate::shapelog::ShapeLog;
use crate::version::VersionIndex;

fn classifier() -> ReadClassifier {
    let mut replicated = HashSet::new();
    replicated.insert("users".to_string());
    replicated.insert("orders".to_string());
    ReadClassifier::new(replicated)
}

#[test]
fn pure_select_over_replicated_table_is_cacheable() {
    let query = classifier()
        .classify("select * from users where id = 1")
        .unwrap();
    assert!(query.tables.contains(&"users".to_string()));
}

#[test]
fn select_over_non_replicated_table_is_rejected() {
    assert!(classifier().classify("select * from secrets").is_err());
}

#[test]
fn insert_is_rejected() {
    assert!(classifier()
        .classify("insert into users (id) values (1)")
        .is_err());
}

#[test]
fn select_for_update_is_rejected() {
    assert!(classifier()
        .classify("select * from users where id = 1 for update")
        .is_err());
}

#[test]
fn volatile_function_is_rejected() {
    assert!(classifier().classify("select now() from users").is_err());
}

#[test]
fn multi_statement_is_rejected() {
    assert!(classifier()
        .classify("select 1 from users; select 2 from orders")
        .is_err());
}

#[test]
fn equality_filter_is_extracted_for_cacheable() {
    let query = classifier()
        .classify("select * from users where id = 7")
        .unwrap();
    assert!(query
        .eq_filters
        .contains(&("id".to_string(), "7".to_string())));
}

fn users_versions() -> VersionIndex {
    let mut pk = HashMap::new();
    pk.insert("users".to_string(), "id".to_string());
    VersionIndex::new(pk, HashSet::new())
}

fn full_events_versions() -> VersionIndex {
    let mut pk = HashMap::new();
    pk.insert("events".to_string(), "id".to_string());
    let mut full = HashSet::new();
    full.insert("events".to_string());
    VersionIndex::new(pk, full)
}

fn update_event_status(old: &str, new: &str, end_lsn: u64) -> CommittedTransaction {
    CommittedTransaction {
        xid: 1,
        commit_lsn: Lsn(end_lsn.saturating_sub(1)),
        end_lsn: Lsn(end_lsn),
        commit_ts: 0,
        changes: vec![RowChange::Update {
            schema: "public".to_string(),
            table: "events".to_string(),
            key: vec![
                ("id".to_string(), Some("1".to_string())),
                ("status".to_string(), Some(old.to_string())),
            ],
            row: vec![
                ("id".to_string(), Some("1".to_string())),
                ("status".to_string(), Some(new.to_string())),
            ],
        }],
    }
}

fn update_users_id(id: &str, end_lsn: u64) -> CommittedTransaction {
    CommittedTransaction {
        xid: 1,
        commit_lsn: Lsn(end_lsn.saturating_sub(1)),
        end_lsn: Lsn(end_lsn),
        commit_ts: 0,
        changes: vec![RowChange::Update {
            schema: "public".to_string(),
            table: "users".to_string(),
            key: vec![("id".to_string(), Some(id.to_string()))],
            row: vec![
                ("id".to_string(), Some(id.to_string())),
                ("name".to_string(), Some("x".to_string())),
            ],
        }],
    }
}

#[test]
fn unrelated_value_change_does_not_bump_version() {
    let versions = users_versions();
    versions.advance(&update_users_id("5", 100));
    let other = versions.version_of(
        &["users".to_string()],
        &[("id".to_string(), "7".to_string())],
    );
    assert_eq!(other.0, 0);
}

#[test]
fn matching_value_change_bumps_version() {
    let versions = users_versions();
    versions.advance(&update_users_id("5", 100));
    let same = versions.version_of(
        &["users".to_string()],
        &[("id".to_string(), "5".to_string())],
    );
    assert_eq!(same.0, 100);
}

#[test]
fn no_filter_falls_back_to_table_level() {
    let versions = users_versions();
    versions.advance(&update_users_id("5", 100));
    let table = versions.version_of(&["users".to_string()], &[]);
    assert_eq!(table.0, 100);
}

#[test]
fn full_identity_table_anchors_non_pk_equality() {
    let versions = full_events_versions();
    versions.advance(&update_event_status("active", "archived", 100));
    let leaving = versions.version_of(
        &["events".to_string()],
        &[("status".to_string(), "active".to_string())],
    );
    let entering = versions.version_of(
        &["events".to_string()],
        &[("status".to_string(), "archived".to_string())],
    );
    let untouched = versions.version_of(
        &["events".to_string()],
        &[("status".to_string(), "draft".to_string())],
    );
    assert_eq!(leaving.0, 100);
    assert_eq!(entering.0, 100);
    assert_eq!(untouched.0, 0);
}

#[test]
fn non_full_non_pk_filter_stays_table_level() {
    let versions = users_versions();
    versions.advance(&update_users_id("5", 100));
    let coarse = versions.version_of(
        &["users".to_string()],
        &[("status".to_string(), "active".to_string())],
    );
    assert_eq!(coarse.0, 100);
}

#[test]
fn join_query_stays_table_level() {
    let versions = users_versions();
    versions.advance(&update_users_id("5", 100));
    let joined = versions.version_of(
        &["users".to_string(), "orders".to_string()],
        &[("id".to_string(), "7".to_string())],
    );
    assert_eq!(joined.0, 100);
}

fn insert_users(id: &str, end_lsn: u64) -> CommittedTransaction {
    CommittedTransaction {
        xid: 1,
        commit_lsn: Lsn(end_lsn.saturating_sub(1)),
        end_lsn: Lsn(end_lsn),
        commit_ts: 0,
        changes: vec![RowChange::Insert {
            schema: "public".to_string(),
            table: "users".to_string(),
            row: vec![("id".to_string(), Some(id.to_string()))],
        }],
    }
}

#[test]
fn shape_log_appends_and_ranges_after_offset() {
    let log = ShapeLog::new();
    log.ingest(&insert_users("1", 10));
    log.ingest(&insert_users("2", 20));
    let range = log.range("users", 10);
    assert_eq!(range.changes.len(), 1);
    assert_eq!(range.changes[0].offset, 20);
    assert_eq!(range.latest_offset, 20);
    assert!(!range.must_refetch);
}

#[test]
fn shape_log_up_to_date_is_empty() {
    let log = ShapeLog::new();
    log.ingest(&insert_users("1", 10));
    let range = log.range("users", 10);
    assert!(range.changes.is_empty());
    assert!(!range.must_refetch);
}

#[test]
fn shape_log_unknown_table_is_empty() {
    let log = ShapeLog::new();
    let range = log.range("ghost", 0);
    assert!(range.changes.is_empty());
    assert!(!range.must_refetch);
}

#[test]
fn shape_log_eviction_signals_refetch() {
    let log = ShapeLog::new();
    for offset in 1..=1100u64 {
        log.ingest(&insert_users(&offset.to_string(), offset));
    }
    assert!(log.range("users", 5).must_refetch);
    assert!(!log.range("users", 1050).must_refetch);
}

#[test]
fn diff_detects_insert_update_delete() {
    let mut prev = HashMap::new();
    prev.insert("1".to_string(), json!({"id": 1, "name": "a"}));
    prev.insert("9".to_string(), json!({"id": 9}));
    let mut next = HashMap::new();
    next.insert("1".to_string(), json!({"id": 1, "name": "b"}));
    next.insert("2".to_string(), json!({"id": 2}));

    let (mut inserts, mut updates, mut deletes) = (0, 0, 0);
    for delta in diff(&prev, &next) {
        match delta {
            Delta::Insert { .. } => inserts += 1,
            Delta::Update { .. } => updates += 1,
            Delta::Delete { .. } => deletes += 1,
        }
    }
    assert_eq!((inserts, updates, deletes), (1, 1, 1));
}

#[test]
fn diff_ignores_unchanged_rows() {
    let mut prev = HashMap::new();
    prev.insert("1".to_string(), json!({"id": 1}));
    let next = prev.clone();
    assert!(diff(&prev, &next).is_empty());
}

#[test]
fn keyed_map_uses_pk_column() {
    let map = keyed_map(r#"[{"id":1,"n":"a"},{"id":2,"n":"b"}]"#, Some("id"));
    assert!(map.contains_key("1"));
    assert!(map.contains_key("2"));
}

#[test]
fn keyed_map_falls_back_to_row_hash() {
    let map = keyed_map(r#"[{"a":1},{"a":2}]"#, None);
    assert_eq!(map.len(), 2);
}
