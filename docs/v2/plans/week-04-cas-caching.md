# Week 4: CAS & Chunk Caching

**Phase**: 1 (MVP)  
**Prerequisites**: Week 3 (Virtual Tree & Basic Ops)  
**Estimated effort**: 5 days

---

## Objective

Implement Content-Addressable Storage (CAS) for chunk deduplication, cache eviction with LRU policy, and connect to FUSE read operations to enable actual file playback.

**Note**: Week 4 treats whole files as single chunks for simplicity. Week 5 adds CDC (Content-Defined Chunking) via FastCDC for efficient delta sync (FR-8.2, FR-11.2).

---

## Deliverables

| Task | Crate | Files | Done |
|------|-------|-------|------|
| CAS store implementation | musicfs-cas | `lib.rs`, `store.rs` | [ ] |
| Chunk storage | musicfs-cas | `chunks.rs` | [ ] |
| Cache eviction (LRU) | musicfs-cache | `eviction.rs` | [ ] |
| FUSE read integration | musicfs-fuse | `filesystem.rs` | [ ] |
| Integration tests | tests/integration | `basic_mount.rs` | [ ] |

---

## Task 1: CAS Store

### 1.1 Update `musicfs-cas/Cargo.toml`

```toml
[package]
name = "musicfs-cas"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
tokio.workspace = true
tracing.workspace = true
serde.workspace = true
sled = "0.34"
xxhash-rust = { version = "0.8", features = ["xxh64"] }
bytes = "1"
rmp-serde = "1"              # msgpack per architecture 4.3.6
hex = "0.4"
dirs = "5"                   # For ~/.cache resolution
thiserror.workspace = true
```

### 1.2 Create `musicfs-cas/src/lib.rs`

```rust
mod store;
mod chunks;

pub use store::{CasStore, CasConfig, CasError, DedupStats};
pub use chunks::{ChunkHash, ChunkLocation, ChunkRef};
```

### 1.3 Create `musicfs-cas/src/chunks.rs`

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Chunk hash (xxHash64, 8 bytes) per architecture 8.3
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkHash(pub [u8; 8]);

impl ChunkHash {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let hash = xxhash_rust::xxh64::xxh64(bytes, 0);
        Self(hash.to_le_bytes())
    }
    
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }
    
    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s).ok()?;
        if bytes.len() != 8 {
            return None;
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes);
        Some(Self(arr))
    }
}

impl std::fmt::Display for ChunkHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_hex())
    }
}

/// Location of a chunk in storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkLocation {
    pub path: PathBuf,
    pub size: u32,
}

/// Reference to a chunk within a file (per architecture 4.3.6 chunk_manifest format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRef {
    pub hash: ChunkHash,
    pub offset: u64,
    pub size: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_chunk_hash_from_bytes() {
        let data = b"hello world";
        let hash = ChunkHash::from_bytes(data);
        assert_eq!(hash.as_hex().len(), 16);
    }
    
    #[test]
    fn test_chunk_hash_deterministic() {
        let data = b"test data";
        let hash1 = ChunkHash::from_bytes(data);
        let hash2 = ChunkHash::from_bytes(data);
        assert_eq!(hash1, hash2);
    }
    
    #[test]
    fn test_chunk_hash_hex_roundtrip() {
        let data = b"roundtrip test";
        let hash = ChunkHash::from_bytes(data);
        let hex = hash.as_hex();
        let restored = ChunkHash::from_hex(&hex).unwrap();
        assert_eq!(hash, restored);
    }
}
```

### 1.4 Create `musicfs-cas/src/store.rs`

```rust
use crate::chunks::{ChunkHash, ChunkLocation};
use bytes::Bytes;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;
use tracing::{debug, warn};

/// CAS configuration
#[derive(Debug, Clone)]
pub struct CasConfig {
    /// Root directory for chunk storage
    pub chunks_dir: PathBuf,
    /// Maximum cache size in bytes (FR-8.2)
    pub max_size: u64,
    /// Number of subdirectory levels (for filesystem performance)
    pub shard_levels: u8,
}

impl Default for CasConfig {
    fn default() -> Self {
        // Per architecture 4.3.2: ~/.cache/musicfs/chunks/
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("musicfs")
            .join("chunks");
        
        Self {
            chunks_dir: cache_dir,
            max_size: 10 * 1024 * 1024 * 1024, // 10 GB per NFR-5.2
            shard_levels: 2,                   // 256 subdirs per architecture 4.3.2
        }
    }
}

