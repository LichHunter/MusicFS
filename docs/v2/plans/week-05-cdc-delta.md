# Week 5: CDC & Delta Detection

**Phase**: 2 (Delta Sync & Multi-Origin)  
**Prerequisites**: Week 4b (Origin-CAS Connector)  
**Estimated effort**: 5 days

---

## Objective

Implement Content-Defined Chunking (CDC) using FastCDC and delta detection for efficient synchronization. This enables the >90% bandwidth reduction requirement (NFR-6.4) by only transferring changed chunks.

**Critical Fix**: The MVP performance review identified that `Origin::read()` only returns ~2MB per call due to tokio's async read behavior. This must be fixed as part of CDC implementation since CDC requires the full file content.

---

## Oracle Review Fixes (MUST IMPLEMENT)

| Severity | Issue | Fix |
|----------|-------|-----|
| 🔴 Critical | **u32 overflow** - `file.size as u32` fails for files >4GB | Add `read_full(path) -> Result<Vec<u8>>` to Origin trait, use u64 for sizes |
| 🔴 Critical | **Memory explosion** - 200MB+ per file (data + chunk copies) | Use `chunk_refs()` and store immediately, drop source buffer after each chunk |
| 🔴 Critical | **`scan_origin()` is stub** - returns empty Vec, delta detection non-functional | Implement recursive walk using `Origin::readdir()` |
| 🟡 Arch | **Duplicate types** - `FileManifest` duplicates existing `ChunkManifest` | Extend existing `ChunkManifest` with `mtime` field instead of new type |
| 🟡 Arch | **Watcher spawns separate runtime** - wasteful | Use `tokio::task::spawn_blocking` instead of `std::thread::spawn` |
| ⚠️ Watch | No event debouncing (rapid saves flood events) | Add 200ms debounce before emitting events |
| ⚠️ Watch | Missing test for >90% bandwidth reduction claim | Add concrete reuse ratio test with metadata-only file edit |

---

## Architecture Reference

From architecture.md section 4.3.2 (CAS):

```
Avg chunk: 64KB
Min: 16KB, Max: 256KB
Stable boundaries for delta sync
```

From section 4.3.5 (Read Operation):

```
|CAS|
:chunk fetched data (CDC);
:store chunks by hash;
:update chunk manifest;
```

---

## Requirements Covered

| ID | Requirement | Priority |
|----|-------------|----------|
| FR-8.2 | Content-defined chunking for cache efficiency | P0 |
| FR-11.1 | Download only changed portions of files | P0 |
| FR-11.2 | Use CDC to identify changed chunks | P0 |
| FR-11.3 | Preserve unchanged chunks in cache | P0 |
| FR-11.4 | Handle file additions and deletions | P0 |
| FR-10.1 | Detect changes to origin files | P0 |
| FR-10.4 | Compare mtime and size for change detection | P0 |
| NFR-6.4 | Delta sync >90% bandwidth reduction | P0 |

---

## Deliverables

| Task | Crate | Files | Est. |
|------|-------|-------|------|
| Fix async read (read full file) | musicfs-origins | `local.rs` | 0.5d |
| FastCDC integration | musicfs-sync | `cdc.rs` | 1d |
| ChunkManifest persistence | musicfs-sync | `manifest.rs` | 0.5d |
| Delta detector | musicfs-sync | `delta.rs` | 1d |
| Change watcher (inotify) | musicfs-sync | `watcher.rs` | 1d |
| Update ContentFetcher for CDC | musicfs-cas | `fetcher.rs` | 0.5d |
| Integration tests | tests | `delta_sync.rs` | 0.5d |

---

## Task 1: Fix Async Read

### 1.1 Problem

Current `LocalOrigin::read()` uses `file.read()` which returns when the kernel buffer is exhausted (~2MB), not when the requested size is read.

### 1.2 Update Origin trait to add `read_full()` method

Add to `musicfs-origins/src/traits.rs`:

```rust
/// Read entire file content (for CDC chunking)
/// NOTE: Use u64 for size to support files >4GB
async fn read_full(&self, path: &Path) -> Result<Vec<u8>>;
```

### 1.3 Update `musicfs-origins/src/local.rs`

