# MusicFS Development Plan

**Version**: 1.0  
**Date**: 2026-05-12  
**Status**: Draft  
**Prerequisites**: [requirements.md](requirements.md), [architecture.md](architecture.md)

---

## 1. Overview

This plan breaks down the 11-week implementation into specific deliverables with Rust modules, test requirements, and acceptance criteria mapped to requirements.

### 1.1 Project Structure

```
musicfs/
├── Cargo.toml                    # Workspace root
├── proto/
│   └── musicfs.proto             # gRPC definitions
├── crates/
│   ├── musicfs-core/             # Core types, traits, errors
│   ├── musicfs-fuse/             # FUSE filesystem implementation
│   ├── musicfs-cache/            # Three-tier caching (L1/L2/L3)
│   ├── musicfs-cas/              # Content-addressable storage
│   ├── musicfs-sync/             # Delta sync, CDC chunking
│   ├── musicfs-origins/          # Origin plugins (local, s3, sftp)
│   ├── musicfs-metadata/         # Audio metadata extraction
│   ├── musicfs-search/           # Full-text search (tantivy)
│   ├── musicfs-plugins/          # Plugin host (native + WASM)
│   ├── musicfs-grpc/             # gRPC control API
│   └── musicfs-cli/              # CLI binary
├── tests/
│   ├── integration/              # Cross-crate integration tests
│   └── e2e/                      # End-to-end FUSE tests
└── benches/                      # Criterion benchmarks
```

### 1.2 Dependency Graph

```
                    musicfs-cli
                         │
              ┌──────────┼──────────┐
              │          │          │
              ▼          ▼          ▼
        musicfs-grpc  musicfs-fuse  musicfs-search
              │          │               │
              └────┬─────┴───────────────┘
                   │
                   ▼
             musicfs-core
              /    |    \
             /     |     \
            ▼      ▼      ▼
    musicfs-cache  musicfs-origins  musicfs-metadata
         │              │
         ▼              │
    musicfs-cas ◄───────┘
         │
         ▼
    musicfs-sync
```

---

## 2. Phase 1: MVP (Weeks 1-4)

**Goal**: Basic functional filesystem with single local origin.

**Requirements Covered**: FR-1, FR-2, FR-3, FR-4, FR-5, FR-6, FR-7, FR-8, FR-9, FR-18, NFR-1.1-1.7

---

### Week 1: Foundation

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Workspace setup | root | `Cargo.toml`, `.cargo/config.toml` | - |
| Core types | musicfs-core | `lib.rs`, `error.rs`, `types.rs` | - |
| Event Bus | musicfs-core | `events.rs` | FR-18.1-18.4 |
| FUSE skeleton | musicfs-fuse | `lib.rs`, `filesystem.rs` | FR-1.1, FR-1.2 |
| Local origin | musicfs-origins | `lib.rs`, `local.rs`, `traits.rs` | FR-12.1 |
| Nix flake | root | `flake.nix` | - |

#### Core Types (`musicfs-core/src/types.rs`)

```rust
// Virtual path in metadata-organized tree
pub struct VirtualPath(PathBuf);

// Real path on origin
pub struct RealPath {
    pub origin_id: OriginId,
    pub path: PathBuf,
}

// File metadata
pub struct FileMeta {
    pub id: FileId,
    pub virtual_path: VirtualPath,
    pub real_path: RealPath,
    pub size: u64,
    pub mtime: SystemTime,
    pub content_hash: ContentHash,
    pub audio: Option<AudioMeta>,
}

pub struct AudioMeta {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub track_number: Option<u32>,
    pub duration_ms: Option<u64>,
    pub format: AudioFormat,
}

// Content-addressable hash (per architecture 8.3)
pub struct ContentHash([u8; 8]);   // xxHash64 for file-level dedup
pub struct ChunkHash([u8; 8]);     // xxHash64 for chunk-level dedup
```

#### Event Bus (`musicfs-core/src/events.rs`)

```rust
use tokio::sync::broadcast;

/// Central event bus for system-wide notifications (per architecture 4.2)
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }
    
    pub fn publish(&self, event: Event) {
        let _ = self.sender.send(event);  // Ignore if no receivers
    }
    
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    FileAdded { path: VirtualPath, origin_id: OriginId },
    FileRemoved { path: VirtualPath },
    FileModified { path: VirtualPath },
    OriginConnected { origin_id: OriginId },
    OriginDisconnected { origin_id: OriginId },
    SyncStarted { origin_id: OriginId },
    SyncCompleted { origin_id: OriginId, files_changed: u64 },
    CacheEviction { bytes_freed: u64 },
}
```

#### Origin Trait (`musicfs-origins/src/traits.rs`)

```rust
#[async_trait]
pub trait Origin: Send + Sync {
    fn id(&self) -> &OriginId;
    fn origin_type(&self) -> OriginType;
    
    /// List entries in directory
    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>>;
    
    /// Get file metadata
    async fn stat(&self, path: &Path) -> Result<FileStat>;
    
    /// Read file content
    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Bytes>;
    
    /// Check if path exists
    async fn exists(&self, path: &Path) -> Result<bool>;
    
    /// Health check
    async fn health(&self) -> HealthStatus;
}
```

#### FUSE Skeleton (`musicfs-fuse/src/filesystem.rs`)