/// Content-Addressable Storage (FR-20.1-20.4)
pub struct CasStore {
    config: CasConfig,
    index: sled::Db,
    current_size: AtomicU64,
}

impl CasStore {
    pub async fn open(config: CasConfig) -> Result<Self, CasError> {
        fs::create_dir_all(&config.chunks_dir).await?;
        
        let index_path = config.chunks_dir.join("index.sled");
        let index = sled::open(&index_path)?;
        
        let current_size = Self::calculate_size(&config.chunks_dir).await;
        
        Ok(Self {
            config,
            index,
            current_size: AtomicU64::new(current_size),
        })
    }
    
    async fn calculate_size(dir: &Path) -> u64 {
        let mut size = 0u64;
        if let Ok(mut entries) = fs::read_dir(dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(meta) = entry.metadata().await {
                    if meta.is_file() {
                        size += meta.len();
                    }
                }
            }
        }
        size
    }
    
    /// Store chunk, returns hash (FR-20.1)
    /// Deduplicates automatically - same content = same hash (FR-20.2)
    pub async fn put(&self, data: &[u8]) -> Result<ChunkHash, CasError> {
        let hash = ChunkHash::from_bytes(data);
        let path = self.chunk_path(&hash);
        
        if path.exists() {
            debug!("Chunk {} already exists (dedup)", hash);
            return Ok(hash);
        }
        
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        
        fs::write(&path, data).await?;
        
        let location = ChunkLocation {
            path: path.clone(),
            size: data.len() as u32,
        };
        // Use msgpack per architecture 4.3.6
        self.index.insert(
            hash.0.as_slice(),
            rmp_serde::to_vec(&location).unwrap(),
        )?;
        
        self.current_size.fetch_add(data.len() as u64, Ordering::SeqCst);
        
        debug!("Stored chunk {} ({} bytes)", hash, data.len());
        Ok(hash)
    }
    
    /// Retrieve chunk by hash (FR-20.1)
    pub async fn get(&self, hash: &ChunkHash) -> Result<Bytes, CasError> {
        let path = self.chunk_path(hash);
        
        if !path.exists() {
            return Err(CasError::NotFound(hash.as_hex()));
        }
        
        let data = fs::read(&path).await?;
        
        if self.config.max_size > 0 {
            self.verify_integrity(hash, &data)?;
        }
        
        Ok(Bytes::from(data))
    }
    
    /// Check if chunk exists (for dedup check)
    pub fn exists(&self, hash: &ChunkHash) -> bool {
        self.chunk_path(hash).exists()
    }
    
    /// Verify chunk integrity (FR-20.4)
    fn verify_integrity(&self, expected: &ChunkHash, data: &[u8]) -> Result<(), CasError> {
        let actual = ChunkHash::from_bytes(data);
        if actual != *expected {
            warn!("Chunk integrity failure: expected {}, got {}", expected, actual);
            return Err(CasError::IntegrityError {
                expected: expected.as_hex(),
                actual: actual.as_hex(),
            });
        }
        Ok(())
    }
    
    /// Get path for a chunk hash (sharded for filesystem performance)
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
    
    /// Delete a chunk
    pub async fn delete(&self, hash: &ChunkHash) -> Result<(), CasError> {
        let path = self.chunk_path(hash);
        
        if path.exists() {
            let meta = fs::metadata(&path).await?;
            fs::remove_file(&path).await?;
            self.index.remove(hash.0.as_slice())?;
            self.current_size.fetch_sub(meta.len(), Ordering::SeqCst);
            debug!("Deleted chunk {}", hash);
        }
        
        Ok(())
    }
    
    /// Get current cache size
    pub fn current_size(&self) -> u64 {
        self.current_size.load(Ordering::SeqCst)
    }
    
    /// Get maximum cache size
    pub fn max_size(&self) -> u64 {
        self.config.max_size
    }
    
    /// List all chunk hashes
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
    
    /// Get deduplication statistics (FR-20.3)
    pub fn dedup_stats(&self) -> DedupStats {
        let chunks_stored = self.index.len() as u64;
        let size_bytes = self.current_size();
        
        DedupStats {
            chunks_stored,
            chunks_unique: chunks_stored, // All stored chunks are unique by definition
            size_bytes,
            size_limit_bytes: self.config.max_size,
        }
    }
}