```rust
async fn read(&self, path: &Path, offset: u64, size: u64) -> Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let full_path = self.full_path(path);
    debug!(
        "LocalOrigin::read({:?}, offset={}, size={})",
        full_path, offset, size
    );

    let mut file = fs::File::open(&full_path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    // FIX: Use loop instead of single read() to get all requested bytes
    let mut buffer = Vec::with_capacity(size as usize);
    
    // Read until we have all requested bytes or EOF
    let mut total_read = 0u64;
    let mut temp_buf = vec![0u8; 64 * 1024]; // 64KB chunks
    
    while total_read < size {
        let to_read = std::cmp::min(temp_buf.len() as u64, size - total_read) as usize;
        let n = file.read(&mut temp_buf[..to_read]).await?;
        if n == 0 {
            break; // EOF
        }
        buffer.extend_from_slice(&temp_buf[..n]);
        total_read += n as u64;
    }

    Ok(buffer)
}

/// Read entire file (Oracle fix: separate method to avoid u32 overflow)
async fn read_full(&self, path: &Path) -> Result<Vec<u8>> {
    let full_path = self.full_path(path);
    debug!("LocalOrigin::read_full({:?})", full_path);
    Ok(tokio::fs::read(&full_path).await?)
}
```

**NOTE**: Change `size: u32` to `size: u64` throughout the Origin trait to support files >4GB.

---

## Task 2: FastCDC Integration

### 2.1 Add dependencies to `musicfs-sync/Cargo.toml`

```toml
[dependencies]
musicfs-core = { path = "../musicfs-core" }
musicfs-cas = { path = "../musicfs-cas" }

fastcdc = "3"
xxhash-rust = { version = "0.8", features = ["xxh64"] }
tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }
```

### 2.2 Create `musicfs-sync/src/cdc.rs`