```rust
pub struct MusicFs {
    origins: Arc<OriginRegistry>,
    cache: Arc<CacheManager>,
    tree: Arc<RwLock<VirtualTree>>,
}

impl fuser::Filesystem for MusicFs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry);
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr);
    fn readdir(&mut self, _req: &Request, ino: u64, fh: u64, offset: i64, reply: ReplyDirectory);
    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen);
    fn read(&mut self, _req: &Request, ino: u64, fh: u64, offset: i64, size: u32, ...);
    fn release(&mut self, _req: &Request, ino: u64, fh: u64, ...);
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_workspace_builds` | Unit | All crates compile |
| `test_local_origin_readdir` | Unit | Local origin lists files |
| `test_fuse_mount_unmount` | Integration | Mount/unmount works (FR-1.1, FR-1.2) |

#### Exit Criteria

- [ ] `cargo build` succeeds for all crates
- [ ] `cargo test` passes
- [ ] FUSE mount creates mount point
- [ ] FUSE unmount removes mount point

---

### Week 2: Metadata Extraction

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Audio parsing | musicfs-metadata | `lib.rs`, `parser.rs`, `formats/` | FR-6.1-6.5 |
| Format detection | musicfs-metadata | `formats/flac.rs`, `formats/mp3.rs`, etc. | FR-24.1 |
| SQLite schema | musicfs-cache | `schema.sql`, `db.rs` | FR-7.1-7.4 |
| Metadata cache | musicfs-cache | `metadata.rs` | FR-7.1 |

#### Metadata Parser (`musicfs-metadata/src/parser.rs`)

```rust
pub struct MetadataParser {
    symphonia: SymphoniaParser,
}

impl MetadataParser {
    /// Extract metadata from audio file
    pub fn parse(&self, reader: impl Read + Seek) -> Result<AudioMeta> {
        // Uses symphonia for format-agnostic parsing
    }
    
    /// Extract just the header (first N KB for metadata overlay)
    pub fn parse_header(&self, reader: impl Read + Seek) -> Result<(AudioMeta, Bytes)> {
        // Returns metadata + raw header bytes for caching
    }
}
```

#### SQLite Schema (`musicfs-cache/src/schema.sql`)

```sql
-- As defined in architecture.md section 4.3.6
-- NOTE: Chunk index stored in sled (chunks.sled/), NOT SQLite

CREATE TABLE files (
    id              INTEGER PRIMARY KEY,
    origin_id       TEXT NOT NULL,      -- Origin identifier
    real_path       TEXT NOT NULL,      -- Path on origin
    virtual_path    TEXT NOT NULL,      -- Metadata-derived path
    
    -- Audio metadata
    title           TEXT,
    artist          TEXT,
    album           TEXT,
    track_number    INTEGER,
    duration_ms     INTEGER,
    bitrate         INTEGER,
    sample_rate     INTEGER,
    format          TEXT,
    
    -- Sync state
    origin_mtime    INTEGER,
    origin_size     INTEGER,
    content_hash    TEXT,
    chunk_manifest  BLOB,       -- msgpack: [(chunk_hash, offset, size)]
    last_sync       INTEGER,
    
    UNIQUE(origin_id, real_path)
);

CREATE TABLE artwork (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER REFERENCES files(id),
    art_type    TEXT,           -- 'front', 'back'
    chunk_hash  TEXT,           -- reference to CAS
    width       INTEGER,
    height      INTEGER,
    UNIQUE(file_id, art_type)
);

CREATE TABLE collections (
    id          INTEGER PRIMARY KEY,
    name        TEXT UNIQUE,
    query_json  TEXT,           -- smart collection query
    created_at  INTEGER
);

-- Indexes for performance (NFR-1.1, NFR-1.2)
CREATE INDEX idx_virtual ON files(virtual_path);
CREATE INDEX idx_artist_album ON files(artist, album);
CREATE INDEX idx_content_hash ON files(content_hash);
```

#### Sled Chunk Index (`musicfs-cas/chunks.sled/`)

```rust
// Chunk hash → storage location (NOT in SQLite per architecture 4.3.2)
// Key: ChunkHash (8 bytes, xxHash64)
// Value: ChunkLocation { path: PathBuf, offset: u64, size: u32 }
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_parse_flac` | Unit | FLAC metadata extraction (FR-6.1-6.5) |
| `test_parse_mp3_id3v2` | Unit | MP3 ID3v2 tags |
| `test_parse_mp3_id3v1` | Unit | MP3 ID3v1 fallback |
| `test_parse_opus` | Unit | Opus/Vorbis comments |
| `test_metadata_cache_insert` | Unit | SQLite insert/query |
| `test_metadata_cache_persistence` | Integration | Data survives restart (FR-7.4) |

#### Exit Criteria

- [ ] Parse FLAC, MP3, Opus, M4A metadata
- [ ] Extract: artist, album, title, track, duration, format
- [ ] SQLite schema created and migrated
- [ ] Metadata persists across restarts

---

### Week 3: Virtual Tree & Basic Ops

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Virtual path resolver | musicfs-core | `resolver.rs` | FR-5.1, FR-5.2 |
| Tree cache | musicfs-cache | `tree.rs` | FR-9.1-9.4 |
| readdir impl | musicfs-fuse | `ops/readdir.rs` | FR-2.1-2.3 |
| stat impl | musicfs-fuse | `ops/stat.rs` | FR-2.1 |
| open/read impl | musicfs-fuse | `ops/read.rs` | FR-3.1-3.5 |

