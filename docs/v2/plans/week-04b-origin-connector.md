# Week 4b: Origin-CAS Connector

**Phase**: 1 (MVP)  
**Prerequisites**: Week 4 (CAS & Chunk Caching)  
**Estimated effort**: 1 day

---

## Objective

Bridge the gap between Origin (source files) and CAS (chunk cache) to enable actual file reads through FUSE. This implements the "cache miss" flow from architecture section 4.3.5.

**Problem**: Week 4 implemented CAS storage and FileReader, but there's no code that:
1. Detects when requested chunks aren't cached
2. Fetches data from Origin
3. Stores chunks in CAS
4. Creates ChunkManifest for the file

**Solution**: Create `ContentFetcher` that orchestrates Origin → CAS data flow on cache miss.

---

## Architecture Reference

From architecture.md section 4.3.5 (Read Operation Activity):

```
|CAS|
:compute chunk range for [offset, offset+size];
if (all chunks cached?) then (yes)
    :read from local chunk files;
else (no)
    |OriginFederation|
    :select healthy origin by priority;
    :fetch missing byte range;
    |CAS|
    :chunk fetched data (CDC);
    :store chunks by hash;
    :update chunk manifest;
endif
```

---

## Deliverables

| Task | Crate | Files | Done |
|------|-------|-------|------|
| ContentFetcher implementation | musicfs-cas | `fetcher.rs` | [ ] |
| FileId → FileMeta resolver | musicfs-cas | `fetcher.rs` | [ ] |
| Update FileReader for cache-miss | musicfs-cas | `reader.rs` | [ ] |
| Update FUSE with fetcher | musicfs-fuse | `filesystem.rs` | [ ] |
| E2E test: cat file through FUSE | tests | `integration.rs` | [ ] |

---

## Task 1: ContentFetcher

### 1.1 Create `musicfs-cas/src/fetcher.rs`

