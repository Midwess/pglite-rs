use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use pglite::{CommittedTransaction, Lsn, RowChange};

#[derive(Clone, Default)]
pub struct VersionIndex {
    tables: Arc<RwLock<HashMap<String, u64>>>,
}

impl VersionIndex {
    pub fn new() -> VersionIndex {
        VersionIndex::default()
    }

    pub fn advance(&self, txn: &CommittedTransaction) {
        let lsn = txn.end_lsn.0;
        let mut guard = self.tables.write().unwrap();
        for change in &txn.changes {
            let entry = guard.entry(change_table(change).to_string()).or_insert(0);
            if lsn > *entry {
                *entry = lsn;
            }
        }
    }

    pub fn version_of(&self, tables: &[String]) -> Lsn {
        let guard = self.tables.read().unwrap();
        let mut max = 0u64;
        for table in tables {
            if let Some(value) = guard.get(table) {
                if *value > max {
                    max = *value;
                }
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