#### Virtual Path Resolver (`musicfs-core/src/resolver.rs`)

```rust
pub struct PathResolver {
    templates: Vec<PathTemplate>,
}

impl PathResolver {
    /// Map real path + metadata → virtual path
    pub fn resolve(&self, real: &RealPath, meta: &AudioMeta) -> VirtualPath {
        // Template: "/{artist}/{album}/{track:02} - {title}.{ext}"
    }
    
    /// Reverse lookup: virtual path → file ID
    pub fn lookup(&self, virtual_path: &VirtualPath, tree: &VirtualTree) -> Option<FileId> {
        // O(1) hash lookup
    }
}

pub struct PathTemplate {
    pub pattern: String,
    pub fallback_artist: String,  // "Unknown Artist"
    pub fallback_album: String,   // "Unknown Album"
}
```

#### Tree Cache (`musicfs-cache/src/tree.rs`)

```rust
pub struct VirtualTree {
    root: DirNode,
    inode_map: HashMap<u64, NodeRef>,
    path_map: HashMap<VirtualPath, u64>,
    next_inode: AtomicU64,
}

pub enum VirtualNode {
    Dir(DirNode),
    File(FileNode),
}

pub struct DirNode {
    pub inode: u64,
    pub name: OsString,
    pub children: BTreeMap<OsString, NodeRef>,
    pub mtime: SystemTime,
}

pub struct FileNode {
    pub inode: u64,
    pub name: OsString,
    pub file_id: FileId,
    pub size: u64,
    pub mtime: SystemTime,
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_resolve_complete_metadata` | Unit | Full path resolution (FR-5.1) |
| `test_resolve_missing_album` | Unit | Fallback handling (FR-5.2) |
| `test_tree_readdir` | Unit | Directory listing (FR-2.1) |
| `test_tree_stat` | Unit | File attributes (FR-2.1) |
| `test_fuse_readdir` | Integration | FUSE readdir (FR-2.2, NFR-1.2) |
| `test_fuse_stat` | Integration | FUSE stat (NFR-1.1) |
| `test_fuse_read` | Integration | FUSE read (FR-3.1) |
| `test_read_only_enforcement` | Integration | Write ops fail (FR-4.1-4.4) |

#### Benchmark

```rust
// benches/tree_ops.rs
fn bench_stat_cached(c: &mut Criterion) {
    // Target: <1ms p99 (NFR-1.1)
}

fn bench_readdir_1000_entries(c: &mut Criterion) {
    // Target: <10ms p99 (NFR-1.2)
}

fn bench_mount_time(c: &mut Criterion) {
    // Target: <100ms, Max: <500ms (NFR-1.7)
    // Mount must be O(1), not O(files)
}
```

#### Exit Criteria

- [ ] Virtual tree built from metadata
- [ ] `ls /mnt/music` shows Artist directories
- [ ] `ls /mnt/music/Artist/Album` shows tracks
- [ ] `stat` returns correct size, mtime
- [ ] `cat` reads file content
- [ ] Write operations return EROFS (FR-4.1)
- [ ] Mount completes in <500ms (NFR-1.7)

---

### Week 4: CAS & Chunk Caching

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| CAS implementation | musicfs-cas | `lib.rs`, `store.rs` | FR-20.1-20.4 |
| Chunk storage | musicfs-cas | `chunks.rs` | FR-8.1-8.4 |
| Cache eviction | musicfs-cache | `eviction.rs` | FR-8.2 |
| Integration tests | tests/integration | `basic_mount.rs` | FR-1, FR-2, FR-3 |

#### CAS Store (`musicfs-cas/src/store.rs`)

```rust
pub struct CasStore {
    chunks_dir: PathBuf,
    index: sled::Db,  // hash → chunk location
}

impl CasStore {
    /// Store chunk, returns hash
    pub async fn put(&self, data: &[u8]) -> Result<ChunkHash> {
        let hash = xxhash64(data);
        let path = self.chunk_path(&hash);
        if !path.exists() {
            tokio::fs::write(&path, data).await?;
        }
        Ok(hash)
    }
    
    /// Retrieve chunk by hash
    pub async fn get(&self, hash: &ChunkHash) -> Result<Bytes> {
        let path = self.chunk_path(hash);
        Ok(tokio::fs::read(&path).await?.into())
    }
    
    /// Check existence (for dedup)
    pub fn exists(&self, hash: &ChunkHash) -> bool {
        self.chunk_path(hash).exists()
    }
}
```

#### Cache Eviction (`musicfs-cache/src/eviction.rs`)

```rust
pub struct LruEviction {
    max_size: u64,
    current_size: AtomicU64,
    access_log: RwLock<BTreeMap<Instant, ChunkHash>>,
}

impl LruEviction {
    /// Evict chunks until under limit
    pub async fn evict_to_target(&self, store: &CasStore, target: u64) -> Result<u64> {
        // LRU eviction based on access time
        // Returns bytes freed
    }
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_cas_put_get` | Unit | Basic store/retrieve (FR-20.1) |
| `test_cas_dedup` | Unit | Same content → same hash (FR-20.2) |
| `test_cas_integrity` | Unit | Verify chunk hash (FR-20.4) |
| `test_cache_eviction` | Unit | LRU eviction works (FR-8.2) |
| `test_cache_persistence` | Integration | Survives restart (FR-8.4) |
| `test_mount_play_audio` | E2E | mpv can play file through FUSE |