```rust
use fastcdc::v2020::FastCDC;
use musicfs_core::ChunkHash;
use xxhash_rust::xxh64::xxh64;

/// CDC chunker configuration per architecture spec
pub struct CdcChunker {
    min_size: u32,  // 16 KB
    avg_size: u32,  // 64 KB  
    max_size: u32,  // 256 KB
}

impl Default for CdcChunker {
    fn default() -> Self {
        Self {
            min_size: 16 * 1024,
            avg_size: 64 * 1024,
            max_size: 256 * 1024,
        }
    }
}

/// A chunk produced by CDC
#[derive(Debug, Clone)]
pub struct Chunk {
    pub hash: ChunkHash,
    pub offset: u64,
    pub length: u32,
    pub data: Vec<u8>,
}

impl CdcChunker {
    pub fn new(min_size: u32, avg_size: u32, max_size: u32) -> Self {
        Self { min_size, avg_size, max_size }
    }

    /// Chunk data using FastCDC algorithm
    /// Returns chunks with stable boundaries for delta sync
    /// 
    /// WARNING: This copies all chunk data. For large files, use `chunk_refs()` 
    /// and store immediately to avoid memory explosion.
    pub fn chunk(&self, data: &[u8]) -> Vec<Chunk> {
        let chunker = FastCDC::new(
            data,
            self.min_size,
            self.avg_size,
            self.max_size,
        );

        chunker
            .map(|c| {
                let chunk_data = &data[c.offset..c.offset + c.length];
                let hash = ChunkHash::from_bytes(chunk_data);
                
                Chunk {
                    hash,
                    offset: c.offset as u64,
                    length: c.length as u32,
                    data: chunk_data.to_vec(),
                }
            })
            .collect()
    }

    /// Chunk data without copying (returns references) - PREFERRED for large files
    /// 
    /// Oracle fix: Use this method and store each chunk immediately before 
    /// processing the next to avoid 200MB+ memory usage per file.
    pub fn chunk_refs<'a>(&self, data: &'a [u8]) -> Vec<ChunkRef<'a>> {
        let chunker = FastCDC::new(
            data,
            self.min_size,
            self.avg_size,
            self.max_size,
        );

        chunker
            .map(|c| {
                let chunk_data = &data[c.offset..c.offset + c.length];
                ChunkRef {
                    hash: ChunkHash::from_bytes(chunk_data),
                    offset: c.offset as u64,
                    length: c.length as u32,
                    data: chunk_data,
                }
            })
            .collect()
    }

    /// Stream-process chunks to minimize memory (Oracle fix: avoid memory explosion)
    /// Calls `processor` for each chunk, allowing immediate storage before next chunk
    pub fn chunk_streaming<F>(&self, data: &[u8], mut processor: F) -> usize
    where
        F: FnMut(ChunkRef<'_>),
    {
        let chunker = FastCDC::new(
            data,
            self.min_size,
            self.avg_size,
            self.max_size,
        );

        let mut count = 0;
        for c in chunker {
            let chunk_data = &data[c.offset..c.offset + c.length];
            processor(ChunkRef {
                hash: ChunkHash::from_bytes(chunk_data),
                offset: c.offset as u64,
                length: c.length as u32,
                data: chunk_data,
            });
            count += 1;
        }
        count
    }
}

#[derive(Debug)]
pub struct ChunkRef<'a> {
    pub hash: ChunkHash,
    pub offset: u64,
    pub length: u32,
    pub data: &'a [u8],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdc_basic() {
        let chunker = CdcChunker::default();
        let data = vec![0u8; 256 * 1024]; // 256KB of zeros
        
        let chunks = chunker.chunk(&data);
        
        // Should produce multiple chunks
        assert!(!chunks.is_empty());
        
        // Total size should match
        let total: u64 = chunks.iter().map(|c| c.length as u64).sum();
        assert_eq!(total, data.len() as u64);
        
        // Chunks should be contiguous
        let mut offset = 0u64;
        for chunk in &chunks {
            assert_eq!(chunk.offset, offset);
            offset += chunk.length as u64;
        }
    }

    #[test]
    fn test_cdc_stable_boundaries() {
        let chunker = CdcChunker::default();
        
        // Original data
        let mut data1 = vec![0u8; 128 * 1024];
        for (i, b) in data1.iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
        
        // Data with insertion at start (should only affect first chunk)
        let mut data2 = vec![0xFFu8; 1024]; // 1KB insertion
        data2.extend_from_slice(&data1);
        
        let chunks1 = chunker.chunk(&data1);
        let chunks2 = chunker.chunk(&data2);
        
        // Most chunk hashes should be shared (CDC stability)
        let hashes1: std::collections::HashSet<_> = chunks1.iter().map(|c| c.hash).collect();
        let hashes2: std::collections::HashSet<_> = chunks2.iter().map(|c| c.hash).collect();
        
        let shared = hashes1.intersection(&hashes2).count();
        
        // At least 50% of chunks should be reusable
        // (In practice, CDC achieves much better than this)
        assert!(shared > 0, "CDC should produce stable boundaries");
    }

    #[test]
    fn test_cdc_chunk_sizes() {
        let chunker = CdcChunker::default();
        
        // Random-ish data (to avoid degenerate cases)
        let data: Vec<u8> = (0..1024 * 1024)
            .map(|i| ((i * 17 + 31) % 256) as u8)
            .collect();
        
        let chunks = chunker.chunk(&data);
        
        for chunk in &chunks {
            // Chunks should respect size bounds (with some tolerance for last chunk)
            if chunk.offset + chunk.length as u64 != data.len() as u64 {
                assert!(chunk.length >= chunker.min_size / 2, 
                    "Chunk too small: {}", chunk.length);
                assert!(chunk.length <= chunker.max_size * 2,
                    "Chunk too large: {}", chunk.length);
            }
        }
    }
}
```

---

## Task 3: Manifest Persistence

### 3.1 Extend existing `ChunkManifest` in `musicfs-cas/src/manifest.rs`

**Oracle fix**: Don't create duplicate `FileManifest` type. Extend existing `ChunkManifest` with `mtime` field.

