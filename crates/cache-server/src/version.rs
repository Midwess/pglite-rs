use std::collections::{HashMap, HashSet};
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
    full: Arc<HashSet<String>>,
}

impl VersionIndex {
    pub fn new(pk: HashMap<String, String>, full: HashSet<String>) -> VersionIndex {
        let pk = pk
            .into_iter()
            .map(|(table, column)| (table.to_ascii_lowercase(), column.to_ascii_lowercase()))
            .collect();
        let full = full.into_iter().map(|t| t.to_ascii_lowercase()).collect();
        VersionIndex {
            state: Arc::new(RwLock::new(State::default())),
            pk: Arc::new(pk),
            full: Arc::new(full),
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
                _ if self.full.contains(&table) => {
                    for (column, value) in change_columns(change) {
                        state
                            .values
                            .insert((table.clone(), column.to_ascii_lowercase(), value), lsn);
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
            if let Some((column, value)) = eq_filters
                .iter()
                .find(|(column, _)| self.is_anchorable(&table, column))
            {
                let key = (table.clone(), column.to_ascii_lowercase(), value.clone());
                let watermark = state.values.get(&key).copied().unwrap_or(0);
                let floor = state.structural.get(&table).copied().unwrap_or(0);
                return Lsn(watermark.max(floor));
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

    fn is_anchorable(&self, table: &str, column: &str) -> bool {
        if self.full.contains(table) {
            return true;
        }
        self.pk.get(table).map(|pk| pk == column).unwrap_or(false)
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

fn change_columns(change: &RowChange) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut add = |columns: &[(String, Option<String>)]| {
        for (column, value) in columns {
            if let Some(value) = value {
                out.push((column.clone(), value.clone()));
            }
        }
    };
    match change {
        RowChange::Insert { row, .. } => add(row),
        RowChange::Update { key, row, .. } => {
            add(key);
            add(row);
        }
        RowChange::Delete { key, .. } => add(key),
        RowChange::Truncate { .. } => {}
    }
    out
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
