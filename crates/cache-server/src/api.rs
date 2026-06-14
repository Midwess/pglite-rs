use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use pglite::{MultiProcessOptions, PGlite, Replica, ReplicaConfig, SslMode};

use crate::cache::QueryCache;
use crate::cdc::CdcBridge;
use crate::classify::ReadClassifier;
use crate::error::CacheError;
use crate::live::LiveHub;
use crate::shapelog::ShapeLog;
use crate::upstream::Upstream;
use crate::version::VersionIndex;

pub struct UpstreamConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub publication: String,
    pub slot: String,
    pub sslmode: SslMode,
}

pub struct ServerConfig {
    pub bind_addr: String,
    pub data_dir: PathBuf,
    pub max_connections: usize,
    pub cache_size_bytes: u64,
    pub upstream: UpstreamConfig,
}

impl ServerConfig {
    pub fn from_env() -> Result<ServerConfig, CacheError> {
        Ok(ServerConfig {
            bind_addr: env_or("CACHE_BIND_ADDR", "127.0.0.1:8080"),
            data_dir: PathBuf::from(env_or("CACHE_DATA_DIR", "./cache-data")),
            max_connections: env_parse("CACHE_MAX_CONNECTIONS", 8)?,
            cache_size_bytes: env_parse("CACHE_SIZE_BYTES", 268_435_456)?,
            upstream: UpstreamConfig {
                host: env_or("UPSTREAM_HOST", "127.0.0.1"),
                port: env_parse("UPSTREAM_PORT", 5432)?,
                user: env_or("UPSTREAM_USER", "postgres"),
                password: env_or("UPSTREAM_PASSWORD", ""),
                database: env_or("UPSTREAM_DATABASE", "postgres"),
                publication: env_required("UPSTREAM_PUBLICATION")?,
                slot: env_required("UPSTREAM_SLOT")?,
                sslmode: parse_sslmode(&env_or("UPSTREAM_SSLMODE", "disable")),
            },
        })
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub db: PGlite,
    pub replica: Replica,
    pub versions: VersionIndex,
    pub cache: QueryCache,
    pub classifier: Arc<ReadClassifier>,
    pub upstream: Arc<Upstream>,
    pub shapes: ShapeLog,
    pub live: LiveHub,
    pub tables: Arc<HashSet<String>>,
}

pub struct CacheServer {
    state: AppState,
    cdc: CdcBridge,
    bind: String,
}

impl CacheServer {
    pub async fn boot(config: ServerConfig) -> Result<CacheServer, CacheError> {
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
            sslmode: config.upstream.sslmode,
            ..Default::default()
        };
        let replica = Replica::start(db.clone(), replica_config).await?;

        let (replicated, pk, full) = scan_schema(&db).await?;
        let versions = VersionIndex::new(pk.clone(), full);
        let cdc = CdcBridge::start(&replica, versions.clone())?;
        let shapes = ShapeLog::start(&cdc);
        let live = LiveHub::start(&cdc, db.clone(), Arc::new(pk));
        let cache = QueryCache::new(config.cache_size_bytes);
        let classifier = Arc::new(ReadClassifier::new(replicated.clone()));
        let tables = Arc::new(replicated);
        let upstream = Arc::new(Upstream::new(
            &config.upstream.host,
            config.upstream.port,
            &config.upstream.user,
            &config.upstream.password,
            &config.upstream.database,
        ));

        let state = AppState {
            db,
            replica,
            versions,
            cache,
            classifier,
            upstream,
            shapes,
            live,
            tables,
        };

        Ok(CacheServer {
            state,
            cdc,
            bind: config.bind_addr,
        })
    }

    pub async fn run(self) -> Result<(), CacheError> {
        let CacheServer { state, cdc, bind } = self;
        let result = crate::http::server::serve(state, bind).await;
        cdc.stop();
        result
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

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_required(key: &str) -> Result<String, CacheError> {
    std::env::var(key).map_err(|_| CacheError::Config(format!("{key} is required")))
}

fn env_parse<T>(key: &str, default: T) -> Result<T, CacheError>
where
    T: std::str::FromStr,
{
    match std::env::var(key) {
        Ok(value) => value
            .parse::<T>()
            .map_err(|_| CacheError::Config(format!("{key} is invalid"))),
        Err(_) => Ok(default),
    }
}

fn parse_sslmode(value: &str) -> SslMode {
    match value.to_ascii_lowercase().as_str() {
        "prefer" => SslMode::Prefer,
        "require" => SslMode::Require,
        "verify-full" | "verify_full" | "verifyfull" => SslMode::VerifyFull,
        _ => SslMode::Disable,
    }
}