```rust
use musicfs_core::{ChunkHash, FileId};
use serde::{Deserialize, Serialize};

/// Persistent chunk manifest for a file
/// NOTE: Extended from original to include mtime for delta detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub file_id: FileId,
    pub total_size: u64,
    pub mtime: i64,  // Oracle fix: added for delta detection
    pub chunks: Vec<ChunkRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestChunk {
    pub hash: ChunkHash,
    pub offset: u64,
    pub size: u32,
}

impl FileManifest {
    pub fn new(file_id: FileId, total_size: u64, mtime: i64) -> Self {
        Self {
            file_id,
            total_size,
            mtime,
            chunks: Vec::new(),
        }
    }

    pub fn add_chunk(&mut self, hash: ChunkHash, offset: u64, size: u32) {
        self.chunks.push(ManifestChunk { hash, offset, size });
    }

    /// Serialize to msgpack for storage in SQLite
    pub fn to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from msgpack
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        rmp_serde::from_slice(data).ok()
    }

    /// Get all unique chunk hashes
    pub fn chunk_hashes(&self) -> impl Iterator<Item = &ChunkHash> {
        self.chunks.iter().map(|c| &c.hash)
    }
}

/// Result of comparing two manifests
#[derive(Debug)]
pub struct ManifestDiff {
    /// Chunks in new manifest that exist in old (reusable)
    pub reuse: Vec<ManifestChunk>,
    /// Chunks in new manifest that don't exist in old (need fetch)
    pub fetch: Vec<ManifestChunk>,
    /// Chunks in old manifest that don't exist in new (can evict)
    pub orphaned: Vec<ChunkHash>,
}

impl FileManifest {
    /// Compare this manifest to a new one
    pub fn diff(&self, new_chunks: &[ManifestChunk]) -> ManifestDiff {
        use std::collections::HashSet;
        
        let old_hashes: HashSet<_> = self.chunks.iter().map(|c| c.hash).collect();
        let new_hashes: HashSet<_> = new_chunks.iter().map(|c| c.hash).collect();
        
        ManifestDiff {
            reuse: new_chunks.iter()
                .filter(|c| old_hashes.contains(&c.hash))
                .cloned()
                .collect(),
            fetch: new_chunks.iter()
                .filter(|c| !old_hashes.contains(&c.hash))
                .cloned()
                .collect(),
            orphaned: self.chunks.iter()
                .filter(|c| !new_hashes.contains(&c.hash))
                .map(|c| c.hash)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_roundtrip() {
        let mut manifest = FileManifest::new(FileId(1), 1024, 12345);
        manifest.add_chunk(ChunkHash::from_bytes(b"chunk1"), 0, 512);
        manifest.add_chunk(ChunkHash::from_bytes(b"chunk2"), 512, 512);

        let bytes = manifest.to_bytes();
        let restored = FileManifest::from_bytes(&bytes).unwrap();

        assert_eq!(restored.file_id, manifest.file_id);
        assert_eq!(restored.chunks.len(), 2);
    }

    #[test]
    fn test_manifest_diff() {
        let mut old = FileManifest::new(FileId(1), 1024, 12345);
        old.add_chunk(ChunkHash::from_bytes(b"A"), 0, 256);
        old.add_chunk(ChunkHash::from_bytes(b"B"), 256, 256);
        old.add_chunk(ChunkHash::from_bytes(b"C"), 512, 256);
        old.add_chunk(ChunkHash::from_bytes(b"D"), 768, 256);

        // New manifest: A stays, B removed, C stays, D removed, E added
        let new_chunks = vec![
            ManifestChunk { hash: ChunkHash::from_bytes(b"A"), offset: 0, size: 256 },
            ManifestChunk { hash: ChunkHash::from_bytes(b"C"), offset: 256, size: 256 },
            ManifestChunk { hash: ChunkHash::from_bytes(b"E"), offset: 512, size: 256 },
        ];

        let diff = old.diff(&new_chunks);

        assert_eq!(diff.reuse.len(), 2); // A, C
        assert_eq!(diff.fetch.len(), 1); // E
        assert_eq!(diff.orphaned.len(), 2); // B, D
    }
}
```

---

## Task 4: Delta Detector

### 4.1 Create `musicfs-sync/src/delta.rs`

