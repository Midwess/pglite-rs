use futures::lock::MutexGuard;
use postgres_types::ToSql;

use crate::db::{PGlite, Via};
use crate::error::Error;
use crate::row::Row;

pub struct Transaction<'a> {
    db: &'a PGlite,
    _guard: Option<MutexGuard<'a, ()>>,
    #[cfg(feature = "multiple-process")]
    pin: Option<crate::multiple_process::pool::PinnedConn>,
    done: bool,
}

impl<'a> Transaction<'a> {
    pub(crate) async fn begin(db: &'a PGlite) -> Result<Transaction<'a>, Error> {
        #[cfg(feature = "multiple-process")]
        if db.backend().is_multi_process() {
            let pin = match db.backend() {
                crate::db::Backend::MultiProcess(pool) => pool.checkout().await?,
                _ => unreachable!(),
            };
            let mut tx = Transaction {
                db,
                _guard: None,
                pin: Some(pin),
                done: false,
            };
            tx.exec_inner("BEGIN").await?;
            return Ok(tx);
        }

        let guard = db.lock_for_transaction().await;
        db.exec_unlocked("BEGIN").await?;
        Ok(Transaction {
            db,
            _guard: Some(guard),
            #[cfg(feature = "multiple-process")]
            pin: None,
            done: false,
        })
    }

    fn via(&self) -> Via<'_> {
        #[cfg(feature = "multiple-process")]
        if let Some(pin) = &self.pin {
            return Via::Pin(pin);
        }
        Via::backend()
    }

    #[cfg(feature = "multiple-process")]
    async fn exec_inner(&mut self, sql: &str) -> Result<(), Error> {
        self.db.exec_via(self.via(), sql).await
    }

    pub async fn exec(&self, sql: &str) -> Result<(), Error> {
        self.db.exec_via(self.via(), sql).await
    }

    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        self.db.query_via(self.via(), sql, params).await
    }

    pub async fn commit(mut self) -> Result<(), Error> {
        self.done = true;
        self.db.exec_via(self.via(), "COMMIT").await
    }

    pub async fn rollback(mut self) -> Result<(), Error> {
        self.done = true;
        self.db.exec_via(self.via(), "ROLLBACK").await
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.done {
            #[cfg(feature = "multiple-process")]
            if let Some(pin) = &self.pin {
                if let Some(wire) = PGlite::rollback_wire() {
                    pin.fire_and_forget(wire);
                }
                return;
            }
            self.db.rollback_fire_and_forget();
        }
    }
}