#### Exit Criteria

- [ ] Chunks stored in CAS with deduplication
- [ ] Cache size limit enforced via eviction
- [ ] Audio playback works through mounted filesystem
- [ ] Cache persists across daemon restarts
- [ ] All Phase 1 requirements pass acceptance tests

---

## 3. Phase 2: Delta Sync & Multi-Origin (Weeks 5-7)

**Goal**: Efficient synchronization and origin federation.

**Requirements Covered**: FR-10, FR-11, FR-12, FR-13, NFR-4, NFR-5

---

### Week 5: CDC & Delta Detection

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| FastCDC integration | musicfs-sync | `cdc.rs` | FR-11.2 |
| Manifest storage | musicfs-sync | `manifest.rs` | FR-11.3 |
| Delta detection | musicfs-sync | `delta.rs` | FR-10.1-10.4, FR-11.1 |
| Change watcher | musicfs-sync | `watcher.rs` | FR-10.3 |

#### CDC Chunking (`musicfs-sync/src/cdc.rs`)

```rust
pub struct CdcChunker {
    min_size: usize,  // 16 KB
    avg_size: usize,  // 64 KB
    max_size: usize,  // 256 KB
}

impl CdcChunker {
    /// Chunk file content using FastCDC
    pub fn chunk(&self, data: &[u8]) -> Vec<Chunk> {
        let chunker = fastcdc::v2020::FastCDC::new(
            data, self.min_size, self.avg_size, self.max_size
        );
        chunker.map(|c| Chunk {
            offset: c.offset,
            length: c.length,
            hash: xxhash64(&data[c.offset..c.offset + c.length]),
        }).collect()
    }
}

pub struct ChunkManifest {
    pub content_hash: ContentHash,
    pub chunks: Vec<(ChunkHash, u64, u32)>,  // (hash, offset, size)
    pub total_size: u64,
}
```

#### Delta Detection (`musicfs-sync/src/delta.rs`)

```rust
pub struct DeltaDetector {
    db: Arc<Database>,
}

impl DeltaDetector {
    /// Compare origin state to cached state
    pub async fn detect_changes(&self, origin: &dyn Origin) -> Result<ChangeSet> {
        let origin_files = origin.list_recursive().await?;
        let cached_files = self.db.list_files(origin.id()).await?;
        
        ChangeSet {
            added: self.find_added(&origin_files, &cached_files),
            removed: self.find_removed(&origin_files, &cached_files),
            modified: self.find_modified(&origin_files, &cached_files).await?,
        }
    }
    
    /// Compute chunk delta for modified file
    pub async fn compute_delta(
        &self, 
        old_manifest: &ChunkManifest,
        new_chunks: &[Chunk],
    ) -> ChunkDelta {
        let old_hashes: HashSet<_> = old_manifest.chunks.iter().map(|c| c.0).collect();
        ChunkDelta {
            reuse: new_chunks.iter().filter(|c| old_hashes.contains(&c.hash)).cloned().collect(),
            fetch: new_chunks.iter().filter(|c| !old_hashes.contains(&c.hash)).cloned().collect(),
        }
    }
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_cdc_stable_boundaries` | Unit | Insertions don't shift all chunks |
| `test_delta_detect_added` | Unit | New files detected (FR-10.1) |
| `test_delta_detect_removed` | Unit | Deleted files detected (FR-10.2) |
| `test_delta_detect_modified` | Unit | Changed files detected (FR-10.4) |
| `test_delta_chunk_reuse` | Unit | Unchanged chunks reused (FR-11.1) |
| `test_bandwidth_reduction` | Integration | >90% reduction on metadata edit (NFR-5.1) |

#### Exit Criteria

- [ ] CDC produces stable chunk boundaries
- [ ] Delta sync detects add/remove/modify
- [ ] Unchanged chunks reused (not re-fetched)
- [ ] Metadata-only edits achieve >90% bandwidth reduction

---

### Week 6: Origin Federation

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Origin registry | musicfs-origins | `registry.rs` | FR-13.1-13.3 |
| Priority routing | musicfs-origins | `router.rs` | FR-13.4 |
| Health checks | musicfs-origins | `health.rs` | FR-13.6 |
| Failover logic | musicfs-origins | `failover.rs` | FR-13.5 |

#### Origin Registry (`musicfs-origins/src/registry.rs`)

```rust
pub struct OriginRegistry {
    origins: RwLock<HashMap<OriginId, Arc<dyn Origin>>>,
    router: Router,
    health: HealthMonitor,
}

impl OriginRegistry {
    /// Register new origin
    pub async fn register(&self, config: OriginConfig) -> Result<OriginId>;
    
    /// Get best origin for path
    pub async fn route(&self, path: &RealPath) -> Result<Arc<dyn Origin>>;
    
    /// Get all origins for path (for redundancy)
    pub async fn route_all(&self, path: &RealPath) -> Vec<Arc<dyn Origin>>;
}
```

#### Router (`musicfs-origins/src/router.rs`)