/// Deduplication statistics (FR-20.3)
#[derive(Debug, Clone)]
pub struct DedupStats {
    pub chunks_stored: u64,
    pub chunks_unique: u64,
    pub size_bytes: u64,
    pub size_limit_bytes: u64,
}

impl DedupStats {
    /// Calculate dedup ratio (space saved)
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
        store.put(b"chunk1").await.unwrap(); // Duplicate
        
        let stats = store.dedup_stats();
        assert_eq!(stats.chunks_stored, 2); // Only 2 unique
        assert_eq!(stats.chunks_unique, 2);
    }
}
```

---

## Task 2: Cache Eviction

### 2.1 Add to `musicfs-cache/src/lib.rs`

```rust
mod eviction;
pub use eviction::{LruEviction, EvictionPolicy};
```

### 2.2 Create `musicfs-cache/src/eviction.rs`

```rust
use musicfs_cas::{CasStore, ChunkHash};
use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::Instant;
use tracing::{debug, info};

/// Eviction policy trait
pub trait EvictionPolicy: Send + Sync {
    fn record_access(&self, hash: ChunkHash);
    fn select_victims(&self, count: usize) -> Vec<ChunkHash>;
    fn remove(&self, hash: &ChunkHash);
}

/// LRU eviction policy (FR-8.2)
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
    
    /// Evict chunks until under target size
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
```

---

## Task 3: File Reader Integration

### 3.1 Create `musicfs-cas/src/reader.rs`

```rust
use crate::{ChunkHash, ChunkRef, CasStore};
use bytes::{Bytes, BytesMut};
use musicfs_core::FileId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// Chunk manifest for a file (per architecture 4.3.6)
/// Stored as msgpack BLOB in SQLite files.chunk_manifest column
/// Format: [(chunk_hash, offset, size), ...]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub file_id: FileId,
    pub total_size: u64,
    pub chunks: Vec<ChunkRef>,
}

impl ChunkManifest {
    /// Serialize chunks to msgpack for database storage (architecture 4.3.6)
    pub fn chunks_to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(&self.chunks).unwrap()
    }
    
    /// Deserialize chunks from database BLOB
    pub fn chunks_from_bytes(data: &[u8]) -> Option<Vec<ChunkRef>> {
        rmp_serde::from_slice(data).ok()
    }
    
    /// Create manifest from database fields
    pub fn from_db(file_id: FileId, total_size: u64, chunk_blob: &[u8]) -> Option<Self> {
        let chunks = Self::chunks_from_bytes(chunk_blob)?;
        Some(Self { file_id, total_size, chunks })
    }
}

/// File reader using CAS chunks
pub struct FileReader {
    store: std::sync::Arc<CasStore>,
    manifests: RwLock<HashMap<FileId, ChunkManifest>>,
}

impl FileReader {
    pub fn new(store: std::sync::Arc<CasStore>) -> Self {
        Self {
            store,
            manifests: RwLock::new(HashMap::new()),
        }
    }
    
    /// Register a file's chunk manifest
    pub fn register_manifest(&self, manifest: ChunkManifest) {
        let mut manifests = self.manifests.write().unwrap();
        manifests.insert(manifest.file_id, manifest);
    }
    
    /// Read bytes from a file at offset
    pub async fn read(
        &self,
        file_id: FileId,
        offset: u64,
        size: u32,
    ) -> Result<Bytes, ReaderError> {
        let manifest = {
            let manifests = self.manifests.read().unwrap();
            manifests.get(&file_id).cloned()
                .ok_or(ReaderError::ManifestNotFound(file_id))?
        };
        
        if offset >= manifest.total_size {
            return Ok(Bytes::new());
        }
        
        let end = std::cmp::min(offset + size as u64, manifest.total_size);
        let mut result = BytesMut::with_capacity((end - offset) as usize);
        
        for chunk_ref in &manifest.chunks {
            let chunk_start = chunk_ref.offset;
            let chunk_end = chunk_ref.offset + chunk_ref.size as u64;
            
            if chunk_end <= offset || chunk_start >= end {
                continue;
            }
            
            let chunk_data = self.store.get(&chunk_ref.hash).await?;
            
            let read_start = if offset > chunk_start {
                (offset - chunk_start) as usize
            } else {
                0
            };
            
            let read_end = if end < chunk_end {
                (end - chunk_start) as usize
            } else {
                chunk_ref.size as usize
            };
            
            result.extend_from_slice(&chunk_data[read_start..read_end]);
        }
        
        Ok(result.freeze())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error("Manifest not found for file {0:?}")]
    ManifestNotFound(FileId),
    
