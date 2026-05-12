# MusicFS: Design Doc

**Authors:** [TBD]  
**Status:** Draft  
**Last Updated:** 2026-05-12  
**Reviewers:** [TBD]  
**Approvers:** [TBD]  
**Requirements:** [requirements.md](requirements.md)

---

[TOC]

---

## 1. Abstract

MusicFS is a read-only FUSE filesystem that presents music libraries organized
by metadata (artist/album/track) rather than physical file paths. It supports
multiple origin storage backends (local, NFS, S3, SFTP), provides intelligent
caching with delta synchronization, and exposes a plugin architecture for
extensibility.

The system addresses limitations of the existing beetfs implementation:
- O(N) mount time → O(1) lazy loading
- Full file in RAM → streaming with content-addressable chunks  
- Single origin → federated multi-origin with failover
- No offline support → cache-first with graceful degradation

Target users are media enthusiasts with large music collections (100K-10M+
tracks) distributed across multiple storage systems who want a unified,
metadata-organized view without modifying original files.

---

## 2. Background

### 2.1 Current State

The existing beetfs implementation is a Python 2.7 FUSE plugin for beets that:
- Presents a virtual filesystem organized by metadata templates
- Overlays metadata from beets database onto file headers
- Supports metadata writes back to the beets database

### 2.2 Pain Points

| Problem | Impact |
|---------|--------|
| O(N) mount time (5-120s for large libraries) | Unusable for large collections |
| Loads entire file into RAM on open | OOM risk, 50-100MB per file |
| Python GIL limits concurrency | Poor performance under load |
| No caching between sessions | Repeated work on every mount |
| Single local origin only | Can't federate across storage |
| No offline support | Unusable without origin access |
| Critical bugs (nested methods, tree building) | Non-functional |

### 2.3 Related Systems

