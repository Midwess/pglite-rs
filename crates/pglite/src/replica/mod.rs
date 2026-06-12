mod backfill;
mod meta;
mod pgoutput;
mod wire;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use futures::executor::block_on;

use crate::db::PGlite;
use crate::error::Error;
pub use meta::Lsn;
use meta::ReplicaState;
use pgoutput::{CellValue, PgOutputMsg, RelColumn, TupleData};
use wire::{ReplConn, ReplMsg};

pub(crate) fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

pub(crate) fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn cell_lit(cell: &CellValue) -> String {
    match cell {
        CellValue::Null => "NULL".to_string(),
        CellValue::Text(s) => lit(s),
        CellValue::UnchangedToast => "NULL".to_string(),
    }
}

fn cell_value(cell: &CellValue) -> Option<String> {
    match cell {
        CellValue::Null | CellValue::UnchangedToast => None,
        CellValue::Text(s) => Some(s.clone()),
    }
}

#[derive(Clone, Debug)]
pub struct ReplicaConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub publication: String,
    pub slot_name: String,
    pub application_name: String,
    pub read_timeout: Duration,
    pub status_interval: Duration,
}

impl Default for ReplicaConfig {
    fn default() -> ReplicaConfig {
        ReplicaConfig {
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            password: String::new(),
            database: "postgres".into(),
            publication: String::new(),
            slot_name: String::new(),
            application_name: "pglite-replica".into(),
            read_timeout: Duration::from_secs(5),
            status_interval: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommittedTransaction {
    pub xid: u32,
    pub commit_lsn: Lsn,
    pub end_lsn: Lsn,
    pub commit_ts: i64,
    pub changes: Vec<RowChange>,
}

#[derive(Debug, Clone)]
pub enum RowChange {
    Insert {
        schema: String,
        table: String,
        row: Vec<(String, Option<String>)>,
    },
    Update {
        schema: String,
        table: String,
        key: Vec<(String, Option<String>)>,
        row: Vec<(String, Option<String>)>,
    },
    Delete {
        schema: String,
        table: String,
        key: Vec<(String, Option<String>)>,
    },
    Truncate {
        schema: String,
        table: String,
    },
}

type ColPairs = Vec<(String, Option<String>)>;

struct Rel {
    schema: String,
    name: String,
    columns: Vec<RelColumn>,
}

impl Rel {
    fn target(&self) -> String {
        format!("{}.{}", ident(&self.schema), ident(&self.name))
    }

    fn check_arity(&self, tuple: &TupleData) -> Result<(), Error> {
        if tuple.0.len() != self.columns.len() {
            return Err(Error::Protocol(format!(
                "tuple arity {} does not match relation {}.{} arity {}",
                tuple.0.len(),
                self.schema,
                self.name,
                self.columns.len()
            )));
        }
        Ok(())
    }

    fn insert_sql(&self, new: &TupleData) -> Result<(String, RowChange), Error> {
        self.check_arity(new)?;
        let mut names = Vec::with_capacity(self.columns.len());
        let mut values = Vec::with_capacity(self.columns.len());
        let mut row = Vec::with_capacity(self.columns.len());
        for (col, cell) in self.columns.iter().zip(&new.0) {
            if matches!(cell, CellValue::UnchangedToast) {
                return Err(Error::Protocol(format!(
                    "unchanged-toast cell in insert into {}.{}",
                    self.schema, self.name
                )));
            }
            names.push(ident(&col.name));
            values.push(cell_lit(cell));
            row.push((col.name.clone(), cell_value(cell)));
        }
        let stmt = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.target(),
            names.join(", "),
            values.join(", ")
        );
        Ok((
            stmt,
            RowChange::Insert {
                schema: self.schema.clone(),
                table: self.name.clone(),
                row,
            },
        ))
    }

    fn where_clause(
        &self,
        key: Option<&TupleData>,
        old: Option<&TupleData>,
        new: Option<&TupleData>,
    ) -> Result<(String, ColPairs), Error> {
        if let Some(old) = old {
            self.check_arity(old)?;
            let mut conds = Vec::new();
            let mut pairs = Vec::new();
            for (col, cell) in self.columns.iter().zip(&old.0) {
                if matches!(cell, CellValue::UnchangedToast) {
                    continue;
                }
                conds.push(format!(
                    "{} IS NOT DISTINCT FROM {}",
                    ident(&col.name),
                    cell_lit(cell)
                ));
                pairs.push((col.name.clone(), cell_value(cell)));
            }
            if conds.is_empty() {
                return Err(Error::Protocol(format!(
                    "empty old tuple for {}.{}",
                    self.schema, self.name
                )));
            }
            return Ok((conds.join(" AND "), pairs));
        }

        let source = key.or(new).ok_or_else(|| {
            Error::ReplicaHalted(format!(
                "no key available for change on {}.{}",
                self.schema, self.name
            ))
        })?;
        self.check_arity(source)?;
        let mut conds = Vec::new();
        let mut pairs = Vec::new();
        for (col, cell) in self.columns.iter().zip(&source.0) {
            if col.flags & 1 == 0 {
                continue;
            }
            match cell {
                CellValue::UnchangedToast => {
                    return Err(Error::Protocol(format!(
                        "unchanged-toast key cell on {}.{}",
                        self.schema, self.name
                    )))
                }
                CellValue::Null => conds.push(format!("{} IS NULL", ident(&col.name))),
                CellValue::Text(s) => conds.push(format!("{} = {}", ident(&col.name), lit(s))),
            }
            pairs.push((col.name.clone(), cell_value(cell)));
        }
        if conds.is_empty() {
            return Err(Error::ReplicaHalted(format!(
                "relation {}.{} has no replica identity key",
                self.schema, self.name
            )));
        }
        Ok((conds.join(" AND "), pairs))
    }

    fn update_sql(
        &self,
        key: Option<&TupleData>,
        old: Option<&TupleData>,
        new: &TupleData,
    ) -> Result<(String, RowChange), Error> {
        self.check_arity(new)?;
        let (where_sql, key_pairs) = self.where_clause(key, old, Some(new))?;
        let mut sets = Vec::new();
        let mut row = Vec::new();
        for (col, cell) in self.columns.iter().zip(&new.0) {
            if matches!(cell, CellValue::UnchangedToast) {
                continue;
            }
            sets.push(format!("{} = {}", ident(&col.name), cell_lit(cell)));
            row.push((col.name.clone(), cell_value(cell)));
        }
        if sets.is_empty() {
            return Err(Error::Protocol(format!(
                "update with no assignable columns on {}.{}",
                self.schema, self.name
            )));
        }
        let stmt = format!(
            "UPDATE {} SET {} WHERE {}",
            self.target(),
            sets.join(", "),
            where_sql
        );
        Ok((
            stmt,
            RowChange::Update {
                schema: self.schema.clone(),
                table: self.name.clone(),
                key: key_pairs,
                row,
            },
        ))
    }

    fn delete_sql(
        &self,
        key: Option<&TupleData>,
        old: Option<&TupleData>,
    ) -> Result<(String, RowChange), Error> {
        let (where_sql, key_pairs) = self.where_clause(key, old, None)?;
        let stmt = format!("DELETE FROM {} WHERE {}", self.target(), where_sql);
        Ok((
            stmt,
            RowChange::Delete {
                schema: self.schema.clone(),
                table: self.name.clone(),
                key: key_pairs,
            },
        ))
    }
}

struct TxnBuf {
    xid: u32,
    commit_ts: i64,
    stmts: Vec<String>,
    changes: Vec<RowChange>,
}

#[derive(Clone)]
pub struct Replica {
    db: PGlite,
    config: Arc<ReplicaConfig>,
    done: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    halted: Arc<AtomicBool>,
    halt_reason: Arc<std::sync::Mutex<Option<String>>>,
    watermark: Arc<AtomicU64>,
    subscribers: Arc<std::sync::Mutex<Vec<mpsc::Sender<Arc<CommittedTransaction>>>>>,
}

impl Replica {
    pub async fn start(db: PGlite, config: ReplicaConfig) -> Result<Replica, Error> {
        for (field, value) in [
            ("host", &config.host),
            ("user", &config.user),
            ("database", &config.database),
            ("publication", &config.publication),
            ("slot_name", &config.slot_name),
        ] {
            if value.is_empty() {
                return Err(Error::ReplicaConfig(format!("{field} must not be empty")));
            }
        }

        meta::ensure_meta_table(&db).await?;
        let state = meta::load_state(&db).await?;

        let replica = Replica {
            db,
            config: Arc::new(config),
            done: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            halted: Arc::new(AtomicBool::new(false)),
            halt_reason: Arc::new(std::sync::Mutex::new(None)),
            watermark: Arc::new(AtomicU64::new(0)),
            subscribers: Arc::new(std::sync::Mutex::new(Vec::new())),
        };

        let (boot_tx, boot_rx) = oneshot::channel();
        let runner = replica.clone();
        std::thread::Builder::new()
            .name("pglite-replica".into())
            .spawn(move || runner.thread_main(state, boot_tx))
            .map_err(Error::Io)?;

        match boot_rx.await {
            Ok(Ok(())) => Ok(replica),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Upstream("replica thread died during startup".into())),
        }
    }

    pub fn stop(&self) {
        self.done.store(true, Ordering::SeqCst);
    }

    pub fn watermark(&self) -> Lsn {
        Lsn(self.watermark.load(Ordering::SeqCst))
    }

    pub fn is_halted(&self) -> bool {
        self.halted.load(Ordering::SeqCst)
    }

    pub fn halt_reason(&self) -> Option<String> {
        self.halt_reason.lock().unwrap().clone()
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> mpsc::Receiver<Arc<CommittedTransaction>> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    fn thread_main(self, state: Option<ReplicaState>, boot_tx: oneshot::Sender<Result<(), Error>>) {
        match self.prepare(state) {
            Ok((mut conn, start, fingerprint)) => {
                self.watermark.store(start.0, Ordering::SeqCst);
                let _ = boot_tx.send(Ok(()));
                if let Err(e) = self.stream_loop(&mut conn, start, &fingerprint) {
                    self.halt(e);
                }
                conn.terminate();
            }
            Err(e) => {
                let _ = boot_tx.send(Err(e));
            }
        }
        self.stopped.store(true, Ordering::SeqCst);
    }

    fn prepare(&self, state: Option<ReplicaState>) -> Result<(ReplConn, Lsn, String), Error> {
        let conn = ReplConn::connect_and_auth(&self.config, true)?;
        match state {
            Some(s) => {
                if s.slot_name != self.config.slot_name || s.publication != self.config.publication
                {
                    return Err(Error::ReplicaConfig(format!(
                        "replica meta mismatch: stored slot/publication {}/{} differ from configured {}/{}",
                        s.slot_name, s.publication, self.config.slot_name, self.config.publication
                    )));
                }
                Ok((conn, s.watermark, s.fingerprint))
            }
            None => self.first_run(conn),
        }
    }

    fn first_run(&self, mut conn: ReplConn) -> Result<(ReplConn, Lsn, String), Error> {
        let create = format!(
            "CREATE_REPLICATION_SLOT \"{}\" LOGICAL pgoutput EXPORT_SNAPSHOT",
            self.config.slot_name.replace('"', "\"\"")
        );
        let rows = match conn.simple_query(&create) {
            Ok(rows) => rows,
            Err(Error::Database { ref sqlstate, .. }) if sqlstate == "42710" => {
                conn.simple_query(&format!(
                    "DROP_REPLICATION_SLOT \"{}\" WAIT",
                    self.config.slot_name.replace('"', "\"\"")
                ))?;
                conn.simple_query(&create)?
            }
            Err(e) => return Err(e),
        };
        let row = rows
            .first()
            .ok_or_else(|| Error::Protocol("CREATE_REPLICATION_SLOT returned no row".into()))?;
        let consistent_point = Lsn::from_pg_str(
            row.get(1)
                .and_then(|v| v.as_deref())
                .ok_or_else(|| Error::Protocol("missing consistent_point".into()))?,
        )?;
        let snapshot = row
            .get(2)
            .and_then(|v| v.as_deref())
            .ok_or_else(|| Error::Protocol("missing exported snapshot name".into()))?
            .to_string();

        let mut snap = ReplConn::connect_and_auth(&self.config, false)?;
        snap.simple_query("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
        snap.simple_query(&format!("SET TRANSACTION SNAPSHOT {}", lit(&snapshot)))?;
        let tables = backfill::introspect(&mut snap, &self.config.publication)?;
        let fingerprint = backfill::fingerprint(&tables);
        backfill::bootstrap_schema(&self.db, &tables)?;
        backfill::copy_tables(&mut snap, &self.db, &tables)?;
        snap.simple_query("COMMIT")?;
        snap.terminate();

        block_on(meta::init_state(
            &self.db,
            &self.config.slot_name,
            &self.config.publication,
            consistent_point,
            &fingerprint,
        ))?;
        Ok((conn, consistent_point, fingerprint))
    }

    fn stream_loop(&self, conn: &mut ReplConn, start: Lsn, fingerprint: &str) -> Result<(), Error> {
        conn.start_replication(&self.config.slot_name, start, &self.config.publication)?;
        conn.set_stream_timeout(self.config.read_timeout)?;
        conn.send_status(self.watermark(), false)?;
        let mut last_status = Instant::now();

        let mut expected: HashMap<(String, String), String> = HashMap::new();
        for line in fingerprint.lines() {
            let mut parts = line.splitn(3, '|');
            if let (Some(schema), Some(table)) = (parts.next(), parts.next()) {
                expected.insert((schema.to_string(), table.to_string()), line.to_string());
            }
        }

        let mut rels: HashMap<u32, Rel> = HashMap::new();
        let mut txn: Option<TxnBuf> = None;

        loop {
            if self.done.load(Ordering::SeqCst) {
                return Ok(());
            }
            if last_status.elapsed() >= self.config.status_interval {
                conn.send_status(self.watermark(), false)?;
                last_status = Instant::now();
            }

            let Some(msg) = conn.read_copy_message()? else {
                continue;
            };
            match msg {
                ReplMsg::Keepalive { reply_requested } => {
                    if reply_requested {
                        conn.send_status(self.watermark(), false)?;
                        last_status = Instant::now();
                    }
                }
                ReplMsg::CopyDone => {
                    return Err(Error::Upstream(
                        "upstream ended the replication stream".into(),
                    ))
                }
                ReplMsg::XLogData { data } => match pgoutput::decode(&data)? {
                    PgOutputMsg::Begin { commit_ts, xid, .. } => {
                        txn = Some(TxnBuf {
                            xid,
                            commit_ts,
                            stmts: Vec::new(),
                            changes: Vec::new(),
                        });
                    }
                    PgOutputMsg::Relation {
                        rel_id,
                        namespace,
                        name,
                        columns,
                        ..
                    } => {
                        let line = backfill::fingerprint_line(
                            &namespace,
                            &name,
                            columns.iter().map(|c| (c.name.as_str(), c.type_oid)),
                        );
                        match expected.get(&(namespace.clone(), name.clone())) {
                            Some(want) if *want == line => {}
                            Some(want) => {
                                return Err(Error::ReplicaHalted(format!(
                                    "schema drift on {namespace}.{name}: expected [{want}], got [{line}]"
                                )))
                            }
                            None => {
                                return Err(Error::ReplicaHalted(format!(
                                    "relation {namespace}.{name} is not part of the replicated fingerprint"
                                )))
                            }
                        }
                        rels.insert(
                            rel_id,
                            Rel {
                                schema: namespace,
                                name,
                                columns,
                            },
                        );
                    }
                    PgOutputMsg::Insert { rel_id, new } => {
                        let rel = Self::rel(&rels, rel_id)?;
                        let buf = Self::open_txn(&mut txn)?;
                        let (stmt, change) = rel.insert_sql(&new)?;
                        buf.stmts.push(stmt);
                        buf.changes.push(change);
                    }
                    PgOutputMsg::Update {
                        rel_id,
                        key,
                        old,
                        new,
                    } => {
                        let rel = Self::rel(&rels, rel_id)?;
                        let buf = Self::open_txn(&mut txn)?;
                        let (stmt, change) = rel.update_sql(key.as_ref(), old.as_ref(), &new)?;
                        buf.stmts.push(stmt);
                        buf.changes.push(change);
                    }
                    PgOutputMsg::Delete { rel_id, key, old } => {
                        let rel = Self::rel(&rels, rel_id)?;
                        let buf = Self::open_txn(&mut txn)?;
                        let (stmt, change) = rel.delete_sql(key.as_ref(), old.as_ref())?;
                        buf.stmts.push(stmt);
                        buf.changes.push(change);
                    }
                    PgOutputMsg::Truncate { rel_ids } => {
                        let buf = Self::open_txn(&mut txn)?;
                        let mut targets = Vec::with_capacity(rel_ids.len());
                        for rel_id in rel_ids {
                            let rel = Self::rel(&rels, rel_id)?;
                            targets.push(rel.target());
                            buf.changes.push(RowChange::Truncate {
                                schema: rel.schema.clone(),
                                table: rel.name.clone(),
                            });
                        }
                        buf.stmts.push(format!("TRUNCATE {}", targets.join(", ")));
                    }
                    PgOutputMsg::Commit {
                        commit_lsn,
                        end_lsn,
                        commit_ts,
                    } => {
                        let buf = txn.take().ok_or_else(|| {
                            Error::Protocol("commit without begin on replication stream".into())
                        })?;
                        let end = Lsn(end_lsn);
                        if end <= self.watermark() {
                            continue;
                        }
                        self.apply(&buf.stmts, end)?;
                        self.watermark.store(end.0, Ordering::SeqCst);
                        if !buf.changes.is_empty() {
                            self.broadcast(CommittedTransaction {
                                xid: buf.xid,
                                commit_lsn: Lsn(commit_lsn),
                                end_lsn: end,
                                commit_ts: if buf.commit_ts != 0 {
                                    buf.commit_ts
                                } else {
                                    commit_ts
                                },
                                changes: buf.changes,
                            });
                        }
                    }
                    PgOutputMsg::Other => {}
                },
            }
        }
    }

    fn rel(rels: &HashMap<u32, Rel>, rel_id: u32) -> Result<&Rel, Error> {
        rels.get(&rel_id)
            .ok_or_else(|| Error::Protocol(format!("change for unknown relation id {rel_id}")))
    }

    fn open_txn(txn: &mut Option<TxnBuf>) -> Result<&mut TxnBuf, Error> {
        txn.as_mut()
            .ok_or_else(|| Error::Protocol("change outside of transaction".into()))
    }

    fn apply(&self, stmts: &[String], end: Lsn) -> Result<(), Error> {
        block_on(async {
            let tx = self.db.transaction().await?;
            for stmt in stmts {
                tx.exec(stmt).await?;
            }
            tx.exec(&format!(
                "UPDATE _pglite_replica SET watermark_lsn = {}, updated_at = now() WHERE id = 1",
                lit(&end.to_pg_str())
            ))
            .await?;
            tx.commit().await
        })
    }

    fn broadcast(&self, committed: CommittedTransaction) {
        let committed = Arc::new(committed);
        self.subscribers
            .lock()
            .unwrap()
            .retain(|tx| tx.send(committed.clone()).is_ok());
    }

    fn halt(&self, e: Error) {
        self.done.store(true, Ordering::SeqCst);
        if matches!(e, Error::Closed) {
            return;
        }
        *self.halt_reason.lock().unwrap() = Some(e.to_string());
        self.halted.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::pgoutput::{CellValue, RelColumn, TupleData};
    use super::*;

    fn rel() -> Rel {
        Rel {
            schema: "public".into(),
            name: "todos".into(),
            columns: vec![
                RelColumn {
                    flags: 1,
                    name: "id".into(),
                    type_oid: 23,
                    type_modifier: -1,
                },
                RelColumn {
                    flags: 0,
                    name: "title".into(),
                    type_oid: 25,
                    type_modifier: -1,
                },
            ],
        }
    }

    #[test]
    fn ident_and_lit_escape() {
        assert_eq!(ident("plain"), "\"plain\"");
        assert_eq!(ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(lit("o'brien"), "'o''brien'");
        assert_eq!(lit("plain"), "'plain'");
    }

    #[test]
    fn insert_sql_builds() {
        let (stmt, _) = rel()
            .insert_sql(&TupleData(vec![
                CellValue::Text("1".into()),
                CellValue::Text("it's done".into()),
            ]))
            .unwrap();
        assert_eq!(
            stmt,
            "INSERT INTO \"public\".\"todos\" (\"id\", \"title\") VALUES ('1', 'it''s done')"
        );
    }

    #[test]
    fn insert_sql_null() {
        let (stmt, _) = rel()
            .insert_sql(&TupleData(vec![
                CellValue::Text("1".into()),
                CellValue::Null,
            ]))
            .unwrap();
        assert_eq!(
            stmt,
            "INSERT INTO \"public\".\"todos\" (\"id\", \"title\") VALUES ('1', NULL)"
        );
    }

    #[test]
    fn update_sql_key_from_new_tuple() {
        let (stmt, _) = rel()
            .update_sql(
                None,
                None,
                &TupleData(vec![
                    CellValue::Text("1".into()),
                    CellValue::Text("new".into()),
                ]),
            )
            .unwrap();
        assert_eq!(
            stmt,
            "UPDATE \"public\".\"todos\" SET \"id\" = '1', \"title\" = 'new' WHERE \"id\" = '1'"
        );
    }

    #[test]
    fn update_sql_skips_unchanged_toast() {
        let (stmt, _) = rel()
            .update_sql(
                None,
                None,
                &TupleData(vec![CellValue::Text("1".into()), CellValue::UnchangedToast]),
            )
            .unwrap();
        assert_eq!(
            stmt,
            "UPDATE \"public\".\"todos\" SET \"id\" = '1' WHERE \"id\" = '1'"
        );
    }

    #[test]
    fn update_sql_uses_key_tuple() {
        let (stmt, _) = rel()
            .update_sql(
                Some(&TupleData(vec![
                    CellValue::Text("1".into()),
                    CellValue::Null,
                ])),
                None,
                &TupleData(vec![
                    CellValue::Text("2".into()),
                    CellValue::Text("moved".into()),
                ]),
            )
            .unwrap();
        assert_eq!(
            stmt,
            "UPDATE \"public\".\"todos\" SET \"id\" = '2', \"title\" = 'moved' WHERE \"id\" = '1'"
        );
    }

    #[test]
    fn delete_sql_uses_full_old_tuple() {
        let (stmt, _) = rel()
            .delete_sql(
                None,
                Some(&TupleData(vec![
                    CellValue::Text("1".into()),
                    CellValue::Null,
                ])),
            )
            .unwrap();
        assert_eq!(
            stmt,
            "DELETE FROM \"public\".\"todos\" WHERE \"id\" IS NOT DISTINCT FROM '1' AND \"title\" IS NOT DISTINCT FROM NULL"
        );
    }

    #[test]
    fn delete_sql_key_tuple() {
        let (stmt, change) = rel()
            .delete_sql(
                Some(&TupleData(vec![
                    CellValue::Text("9".into()),
                    CellValue::Null,
                ])),
                None,
            )
            .unwrap();
        assert_eq!(stmt, "DELETE FROM \"public\".\"todos\" WHERE \"id\" = '9'");
        match change {
            RowChange::Delete { key, .. } => {
                assert_eq!(key, vec![("id".to_string(), Some("9".to_string()))])
            }
            other => panic!("unexpected change: {other:?}"),
        }
    }

    #[test]
    fn no_identity_halts() {
        let no_key = Rel {
            schema: "public".into(),
            name: "nokey".into(),
            columns: vec![RelColumn {
                flags: 0,
                name: "v".into(),
                type_oid: 25,
                type_modifier: -1,
            }],
        };
        let err = no_key
            .delete_sql(None, None)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("no key available"), "{err}");
        let err = no_key
            .update_sql(None, None, &TupleData(vec![CellValue::Text("x".into())]))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("no replica identity"), "{err}");
    }

    #[test]
    fn arity_mismatch_fails() {
        assert!(rel()
            .insert_sql(&TupleData(vec![CellValue::Text("1".into())]))
            .is_err());
    }

    #[test]
    fn fingerprint_line_matches_between_sources() {
        let from_introspect = backfill::fingerprint_line(
            "public",
            "todos",
            [("id", 23u32), ("title", 25u32)].into_iter(),
        );
        let r = rel();
        let from_relation = backfill::fingerprint_line(
            &r.schema,
            &r.name,
            r.columns.iter().map(|c| (c.name.as_str(), c.type_oid)),
        );
        assert_eq!(from_introspect, from_relation);
        assert_eq!(from_introspect, "public|todos|id:23|title:25");
    }
}