    #[error("CAS error: {0}")]
    Cas(#[from] crate::CasError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CasConfig;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_file_reader_simple() {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = std::sync::Arc::new(CasStore::open(config).await.unwrap());
        
        let data = b"Hello, World!";
        let hash = store.put(data).await.unwrap();
        
        let reader = FileReader::new(store);
        reader.register_manifest(ChunkManifest {
            file_id: FileId(1),
            total_size: data.len() as u64,
            chunks: vec![ChunkRef {
                hash,
                offset: 0,
                size: data.len() as u32,
            }],
        });
        
        let result = reader.read(FileId(1), 0, data.len() as u32).await.unwrap();
        assert_eq!(&result[..], data);
    }
    
    #[tokio::test]
    async fn test_file_reader_partial() {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = std::sync::Arc::new(CasStore::open(config).await.unwrap());
        
        let data = b"ABCDEFGHIJ";
        let hash = store.put(data).await.unwrap();
        
        let reader = FileReader::new(store);
        reader.register_manifest(ChunkManifest {
            file_id: FileId(1),
            total_size: data.len() as u64,
            chunks: vec![ChunkRef {
                hash,
                offset: 0,
                size: data.len() as u32,
            }],
        });
        
        let result = reader.read(FileId(1), 3, 4).await.unwrap();
        assert_eq!(&result[..], b"DEFG");
    }
}
```

### 3.2 Update `musicfs-cas/src/lib.rs`

```rust
mod store;
mod chunks;
mod reader;

pub use store::{CasStore, CasConfig, CasError, DedupStats};
pub use chunks::{ChunkHash, ChunkLocation, ChunkRef};
pub use reader::{FileReader, ChunkManifest, ReaderError};
```

---

## Task 4: FUSE Read Integration

### 4.1 Update `musicfs-fuse/Cargo.toml`

```toml
[dependencies]
musicfs-core = { path = "../musicfs-core" }
musicfs-cache = { path = "../musicfs-cache" }
musicfs-cas = { path = "../musicfs-cas" }
musicfs-origins = { path = "../musicfs-origins" }
# ... rest of dependencies
```

### 4.2 Update `musicfs-fuse/src/filesystem.rs` read method

Replace the placeholder `read` implementation:

```rust
use musicfs_cas::{FileReader, ChunkManifest};

pub struct MusicFs {
    tree: Arc<RwLock<VirtualTree>>,
    reader: Arc<FileReader>,
    uid: u32,
    gid: u32,
}

impl MusicFs {
    pub fn new(
        tree: Arc<RwLock<VirtualTree>>,
        reader: Arc<FileReader>,
    ) -> Self {
        Self {
            tree,
            reader,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
    }
}

// In Filesystem impl:
fn read(
    &mut self,
    _req: &Request,
    ino: u64,
    _fh: u64,
    offset: i64,
    size: u32,
    _flags: i32,
    _lock_owner: Option<u64>,
    reply: ReplyData,
) {
    debug!("read(ino={}, offset={}, size={})", ino, offset, size);
    
    let file_id = {
        let tree = self.tree.read().unwrap();
        if let Some(VirtualNode::File(file)) = tree.get(ino) {
            file.file_id
        } else {
            reply.error(libc::ENOENT);
            return;
        }
    };
    
    // Use tokio runtime for async read
    let reader = self.reader.clone();
    let result = tokio::runtime::Handle::current().block_on(async {
        reader.read(file_id, offset as u64, size).await
    });
    
    match result {
        Ok(data) => reply.data(&data),
        Err(e) => {
            warn!("Read error: {}", e);
            reply.error(libc::EIO);
        }
    }
}
```

---

## Task 5: Integration Tests

### 5.1 Create `tests/integration/basic_mount.rs`

```rust
use musicfs_cache::{TreeBuilder, VirtualTree};
use musicfs_cas::{CasStore, CasConfig, FileReader, ChunkManifest, ChunkRef};
use musicfs_core::{FileId, FileMeta, OriginId, RealPath, VirtualPath};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;
use tempfile::TempDir;

fn make_file_meta(id: i64, vpath: &str, size: u64) -> FileMeta {
    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(vpath),
        real_path: RealPath {
            origin_id: OriginId::from("test"),
            path: PathBuf::from("/test"),
        },
        size,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: None,
    }
}

