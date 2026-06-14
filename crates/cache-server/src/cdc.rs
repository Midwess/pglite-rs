use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use pglite::{CommittedTransaction, Replica};
use tokio::sync::broadcast;

use crate::error::CacheError;
use crate::version::VersionIndex;

#[derive(Clone)]
#[allow(dead_code)]
pub struct CdcBridge {
    stop: Arc<AtomicBool>,
    tx: broadcast::Sender<Arc<CommittedTransaction>>,
    handle: Arc<JoinHandle<()>>,
}

impl CdcBridge {
    pub fn start(replica: &Replica, versions: VersionIndex) -> Result<CdcBridge, CacheError> {
        let rx = replica.subscribe();
        let (tx, _) = broadcast::channel(1024);
        let stop = Arc::new(AtomicBool::new(false));

        let thread_tx = tx.clone();
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("cache-cdc".into())
            .spawn(move || {
                while let Ok(txn) = rx.recv() {
                    if thread_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    versions.advance(txn.as_ref());
                    let _ = thread_tx.send(txn);
                }
            })
            .map_err(CacheError::Io)?;

        Ok(CdcBridge {
            stop,
            tx,
            handle: Arc::new(handle),
        })
    }

    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<CommittedTransaction>> {
        self.tx.subscribe()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        !self.handle.is_finished()
    }
}
