use futures::lock::MutexGuard;
use postgres_types::ToSql;

use crate::db::PGlite;
use crate::error::Error;
use crate::row::Row;

pub struct Transaction<'a> {
    db: &'a PGlite,
    _guard: MutexGuard<'a, ()>,
    done: bool,
}

impl<'a> Transaction<'a> {
    pub(crate) async fn begin(db: &'a PGlite) -> Result<Transaction<'a>, Error> {
        let guard = db.lock_for_transaction().await;
        db.exec_unlocked("BEGIN").await?;
        Ok(Transaction { db, _guard: guard, done: false })
    }

    pub async fn exec(&self, sql: &str) -> Result<(), Error> {
        self.db.exec_unlocked(sql).await
    }

    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        self.db.query_unlocked(sql, params).await
    }

    pub async fn commit(mut self) -> Result<(), Error> {
        self.done = true;
        self.db.exec_unlocked("COMMIT").await
    }

    pub async fn rollback(mut self) -> Result<(), Error> {
        self.done = true;
        self.db.exec_unlocked("ROLLBACK").await
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.db.rollback_fire_and_forget();
        }
    }
}