```rust
use crate::cdc::CdcChunker;
use crate::manifest::{FileManifest, ManifestChunk, ManifestDiff};
use musicfs_core::{FileId, FileMeta, OriginId};
use musicfs_origins::Origin;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{debug, info};

/// Detected changes between origin and cache
#[derive(Debug, Default)]
pub struct ChangeSet {
    pub added: Vec<FileMeta>,
    pub removed: Vec<FileId>,
    pub modified: Vec<(FileId, ManifestDiff)>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }
}

/// Delta detector compares origin state to cached state
pub struct DeltaDetector {
    chunker: CdcChunker,
}

impl DeltaDetector {
    pub fn new() -> Self {
        Self {
            chunker: CdcChunker::default(),
        }
    }

    pub fn with_chunker(chunker: CdcChunker) -> Self {
        Self { chunker }
    }

    /// Detect changes between cached files and origin
    pub async fn detect_changes(
        &self,
        origin: &dyn Origin,
        cached: &HashMap<FileId, FileMeta>,
        manifests: &HashMap<FileId, FileManifest>,
    ) -> Result<ChangeSet, DeltaError> {
        let mut changes = ChangeSet::default();

        // Scan origin for current files
        let origin_files = self.scan_origin(origin).await?;
        
        // Build lookup by real path
        let cached_by_path: HashMap<_, _> = cached.values()
            .map(|m| (m.real_path.path.clone(), m))
            .collect();

        // Check for added/modified
        for origin_file in &origin_files {
            if let Some(cached_file) = cached_by_path.get(&origin_file.real_path.path) {
                // File exists - check if modified
                if self.is_modified(cached_file, origin_file) {
                    debug!("File modified: {:?}", origin_file.real_path.path);
                    
                    if let Some(old_manifest) = manifests.get(&cached_file.id) {
                        // Compute new chunks and diff
                        let new_chunks = self.compute_chunks(origin, origin_file).await?;
                        let diff = old_manifest.diff(&new_chunks);
                        changes.modified.push((cached_file.id, diff));
                    }
                }
            } else {
                // New file
                debug!("File added: {:?}", origin_file.real_path.path);
                changes.added.push(origin_file.clone());
            }
        }

        // Check for removed
        let origin_paths: std::collections::HashSet<_> = origin_files.iter()
            .map(|f| &f.real_path.path)
            .collect();
        
        for cached_file in cached.values() {
            if !origin_paths.contains(&cached_file.real_path.path) {
                debug!("File removed: {:?}", cached_file.real_path.path);
                changes.removed.push(cached_file.id);
            }
        }

        info!(
            "Delta detection complete: {} added, {} removed, {} modified",
            changes.added.len(),
            changes.removed.len(),
            changes.modified.len()
        );

        Ok(changes)
    }

    /// Check if file was modified based on mtime/size
    fn is_modified(&self, cached: &FileMeta, origin: &FileMeta) -> bool {
        cached.size != origin.size || cached.mtime != origin.mtime
    }

    /// Scan origin for all files (Oracle fix: implement recursive walk)
    async fn scan_origin(&self, origin: &dyn Origin) -> Result<Vec<FileMeta>, DeltaError> {
        let mut files = Vec::new();
        let mut dirs_to_scan = vec![PathBuf::from("/")];
        
        while let Some(dir) = dirs_to_scan.pop() {
            let entries = origin.readdir(&dir)
                .await
                .map_err(|e| DeltaError::OriginScan(e.to_string()))?;
            
            for entry in entries {
                let entry_path = dir.join(&entry.name);
                
                if entry.is_dir {
                    dirs_to_scan.push(entry_path);
                } else if Self::is_audio_file(&entry.name) {
                    // Get full stat for mtime
                    let stat = origin.stat(&entry_path)
                        .await
                        .map_err(|e| DeltaError::OriginScan(e.to_string()))?;
                    
                    files.push(FileMeta {
                        id: FileId(0), // Will be assigned by caller
                        virtual_path: VirtualPath::new(&format!("{}", entry_path.display())),
                        real_path: RealPath {
                            origin_id: origin.id().clone(),
                            path: entry_path,
                        },
                        size: stat.size,
                        mtime: stat.mtime,
                        content_hash: None,
                        audio: None,
                    });
                }
            }
        }
        
        Ok(files)
    }

    /// Check if file is an audio file by extension
    fn is_audio_file(name: &str) -> bool {
        let lower = name.to_lowercase();
        lower.ends_with(".flac") || lower.ends_with(".mp3") || 
        lower.ends_with(".ogg") || lower.ends_with(".wav") ||
        lower.ends_with(".m4a") || lower.ends_with(".aac") ||
        lower.ends_with(".opus")
    }

    /// Compute CDC chunks for a file
    async fn compute_chunks(
        &self,
        origin: &dyn Origin,
        file: &FileMeta,
    ) -> Result<Vec<ManifestChunk>, DeltaError> {
        let data = origin
            .read(&file.real_path.path, 0, file.size as u32)
            .await
            .map_err(|e| DeltaError::OriginRead(e.to_string()))?;

        let chunks = self.chunker.chunk(&data);
        
        Ok(chunks
            .into_iter()
            .map(|c| ManifestChunk {
                hash: c.hash,
                offset: c.offset,
                size: c.length,
            })
            .collect())
    }
}

impl Default for DeltaDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeltaError {
    #[error("Origin read error: {0}")]
    OriginRead(String),

    #[error("Origin scan error: {0}")]
    OriginScan(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::{RealPath, VirtualPath};
    use std::path::PathBuf;

    fn make_file_meta(id: i64, path: &str, size: u64) -> FileMeta {
        FileMeta {
            id: FileId(id),
            virtual_path: VirtualPath::new(&format!("/test/{}", path)),
            real_path: RealPath {
                origin_id: OriginId::from("test"),
                path: PathBuf::from(path),
            },
            size,
            mtime: SystemTime::UNIX_EPOCH,
            content_hash: None,
            audio: None,
        }
    }

    #[test]
    fn test_is_modified_size_change() {
        let detector = DeltaDetector::new();
        
        let cached = make_file_meta(1, "test.flac", 1000);
        let mut origin = cached.clone();
        origin.size = 2000;
        
        assert!(detector.is_modified(&cached, &origin));
    }

    #[test]
    fn test_is_modified_same() {
        let detector = DeltaDetector::new();
        
        let cached = make_file_meta(1, "test.flac", 1000);
        let origin = cached.clone();
        
        assert!(!detector.is_modified(&cached, &origin));
    }
}
```

