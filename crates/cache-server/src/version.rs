use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use pglite::{CommittedTransaction, Lsn, RowChange};

#[derive(Default)]
struct State {
    tables: HashMap<String, u64>,
    values: HashMap<(String, String, String), u64>,
    structural: HashMap<String, u64>,
}

#[derive(Clone)]
pub struct VersionIndex {
    state: Arc<RwLock<State>>,
    pk: Arc<HashMap<String, String>>,
}

impl VersionIndex {
    pub fn new(pk: HashMap<String, String>) -> VersionIndex {
        let pk = pk
            .into_iter()
            .map(|(table, column)| (table.to_ascii_lowercase(), column.to_ascii_lowercase()))
            .collect();
        VersionIndex {
            state: Arc::new(RwLock::new(State::default())),
            pk: Arc::new(pk),
        }
    }

    pub fn advance(&self, txn: &CommittedTransaction) {
        let lsn = txn.end_lsn.0;
        let mut state = self.state.write().unwrap();
        for change in &txn.changes {
            let table = change_table(change).to_ascii_lowercase();
            let entry = state.tables.entry(table.clone()).or_insert(0);
            if lsn > *entry {
                *entry = lsn;
            }
            match change {
                RowChange::Truncate { .. } => {
                    let floor = state.structural.entry(table).or_insert(0);
                    if lsn > *floor {
                        *floor = lsn;
                    }
                }
                _ => {
                    if let Some(pk) = self.pk.get(&table) {
                        for value in pk_values(change, pk) {
                            state.values.insert((table.clone(), pk.clone(), value), lsn);
                        }
                    }
                }
            }
        }
    }

    pub fn version_of(&self, tables: &[String], eq_filters: &[(String, String)]) -> Lsn {
        let state = self.state.read().unwrap();

        if tables.len() == 1 {
            let table = tables[0].to_ascii_lowercase();
            if let Some(pk) = self.pk.get(&table) {
                if let Some((_, value)) = eq_filters.iter().find(|(column, _)| column == pk) {
                    let watermark = state
                        .values
                        .get(&(table.clone(), pk.clone(), value.clone()))
                        .copied()
                        .unwrap_or(0);
                    let floor = state.structural.get(&table).copied().unwrap_or(0);
                    return Lsn(watermark.max(floor));
                }
            }
        }

        let mut max = 0u64;
        for table in tables {
            let table = table.to_ascii_lowercase();
            if let Some(value) = state.tables.get(&table) {
                max = max.max(*value);
            }
            if let Some(value) = state.structural.get(&table) {
                max = max.max(*value);
            }
        }
        Lsn(max)
    }
}

fn change_table(change: &RowChange) -> &str {
    match change {
        RowChange::Insert { table, .. }
        | RowChange::Update { table, .. }
        | RowChange::Delete { table, .. }
        | RowChange::Truncate { table, .. } => table,
    }
}

fn pk_values(change: &RowChange, pk: &str) -> Vec<String> {
    match change {
        RowChange::Insert { row, .. } => find_value(row, pk).into_iter().collect(),
        RowChange::Update { key, row, .. } => {
            let mut values = Vec::new();
            if let Some(value) = find_value(key, pk) {
                values.push(value);
            }
            if let Some(value) = find_value(row, pk) {
                values.push(value);
            }
            values
        }
        RowChange::Delete { key, .. } => find_value(key, pk).into_iter().collect(),
        RowChange::Truncate { .. } => Vec::new(),
    }
}

fn find_value(columns: &[(String, Option<String>)], pk: &str) -> Option<String> {
    columns
        .iter()
        .find(|(column, _)| column.eq_ignore_ascii_case(pk))
        .and_then(|(_, value)| value.clone())
}