```rust
pub struct Router {
    priority_map: RwLock<HashMap<OriginId, Priority>>,
    latency_stats: DashMap<OriginId, LatencyStats>,
}

impl Router {
    /// Select best origin based on priority + health + latency
    pub fn select(&self, candidates: &[OriginId], health: &HealthSnapshot) -> Option<OriginId> {
        candidates
            .iter()
            .filter(|id| health.is_healthy(id))
            .min_by_key(|id| {
                let priority = self.priority_map.read().get(id).copied().unwrap_or(100);
                let latency = self.latency_stats.get(id).map(|s| s.p50_ms).unwrap_or(1000);
                (priority, latency)
            })
            .copied()
    }
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_register_multiple_origins` | Unit | Multi-origin support (FR-13.1) |
| `test_route_by_priority` | Unit | Priority routing (FR-13.4) |
| `test_failover_on_unhealthy` | Unit | Automatic failover (FR-13.5) |
| `test_health_check_interval` | Integration | Periodic health checks (FR-13.6) |
| `test_origin_offline_graceful` | Integration | Serve cached when offline (NFR-7.1) |

#### Exit Criteria

- [ ] Multiple origins configurable
- [ ] Requests route to healthiest origin
- [ ] Failover when primary fails
- [ ] Offline origin doesn't crash daemon

---

### Week 7: Remote Origins

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| NFS origin | musicfs-origins | `nfs.rs` | FR-12.2 |
| SMB origin | musicfs-origins | `smb.rs` | FR-12.3 |
| S3 origin | musicfs-origins | `s3.rs` | FR-12.4 |
| SFTP origin | musicfs-origins | `sftp.rs` | FR-12.5 |
| Credential handling | musicfs-core | `credentials.rs` | Security |

#### NFS Origin (`musicfs-origins/src/nfs.rs`)

```rust
/// NFS origin treats mounted NFS as local filesystem
/// User mounts NFS externally; we just read from mount point
pub struct NfsOrigin {
    mount_point: PathBuf,
    id: OriginId,
}

#[async_trait]
impl Origin for NfsOrigin {
    // Delegates to LocalOrigin implementation
    // NFS-specific: handle stale file handles, network timeouts
    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Bytes> {
        // Retry on ESTALE (stale NFS file handle)
        for attempt in 0..3 {
            match self.do_read(path, offset, size).await {
                Ok(data) => return Ok(data),
                Err(e) if e.raw_os_error() == Some(libc::ESTALE) => {
                    tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::NfsStaleHandle)
    }
}
```

#### S3 Origin (`musicfs-origins/src/s3.rs`)

```rust
pub struct S3Origin {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
    id: OriginId,
}

#[async_trait]
impl Origin for S3Origin {
    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let prefix = self.prefix.join(path);
        let resp = self.client.list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .delimiter("/")
            .send().await?;
        // Convert S3 response to DirEntry
    }
    
    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Bytes> {
        let range = format!("bytes={}-{}", offset, offset + size as u64 - 1);
        let resp = self.client.get_object()
            .bucket(&self.bucket)
            .key(&self.prefix.join(path))
            .range(range)
            .send().await?;
        Ok(resp.body.collect().await?.into_bytes())
    }
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_nfs_origin_stale_handle` | Unit | NFS ESTALE retry (FR-12.2) |
| `test_smb_origin_mock` | Unit | SMB protocol (FR-12.3) |
| `test_s3_origin_mock` | Unit | S3 protocol (FR-12.4) |
| `test_sftp_origin_mock` | Unit | SFTP protocol (FR-12.5) |
| `test_s3_origin_real` | Integration | Real S3 (requires creds) |
| `test_mixed_origins` | Integration | Local + remote together |

#### Exit Criteria

- [ ] NFS origin functional (treats NFS mount as local)
- [ ] SMB origin functional
- [ ] S3 origin functional
- [ ] SFTP origin functional  
- [ ] Mixed local + remote works
- [ ] All Phase 2 requirements pass acceptance tests

---

## 4. Phase 3: Search & Smart Features (Weeks 8-9)

**Goal**: Full-text search and intelligent caching.

**Requirements Covered**: FR-14, FR-15, FR-16, FR-19

---

### Week 8: Search Index

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| tantivy integration | musicfs-search | `index.rs` | FR-14.1-14.4 |
| Search virtual dir | musicfs-fuse | `ops/search.rs` | FR-14.3 |
| Query parser | musicfs-search | `query.rs` | FR-14.2 |
| Incremental indexing | musicfs-search | `indexer.rs` | FR-14.4 |

#### Search Index (`musicfs-search/src/index.rs`)

```rust
pub struct SearchIndex {
    index: tantivy::Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
}

impl SearchIndex {
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let searcher = self.reader.searcher();
        let query_parser = QueryParser::for_index(
            &self.index,
            vec![
                self.schema.get_field("artist")?,
                self.schema.get_field("album")?,
                self.schema.get_field("title")?,
            ],
        );
        let query = query_parser.parse_query(query)?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;
        // Convert to SearchHit
    }
    
    pub fn index_file(&self, file: &FileMeta) -> Result<()> {
        let mut writer = self.writer.lock();
        writer.add_document(doc!(
            self.schema.get_field("path")? => file.virtual_path.as_str(),
            self.schema.get_field("artist")? => file.audio.as_ref().and_then(|a| a.artist.as_deref()).unwrap_or(""),
            self.schema.get_field("album")? => file.audio.as_ref().and_then(|a| a.album.as_deref()).unwrap_or(""),
            self.schema.get_field("title")? => file.audio.as_ref().and_then(|a| a.title.as_deref()).unwrap_or(""),
        ))?;
        Ok(())
    }
}
```

#### Search Virtual Directory (`musicfs-fuse/src/ops/search.rs`)