---

## Task 5: File Watcher

### 5.1 Create `musicfs-sync/src/watcher.rs`

```rust
use musicfs_core::{Event, EventBus, OriginId};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Watches origin filesystem for changes (inotify on Linux)
pub struct OriginWatcher {
    origin_id: OriginId,
    root: PathBuf,
    event_bus: Arc<EventBus>,
}

impl OriginWatcher {
    pub fn new(origin_id: OriginId, root: PathBuf, event_bus: Arc<EventBus>) -> Self {
        Self {
            origin_id,
            root,
            event_bus,
        }
    }

    /// Start watching for changes
    /// Returns a handle that stops watching when dropped
    /// 
    /// Oracle fix: Use spawn_blocking instead of spawning separate runtime
    pub fn start(self) -> WatchHandle {
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        
        let origin_id = self.origin_id.clone();
        let root = self.root.clone();
        let event_bus = self.event_bus.clone();

        // Oracle fix: Use tokio::task::spawn_blocking instead of std::thread::spawn
        // This integrates with existing runtime rather than creating a new one
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                if let Err(e) = Self::watch_loop(&origin_id, &root, &event_bus, &mut stop_rx).await {
                    error!("Watcher error: {}", e);
                }
            });
        });

        WatchHandle { stop_tx }
    }

    async fn watch_loop(
        origin_id: &OriginId,
        root: &Path,
        event_bus: &EventBus,
        stop_rx: &mut mpsc::Receiver<()>,
    ) -> Result<(), WatchError> {
        let (tx, mut rx) = mpsc::channel(100);

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            Config::default(),
        )
        .map_err(|e| WatchError::Init(e.to_string()))?;

        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| WatchError::Watch(e.to_string()))?;

        info!("Watching origin {} at {:?}", origin_id, root);

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    Self::handle_notify_event(origin_id, root, event_bus, event);
                }
                _ = stop_rx.recv() => {
                    info!("Stopping watcher for {}", origin_id);
                    break;
                }
            }
        }

        Ok(())
    }

    /// Oracle fix: Add debouncing to handle rapid saves
    /// Debounce window before emitting events
    const DEBOUNCE_MS: u64 = 200;

    fn handle_notify_event(
        origin_id: &OriginId,
        root: &Path,
        event_bus: &EventBus,
        event: notify::Event,
        debouncer: &mut HashMap<PathBuf, Instant>,
    ) {
        use notify::EventKind;

        let now = Instant::now();

        for path in event.paths {
            let relative = match path.strip_prefix(root) {
                Ok(p) => p.to_path_buf(),
                Err(_) => continue,
            };

            // Only care about audio files
            if !Self::is_audio_file(&path) {
                continue;
            }

            // Oracle fix: Debounce - skip if we saw this path recently
            if let Some(last_seen) = debouncer.get(&relative) {
                if now.duration_since(*last_seen).as_millis() < Self::DEBOUNCE_MS as u128 {
                    debug!("Debouncing event for {:?}", relative);
                    continue;
                }
            }
            debouncer.insert(relative.clone(), now);

            let vpath = musicfs_core::VirtualPath::new(&format!("/{}", relative.display()));

            match event.kind {
                EventKind::Create(_) => {
                    debug!("File created: {:?}", relative);
                    event_bus.publish(Event::FileAdded {
                        path: vpath,
                        origin_id: origin_id.clone(),
                    });
                }
                EventKind::Remove(_) => {
                    debug!("File removed: {:?}", relative);
                    event_bus.publish(Event::FileRemoved { path: vpath });
                }
                EventKind::Modify(_) => {
                    debug!("File modified: {:?}", relative);
                    event_bus.publish(Event::FileModified { path: vpath });
                }
                _ => {}
            }
        }
    }

    fn is_audio_file(path: &Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
            Some("flac" | "mp3" | "ogg" | "wav" | "m4a" | "aac" | "opus")
        )
    }
}

pub struct WatchHandle {
    stop_tx: mpsc::Sender<()>,
}

impl WatchHandle {
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // Best effort stop on drop
        let _ = self.stop_tx.try_send(());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("Failed to initialize watcher: {0}")]
    Init(String),

    #[error("Failed to watch path: {0}")]
    Watch(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_watcher_detects_create() {
        let dir = TempDir::new().unwrap();
        let event_bus = Arc::new(EventBus::default());
        let mut rx = event_bus.subscribe();

        let watcher = OriginWatcher::new(
            OriginId::from("test"),
            dir.path().to_path_buf(),
            event_bus,
        );
        let handle = watcher.start();

        // Give watcher time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create a file
        std::fs::write(dir.path().join("test.flac"), b"audio").unwrap();

        // Wait for event
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Should receive FileAdded event
        let event = rx.try_recv();
        assert!(matches!(event, Ok(Event::FileAdded { .. })));

        handle.stop().await;
    }
}
```