| System | Relationship |
|--------|--------------|
| [beets](https://beets.io/) | Source of inspiration; potential import source |
| [rclone mount](https://rclone.org/commands/rclone_mount/) | Similar FUSE + remote storage; no metadata organization |
| [Plex/Jellyfin](https://jellyfin.org/) | Media servers with metadata; not filesystem-based |

---

## 3. Goals & Non-Goals

### 3.1 Goals

| ID | Goal | Success Metric |
|----|------|----------------|
| G1 | O(1) mount time | <500ms regardless of library size |
| G2 | Minimal memory footprint | <50MB idle, <500MB peak |
| G3 | Support multiple origins | ≥2 origins with automatic failover |
| G4 | Offline-first operation | Serve cached data when origin unavailable |
| G5 | Delta synchronization | >90% bandwidth reduction vs full sync |
| G6 | Plugin extensibility | Support custom origins, formats, metadata sources |
| G7 | Full-text search | Sub-second search across 1M+ tracks |

### 3.2 Design Requirements

The following quantitative requirements drive architectural decisions. Full
specification in [requirements.md](requirements.md).

#### 3.2.1 Latency Requirements

| Operation | Target | Maximum | Requirement |
|-----------|--------|---------|-------------|
| `stat()` cached | <1ms | 5ms | NFR-1.1 |
| `readdir()` cached | <10ms | 50ms | NFR-1.2 |
| `open()` cached | <5ms | 20ms | NFR-1.3 |
| `read()` cached | <1ms | 5ms | NFR-1.4 |
| `read()` cache miss (local) | <50ms | 200ms | NFR-1.5 |
| `read()` cache miss (remote) | <200ms | 1000ms | NFR-1.6 |
| Mount completion | <100ms | 500ms | NFR-1.7 |
| Search query (1M files) | <500ms | 1000ms | FR-14 |

**Design Response:**
- Lazy loading eliminates mount-time I/O → O(1) mount
- In-memory LRU cache for hot metadata → <1ms stat
- SQLite with indexes → O(log n) lookups
- Async I/O via tokio → non-blocking operations

#### 3.2.2 Throughput Requirements

| Metric | Target | Requirement |
|--------|--------|-------------|
| Sequential read (cached) | >500 MB/s | NFR-2.1 |
| Sequential read (local origin) | >200 MB/s | NFR-2.2 |
| Metadata ops/sec | >1000 | NFR-2.3 |
| Concurrent file handles | >1000 | NFR-2.4 |

**Design Response:**
- Memory-mapped chunk files → kernel-optimized reads
- No GIL (Rust) → true parallelism
- Async FUSE ops → handle many concurrent requests

#### 3.2.3 Scalability Requirements

| Metric | Target | Stretch | Requirement |
|--------|--------|---------|-------------|
| Library size | 1M files | 10M files | NFR-3.1, NFR-3.5 |
| Directory entries | 100K | 1M | NFR-3.2 |
| Concurrent clients | 10 | 100+ | NFR-3.6 |
| Mount time scaling | O(1) | O(1) | NFR-3.3 |

**Design Response:**
- Lazy tree loading → mount time independent of size
- SQLite indexes → O(log n) regardless of scale
- Streaming readdir → handle large directories
- Connection pooling → support many clients

#### 3.2.4 Resource Requirements

| Resource | Idle | Active (1K files) | Peak | Requirement |
|----------|------|-------------------|------|-------------|
| Memory | <50 MB | <200 MB | <500 MB | NFR-4.1-4.3 |
| Per-file overhead | - | <1 KB | - | NFR-4.4 |
| Metadata cache | - | 100 MB default | configurable | NFR-5.1 |
| Content cache | - | 10 GB default | configurable | NFR-5.2 |

**Design Response:**
- Streaming reads → never load full file in memory
- Content-addressed chunks → bounded cache with LRU eviction
- Metadata in SQLite → minimal per-file RAM overhead

#### 3.2.5 Efficiency Requirements

| Metric | Target | Requirement |
|--------|--------|-------------|
| Delta sync bandwidth reduction | >90% | NFR-6.4 |
| Cache hit rate (warm) | >95% | Derived |
| Deduplication ratio | >10% typical | FR-20 |

**Design Response:**
- CDC chunking → stable boundaries, minimal re-transfer
- Content-addressable storage → automatic deduplication
- Prefetch engine → anticipate access patterns

#### 3.2.6 Reliability Requirements

| Scenario | Behavior | Requirement |
|----------|----------|-------------|
| Origin offline | Serve cached data | NFR-7.1 |
| Network failure | Graceful degradation, no crash | NFR-7.2 |
| Failed operation | Retry with backoff (100ms, 500ms, 2s) | NFR-7.3 |
| Malformed audio | Skip file, log error, don't crash | NFR-7.4 |
| Chunk corruption | Detect via checksum, re-fetch | NFR-8.1, NFR-8.4 |
| Interrupted sync | Resume from last good state | NFR-8.3 |
| Unclean unmount | Recover on next mount | NFR-8.2 |

**Design Response:**
- Cache-first architecture → offline operation by default
- Origin federation with health checks → survive single origin failure
- xxHash checksums on all chunks → detect corruption
- WAL mode SQLite → ACID transactions, crash recovery

#### 3.2.7 Concurrent Access Requirements

| Scenario | Limit | Latency Impact | Requirement |
|----------|-------|----------------|-------------|
| Simultaneous open files | >1000 handles | None | NFR-2.4 |
| Parallel read ops | >100 concurrent | <2x p99 latency | Derived |
| Multiple clients | >10 (target 100+) | Linear degradation | NFR-3.6 |
| Readdir during sync | No blocking | Serve stale if needed | FR-9.2 |

**Design Response:**
- Async I/O (tokio) → non-blocking operations
- No GIL → true parallelism across cores
- Read-write locks on cache → readers don't block readers
- Stale-while-revalidate → serve cached during refresh

### 3.3 Non-Goals

| ID | Non-Goal | Rationale |
|----|----------|-----------|
| NG1 | Write to origin files | Read-only by design; preserves originals |
| NG2 | Transcoding | Out of scope for MVP; plugin possible later |
| NG3 | Video file support | Focus on audio; deferred to future |
| NG4 | Distributed/clustered mode | Single-node for MVP; architecture supports later |
| NG5 | Mobile app | CLI/daemon only; filesystem interface |

---

## 4. Proposed Design

### 4.1 High-Level Architecture

```plantuml
@startuml
!theme plain
skinparam componentStyle rectangle

package "User Space" {
    [Media Players\n(mpv, VLC, Plex)] as Apps
    
    package "MusicFS Daemon" {
        [FUSE Interface] as FUSE
        [Control API] as Control
        [Metrics] as Metrics
        
        package "Core Services" {
            [Virtual Path\nResolver] as VPR
            [Event Bus] as Events
            [Search Engine\n(tantivy)] as Search
        }
        
        package "Plugin Host" {
            [Origin\nPlugins] as OriginPlugins
            [Metadata\nPlugins] as MetaPlugins
            [Format\nPlugins] as FormatPlugins
        }
        
        package "Storage Layer" {
            [Content-Addressable\nStore (CAS)] as CAS
            database "SQLite\n(metadata)" as SQLite
            database "sled\n(chunks)" as Sled
        }
        
        [Origin\nFederation] as Federation
    }
}

package "Origins (Read-Only)" {
    [Local FS] as Local
    [NFS] as NFS
    [S3] as S3
    [SFTP] as SFTP
}

Apps --> FUSE : POSIX
FUSE --> VPR
VPR --> Events
VPR --> Search
VPR --> CAS
CAS --> SQLite
CAS --> Sled
VPR --> Federation
Federation --> OriginPlugins
OriginPlugins --> Local
OriginPlugins --> NFS
OriginPlugins --> S3
OriginPlugins --> SFTP
Control --> Events
Metrics --> Events

@enduml
```

### 4.2 Component Overview

| Component | Responsibility | Technology |
|-----------|---------------|------------|
| FUSE Interface | Translate POSIX ops to internal calls | fuser (Rust) |
| Virtual Path Resolver | Map virtual ↔ real paths | Custom |
| Event Bus | Decouple components, enable observability | tokio broadcast |
| Search Engine | Full-text metadata search | tantivy |
| Plugin Host | Load/manage plugins | Native + WASM |
| CAS | Content-addressed chunk storage | Custom + sled |
| Origin Federation | Multi-origin routing with failover | Custom |

### 4.3 Detailed Design

#### 4.3.1 Virtual Path Resolution

The resolver maps metadata-based virtual paths to real origin paths.

```plantuml
@startuml
!theme plain

participant "FUSE" as F
participant "VirtualPathResolver" as VPR
participant "MetadataIndex" as MI
participant "TreeCache" as TC
participant "OriginFederation" as OF

F -> VPR : lookup("/Metallica/72 Seasons/01.flac")
VPR -> TC : get_cached(path)
alt cache hit
    TC --> VPR : CachedEntry
else cache miss
    VPR -> MI : query(artist="Metallica", album="72 Seasons", track=1)
    MI --> VPR : FileRecord { origin_id, real_path, metadata }
    VPR -> TC : store(path, entry)
end
VPR -> OF : resolve_origin(origin_id)
OF --> VPR : OriginHandle
VPR --> F : ResolvedPath { origin, real_path, inode }

@enduml
```

**Path Template Grammar:**
```
template     = segment ("/" segment)*
segment      = (literal | variable)+
variable     = "$" identifier
identifier   = "artist" | "album" | "title" | "track" | "year" | "genre" 
             | "format" | "format_upper" | "disc"
```

**Default Template:**
```
$artist/$album ($year) [$format_upper]/$track - $title.$format
```

#### 4.3.2 Content-Addressable Store (CAS)

All file content is stored as content-addressed chunks, enabling deduplication
and efficient delta sync.

```plantuml
@startuml
!theme plain

package "Content-Addressable Store" {
    component "Chunk Manager" as CM
    component "CDC Chunker\n(FastCDC)" as CDC
    component "Hash Index\n(xxHash64)" as Hash
    
    database "Chunk Files\n~/.cache/musicfs/chunks/" as Chunks
    database "Index DB\n(sled)" as Index
    
    CM --> CDC : chunk data
    CDC --> Hash : compute hash
    Hash --> Index : store hash → location
    CM --> Chunks : write chunk file
}

note right of CDC
  Avg chunk: 64KB
  Min: 16KB, Max: 256KB
  Stable boundaries for delta sync
end note

@enduml
```

**Chunk Storage Layout:**
```
~/.cache/musicfs/
├── chunks/
│   ├── aa/
│   │   ├── aa1b2c3d4e5f6789...  (64KB chunk)
│   │   └── aa9f8e7d6c5b4a32...
│   ├── ab/
│   └── ...  (256 subdirs for distribution)
├── metadata.db     (SQLite: file metadata, tree cache)
├── search.idx/     (tantivy: full-text index)
└── chunks.sled/    (sled: hash → chunk location)
```

#### 4.3.3 Origin Federation

Multiple origins are managed with priority-based routing and health tracking.

```plantuml
@startuml
!theme plain

participant "VirtualPathResolver" as VPR
participant "OriginFederation" as OF
participant "HealthChecker" as HC
participant "Origin[Local]" as O1
participant "Origin[NFS]" as O2
participant "Origin[S3]" as O3

VPR -> OF : read(real_path, offset, size)
OF -> OF : select_origin(priority, health)

alt Origin[Local] healthy (pri=1)
    OF -> O1 : read()
    O1 --> OF : data
else Origin[Local] unhealthy, try NFS (pri=2)
    OF -> O2 : read()
    alt success
        O2 --> OF : data
    else failure
        OF -> O3 : read()
        O3 --> OF : data
    end
end

OF --> VPR : data

note over HC
  Background health checks
  every 30s per origin
end note

@enduml
```

**Origin Configuration:**
```toml
[[origins]]
id = "local"
type = "local"
path = "/mnt/nas/music"
priority = 1

[[origins]]
id = "backup"
type = "s3"
bucket = "music-backup"
priority = 2
```

#### 4.3.4 Plugin System

Plugins extend functionality without modifying core code.

```plantuml
@startuml
!theme plain

interface "Plugin" {
    +name(): String
    +version(): Version
    +init(config)
    +shutdown()
}

interface "OriginPlugin" {
    +list_dir(path): Vec<DirEntry>
    +read(path, offset, size): Vec<u8>
    +stat(path): FileStat
    +watch(path, callback): WatchHandle
}

interface "MetadataPlugin" {
    +extract(data, format): Metadata
    +can_handle(format): bool
}

interface "FormatPlugin" {
    +extensions(): Vec<String>
    +parse_header(data): AudioHeader
    +synthesize_header(metadata): Vec<u8>
}

Plugin <|-- OriginPlugin
Plugin <|-- MetadataPlugin
Plugin <|-- FormatPlugin

class "LocalFSPlugin" implements OriginPlugin
class "S3Plugin" implements OriginPlugin
class "SymphoniaPlugin" implements MetadataPlugin
class "FlacPlugin" implements FormatPlugin
class "Mp3Plugin" implements FormatPlugin

@enduml
```

**Plugin Loading:**
1. **Built-in:** Compiled into binary (Local, S3, SFTP, symphonia)
2. **Native:** Dynamic libraries (`.so`/`.dylib`) loaded at runtime
3. **WASM:** Sandboxed plugins via wasmtime (future)

#### 4.3.5 Data Flow: Read Operation

```plantuml
@startuml
!theme plain

|FUSE|
start
:receive read(path, offset, size);

|VirtualPathResolver|
:resolve virtual path to real path;
:lookup file metadata;

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

|EventBus|
:emit FileAccessed event;

|FUSE|
:return data to application;
stop

@enduml
```

#### 4.3.6 Data Schema

**Metadata Index (SQLite):**
```sql
CREATE TABLE files (
    id              INTEGER PRIMARY KEY,
    origin_id       TEXT NOT NULL,
    real_path       TEXT NOT NULL,
    virtual_path    TEXT NOT NULL,
    
    -- Metadata (see FR-6 in requirements.md)
    title           TEXT,
    artist          TEXT,
    album           TEXT,
    album_artist    TEXT,
    genre           TEXT,
    year            INTEGER,
    track           INTEGER,
    disc            INTEGER,
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

CREATE INDEX idx_virtual ON files(virtual_path);
CREATE INDEX idx_artist_album ON files(artist, album);
CREATE INDEX idx_content_hash ON files(content_hash);

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
```

#### 4.3.7 Control API

**Unix Socket Protocol (JSON-RPC 2.0):**

```json
// Request: Get cache statistics
{"jsonrpc": "2.0", "method": "cache.stats", "id": 1}

// Response
{
    "jsonrpc": "2.0",
    "id": 1,
    "result": {
        "hits": 15234,
        "misses": 421,
        "hit_rate": 0.973,
        "chunks_stored": 84521,
        "chunks_unique": 71203,
        "dedup_ratio": 0.157,
        "size_bytes": 5368709120
    }
}

// Request: Search
{"jsonrpc": "2.0", "method": "search", "params": {"query": "metallica"}, "id": 2}

// Request: Refresh origin
{"jsonrpc": "2.0", "method": "origin.rescan", "params": {"id": "local"}, "id": 3}
```

**CLI Interface:**
```bash
musicfs mount /mnt/music           # Mount filesystem
musicfs status                      # Show daemon status
musicfs cache stats                 # Cache statistics
musicfs cache clear --origin=local  # Clear cache for origin
musicfs search "metallica heavy"    # Search library
musicfs origin list                 # List origins and health
musicfs origin rescan local         # Force rescan
```

---

## 5. Cross-Cutting Concerns

### 5.1 Security & Privacy

| Concern | Mitigation |
|---------|------------|
| Credential storage | Use system keyring (secret-service) or env vars; never in config file |
| Credential exposure | Redact from logs; exclude from `/proc/cmdline` |
| Cache at rest | Optional encryption via age/libsodium (P3 requirement) |
| Plugin sandboxing | WASM plugins run in wasmtime sandbox; native plugins require trust |
| Access control | Respect origin permissions; run as unprivileged user |
| No PII handling | Filesystem metadata only; no user data collected |

### 5.2 Observability

**Metrics (Prometheus format):**
```
musicfs_fuse_ops_total{op="read"} 152341
musicfs_fuse_ops_total{op="readdir"} 8234
musicfs_fuse_latency_seconds{op="read",quantile="0.99"} 0.004
musicfs_cache_hits_total 142107
musicfs_cache_misses_total 10234
musicfs_cache_size_bytes 5368709120
musicfs_origin_health{origin="local"} 1
musicfs_origin_health{origin="s3"} 0
musicfs_sync_files_changed{origin="local"} 15
```

**Logging Levels:**
| Level | Content |
|-------|---------|
| ERROR | Unrecoverable failures, data corruption |
| WARN | Recoverable failures, origin timeouts |
| INFO | Mount/unmount, sync completion, config reload |
| DEBUG | Cache hits/misses, origin selection |
| TRACE | Individual FUSE operations, chunk I/O |

**Golden Signals Dashboard:**
1. **Latency:** p50/p95/p99 for read, stat, readdir
2. **Traffic:** FUSE ops/sec, bytes read/sec
3. **Errors:** Origin failures, cache corruption
4. **Saturation:** Cache fullness, open file handles

### 5.3 Scalability & Performance

**Expected Load:**
| Metric | Target | Maximum |
|--------|--------|---------|
| Library size | 1M files | 10M files |
| Concurrent clients | 10 | 100+ |
| FUSE ops/sec | 1,000 | 10,000 |
| Read throughput | 500 MB/s | 1 GB/s |

**Scaling Strategy:**
- **Horizontal:** Not supported (single daemon per mountpoint)
- **Vertical:** Increase cache size, add origins

**Resource Requirements:**
| Resource | Minimum | Recommended |
|----------|---------|-------------|
| CPU | 1 core | 4 cores |
| RAM | 256 MB | 2 GB |
| Disk (cache) | 1 GB | 50 GB |
| Network | 10 Mbps | 1 Gbps |

### 5.4 Testing Plan

| Test Type | Scope | Tools |
|-----------|-------|-------|
| Unit | Individual components | cargo test |
| Integration | Component interaction | cargo test --features integration |
| E2E | Full FUSE operations | pytest + real mount |
| Performance | Latency, throughput | criterion.rs, custom benchmarks |
| Stress | High load, large libraries | locust, custom generators |
| Chaos | Origin failures, network issues | toxiproxy |

**Test Matrix:**
```
Origins:     [local, s3, sftp] × [healthy, degraded, offline]
Cache:       [cold, warm, full]
Library:     [100, 10K, 1M, 10M] files
Operations:  [mount, readdir, stat, read, search]
```

---

## 6. Alternatives Considered

### 6.1 Alternative A: Extend beetfs (Python)

**Description:** Fix bugs in existing beetfs, add features incrementally.

**Rejected Because:**
- Python GIL fundamentally limits concurrency
- Python 2.7 EOL; migration to Python 3 substantial
- Architecture (full file in RAM) requires rewrite anyway
- No async I/O support in fuse-python

### 6.2 Alternative B: Use rclone mount

**Description:** Use rclone's FUSE mount with VFS caching.

**Rejected Because:**
- No metadata-based virtual path organization
- No metadata overlay functionality
- Limited plugin extensibility
- Would require forking and heavy modification

### 6.3 Alternative C: Build as Plex/Jellyfin Plugin

**Description:** Extend existing media server with virtual filesystem view.

**Rejected Because:**
- Tied to specific media server
- Not a true filesystem (no POSIX interface)
- Heavy runtime dependency
- Different use case (streaming vs filesystem)

### 6.4 Alternative D: Go Implementation

**Description:** Implement in Go using go-fuse.

**Considered Trade-offs:**
| Aspect | Rust | Go |
|--------|------|-----|
| Memory safety | Compile-time | GC pauses |
| Concurrency | async/await, no GC | goroutines, GC |
| FUSE library | fuser (mature) | go-fuse (mature) |
| Learning curve | Steeper | Gentler |
| Binary size | Smaller | Larger |

**Decision:** Rust chosen for zero-cost abstractions, no GC pauses during I/O,
and better fit for systems programming.

---

## 7. Implementation Plan

### 7.1 Phase 1: MVP (4 weeks)

**Goal:** Basic functional filesystem with single origin.

| Week | Deliverables |
|------|--------------|
| 1 | Project setup, FUSE skeleton, local origin plugin |
| 2 | Metadata extraction (symphonia), SQLite schema |
| 3 | Virtual path resolver, tree cache, basic readdir/stat/read |
| 4 | CAS implementation, chunk caching, integration tests |

**Exit Criteria:**
- Mount and browse local music library
- Play audio files through mounted filesystem
- Cache persists across restarts

### 7.2 Phase 2: Delta Sync & Multi-Origin (3 weeks)

**Goal:** Efficient synchronization and origin federation.

| Week | Deliverables |
|------|--------------|
| 5 | CDC chunking (FastCDC), delta detection |
| 6 | Origin federation, priority routing, health checks |
| 7 | S3 origin plugin, SFTP origin plugin |

**Exit Criteria:**
- Delta sync achieves >90% bandwidth reduction
- Automatic failover between origins
- Remote origins functional

### 7.3 Phase 3: Search & Smart Features (2 weeks)

**Goal:** Full-text search and intelligent caching.

| Week | Deliverables |
|------|--------------|
| 8 | tantivy integration, search indexing, `/.search/` virtual dir |
| 9 | Smart collections, prefetch engine, access pattern learning |

**Exit Criteria:**
- Search returns results in <1s for 1M tracks
- Prefetch reduces cache misses by >50%

### 7.4 Phase 4: Plugin System & Polish (2 weeks)

**Goal:** Extensibility and production readiness.

| Week | Deliverables |
|------|--------------|
| 10 | Plugin host, plugin API stabilization, example plugins |
| 11 | Control API, metrics, documentation, packaging |

**Exit Criteria:**
- Custom origin plugin loadable at runtime
- Prometheus metrics exported
- systemd service functional

### 7.5 Rollout Strategy

```plantuml
@startuml
!theme plain

[*] --> Alpha
Alpha --> Beta : Internal testing complete
Beta --> GA : Community testing complete

state Alpha {
    [*] --> DevTesting
    DevTesting --> DogFood : Core features work
}

state Beta {
    [*] --> LimitedRelease
    LimitedRelease --> PublicBeta : No critical bugs
}

state GA {
    [*] --> Stable
}

note right of Alpha : 2-4 weeks\nDevelopers only
note right of Beta : 4-8 weeks\nEarly adopters
note right of GA : Stable releases

@enduml
```

**Feature Flags:**
```toml
[features]
search_enabled = true
smart_collections = false  # Beta
wasm_plugins = false       # Experimental
```

**Rollback:** Binary replacement + cache clear; no data migration needed.

---

## 8. Glossary & References

### 8.1 Glossary

| Term | Definition |
|------|------------|
| **CAS** | Content-Addressable Store; data stored/retrieved by hash |
| **CDC** | Content-Defined Chunking; chunking with stable boundaries |
| **FUSE** | Filesystem in Userspace; kernel interface for user-space filesystems |
| **Origin** | Source storage backend (local, S3, NFS, etc.) |
| **Virtual Path** | Metadata-derived path shown to users |
| **Real Path** | Actual path on origin storage |

### 8.2 References

| Document | Link |
|----------|------|
| Requirements Specification | [requirements.md](requirements.md) |
| beetfs (Original) | [beetsplug/beetFs.py](../../beetsplug/beetFs.py) |
| beetfs Features | [v1/features.md](../v1/features.md) |
| fuser (Rust FUSE) | https://github.com/cberner/fuser |
| tantivy (Search) | https://github.com/quickwit-oss/tantivy |
| symphonia (Audio) | https://github.com/pdrat/symphonia |
| FastCDC | https://github.com/nlfiedler/fastcdc-rs |
| wasmtime | https://wasmtime.dev/ |

### 8.3 Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| fuser | 0.14+ | FUSE interface |
| tokio | 1.x | Async runtime |
| rusqlite | 0.31+ | SQLite bindings |
| sled | 0.34+ | Embedded key-value store |
| tantivy | 0.21+ | Full-text search |
| symphonia | 0.5+ | Audio metadata extraction |
| fastcdc | 3.x | Content-defined chunking |
| xxhash-rust | 0.8+ | Fast hashing |
| serde | 1.x | Serialization |
| toml | 0.8+ | Configuration |
| tracing | 0.1+ | Logging/instrumentation |
| metrics | 0.22+ | Prometheus metrics |
