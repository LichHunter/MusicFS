use crate::patterns::{AccessContext, PatternStore};
use musicfs_cas::ContentFetcher;
use musicfs_core::{Event, EventBus, FileId};
use parking_lot::Mutex as ParkingMutex;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

const DEFAULT_PREFETCH_LOOKAHEAD: usize = 3;
const DEFAULT_MAX_CONCURRENT: usize = 2;
const DEFAULT_COOLDOWN_MS: u64 = 100;

#[derive(Debug, Clone)]
pub struct PrefetchConfig {
    pub lookahead: usize,
    pub max_concurrent: usize,
    pub cooldown: Duration,
    pub enabled: bool,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            lookahead: DEFAULT_PREFETCH_LOOKAHEAD,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            cooldown: Duration::from_millis(DEFAULT_COOLDOWN_MS),
            enabled: true,
        }
    }
}

pub struct PrefetchEngine {
    config: PrefetchConfig,
    fetcher: Arc<ContentFetcher>,
    in_flight: Arc<ParkingMutex<HashSet<FileId>>>,
    semaphore: Arc<Semaphore>,
    running: Arc<AtomicBool>,
}

pub struct PrefetchHandle {
    handle: JoinHandle<()>,
    running: Arc<AtomicBool>,
}

impl PrefetchHandle {
    pub async fn stop(self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = self.handle.await;
    }
}

impl PrefetchEngine {
    pub fn new(
        config: PrefetchConfig,
        _pattern_store: Arc<PatternStore>,
        fetcher: Arc<ContentFetcher>,
    ) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent));

        Self {
            config,
            fetcher,
            in_flight: Arc::new(ParkingMutex::new(HashSet::new())),
            semaphore,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(
        self: Arc<Self>,
        event_bus: Arc<EventBus>,
        pattern_store: Arc<PatternStore>,
    ) -> PrefetchHandle {
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();

        let config = self.config.clone();
        let fetcher = self.fetcher.clone();
        let in_flight = self.in_flight.clone();
        let semaphore = self.semaphore.clone();
        let running_inner = running.clone();

        let handle = tokio::spawn(async move {
            let mut rx = event_bus.subscribe();

            while running_inner.load(Ordering::SeqCst) {
                match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                    Ok(Ok(event)) => {
                        if let Event::FileAccessed { file_id, .. } = event {
                            if config.enabled {
                                let ctx = AccessContext::default();
                                if let Err(e) = pattern_store.record(file_id, ctx) {
                                    warn!("Failed to record access pattern: {}", e);
                                    continue;
                                }

                                let predictions =
                                    pattern_store.predict_next(file_id, config.lookahead);

                                for predicted_id in predictions {
                                    prefetch_file(predicted_id, &fetcher, &in_flight, &semaphore)
                                        .await;
                                }

                                tokio::time::sleep(config.cooldown).await;
                            }
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => continue,
                }
            }

            info!("Prefetch engine stopped");
        });

        PrefetchHandle { handle, running }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.lock().len()
    }

    pub fn update_config(&mut self, config: PrefetchConfig) {
        self.config = config;
    }
}

async fn prefetch_file(
    file_id: FileId,
    fetcher: &Arc<ContentFetcher>,
    in_flight: &Arc<ParkingMutex<HashSet<FileId>>>,
    semaphore: &Arc<Semaphore>,
) {
    {
        let mut guard = in_flight.lock();
        if guard.contains(&file_id) {
            debug!("Skipping prefetch for {:?} - already in flight", file_id);
            return;
        }
        guard.insert(file_id);
    }

    let permit = match semaphore.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            debug!("Skipping prefetch for {:?} - concurrency limit", file_id);
            in_flight.lock().remove(&file_id);
            return;
        }
    };

    let fetcher = fetcher.clone();
    let in_flight = in_flight.clone();

    tokio::spawn(async move {
        debug!("Prefetching file {:?}", file_id);

        match fetcher.ensure_cached(file_id).await {
            Ok(manifest) => {
                info!(
                    "Prefetched {:?}: {} chunks, {} bytes",
                    file_id,
                    manifest.chunks.len(),
                    manifest.total_size
                );
            }
            Err(e) => {
                debug!("Prefetch failed for {:?}: {}", file_id, e);
            }
        }

        in_flight.lock().remove(&file_id);
        drop(permit);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefetch_config_defaults() {
        let config = PrefetchConfig::default();
        assert_eq!(config.lookahead, 3);
        assert_eq!(config.max_concurrent, 2);
        assert!(config.enabled);
    }
}