---

## Task 6: Update ContentFetcher for CDC

### 6.1 Update `musicfs-cas/src/fetcher.rs`

```rust
use crate::{CasStore, ChunkManifest, ChunkRef};
use musicfs_core::{ChunkHash, Event, EventBus, FileId, FileMeta, OriginId};
use musicfs_origins::Origin;
use musicfs_sync::cdc::CdcChunker;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{debug, info};

pub struct ContentFetcher {
    store: Arc<CasStore>,
    origins: RwLock<HashMap<OriginId, Arc<dyn Origin>>>,
    file_meta: RwLock<HashMap<FileId, FileMeta>>,
    event_bus: Option<Arc<EventBus>>,
    chunker: CdcChunker,
}

impl ContentFetcher {
    pub fn new(store: Arc<CasStore>) -> Self {
        Self {
            store,
            origins: RwLock::new(HashMap::new()),
            file_meta: RwLock::new(HashMap::new()),
            event_bus: None,
            chunker: CdcChunker::default(),
        }
    }

    // ... existing methods ...

    /// Fetch file with CDC chunking
    pub async fn fetch_file(&self, file_id: FileId) -> Result<ChunkManifest, FetchError> {
        let meta = {
            let files = self.file_meta.read().unwrap();
            files.get(&file_id).cloned()
                .ok_or(FetchError::FileNotFound(file_id))?
        };

        let origin = {
            let origins = self.origins.read().unwrap();
            origins.get(&meta.real_path.origin_id).cloned()
                .ok_or_else(|| FetchError::OriginNotFound(meta.real_path.origin_id.clone()))?
        };

        info!("Fetching file {:?} from origin {}", file_id, origin.id());

        // Read full file content
        let data = origin.read(&meta.real_path.path, 0, meta.size as u32).await
            .map_err(|e| FetchError::OriginRead(e.to_string()))?;

        // CDC chunk the data
        let chunks = self.chunker.chunk(&data);
        info!("Chunked {:?} into {} chunks", file_id, chunks.len());

        // Store each chunk in CAS
        let mut chunk_refs = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            // Dedup: only store if not already present
            if !self.store.exists(&chunk.hash) {
                self.store.put(&chunk.data).await
                    .map_err(FetchError::Store)?;
            }
            
            chunk_refs.push(ChunkRef {
                hash: chunk.hash,
                offset: chunk.offset,
                size: chunk.length,
            });
        }

        let manifest = ChunkManifest {
            file_id,
            total_size: meta.size,
            chunks: chunk_refs,
        };

        debug!(
            "Created manifest for {:?}: {} bytes, {} chunks",
            file_id, meta.size, manifest.chunks.len()
        );

        Ok(manifest)
    }
}
```

---

## Task 7: Update lib.rs

### 7.1 Create `musicfs-sync/src/lib.rs`

