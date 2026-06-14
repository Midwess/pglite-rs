use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use serde_json::Value;

pub enum Delta {
    Insert { key: String, row: Value },
    Update { key: String, row: Value },
    Delete { key: String },
}

pub fn diff(prev: &HashMap<String, Value>, next: &HashMap<String, Value>) -> Vec<Delta> {
    let mut deltas = Vec::new();
    for (key, row) in next {
        match prev.get(key) {
            None => deltas.push(Delta::Insert {
                key: key.clone(),
                row: row.clone(),
            }),
            Some(old) if old != row => deltas.push(Delta::Update {
                key: key.clone(),
                row: row.clone(),
            }),
            Some(_) => {}
        }
    }
    for key in prev.keys() {
        if !next.contains_key(key) {
            deltas.push(Delta::Delete { key: key.clone() });
        }
    }
    deltas
}

pub fn keyed_map(json_array: &str, pk: Option<&str>) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    if let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(json_array) {
        for row in rows {
            let key = pk
                .and_then(|column| row.get(column))
                .map(value_key)
                .unwrap_or_else(|| row_hash(&row));
            map.insert(key, row);
        }
    }
    map
}

fn value_key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn row_hash(row: &Value) -> String {
    let mut hasher = DefaultHasher::new();
    row.to_string().hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