```rust
use crate::{CasStore, ChunkManifest, ChunkRef};
use musicfs_core::{Event, EventBus, FileId, FileMeta, OriginId, RealPath};
use musicfs_origins::Origin;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tracing::{debug, info};

pub struct ContentFetcher {
    store: Arc<CasStore>,
    origins: RwLock<HashMap<OriginId, Arc<dyn Origin>>>,
    file_meta: RwLock<HashMap<FileId, FileMeta>>,
    event_bus: Option<Arc<EventBus>>,
}

impl ContentFetcher {
    pub fn new(store: Arc<CasStore>) -> Self {
        Self {
            store,
            origins: RwLock::new(HashMap::new()),
            file_meta: RwLock::new(HashMap::new()),
            event_bus: None,
        }
    }

    pub fn with_event_bus(store: Arc<CasStore>, event_bus: Arc<EventBus>) -> Self {
        Self {
            store,
            origins: RwLock::new(HashMap::new()),
            file_meta: RwLock::new(HashMap::new()),
            event_bus: Some(event_bus),
        }
    }

    pub fn register_origin(&self, origin: Arc<dyn Origin>) {
        let id = origin.id().clone();
        self.origins.write().unwrap().insert(id, origin);
    }

    pub fn register_file(&self, meta: FileMeta) {
        self.file_meta.write().unwrap().insert(meta.id, meta);
    }

    pub fn register_files(&self, files: impl IntoIterator<Item = FileMeta>) {
        let mut map = self.file_meta.write().unwrap();
        for meta in files {
            map.insert(meta.id, meta);
        }
    }

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

        let data = origin.read(&meta.real_path.path, 0, meta.size as u32).await
            .map_err(|e| FetchError::OriginRead(e.to_string()))?;

        let hash = self.store.put(&data).await
            .map_err(FetchError::Store)?;

        let manifest = ChunkManifest {
            file_id,
            total_size: meta.size,
            chunks: vec![ChunkRef {
                hash,
                offset: 0,
                size: data.len() as u32,
            }],
        };

        debug!("Created manifest for {:?}: {} bytes, 1 chunk", file_id, meta.size);

        Ok(manifest)
    }

    pub fn emit_access_event(&self, meta: &FileMeta, offset: u64, size: u32) {
        if let Some(bus) = &self.event_bus {
            bus.publish(Event::FileAccessed {
                path: meta.virtual_path.clone(),
                origin_id: meta.real_path.origin_id.clone(),
                offset,
                size,
            });
        }
    }

    pub async fn ensure_cached(&self, file_id: FileId) -> Result<ChunkManifest, FetchError> {
        self.fetch_file(file_id).await
    }

    pub fn get_file_meta(&self, file_id: FileId) -> Option<FileMeta> {
        self.file_meta.read().unwrap().get(&file_id).cloned()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("File not found: {0:?}")]
    FileNotFound(FileId),

    #[error("Origin not found: {0}")]
    OriginNotFound(OriginId),

    #[error("Origin read error: {0}")]
    OriginRead(String),

    #[error("Store error: {0}")]
    Store(#[from] crate::CasError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CasConfig;
    use musicfs_core::VirtualPath;
    use musicfs_origins::LocalOrigin;
    use std::path::PathBuf;
    use std::time::SystemTime;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_fetch_file() {
        let cas_dir = TempDir::new().unwrap();
        let origin_dir = TempDir::new().unwrap();

        std::fs::write(origin_dir.path().join("test.flac"), b"fake audio data").unwrap();

        let config = CasConfig {
            chunks_dir: cas_dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(config).await.unwrap());
        let fetcher = ContentFetcher::new(store.clone());

        let origin = Arc::new(LocalOrigin::new("local", origin_dir.path()));
        fetcher.register_origin(origin);

        let meta = FileMeta {
            id: FileId(1),
            virtual_path: VirtualPath::new("/Artist/Album/test.flac"),
            real_path: RealPath {
                origin_id: OriginId::from("local"),
                path: PathBuf::from("/test.flac"),
            },
            size: 15,
            mtime: SystemTime::now(),
            content_hash: None,
            audio: None,
        };
        fetcher.register_file(meta);

        let manifest = fetcher.fetch_file(FileId(1)).await.unwrap();
        assert_eq!(manifest.total_size, 15);
        assert_eq!(manifest.chunks.len(), 1);

        let data = store.get(&manifest.chunks[0].hash).await.unwrap();
        assert_eq!(&data[..], b"fake audio data");
    }

    #[tokio::test]
    async fn test_fetch_file_not_found() {
        let cas_dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: cas_dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(config).await.unwrap());
        let fetcher = ContentFetcher::new(store);

        let result = fetcher.fetch_file(FileId(999)).await;
        assert!(matches!(result, Err(FetchError::FileNotFound(_))));
    }

    #[tokio::test]
    async fn test_fetch_emits_event() {
        let cas_dir = TempDir::new().unwrap();
        let origin_dir = TempDir::new().unwrap();
        std::fs::write(origin_dir.path().join("test.flac"), b"audio").unwrap();

        let config = CasConfig {
            chunks_dir: cas_dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(config).await.unwrap());
        let event_bus = Arc::new(EventBus::default());
        let mut rx = event_bus.subscribe();

        let fetcher = ContentFetcher::with_event_bus(store, event_bus);
        let origin = Arc::new(LocalOrigin::new("local", origin_dir.path()));
        fetcher.register_origin(origin);

        let meta = FileMeta {
            id: FileId(1),
            virtual_path: VirtualPath::new("/Artist/test.flac"),
            real_path: RealPath {
                origin_id: OriginId::from("local"),
                path: PathBuf::from("/test.flac"),
            },
            size: 5,
            mtime: SystemTime::now(),
            content_hash: None,
            audio: None,
        };
        fetcher.register_file(meta.clone());

        fetcher.emit_access_event(&meta, 0, 5);

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, Event::FileAccessed { .. }));
    }
}
```

---

## Task 2: Update FileReader

### 2.1 Update `musicfs-cas/src/reader.rs`

Add fetcher integration for cache-miss handling:

```rust
use crate::fetcher::{ContentFetcher, FetchError};

pub struct FileReader {
    store: Arc<CasStore>,
    fetcher: Option<Arc<ContentFetcher>>,
    manifests: RwLock<HashMap<FileId, ChunkManifest>>,
}

impl FileReader {
    pub fn new(store: Arc<CasStore>) -> Self {
        Self {
            store,
            fetcher: None,
            manifests: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_fetcher(store: Arc<CasStore>, fetcher: Arc<ContentFetcher>) -> Self {
        Self {
            store,
            fetcher: Some(fetcher),
            manifests: RwLock::new(HashMap::new()),
        }
    }

    pub async fn read(
        &self,
        file_id: FileId,
        offset: u64,
        size: u32,
    ) -> Result<Bytes, ReaderError> {
        let manifest = self.get_or_fetch_manifest(file_id).await?;
        
        if let Some(fetcher) = &self.fetcher {
            if let Some(meta) = fetcher.get_file_meta(file_id) {
                fetcher.emit_access_event(&meta, offset, size);
            }
        }
        
        // ... rest of read logic unchanged
    }

    async fn get_or_fetch_manifest(&self, file_id: FileId) -> Result<ChunkManifest, ReaderError> {
        {
            let manifests = self.manifests.read().unwrap();
            if let Some(m) = manifests.get(&file_id) {
                return Ok(m.clone());
            }
        }

        let Some(fetcher) = &self.fetcher else {
            return Err(ReaderError::ManifestNotFound(file_id));
        };

        let manifest = fetcher.ensure_cached(file_id).await
            .map_err(ReaderError::Fetch)?;

        self.manifests.write().unwrap().insert(file_id, manifest.clone());
        Ok(manifest)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error("Manifest not found for file {0:?}")]
    ManifestNotFound(FileId),

    #[error("Fetch error: {0}")]
    Fetch(#[from] FetchError),

    #[error("CAS error: {0}")]
    Cas(#[from] crate::CasError),
}
```

