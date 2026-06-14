use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use pglite::{MultiProcessOptions, PGlite, Replica, ReplicaConfig, SslMode};
use tokio::sync::OnceCell;

use crate::cache::QueryCache;
use crate::cdc::CdcBridge;
use crate::classify::ReadClassifier;
use crate::error::CacheError;
use crate::live::LiveHub;
use crate::version::VersionIndex;

static INSTANCE: OnceCell<Di> = OnceCell::const_new();

pub struct UpstreamConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub publication: String,
    pub slot: String,
    pub sslmode: String,
}

pub struct ServerConfig {
    pub bind_addr: String,
    pub data_dir: PathBuf,
    pub max_connections: usize,
    pub cache_size_bytes: u64,
    pub upstream: UpstreamConfig,
}

pub struct Di {
    db: PGlite,
    replica: Replica,
    versions: VersionIndex,
    cache: QueryCache,
    classifier: ReadClassifier,
    live: LiveHub,
    tables: HashSet<String>,
    bind_addr: String,
    #[allow(dead_code)]
    cdc: CdcBridge,
}

impl Di {
    pub async fn init(config: ServerConfig) -> Result<(), CacheError> {
        crate::setup::preflight(&config.upstream).await?;

        let options = MultiProcessOptions {
            max_connections: config.max_connections,
            ..Default::default()
        };
        let db = PGlite::open_multi_process(&config.data_dir, options).await?;

        let replica_config = ReplicaConfig {
            host: config.upstream.host.clone(),
            port: config.upstream.port,
            user: config.upstream.user.clone(),
            password: config.upstream.password.clone(),
            database: config.upstream.database.clone(),
            publication: config.upstream.publication.clone(),
            slot_name: config.upstream.slot.clone(),
            sslmode: parse_sslmode(&config.upstream.sslmode),
            ..Default::default()
        };
        let replica = Replica::start(db.clone(), replica_config).await?;

        let (replicated, pk, full) = scan_schema(&db).await?;
        let versions = VersionIndex::new(pk.clone(), full);
        let cdc = CdcBridge::start(&replica, versions.clone())?;
        let live = LiveHub::start(&cdc, db.clone(), Arc::new(pk));
        let cache = QueryCache::new(config.cache_size_bytes);
        let classifier = ReadClassifier::new(replicated.clone());

        let di = Di {
            db,
            replica,
            versions,
            cache,
            classifier,
            live,
            tables: replicated,
            bind_addr: config.bind_addr,
            cdc,
        };

        INSTANCE
            .set(di)
            .map_err(|_| CacheError::Config("dependencies already initialized".to_string()))
    }

    pub fn instance() -> &'static Di {
        INSTANCE.get().expect("dependencies not initialized")
    }

    pub async fn shutdown(&self) {
        self.replica.stop();
        self.cdc.stop();
        let _ = self.db.shutdown().await;
    }

    pub fn db(&self) -> &PGlite {
        &self.db
    }

    pub fn replica(&self) -> &Replica {
        &self.replica
    }

    pub fn versions(&self) -> &VersionIndex {
        &self.versions
    }

    pub fn cache(&self) -> &QueryCache {
        &self.cache
    }

    pub fn classifier(&self) -> &ReadClassifier {
        &self.classifier
    }

    pub fn live(&self) -> &LiveHub {
        &self.live
    }

    pub fn tables(&self) -> &HashSet<String> {
        &self.tables
    }

    pub fn bind_addr(&self) -> &str {
        &self.bind_addr
    }
}

async fn scan_schema(
    db: &PGlite,
) -> Result<(HashSet<String>, HashMap<String, String>, HashSet<String>), CacheError> {
    let table_rows = db
        .query(
            "select tablename from pg_tables \
             where schemaname not in ('pg_catalog', 'information_schema')",
            &[],
        )
        .await?;
    let mut tables = HashSet::new();
    for row in table_rows {
        let name: String = row.get(0)?;
        if name != "_pglite_replica" {
            tables.insert(name);
        }
    }

    let pk_rows = db
        .query(
            "select tc.table_name, kcu.column_name \
             from information_schema.table_constraints tc \
             join information_schema.key_column_usage kcu \
               on kcu.constraint_name = tc.constraint_name \
               and kcu.table_schema = tc.table_schema \
             where tc.constraint_type = 'PRIMARY KEY' \
               and tc.table_schema not in ('pg_catalog', 'information_schema')",
            &[],
        )
        .await?;
    let mut pk_columns: HashMap<String, Vec<String>> = HashMap::new();
    for row in pk_rows {
        let table: String = row.get(0)?;
        let column: String = row.get(1)?;
        pk_columns.entry(table).or_default().push(column);
    }
    let pk = pk_columns
        .into_iter()
        .filter(|(_, columns)| columns.len() == 1)
        .map(|(table, mut columns)| (table, columns.remove(0)))
        .collect();

    let full_rows = db
        .query(
            "select c.relname from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where c.relkind = 'r' and c.relreplident = 'f' \
               and n.nspname not in ('pg_catalog', 'information_schema')",
            &[],
        )
        .await?;
    let mut full = HashSet::new();
    for row in full_rows {
        let name: String = row.get(0)?;
        full.insert(name);
    }

    Ok((tables, pk, full))
}

fn parse_sslmode(value: &str) -> SslMode {
    match value.to_ascii_lowercase().as_str() {
        "prefer" => SslMode::Prefer,
        "require" => SslMode::Require,
        "verify-full" | "verify_full" | "verifyfull" => SslMode::VerifyFull,
        _ => SslMode::Disable,
    }
}