```rust
// /.search/metallica/ → symlinks to matching files
impl MusicFs {
    fn handle_search_readdir(&self, query: &str, reply: ReplyDirectory) {
        let results = self.search.search(query, 1000)?;
        for (i, hit) in results.iter().enumerate() {
            reply.add(
                hit.inode,
                (i + 1) as i64,
                FileType::Symlink,
                &hit.display_name(),
            );
        }
    }
    
    fn handle_search_readlink(&self, inode: u64, reply: ReplyData) {
        // Return path to actual file
        let target = self.search.resolve_inode(inode)?;
        reply.data(target.as_bytes());
    }
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_search_artist` | Unit | Artist search (FR-14.1) |
| `test_search_album` | Unit | Album search (FR-14.1) |
| `test_search_multi_field` | Unit | Cross-field search (FR-14.2) |
| `test_search_performance_1m` | Benchmark | <1s for 1M tracks (NFR-2.5) |
| `test_search_virtual_dir` | Integration | /.search/query works (FR-14.3) |
| `test_search_incremental` | Integration | New files indexed (FR-14.4) |

#### Exit Criteria

- [ ] Full-text search works
- [ ] `/.search/{query}/` returns symlinks
- [ ] Search <1s for 1M tracks
- [ ] New files automatically indexed

---

### Week 9: Smart Features

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Smart collections | musicfs-search | `collections.rs` | FR-15.1-15.5 |
| Cover art cache | musicfs-cache | `artwork.rs` | FR-16.1-16.4 |
| Prefetch engine | musicfs-cache | `prefetch.rs` | FR-19.1-19.4 |
| Access patterns | musicfs-cache | `patterns.rs` | FR-19.3 |

#### Smart Collections (`musicfs-search/src/collections.rs`)

```rust
pub struct SmartCollection {
    pub name: String,
    pub query: CollectionQuery,
}

pub enum CollectionQuery {
    DateRange { field: String, start: NaiveDate, end: NaiveDate },
    Match { field: String, pattern: String },
    Rating { min: u8 },
    RecentlyAdded { days: u32 },
    Compound { op: BoolOp, children: Vec<CollectionQuery> },
}

impl SmartCollection {
    /// Materialize as virtual directory
    pub fn resolve(&self, index: &SearchIndex) -> Vec<FileId> {
        index.query(&self.query.to_tantivy_query())
    }
}
```

#### Prefetch Engine (`musicfs-cache/src/prefetch.rs`)

```rust
pub struct PrefetchEngine {
    pattern_db: PatternStore,
    queue: PriorityQueue<PrefetchTask>,
}

impl PrefetchEngine {
    /// Record file access
    pub fn record_access(&self, file_id: FileId, context: AccessContext) {
        self.pattern_db.record(file_id, context);
    }
    
    /// Predict next likely accesses
    pub fn predict(&self, current: FileId) -> Vec<FileId> {
        // Album-aware: if track N accessed, prefetch N+1, N+2
        // Artist-aware: other albums by same artist
        // Time-based: tracks often played together
    }
    
    /// Background prefetch worker
    pub async fn run(&self, cache: Arc<CacheManager>) {
        loop {
            if let Some(task) = self.queue.pop() {
                cache.prefetch(&task.file_id).await;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_collection_date_range` | Unit | Date-based collections (FR-15.2) |
| `test_collection_recently_added` | Unit | Recent additions (FR-15.2) |
| `test_artwork_extraction` | Unit | Cover art from audio (FR-16.1) |
| `test_artwork_thumbnail` | Unit | Thumbnail generation (FR-16.3) |
| `test_prefetch_album_sequence` | Unit | Sequential track prefetch (FR-19.1) |
| `test_prefetch_reduces_misses` | Integration | >50% miss reduction (FR-19.4) |

#### Exit Criteria

- [ ] Smart collections as virtual directories
- [ ] Album art extracted and thumbnailed
- [ ] Prefetch reduces cache misses >50%
- [ ] All Phase 3 requirements pass acceptance tests

---

## 5. Phase 4: Plugin System & Polish (Weeks 10-11)

**Goal**: Extensibility and production readiness.

**Requirements Covered**: FR-17, FR-18, FR-23, FR-24, NFR-6

---

### Week 10: Plugin System

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Plugin traits | musicfs-plugins | `traits.rs` | FR-23.1-23.4 |
| Native host | musicfs-plugins | `native.rs` | FR-23.2 |
| WASM host | musicfs-plugins | `wasm.rs` | FR-23.3 |
| Example plugins | plugins/ | `local-origin/`, `flac-format/` | FR-23.5 |

#### Plugin Traits (`musicfs-plugins/src/traits.rs`)

```rust
/// Origin plugin interface
pub trait OriginPlugin: Send + Sync {
    fn id(&self) -> &str;
    fn origin_type(&self) -> &str;
    fn create(&self, config: Value) -> Result<Box<dyn Origin>>;
}

/// Metadata source plugin
pub trait MetadataPlugin: Send + Sync {
    fn id(&self) -> &str;
    fn lookup(&self, query: &MetadataQuery) -> Result<Option<ExternalMetadata>>;
}

/// Format plugin (for custom audio formats)
pub trait FormatPlugin: Send + Sync {
    fn id(&self) -> &str;
    fn extensions(&self) -> &[&str];
    fn parse(&self, reader: &mut dyn Read) -> Result<AudioMeta>;
}
```

