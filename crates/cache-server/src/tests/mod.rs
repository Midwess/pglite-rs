use std::collections::{HashMap, HashSet};

use pglite::{CommittedTransaction, Lsn, RowChange};

use crate::classify::{Plan, ReadClassifier};
use crate::version::VersionIndex;

fn classifier() -> ReadClassifier {
    let mut replicated = HashSet::new();
    replicated.insert("users".to_string());
    replicated.insert("orders".to_string());
    ReadClassifier::new(replicated)
}

#[test]
fn pure_select_over_replicated_table_is_cacheable() {
    match classifier()
        .classify("select * from users where id = 1")
        .unwrap()
    {
        Plan::Cacheable { tables, .. } => assert!(tables.contains(&"users".to_string())),
        _ => panic!("expected cacheable"),
    }
}

#[test]
fn select_over_non_replicated_table_is_rejected() {
    assert!(classifier().classify("select * from secrets").is_err());
}

#[test]
fn insert_is_forwarded() {
    assert!(matches!(
        classifier()
            .classify("insert into users (id) values (1)")
            .unwrap(),
        Plan::Forward { .. }
    ));
}

#[test]
fn select_for_update_is_forwarded() {
    assert!(matches!(
        classifier()
            .classify("select * from users where id = 1 for update")
            .unwrap(),
        Plan::Forward { .. }
    ));
}

#[test]
fn volatile_function_passes_through_uncached() {
    assert!(matches!(
        classifier().classify("select now()").unwrap(),
        Plan::PassThrough { .. }
    ));
}

#[test]
fn multi_statement_is_rejected() {
    assert!(classifier()
        .classify("select 1 from users; select 2 from orders")
        .is_err());
}

#[test]
fn equality_filter_is_extracted_for_cacheable() {
    match classifier()
        .classify("select * from users where id = 7")
        .unwrap()
    {
        Plan::Cacheable { eq_filters, .. } => {
            assert!(eq_filters.contains(&("id".to_string(), "7".to_string())))
        }
        _ => panic!("expected cacheable"),
    }
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