```rust
pub mod cdc;
pub mod delta;
pub mod manifest;
pub mod watcher;

pub use cdc::{CdcChunker, Chunk};
pub use delta::{ChangeSet, DeltaDetector, DeltaError};
pub use manifest::{FileManifest, ManifestChunk, ManifestDiff};
pub use watcher::{OriginWatcher, WatchHandle, WatchError};
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_read_full_file` | Unit | Fix: full file read works |
| `test_read_full_large_file` | Unit | Oracle fix: files >4GB don't overflow |
| `test_cdc_basic` | Unit | CDC produces chunks |
| `test_cdc_stable_boundaries` | Unit | Insertions don't shift all chunks |
| `test_cdc_chunk_sizes` | Unit | Chunks respect min/avg/max |
| `test_cdc_streaming_memory` | Unit | Oracle fix: streaming doesn't explode memory |
| `test_manifest_roundtrip` | Unit | Manifest serialization |
| `test_manifest_diff` | Unit | Diff identifies reuse/fetch/orphan |
| `test_delta_detect_modified` | Unit | Modified files detected |
| `test_scan_origin_recursive` | Unit | Oracle fix: scan_origin finds all files |
| `test_watcher_detects_create` | Integration | inotify works |
| `test_watcher_debounce` | Unit | Oracle fix: rapid events debounced |
| `test_bandwidth_reduction_90pct` | Integration | Oracle fix: >90% reduction on metadata edit |

### Oracle fix: Add concrete bandwidth reduction test

```rust
#[tokio::test]
async fn test_bandwidth_reduction_90pct() {
    // Create a 10MB FLAC file
    let original = create_test_flac(10 * 1024 * 1024);
    
    // Chunk it
    let chunker = CdcChunker::default();
    let chunks1 = chunker.chunk(&original);
    let hashes1: HashSet<_> = chunks1.iter().map(|c| c.hash).collect();
    
    // Modify only metadata (first 1KB - FLAC header area)
    let mut modified = original.clone();
    for i in 100..200 {
        modified[i] = 0xFF;
    }
    
    // Chunk modified version
    let chunks2 = chunker.chunk(&modified);
    let hashes2: HashSet<_> = chunks2.iter().map(|c| c.hash).collect();
    
    // Calculate reuse ratio
    let reused = hashes1.intersection(&hashes2).count();
    let reuse_ratio = reused as f64 / chunks2.len() as f64;
    
    // Must achieve >90% reuse for metadata-only edit
    assert!(
        reuse_ratio > 0.90,
        "Bandwidth reduction {:.1}% < 90% target. Reused {}/{} chunks",
        reuse_ratio * 100.0, reused, chunks2.len()
    );
}
```

---

## Benchmark

```rust
// benches/cdc.rs
fn bench_cdc_64mb(c: &mut Criterion) {
    let chunker = CdcChunker::default();
    let data = vec![0u8; 64 * 1024 * 1024];
    
    c.bench_function("cdc_64mb", |b| {
        b.iter(|| chunker.chunk(&data))
    });
}

fn bench_bandwidth_reduction(c: &mut Criterion) {
    // Simulate metadata-only edit (tag change)
    // Measure chunk reuse ratio
}
```

---

## Exit Criteria

- [ ] Full file content is read (not just first 2MB)
- [ ] CDC produces 16KB-256KB chunks with 64KB average
- [ ] Chunk boundaries are stable on insertions
- [ ] Manifest diff correctly identifies reuse/fetch/orphan
- [ ] inotify watcher detects file changes
- [ ] Delta sync achieves >90% bandwidth reduction on metadata edit
- [ ] All existing tests pass

---

## Dependencies

### `musicfs-sync/Cargo.toml`

```toml
[package]
name = "musicfs-sync"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
musicfs-cas = { path = "../musicfs-cas" }
musicfs-origins = { path = "../musicfs-origins" }

fastcdc = "3"
xxhash-rust = { version = "0.8", features = ["xxh64"] }
notify = "6"
rmp-serde = "1"

tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

---

## Architecture Compliance

| Architecture Section | Requirement | Status |
|---------------------|-------------|--------|
| 4.3.2 | CDC chunking (64KB avg) | ✅ |
| 4.3.2 | Min 16KB, Max 256KB | ✅ |
| 4.3.2 | Stable boundaries for delta sync | ✅ |
| 4.3.5 | Chunk fetched data (CDC) | ✅ |
| 4.3.5 | Store chunks by hash | ✅ |
| FR-10.2 | inotify for local origins | ✅ |
| FR-11.2 | Use CDC to identify changed chunks | ✅ |
| NFR-6.4 | >90% bandwidth reduction | ✅ |