#### WASM Host (`musicfs-plugins/src/wasm.rs`)

```rust
pub struct WasmPluginHost {
    engine: wasmtime::Engine,
    linker: wasmtime::Linker<PluginState>,
}

impl WasmPluginHost {
    pub fn load(&self, wasm_bytes: &[u8]) -> Result<WasmPlugin> {
        let module = wasmtime::Module::new(&self.engine, wasm_bytes)?;
        let instance = self.linker.instantiate(&mut store, &module)?;
        // Extract plugin interface
    }
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_native_plugin_load` | Unit | Native plugin loading (FR-23.2) |
| `test_wasm_plugin_sandbox` | Unit | WASM isolation (FR-23.3) |
| `test_plugin_hot_reload` | Integration | Reload without restart (FR-23.4) |
| `test_example_origin_plugin` | Integration | Custom origin works |

#### Exit Criteria

- [ ] Native plugins loadable at runtime
- [ ] WASM plugins sandboxed
- [ ] Example plugins functional
- [ ] Plugins hot-reloadable

---

### Week 11: Control API & Production

#### Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| gRPC server | musicfs-grpc | `server.rs` | FR-17.1-17.5 |
| Proto codegen | proto/ | `musicfs.proto`, `build.rs` | FR-17.2 |
| Event streaming | musicfs-grpc | `events.rs` | FR-18.1-18.4 |
| Metrics export | musicfs-core | `metrics.rs` | NFR-6.1-6.4 |
| CLI completion | musicfs-cli | `main.rs` | FR-17 |
| systemd unit | dist/ | `musicfs.service` | Production |
| Packaging | dist/ | `PKGBUILD`, `musicfs.spec` | Production |

#### Proto Definitions (`proto/musicfs.proto`)

