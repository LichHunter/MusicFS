use crate::chunks::ChunkLocation;
use bytes::Bytes;
use musicfs_core::ChunkHash;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;
use tracing::{debug, info, trace, warn};

#[cfg(feature = "failpoints")]
use fail::fail_point;

const DEFAULT_MAX_SIZE_10GB: u64 = 10 * 1024 * 1024 * 1024;
const DEFAULT_SHARD_LEVELS_256_SUBDIRS: u8 = 2;

#[derive(Debug, Clone)]
pub struct CasConfig {
    pub chunks_dir: PathBuf,
    pub max_size: u64,
    pub shard_levels: u8,
}

impl Default for CasConfig {
    fn default() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("musicfs")
            .join("chunks");

        Self {
            chunks_dir: cache_dir,
            max_size: DEFAULT_MAX_SIZE_10GB,
            shard_levels: DEFAULT_SHARD_LEVELS_256_SUBDIRS,
        }
    }
}

pub struct CasStore {
    config: CasConfig,
    index: sled::Db,
    current_size: AtomicU64,
}

impl CasStore {
    pub async fn open(config: CasConfig) -> Result<Self, CasError> {
        fs::create_dir_all(&config.chunks_dir).await?;

        let index_path = config.chunks_dir.join("index.sled");
        let index = match sled::open(&index_path) {
            Ok(db) => db,
            Err(e) => {
                warn!(error = %e, path = ?index_path, "sled index corrupted, attempting recovery");

                match sled::Config::new().path(&index_path).open() {
                    Ok(db) => {
                        info!("sled index repaired successfully");
                        db
                    }
                    Err(repair_err) => {
                        warn!(error = %repair_err, "sled repair failed, recreating index");
                        if index_path.exists() {
                            std::fs::remove_dir_all(&index_path).map_err(CasError::Io)?;
                        }
                        sled::open(&index_path)?
                    }
                }
            }
        };

        let current_size = Self::calculate_size(&config.chunks_dir).await;

        Ok(Self {
            config,
            index,
            current_size: AtomicU64::new(current_size),
        })
    }

    async fn calculate_size(dir: &Path) -> u64 {
        Self::calculate_size_recursive(dir).await
    }

