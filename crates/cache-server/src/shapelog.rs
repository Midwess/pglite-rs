use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use pglite::{CommittedTransaction, RowChange};
use serde_json::{Map, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Notify;

use crate::cdc::CdcBridge;

const PER_TABLE_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct ShapeEntry {
    pub offset: u64,
    pub op: &'static str,
    pub schema: String,
    pub table: String,
    pub row: Value,
}

struct TableLog {
    entries: VecDeque<ShapeEntry>,
    min_offset: u64,
    latest_offset: u64,
}

pub struct ShapeRange {
    pub changes: Vec<ShapeEntry>,
    pub latest_offset: u64,
    pub must_refetch: bool,
}

#[derive(Clone)]
pub struct ShapeLog {
    tables: Arc<Mutex<HashMap<String, TableLog>>>,
    notify: Arc<Notify>,
}

impl ShapeLog {
    pub fn new() -> ShapeLog {
        ShapeLog {
            tables: Arc::new(Mutex::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn start(bridge: &CdcBridge) -> ShapeLog {
        let log = ShapeLog::new();
        let mut receiver = bridge.subscribe();
        let sink = log.clone();
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(txn) => sink.ingest(&txn),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
        log
    }

    pub(crate) fn ingest(&self, txn: &CommittedTransaction) {
        let offset = txn.end_lsn.0;
        let mut tables = self.tables.lock().unwrap();
        for change in &txn.changes {
            let entry = to_entry(change, offset);
            let log = tables
                .entry(entry.table.clone())
                .or_insert_with(|| TableLog {
                    entries: VecDeque::new(),
                    min_offset: 0,
                    latest_offset: 0,
                });
            log.entries.push_back(entry);
            log.latest_offset = offset;
            while log.entries.len() > PER_TABLE_CAPACITY {
                if let Some(removed) = log.entries.pop_front() {
                    log.min_offset = removed.offset;
                }
            }
        }
        drop(tables);
        self.notify.notify_waiters();
    }

    pub fn range(&self, table: &str, after: u64) -> ShapeRange {
        let tables = self.tables.lock().unwrap();
        match tables.get(table) {
            Some(log) => {
                let must_refetch = after < log.min_offset;
                let changes = if must_refetch {
                    Vec::new()
                } else {
                    log.entries
                        .iter()
                        .filter(|entry| entry.offset > after)
                        .cloned()
                        .collect()
                };
                ShapeRange {
                    changes,
                    latest_offset: log.latest_offset.max(after),
                    must_refetch,
                }
            }
            None => ShapeRange {
                changes: Vec::new(),
                latest_offset: after,
                must_refetch: false,
            },
        }
    }

    pub async fn wait_for_change(&self) {
        self.notify.notified().await;
    }
}

fn to_entry(change: &RowChange, offset: u64) -> ShapeEntry {
    let (op, schema, table, row) = match change {
        RowChange::Insert { schema, table, row } => ("insert", schema, table, columns_to_json(row)),
        RowChange::Update {
            schema, table, row, ..
        } => ("update", schema, table, columns_to_json(row)),
        RowChange::Delete {
            schema, table, key, ..
        } => ("delete", schema, table, columns_to_json(key)),
        RowChange::Truncate { schema, table } => ("truncate", schema, table, Value::Null),
    };
    ShapeEntry {
        offset,
        op,
        schema: schema.clone(),
        table: table.clone(),
        row,
    }
}

fn columns_to_json(columns: &[(String, Option<String>)]) -> Value {
    let mut map = Map::new();
    for (column, value) in columns {
        let value = match value {
            Some(text) => Value::String(text.clone()),
            None => Value::Null,
        };
        map.insert(column.clone(), value);
    }
    Value::Object(map)
}