**Source**: Copy verbatim from [architecture.md section 4.3.7](architecture.md#437-control-api)

```protobuf
syntax = "proto3";
package musicfs.v1;

service MusicFS {
    // Daemon lifecycle
    rpc GetStatus(Empty) returns (StatusResponse);
    rpc Shutdown(ShutdownRequest) returns (Empty);
    
    // Cache management
    rpc GetCacheStats(Empty) returns (CacheStats);
    rpc ClearCache(ClearCacheRequest) returns (ClearCacheResponse);
    rpc Prefetch(PrefetchRequest) returns (stream PrefetchProgress);
    
    // Origin management
    rpc ListOrigins(Empty) returns (OriginsResponse);
    rpc GetOriginHealth(OriginRequest) returns (OriginHealth);
    rpc RescanOrigin(OriginRequest) returns (stream SyncProgress);
    
    // Search
    rpc Search(SearchRequest) returns (SearchResponse);
    rpc SearchStream(SearchRequest) returns (stream SearchResult);
    
    // Events (server-streaming)
    rpc SubscribeEvents(EventFilter) returns (stream Event);
}

// Full message definitions in architecture.md section 4.3.7
// Including: StatusResponse, CacheStats, TierStats, OriginInfo,
// OriginHealth, SyncProgress, SearchRequest, SearchResponse,
// EventFilter, Event, and all enums
```

#### gRPC Server (`musicfs-grpc/src/server.rs`)

```rust
pub struct MusicFsService {
    core: Arc<MusicFsCore>,
    events: broadcast::Sender<Event>,
}

#[tonic::async_trait]
impl musicfs::v1::music_fs_server::MusicFs for MusicFsService {
    async fn get_status(&self, _: Request<Empty>) -> Result<Response<StatusResponse>, Status> {
        let status = self.core.status().await;
        Ok(Response::new(status.into()))
    }
    
    type SubscribeEventsStream = ReceiverStream<Result<Event, Status>>;
    
    async fn subscribe_events(
        &self,
        request: Request<EventFilter>,
    ) -> Result<Response<Self::SubscribeEventsStream>, Status> {
        let filter = request.into_inner();
        let rx = self.events.subscribe();
        let stream = BroadcastStream::new(rx)
            .filter(|e| filter.matches(e))
            .map(|e| Ok(e.into()));
        Ok(Response::new(ReceiverStream::new(stream)))
    }
}
```

#### Metrics (`musicfs-core/src/metrics.rs`)

```rust
lazy_static! {
    pub static ref FUSE_OPS: IntCounterVec = register_int_counter_vec!(
        "musicfs_fuse_ops_total",
        "Total FUSE operations",
        &["op"]
    ).unwrap();
    
    pub static ref FUSE_LATENCY: HistogramVec = register_histogram_vec!(
        "musicfs_fuse_latency_seconds",
        "FUSE operation latency",
        &["op"],
        vec![0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0]
    ).unwrap();
    
    pub static ref CACHE_HITS: IntCounter = register_int_counter!(
        "musicfs_cache_hits_total",
        "Cache hits"
    ).unwrap();
}
```

#### Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_grpc_status` | Unit | GetStatus RPC (FR-17.1) |
| `test_grpc_cache_clear` | Unit | ClearCache RPC (FR-17.3) |
| `test_grpc_events_stream` | Integration | Event streaming (FR-18.1) |
| `test_metrics_prometheus` | Unit | Prometheus format (NFR-6.1) |
| `test_cli_commands` | Integration | CLI works |
| `test_systemd_service` | E2E | Service lifecycle |

#### Exit Criteria

- [ ] gRPC API fully functional
- [ ] Event streaming works
- [ ] Prometheus metrics exported
- [ ] CLI feature-complete
- [ ] systemd service works
- [ ] All acceptance tests pass

---

## 6. Acceptance Test Matrix

| Test Suite | Requirements | Phase |
|------------|--------------|-------|
| `test_basic_mount` | FR-1.1-1.5, NFR-1.4-1.5, NFR-1.7 | 1 |
| `test_directory_ops` | FR-2.1-2.3, NFR-1.2 | 1 |
| `test_file_read` | FR-3.1-3.5, NFR-1.3 | 1 |
| `test_read_only` | FR-4.1-4.4 | 1 |
| `test_metadata_extraction` | FR-6.1-6.5 | 1 |
| `test_cache_persistence` | FR-7.1-7.4, FR-8.1-8.4 | 1 |
| `test_delta_sync` | FR-10.1-10.4, FR-11.1-11.4, NFR-5.1 | 2 |
| `test_multi_origin` | FR-13.1-13.6 | 2 |
| `test_remote_origins` | FR-12.1-12.5 | 2 |
| `test_search` | FR-14.1-14.4, NFR-2.5 | 3 |
| `test_smart_collections` | FR-15.1-15.5 | 3 |
| `test_album_art` | FR-16.1-16.4 | 3 |
| `test_prefetch` | FR-19.1-19.4 | 3 |
| `test_plugins` | FR-23.1-23.5 | 4 |
| `test_control_api` | FR-17.1-17.5 | 4 |
| `test_events` | FR-18.1-18.4 | 4 |
| `test_performance` | NFR-1.1-1.5, NFR-2.1-2.4 | All |
| `test_scalability` | NFR-3.1-3.4 | All |

---

## 7. Risk Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| FUSE performance ceiling | Medium | High | Early benchmarking in Week 3; consider io_uring bypass |
| symphonia format gaps | Low | Medium | Document unsupported formats; allow format plugins |
| WASM plugin overhead | Medium | Low | Benchmark; native fallback for perf-critical plugins |
| S3 latency variability | High | Medium | Aggressive prefetch; health-based routing |
| SQLite contention | Low | Medium | WAL mode; read-only connections; consider sled fallback |

---

## 8. Deferred Requirements (Post-MVP)

The following P1 requirements are explicitly deferred from the 11-week plan:

### FR-21: External Metadata Sources [P1] → Phase 5

| ID | Requirement | Rationale for Deferral |
|----|-------------|------------------------|
| FR-21.1 | MusicBrainz integration | Requires rate limiting, caching, API key management |
| FR-21.2 | Discogs integration | OAuth flow complexity; lower priority than core function |
| FR-21.3 | Last.fm integration | Scrobbling is write operation; conflicts with read-only model |
| FR-21.4 | AcoustID fingerprinting | CPU-intensive; requires chromaprint dependency |
| FR-21.5 | Custom metadata plugins | Covered by FR-23 plugin system; metadata plugin type needed |

**Recommended Phase 5 scope (Weeks 12-13):**
- MusicBrainz lookup plugin (native)
- Metadata plugin trait in musicfs-plugins
- Example WASM metadata plugin

### FR-22: Import & Migration [P1] → Phase 5

| ID | Requirement | Rationale for Deferral |
|----|-------------|------------------------|
| FR-22.1 | Import from beets database | Requires beets schema knowledge; optional for fresh installs |
| FR-22.2 | Import from iTunes library | XML parsing; Apple ecosystem complexity |
| FR-22.3 | Export library metadata | Nice-to-have; users can query SQLite directly |

**Recommended Phase 5 scope (Week 14):**
- `musicfs import beets /path/to/beets.db` CLI command
- Export to JSON/CSV via CLI

### Why Defer?

1. **Core functionality first**: MVP must prove FUSE + caching + multi-origin works
2. **Plugin system prerequisite**: FR-21.5 depends on FR-23 being stable
3. **Limited impact**: Users can populate library from origins without import
4. **Scope control**: 11 weeks is aggressive; buffer for unknowns

---

## 9. Definition of Done

A feature is complete when:

1. [ ] Implementation matches architecture.md specification
2. [ ] Unit tests pass with >80% coverage
3. [ ] Integration tests pass
4. [ ] Performance meets NFR targets (benchmarked)
5. [ ] Documentation updated
6. [ ] No new `unsafe` without justification
7. [ ] Clippy clean (deny warnings)
8. [ ] Code reviewed

---

## 9. Getting Started (Week 1, Day 1)

```bash
# Create workspace
mkdir -p musicfs && cd musicfs
cargo init --name musicfs

# Create crate structure
mkdir -p crates/{musicfs-core,musicfs-fuse,musicfs-cache,musicfs-cas,musicfs-sync,musicfs-origins,musicfs-metadata,musicfs-search,musicfs-plugins,musicfs-grpc,musicfs-cli}
mkdir -p proto tests/{integration,e2e} benches

# Initialize each crate
for crate in crates/*; do
    cargo init --lib "$crate"
done
cargo init crates/musicfs-cli

# Add to workspace Cargo.toml
cat > Cargo.toml << 'EOF'
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
rust-version = "1.75"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
thiserror = "1"
tracing = "0.1"
serde = { version = "1", features = ["derive"] }
EOF

# Create flake.nix for reproducible builds
# ... (nix flake with rust-overlay)
```

Ready to begin implementation?
