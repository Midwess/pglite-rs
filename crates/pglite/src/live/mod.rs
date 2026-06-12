mod tables;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use postgres_types::ToSql;

use crate::db::PGlite;
use crate::error::Error;
use crate::row::Row;

pub struct LiveQuery {
    db: PGlite,
    view_name: String,
    watched: Vec<(u32, u32, String, String)>,
    tokens: Vec<(String, u64)>,
    wake_tx: mpsc::Sender<()>,
    done: Arc<AtomicBool>,
}

impl PGlite {
    pub async fn live_query<F>(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
        callback: F,
    ) -> Result<LiveQuery, Error>
    where
        F: Fn(&[Row]) + Send + Sync + 'static,
    {
        let formatted = self.format_literals(sql, params).await?;
        let id = format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let view_name = format!("live_query_{id}_view");

        let tx = self.transaction().await?;
        tx.exec(&format!("CREATE VIEW \"{view_name}\" AS {formatted}"))
            .await?;
        let tables = tx
            .query(tables::WATCHED_TABLES_SQL, &[&view_name.as_str()])
            .await?;

        let mut watched = Vec::new();
        for table in &tables {
            let table_name: &str = table.get(0)?;
            let schema_name: &str = table.get(1)?;
            let table_oid: i32 = table.get(2)?;
            let schema_oid: i32 = table.get(3)?;
            watched.push((
                schema_oid as u32,
                table_oid as u32,
                schema_name.to_string(),
                table_name.to_string(),
            ));
        }
        for (schema_oid, table_oid, schema_name, table_name) in &watched {
            let fresh = {
                let mut triggers = self.live_triggers().lock().unwrap();
                let count = triggers.entry((*schema_oid, *table_oid)).or_insert(0);
                *count += 1;
                *count == 1
            };
            if fresh {
                tx.exec(&LiveQuery::trigger_ddl(
                    *schema_oid,
                    *table_oid,
                    schema_name,
                    table_name,
                ))
                .await?;
            }
        }
        tx.commit().await?;

        let (wake_tx, wake_rx) = mpsc::channel::<()>();
        let done = Arc::new(AtomicBool::new(false));

        let mut tokens = Vec::new();
        for (schema_oid, table_oid, _, _) in &watched {
            let channel = LiveQuery::channel_name(*schema_oid, *table_oid);
            let tx_clone = wake_tx.clone();
            let done_clone = done.clone();
            let token = self
                .listen(&channel, move |_| {
                    if !done_clone.load(Ordering::SeqCst) {
                        let _ = tx_clone.send(());
                    }
                })
                .await?;
            tokens.push((channel, token));
        }

        let select = format!("SELECT * FROM \"{view_name}\"");
        let rows = self.query(&select, &[]).await?;
        callback(&rows);

        let refresher_db = self.clone();
        let refresher_done = done.clone();
        std::thread::Builder::new()
            .name("pglite-live-refresh".into())
            .spawn(move || {
                while wake_rx.recv().is_ok() {
                    while wake_rx.try_recv().is_ok() {}
                    if refresher_done.load(Ordering::SeqCst) {
                        return;
                    }
                    match futures::executor::block_on(refresher_db.query(&select, &[])) {
                        Ok(rows) => callback(&rows),
                        Err(_) => return,
                    }
                }
            })
            .map_err(Error::Io)?;

        Ok(LiveQuery {
            db: self.clone(),
            view_name,
            watched,
            tokens,
            wake_tx,
            done,
        })
    }
}

impl PGlite {
    pub(crate) async fn sweep_live_views(&self) -> Result<(), Error> {
        let rows = self
            .query(
                "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind = 'v' AND n.nspname = 'public' AND c.relname LIKE 'live\\_query\\_%\\_view'",
                &[],
            )
            .await?;
        for row in &rows {
            let name: &str = row.get(0)?;
            self.exec(&format!("DROP VIEW IF EXISTS \"{name}\""))
                .await?;
        }
        Ok(())
    }
}

impl LiveQuery {
    pub fn refresh(&self) {
        let _ = self.wake_tx.send(());
    }

    pub async fn unsubscribe(self) -> Result<(), Error> {
        self.done.store(true, Ordering::SeqCst);
        let _ = self.wake_tx.send(());

        for (channel, token) in &self.tokens {
            self.db.unlisten_token(channel, *token).await?;
        }

        for (schema_oid, table_oid, schema_name, table_name) in &self.watched {
            let teardown = {
                let mut triggers = self.db.live_triggers().lock().unwrap();
                match triggers.get_mut(&(*schema_oid, *table_oid)) {
                    Some(count) if *count > 1 => {
                        *count -= 1;
                        false
                    }
                    Some(_) => {
                        triggers.remove(&(*schema_oid, *table_oid));
                        true
                    }
                    None => false,
                }
            };
            if teardown {
                self.db
                    .exec(&format!(
                        "DROP TRIGGER IF EXISTS \"_notify_trigger_{schema_oid}_{table_oid}\" ON \"{schema_name}\".\"{table_name}\";
DROP FUNCTION IF EXISTS \"_notify_{schema_oid}_{table_oid}\"()"
                    ))
                    .await?;
            }
        }

        self.db
            .exec(&format!("DROP VIEW IF EXISTS \"{}\"", self.view_name))
            .await
    }
}