    fn calculate_size_recursive(
        dir: &Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + '_>> {
        Box::pin(async move {
            let mut size = 0u64;
            if let Ok(mut entries) = fs::read_dir(dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    if let Ok(meta) = entry.metadata().await {
                        if meta.is_file() {
                            size += meta.len();
                        } else if meta.is_dir() {
                            // Skip sled index directory
                            let name = entry.file_name();
                            if name != "index.sled" {
                                size += Self::calculate_size_recursive(&entry.path()).await;
                            }
                        }
                    }
                }
            }
            size
        })
    }

    pub async fn put(&self, data: &[u8]) -> Result<ChunkHash, CasError> {
        let hash = ChunkHash::from_bytes(data);
        let path = self.chunk_path(&hash);

        if path.exists() {
            trace!(hash = %hash, size_bytes = data.len(), "dedup hit");
            return Ok(hash);
        }

        if self.config.max_size > 0 {
            let new_size = self.current_size.load(Ordering::SeqCst) + data.len() as u64;
            if new_size > self.config.max_size {
                warn!(
                    current_size = self.current_size.load(Ordering::SeqCst),
                    chunk_size = data.len(),
                    max_size = self.config.max_size,
                    "CAS store full, rejecting write"
                );
                return Err(CasError::StoreFull {
                    current: self.current_size.load(Ordering::SeqCst),
                    max: self.config.max_size,
                });
            }
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        #[cfg(feature = "failpoints")]
        fail_point!("cas-put-before-write", |_| {
            Err(CasError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failpoint: cas-put-before-write",
            )))
        });

        fs::write(&path, data).await?;

        #[cfg(feature = "failpoints")]
        fail_point!("cas-put-after-write-before-index", |_| {
            Err(CasError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failpoint: cas-put-after-write-before-index",
            )))
        });

        let location = ChunkLocation {
            path: path.clone(),
            size: data.len() as u32,
        };
        self.index.insert(
            hash.0.as_slice(),
            rmp_serde::to_vec(&location).map_err(|e| CasError::Serialization(e.to_string()))?,
        )?;

        self.current_size
            .fetch_add(data.len() as u64, Ordering::SeqCst);

        debug!(hash = %hash, size_bytes = data.len(), "chunk stored");
        Ok(hash)
    }

    pub async fn get(&self, hash: &ChunkHash) -> Result<Bytes, CasError> {
        let path = self.chunk_path(hash);

        if !path.exists() {
            return Err(CasError::NotFound(hash.as_hex()));
        }

        let data = fs::read(&path).await?;

        if self.config.max_size > 0 {
            self.verify_integrity(hash, &data)?;
        }

        debug!(hash = %hash, size_bytes = data.len(), "chunk retrieved");
        Ok(Bytes::from(data))
    }

    pub fn exists(&self, hash: &ChunkHash) -> bool {
        self.chunk_path(hash).exists()
    }

    fn verify_integrity(&self, expected: &ChunkHash, data: &[u8]) -> Result<(), CasError> {
        let actual = ChunkHash::from_bytes(data);
        if actual != *expected {
            warn!(
                "Chunk integrity failure: expected {}, got {}",
                expected, actual
            );
            return Err(CasError::IntegrityError {
                expected: expected.as_hex(),
                actual: actual.as_hex(),
            });
        }
        Ok(())
    }

    fn chunk_path(&self, hash: &ChunkHash) -> PathBuf {
        let hex = hash.as_hex();
        let mut path = self.config.chunks_dir.clone();

        for i in 0..self.config.shard_levels as usize {
            let start = i * 2;
            let end = start + 2;
            if end <= hex.len() {
                path = path.join(&hex[start..end]);
            }
        }

        path.join(&hex)
    }

    pub async fn delete(&self, hash: &ChunkHash) -> Result<(), CasError> {
        let path = self.chunk_path(hash);

        if path.exists() {
            let meta = fs::metadata(&path).await?;
            fs::remove_file(&path).await?;
            self.index.remove(hash.0.as_slice())?;
            self.current_size.fetch_sub(meta.len(), Ordering::SeqCst);
            debug!(hash = %hash, size_bytes = meta.len(), "chunk deleted");
        }

        Ok(())
    }

    pub fn current_size(&self) -> u64 {
        self.current_size.load(Ordering::SeqCst)
    }

    pub fn max_size(&self) -> u64 {
        self.config.max_size
    }

    pub fn list_chunks(&self) -> impl Iterator<Item = ChunkHash> + '_ {
        self.index.iter().filter_map(|r| {
            r.ok().and_then(|(k, _)| {
                if k.len() == 8 {
                    let mut arr = [0u8; 8];
                    arr.copy_from_slice(&k);
                    Some(ChunkHash(arr))
                } else {
                    None
                }
            })
        })
    }

    pub fn dedup_stats(&self) -> DedupStats {
        let chunks_stored = self.index.len() as u64;
        let size_bytes = self.current_size();

        DedupStats {
            chunks_stored,
            chunks_unique: chunks_stored,
            size_bytes,
            size_limit_bytes: self.config.max_size,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DedupStats {
    pub chunks_stored: u64,
    pub chunks_unique: u64,
    pub size_bytes: u64,
    pub size_limit_bytes: u64,
}

impl DedupStats {
    pub fn dedup_ratio(&self) -> f64 {
        if self.chunks_stored == 0 {
            0.0
        } else {
            1.0 - (self.chunks_unique as f64 / self.chunks_stored as f64)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CasError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Sled error: {0}")]
    Sled(#[from] sled::Error),

    #[error("Chunk not found: {0}")]
    NotFound(String),

    #[error("Integrity error: expected {expected}, got {actual}")]
    IntegrityError { expected: String, actual: String },

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Store full: {current} / {max} bytes")]
    StoreFull { current: u64, max: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_store() -> (CasStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            max_size: 1024 * 1024,
            shard_levels: 2,
        };
        let store = CasStore::open(config).await.unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn test_cas_put_get() {
        let (store, _dir) = test_store().await;

        let data = b"test chunk data";
        let hash = store.put(data).await.unwrap();

        let retrieved = store.get(&hash).await.unwrap();
        assert_eq!(&retrieved[..], data);
    }

    #[tokio::test]
    async fn test_cas_dedup() {
        let (store, _dir) = test_store().await;

        let data = b"duplicate data";
        let hash1 = store.put(data).await.unwrap();
        let hash2 = store.put(data).await.unwrap();

        assert_eq!(hash1, hash2);
    }

    #[tokio::test]
    async fn test_cas_exists() {
        let (store, _dir) = test_store().await;

        let data = b"existence test";
        let hash = store.put(data).await.unwrap();

        assert!(store.exists(&hash));

        let fake_hash = ChunkHash::from_bytes(b"nonexistent");
        assert!(!store.exists(&fake_hash));
    }

    #[tokio::test]
    async fn test_cas_delete() {
        let (store, _dir) = test_store().await;

        let data = b"delete me";
        let hash = store.put(data).await.unwrap();

        assert!(store.exists(&hash));

        store.delete(&hash).await.unwrap();

        assert!(!store.exists(&hash));
    }

    #[tokio::test]
    async fn test_cas_integrity() {
        let (store, _dir) = test_store().await;

        let data = b"integrity test";
        let hash = store.put(data).await.unwrap();

        let retrieved = store.get(&hash).await.unwrap();
        assert_eq!(&retrieved[..], data);
    }

    #[tokio::test]
    async fn test_cas_dedup_stats() {
        let (store, _dir) = test_store().await;

        store.put(b"chunk1").await.unwrap();
        store.put(b"chunk2").await.unwrap();
        store.put(b"chunk1").await.unwrap();

        let stats = store.dedup_stats();
        assert_eq!(stats.chunks_stored, 2);
        assert_eq!(stats.chunks_unique, 2);
    }
}
