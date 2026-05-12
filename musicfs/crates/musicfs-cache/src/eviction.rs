use musicfs_cas::CasStore;
use musicfs_core::ChunkHash;
use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::Instant;
use tracing::info;

pub trait EvictionPolicy: Send + Sync {
    fn record_access(&self, hash: ChunkHash);
    fn select_victims(&self, count: usize) -> Vec<ChunkHash>;
    fn remove(&self, hash: &ChunkHash);
}

pub struct LruEviction {
    access_times: RwLock<BTreeMap<Instant, ChunkHash>>,
    hash_to_time: RwLock<std::collections::HashMap<ChunkHash, Instant>>,
}

impl LruEviction {
    pub fn new() -> Self {
        Self {
            access_times: RwLock::new(BTreeMap::new()),
            hash_to_time: RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub async fn evict_to_target(
        &self,
        store: &CasStore,
        target_size: u64,
    ) -> Result<u64, EvictionError> {
        let mut bytes_freed = 0u64;

        while store.current_size() > target_size {
            let victims = self.select_victims(10);

            if victims.is_empty() {
                break;
            }

            for hash in victims {
                if let Ok(data) = store.get(&hash).await {
                    bytes_freed += data.len() as u64;
                    store.delete(&hash).await?;
                    self.remove(&hash);
                }
            }
        }

        if bytes_freed > 0 {
            info!("Evicted {} bytes from cache", bytes_freed);
        }

        Ok(bytes_freed)
    }
}

impl Default for LruEviction {
    fn default() -> Self {
        Self::new()
    }
}

impl EvictionPolicy for LruEviction {
    fn record_access(&self, hash: ChunkHash) {
        let now = Instant::now();
        let mut times = self.access_times.write().unwrap();
        let mut h2t = self.hash_to_time.write().unwrap();

        if let Some(old_time) = h2t.remove(&hash) {
            times.remove(&old_time);
        }

        times.insert(now, hash);
        h2t.insert(hash, now);
    }

    fn select_victims(&self, count: usize) -> Vec<ChunkHash> {
        let times = self.access_times.read().unwrap();
        times.values().take(count).copied().collect()
    }

    fn remove(&self, hash: &ChunkHash) {
        let mut times = self.access_times.write().unwrap();
        let mut h2t = self.hash_to_time.write().unwrap();

        if let Some(time) = h2t.remove(hash) {
            times.remove(&time);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvictionError {
    #[error("CAS error: {0}")]
    Cas(#[from] musicfs_cas::CasError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lru_access_order() {
        let lru = LruEviction::new();

        let h1 = ChunkHash::from_bytes(b"chunk1");
        let h2 = ChunkHash::from_bytes(b"chunk2");
        let h3 = ChunkHash::from_bytes(b"chunk3");

        lru.record_access(h1);
        std::thread::sleep(std::time::Duration::from_millis(1));
        lru.record_access(h2);
        std::thread::sleep(std::time::Duration::from_millis(1));
        lru.record_access(h3);

        let victims = lru.select_victims(2);
        assert_eq!(victims.len(), 2);
        assert_eq!(victims[0], h1);
        assert_eq!(victims[1], h2);
    }

    #[test]
    fn test_lru_reaccess_updates_order() {
        let lru = LruEviction::new();

        let h1 = ChunkHash::from_bytes(b"chunk1");
        let h2 = ChunkHash::from_bytes(b"chunk2");

        lru.record_access(h1);
        std::thread::sleep(std::time::Duration::from_millis(1));
        lru.record_access(h2);
        std::thread::sleep(std::time::Duration::from_millis(1));
        lru.record_access(h1);

        let victims = lru.select_victims(1);
        assert_eq!(victims[0], h2);
    }

    #[test]
    fn test_lru_remove() {
        let lru = LruEviction::new();

        let h1 = ChunkHash::from_bytes(b"chunk1");
        let h2 = ChunkHash::from_bytes(b"chunk2");

        lru.record_access(h1);
        lru.record_access(h2);
        lru.remove(&h1);

        let victims = lru.select_victims(10);
        assert_eq!(victims.len(), 1);
        assert_eq!(victims[0], h2);
    }
}
