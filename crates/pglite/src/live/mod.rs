mod tables;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use crate::db::PGlite;
use crate::error::Error;
use crate::row::Row;

pub struct LiveQuery {
    db: PGlite,
    view_name: String,
    wake_tx: mpsc::Sender<()>,
    done: Arc<AtomicBool>,
}

impl PGlite {
    pub async fn live_query<F>(&self, sql: &str, callback: F) -> Result<LiveQuery, Error>
    where
        F: Fn(&[Row]) + Send + Sync + 'static,
    {
        let id = format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let view_name = format!("live_query_{id}_view");

        let tx = self.transaction().await?;
        tx.exec(&format!("CREATE TEMP VIEW \"{view_name}\" AS {sql}"))
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
            let fresh = self
                .live_triggers()
                .lock()
                .unwrap()
                .insert((*schema_oid, *table_oid));
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

        for (schema_oid, table_oid, _, _) in &watched {
            let channel = LiveQuery::channel_name(*schema_oid, *table_oid);
            let tx_clone = wake_tx.clone();
            let done_clone = done.clone();
            self.listen(&channel, move |_| {
                if !done_clone.load(Ordering::SeqCst) {
                    let _ = tx_clone.send(());
                }
            })
            .await?;
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
            wake_tx,
            done,
        })
    }
}

impl LiveQuery {
    pub fn refresh(&self) {
        let _ = self.wake_tx.send(());
    }

    pub async fn unsubscribe(self) -> Result<(), Error> {
        self.done.store(true, Ordering::SeqCst);
        let _ = self.wake_tx.send(());
        self.db
            .exec(&format!("DROP VIEW IF EXISTS \"{}\"", self.view_name))
            .await
    }
}
