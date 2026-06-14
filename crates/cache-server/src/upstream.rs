use serde_json::{Map, Value};
use tokio_postgres::{Config, NoTls, SimpleQueryMessage};

use crate::error::CacheError;

pub struct Upstream {
    config: Config,
}

impl Upstream {
    pub fn new(host: &str, port: u16, user: &str, password: &str, database: &str) -> Upstream {
        let mut config = Config::new();
        config
            .host(host)
            .port(port)
            .user(user)
            .password(password)
            .dbname(database);
        Upstream { config }
    }

    pub async fn forward(&self, sql: &str) -> Result<String, CacheError> {
        let (client, connection) = self.config.connect(NoTls).await?;
        let driver = tokio::spawn(connection);
        let result = client.simple_query(sql).await;
        drop(client);
        let _ = driver.await;
        let messages = result?;

        let mut rows: Vec<Value> = Vec::new();
        let mut affected: u64 = 0;
        for message in messages {
            match message {
                SimpleQueryMessage::Row(row) => {
                    let mut object = Map::new();
                    for (idx, column) in row.columns().iter().enumerate() {
                        let value = match row.get(idx) {
                            Some(text) => Value::String(text.to_string()),
                            None => Value::Null,
                        };
                        object.insert(column.name().to_string(), value);
                    }
                    rows.push(Value::Object(object));
                }
                SimpleQueryMessage::CommandComplete(count) => {
                    affected += count;
                }
                _ => {}
            }
        }

        if rows.is_empty() {
            Ok(serde_json::json!({ "command": "OK", "rowCount": affected }).to_string())
        } else {
            Ok(Value::Array(rows).to_string())
        }
    }
}