---

## Task 3: Update lib.rs

### 3.1 Update `musicfs-cas/src/lib.rs`

```rust
mod chunks;
mod fetcher;
mod reader;
mod store;

pub use chunks::{ChunkLocation, ChunkRef};
pub use fetcher::{ContentFetcher, FetchError};
pub use reader::{ChunkManifest, FileReader, ReaderError};
pub use store::{CasConfig, CasError, CasStore, DedupStats};
```

---

## Task 4: Update Cargo.toml

### 4.1 Update `musicfs-cas/Cargo.toml`

```toml
[dependencies]
musicfs-core = { path = "../musicfs-core" }
musicfs-origins = { path = "../musicfs-origins" }
# ... rest unchanged
```

---

## Task 5: Update FUSE Integration

### 5.1 Update `musicfs-fuse/src/filesystem.rs`

```rust
use musicfs_cas::{ContentFetcher, FileReader};

pub struct MusicFs {
    tree: Arc<RwLock<VirtualTree>>,
    reader: Option<Arc<FileReader>>,
    fetcher: Option<Arc<ContentFetcher>>,
    uid: u32,
    gid: u32,
}

impl MusicFs {
    pub fn with_content_access(
        tree: Arc<RwLock<VirtualTree>>,
        reader: Arc<FileReader>,
        fetcher: Arc<ContentFetcher>,
    ) -> Self {
        Self {
            tree,
            reader: Some(reader),
            fetcher: Some(fetcher),
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
    }
}
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_fetch_file` | Unit | Origin → CAS fetch works |
| `test_fetch_file_not_found` | Unit | Missing file error |
| `test_fetch_emits_event` | Unit | FileAccessed event emitted (FR-18.1) |
| `test_reader_with_fetcher` | Unit | Cache-miss triggers fetch |
| `test_e2e_cat_file` | Integration | `cat` returns file content |

---

## Exit Criteria

- [ ] `ContentFetcher` fetches from Origin and stores in CAS
- [ ] `FileReader` calls fetcher on cache miss
- [ ] File metadata (FileId → FileMeta) is resolvable
- [ ] `cat /mnt/musicfs/Artist/Album/track.flac` returns actual audio data
- [ ] All existing tests still pass

---

## Dependencies

### `musicfs-cas/Cargo.toml`

```toml
[dependencies]
musicfs-origins = { path = "../musicfs-origins" }
```

---

## Implementation Notes

1. **Week 4 treated whole files as single chunks** - this continues that approach
2. **CDC chunking deferred to Week 5** - fetcher will be updated then
3. **No OriginFederation yet** - single origin lookup for MVP
4. **FileMeta registration** - caller must register files before they can be fetched
5. **EventBus integration** - emits `FileAccessed` event per FR-18.1 (P0)
6. **Full file fetch** - currently fetches entire file on cache miss; byte-range optimization deferred

## Architecture Compliance

| Architecture Section | Requirement | Status |
|---------------------|-------------|--------|
| 4.3.5 | Cache miss → fetch from origin | ✅ |
| 4.3.5 | Store chunks by hash | ✅ |
| 4.3.5 | Update chunk manifest | ✅ |
| 4.3.5 | Emit FileAccessed event | ✅ |
| 4.3.3 | OriginFederation (multi-origin) | ⏳ Deferred |
| 4.3.5 | Byte-range fetch | ⏳ Deferred |
| 4.3.5 | CDC chunking | ⏳ Week 5 |

---

## Next Steps

After this, the MVP is complete:
- Mount filesystem
- Browse virtual tree (Artist/Album/Track)
- Read actual file content through FUSE
- Audio playback works

Week 5 adds CDC chunking for efficient delta sync.