#[tokio::test]
async fn test_cas_and_tree_integration() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        ..Default::default()
    };
    let store = Arc::new(CasStore::open(config).await.unwrap());
    
    let file_data = b"This is test audio file content for testing.";
    let chunk_hash = store.put(file_data).await.unwrap();
    
    let mut builder = TreeBuilder::new();
    builder.add_file(&make_file_meta(1, "/Artist/Album/Track.flac", file_data.len() as u64));
    let tree = Arc::new(RwLock::new(builder.build()));
    
    let reader = Arc::new(FileReader::new(store.clone()));
    reader.register_manifest(ChunkManifest {
        file_id: FileId(1),
        total_size: file_data.len() as u64,
        chunks: vec![ChunkRef {
            hash: chunk_hash,
            offset: 0,
            size: file_data.len() as u32,
        }],
    });
    
    let result = reader.read(FileId(1), 0, file_data.len() as u32).await.unwrap();
    assert_eq!(&result[..], file_data);
}

#[tokio::test]
async fn test_cache_persistence() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        ..Default::default()
    };
    
    let data = b"persistent data";
    let hash = {
        let store = CasStore::open(config.clone()).await.unwrap();
        store.put(data).await.unwrap()
    };
    
    let store = CasStore::open(config).await.unwrap();
    let retrieved = store.get(&hash).await.unwrap();
    assert_eq!(&retrieved[..], data);
}

#[tokio::test]
async fn test_deduplication() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        ..Default::default()
    };
    let store = CasStore::open(config).await.unwrap();
    
    let data = b"duplicate this content";
    
    let hash1 = store.put(data).await.unwrap();
    let size_after_first = store.current_size();
    
    let hash2 = store.put(data).await.unwrap();
    let size_after_second = store.current_size();
    
    assert_eq!(hash1, hash2);
    assert_eq!(size_after_first, size_after_second);
}
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_cas_put_get` | Unit | Basic store/retrieve (FR-20.1) |
| `test_cas_dedup` | Unit | Same content → same hash (FR-20.2) |
| `test_cas_dedup_stats` | Unit | Dedup statistics reported (FR-20.3) |
| `test_cas_integrity` | Unit | Verify chunk hash (FR-20.4) |
| `test_lru_access_order` | Unit | LRU ordering correct |
| `test_lru_reaccess_updates_order` | Unit | Re-access moves to end |
| `test_cache_eviction` | Unit | LRU eviction works (FR-8.4) |
| `test_cache_persistence` | Integration | Survives restart (FR-8.4) |
| `test_file_reader_simple` | Unit | Full file read |
| `test_file_reader_partial` | Unit | Offset/size read |
| `test_cas_and_tree_integration` | Integration | End-to-end read |
| `test_deduplication` | Integration | Dedup saves space |

---

## Exit Criteria

- [ ] Chunks stored in CAS with deduplication (FR-20.1, FR-20.2)
- [ ] Deduplication statistics reported via `dedup_stats()` (FR-20.3)
- [ ] Chunk integrity verified on read (FR-20.4)
- [ ] Cache size limit enforced via LRU eviction (FR-8.4)
- [ ] Cache persists across daemon restarts (FR-8.4)
- [ ] FUSE `read()` returns actual file content
- [ ] Audio playback works through mounted filesystem
- [ ] All Phase 1 requirements pass acceptance tests

---

## Dependencies to Add

### Workspace `Cargo.toml`

```toml
[workspace.dependencies]
# ... existing ...
sled = "0.34"
xxhash-rust = { version = "0.8", features = ["xxh64"] }
bytes = "1"
rmp-serde = "1"              # msgpack per architecture 4.3.6
hex = "0.4"
dirs = "5"                   # For ~/.cache resolution
tempfile = "3"
```

---

## Next Week

Week 5 will implement CDC chunking and delta detection for efficient synchronization.
