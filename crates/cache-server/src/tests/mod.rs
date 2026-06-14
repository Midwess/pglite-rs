use std::collections::HashSet;

use crate::classify::{Plan, ReadClassifier};

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
