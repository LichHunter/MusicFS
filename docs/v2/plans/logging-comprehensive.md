# Comprehensive Logging Plan

**Goal**: Add production-grade logging with trace-level observability, file rotation, and systemd integration  
**Effort**: ~10-12 hours  
**Dependencies**: Existing libraries only (no custom code)

> **Review Status**: Reviewed by Oracle - all gaps addressed

---

## Libraries Used

| Need | Library | Status |
|------|---------|--------|
| Instrumentation | `tracing` | Already in workspace |
| Subscriber/filtering | `tracing-subscriber` | Already in workspace |
| File rotation | `tracing-appender` | Add to workspace |
| systemd journal | `tracing-journald` | Add to workspace |
| Compression | `logrotate` (Linux tool) | Config file only |

---

## Phase 1: Config & Dependencies (2 hours)

### 1.1 Add dependencies to workspace

```toml
# Cargo.toml [workspace.dependencies]
tracing-appender = "0.2"
tracing-journald = "0.3"
```

```toml
# crates/musicfs-cli/Cargo.toml
tracing-appender.workspace = true
tracing-journald.workspace = true
```

### 1.2 Add LoggingConfig to config.rs

```rust
// crates/musicfs-core/src/config.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub mount_point: PathBuf,
    pub cache_dir: PathBuf,
    pub origins: Vec<OriginConfig>,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub logging: LoggingConfig,  // NEW
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    
    #[serde(default)]
    pub json_output: bool,
    
    #[serde(default = "default_true")]
    pub journald: bool,
    
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            log_dir: default_log_dir(),
            json_output: false,
            journald: true,
            level: default_log_level(),
        }
    }
}

fn default_log_dir() -> PathBuf {
    PathBuf::from("/var/log/musicfs")
}
fn default_log_level() -> String {
    "musicfs=info,warn".to_string()
}
fn default_true() -> bool {
    true
}
```

### 1.3 Expand init_logging() in main.rs

```rust
// crates/musicfs-cli/src/main.rs

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

fn init_logging(config: &LoggingConfig) -> Result<WorkerGuard> {
    std::fs::create_dir_all(&config.log_dir)?;
    
    // File layer with daily rotation
    let file_appender = tracing_appender::rolling::daily(&config.log_dir, "musicfs.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    
    let file_layer = if config.json_output {
        fmt::layer()
            .json()
            .with_writer(non_blocking)
            .with_ansi(false)
            .boxed()
    } else {
        fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .boxed()
    };
    
    // Journald layer (Linux only)
    #[cfg(target_os = "linux")]
    let journald_layer = if config.journald {
        tracing_journald::layer()
            .ok()
            .map(|l| l.with_syslog_identifier("musicfs".to_string()))
    } else {
        None
    };
    
    // Stderr layer for interactive use
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .compact();
    
    // Filter from config or env
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.level));
    
    // Compose
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer);
    
    #[cfg(target_os = "linux")]
    let subscriber = subscriber.with(journald_layer);
    
    subscriber.init();
    
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "MusicFS starting");
    Ok(guard)
}
```

### 1.4 Add logrotate config

```bash
# dist/logrotate.d/musicfs
/var/log/musicfs/*.log {
    daily
    rotate 30
    compress
    delaycompress
    missingok
    notifempty
    create 0640 musicfs musicfs
}
```

---

## Phase 2: Add tracing to musicfs-core (1 hour)

### 2.1 Add dependency

```toml
# crates/musicfs-core/Cargo.toml
[dependencies]
tracing.workspace = true  # ADD THIS
```

### 2.2 Instrument core modules

| File | What to Add |
|------|-------------|
| `config.rs` | Log config file loading, parse errors |
| `credentials.rs` | Log credential loading (redacted values) |
| `events.rs` | Log event publishing with counts |

---

## Phase 3: Instrument Hot Paths (4 hours)

### Priority order by impact

| Crate | Files | What to Add |
|-------|-------|-------------|
| musicfs-fuse | `filesystem.rs` | `#[instrument]` on all FUSE ops, trace at decision points |
| musicfs-origins | `failover.rs`, `health.rs`, `router.rs` | Retry loops, state transitions, selection logic |
| musicfs-cache | `tree.rs`, `metadata.rs` | Tree mutations, cache hit/miss |
| musicfs-cas | `reader.rs`, `store.rs` | Chunk operations, dedup decisions |
| musicfs-sync | `delta.rs`, `watcher.rs` | Change detection, file events |

### Instrumentation patterns

```rust
// Function level - add to all public async functions
#[tracing::instrument(level = "debug", skip(self), fields(path = %path))]
pub async fn read(&self, path: &str) -> Result<Bytes> {
    // ...
}

// Decision points - add trace! at match/if branches
match result {
    Ok(data) => {
        tracing::trace!(bytes = data.len(), "read success");
        data
    }
    Err(e) => {
        tracing::trace!(error = %e, "read failed");
        return Err(e);
    }
}

// State changes - use info! for important transitions
tracing::info!(old = ?old_status, new = ?new_status, origin = %id, "health changed");

// Cache operations
tracing::trace!(hit = true, fresh = true, "cache hit");
tracing::trace!(hit = false, "cache miss");
```

### FUSE operations (filesystem.rs) - highest priority

| Operation | Level | Fields |
|-----------|-------|--------|
| `lookup()` | debug | parent, name, result_ino |
| `getattr()` | debug | ino, file_type |
| `readdir()` | debug | ino, entry_count |
| `read()` | debug | ino, offset, size, bytes_read |
| `open()` | debug | ino, flags |
| `release()` | trace | ino |

### Origin operations - critical for debugging

| Function | Level | Fields |
|----------|-------|--------|
| `read_with_failover()` | debug | path, origins_tried, success |
| `read_with_retry()` | trace | origin, attempt, success |
| `check_health()` | debug | origin, old_status, new_status |
| `select_origin()` | trace | candidates, selected, reason |

---

## Phase 4: Update Production Files (1 hour)

### 4.1 Update systemd service

```ini
# dist/musicfs.service (add these lines)
Environment="RUST_LOG=musicfs=info,warn"
StandardOutput=journal
StandardError=journal
SyslogIdentifier=musicfs
RateLimitIntervalSec=30s
RateLimitBurst=1000
```

### 4.2 Example config.toml

```toml
# dist/config.example.toml
mount_point = "/mnt/music"
cache_dir = "/var/cache/musicfs"

[logging]
log_dir = "/var/log/musicfs"
json_output = true
journald = true
level = "musicfs=info,warn"

[cache]
metadata_cache_mb = 100
content_cache_gb = 10

[health]
check_interval_secs = 30
timeout_ms = 5000

[[origins]]
id = "local"
origin_type = "local"
priority = 1
path = "/srv/music"
```

---

## Detailed Log Locations by Level

### ERROR Level (25+ locations) - Unrecoverable Failures

| File | Line | Log Message |
|------|------|-------------|
| `musicfs-grpc/src/webhook.rs` | 43 | `error!("Failed to initialize webhook HTTP client: {error}")` |
| `musicfs-grpc/src/webhook.rs` | 133 | `error!("Invalid HMAC secret key for webhook signature: {error}")` |
| `musicfs-plugins/src/manager.rs` | 272 | `error!("Plugin manager initialization failed: {error}")` |
| `musicfs-plugins/src/wasm.rs` | 142,183 | `error!("WASM plugin host initialization failed: {error}")` |
| `musicfs-search/src/index.rs` | 211,217 | `error!("Search index corrupted: failed to deserialize at position {pos}")` |
| `musicfs-cas/src/store.rs` | 105 | `error!("CAS chunk not found: {hash} - possible data loss")` |
| `musicfs-cas/src/store.rs` | 124-131 | `error!("CAS integrity check failed: expected {expected}, got {actual}")` |
| `musicfs-fuse/src/filesystem.rs` | 103 | `error!("Failed to mount filesystem at {mountpoint}: {error}")` |
| `musicfs-origins/src/failover.rs` | 76 | `error!("No origins available for path {path}")` |
| `musicfs-origins/src/failover.rs` | 125,186 | `error!("Max retries ({max_attempts}) exceeded for origin {origin_id}")` |
| `musicfs-origins/src/nfs.rs` | 63 | `error!("NFS stale file handle after {max_retries} retries for {path}")` |
| `musicfs-cas/src/reader.rs` | 75 | `error!("File manifest not found for file_id {file_id}")` |
| `musicfs-cas/src/fetcher.rs` | 60,68 | `error!("File/Origin not found for file_id {file_id}")` |
| `musicfs-search/src/indexer.rs` | 44,56 | `error!("Search indexer/commit failed: {error}")` |
| `musicfs-sync/src/watcher.rs` | 36,59,63 | `error!("Watcher failed for origin {origin_id}: {error}")` |

### WARN Level (50+ locations) - Recoverable Issues

| Category | File | Line | Log Message |
|----------|------|------|-------------|
| **Retry Logic** | `failover.rs` | 90 | `warn!("Origin {origin_id} failed: {error}, trying next (attempt {n}/{total})")` |
| **Retry Logic** | `failover.rs` | 111-118 | `warn!("Retrying origin {origin_id} after {delay:?} (attempt {n}/{max})")` |
| **Retry Logic** | `nfs.rs` | 47-52 | `warn!("NFS stale handle for {path} (attempt {n}/{max}), retrying")` |
| **Retry Logic** | `smb.rs` | 45 | `warn!("SMB connection lost (ENOTCONN), retrying (attempt {n}/{max})")` |
| **Retry Logic** | `webhook.rs` | 94-108 | `warn!("Webhook delivery failed to {url} (attempt {n}/{max}): {error}")` |
| **Fallback** | `failover.rs` | 70-73 | `warn!("No healthy origins for {path}, using fallback {origin_id}")` |
| **Timeout** | `smb.rs` | 107-109 | `warn!("SMB health check timed out after 5s for {origin_id}")` |
| **Timeout** | `nfs.rs` | 104-106 | `warn!("NFS health check timed out after 5s for {origin_id}")` |
| **Timeout** | `prefetch.rs` | 91 | `warn!("Prefetch event receive timed out after 1s")` |
| **Health** | `health.rs` | 209 | `warn!("Origin {origin_id} is degraded (failures: {count})")` |
| **Health** | `health.rs` | 217-220 | `warn!("Origin {origin_id} is now unhealthy after {n} consecutive failures")` |
| **Remote FS** | `smb.rs` | 118 | `warn!("SMB watch using inotify on {share_path} - may be unreliable")` |
| **Remote FS** | `nfs.rs` | 115 | `warn!("NFS watch using inotify on {mount_point} - may be unreliable")` |
| **Plugin** | `manager.rs` | 152 | `warn!("Failed to load plugin from {path}: {error}")` |
| **Plugin** | `manager.rs` | 193-194 | `warn!("Failed to unload plugin {plugin_id}: {error}")` |
| **Prefetch** | `prefetch.rs` | 97 | `warn!("Failed to record access pattern for {file_id}: {error}")` |
| **Prefetch** | `prefetch.rs` | 159-161 | `warn!("Prefetch skipped: concurrency limit reached ({max})")` |
| **Search** | `indexer.rs` | 49 | `warn!("Search indexer event receive error: {error}")` |
| **Search** | `indexer.rs` | 82 | `warn!("No metadata found for file {path}, skipping indexing")` |
| **Collections** | `collections.rs` | 146,180 | `warn!("Failed to save/delete collection {name}: {error}")` |

### INFO Level (35+ locations) - Lifecycle & Major Operations

| Category | File | Line | Log Message |
|----------|------|------|-------------|
| **Lifecycle** | `main.rs` | 118 | `info!(version = env!("CARGO_PKG_VERSION"), "MusicFS starting")` |
| **Lifecycle** | `filesystem.rs` | 94 | `info!("Mounting MusicFS at {:?}", mountpoint)` |
| **Lifecycle** | `filesystem.rs` | 154 | `info!("MusicFS initialized")` |
| **Lifecycle** | `filesystem.rs` | 159 | `info!("MusicFS destroyed")` |
| **Origin** | `registry.rs` | 28 | `info!("Registering origin {} with priority {}", id, priority)` |
| **Origin** | `registry.rs` | 36 | `info!("Unregistering origin {}", id)` |
| **Origin** | `watcher.rs` | 65 | `info!("Watching origin {} at {:?}", origin_id, path)` |
| **Config** | `main.rs` | 127 | `info!("Cache directory: {:?}", cache_dir)` |
| **Config** | `main.rs` | 141 | `info!("CAS store initialized")` |
| **Config** | `store.rs` | 51 | `info!("CAS store opened: {} chunks, {} bytes", count, size)` (ADD) |
| **Sync** | `main.rs` | 150,152 | `info!("Scanning music files...")` / `info!("Found {} music files", count)` |
| **Sync** | `delta.rs` | 104 | `info!("Delta complete: {} added, {} removed, {} modified", a, r, m)` |
| **Sync** | `delta.rs` | 63 | `info!("Sync started for origin {}", origin_id)` (ADD) |
| **Index** | `main.rs` | 160 | `info!("Virtual tree built")` |
| **Index** | `indexer.rs` | 62 | `info!("Indexer stopping")` |
| **Index** | `indexer.rs` | 114 | `info!("Indexed {} files", count)` |
| **Index** | `index.rs` | 170 | `info!("Search index committed")` |
| **Health** | `health.rs` | 202 | `info!("Origin {} is now healthy", id)` |
| **Health** | `health.rs` | 150 | `info!("Health monitor started with interval {:?}", interval)` (ADD) |
| **Plugin** | `manager.rs` | 127 | `info!("Initializing plugin system")` |
| **Plugin** | `manager.rs` | 150 | `info!("Loaded plugin '{}' with id {:?}", name, id)` |
| **Plugin** | `manager.rs` | 256 | `info!("Shutting down plugin system")` |
| **Cache** | `prefetch.rs` | 123 | `info!("Prefetch engine stopped")` |
| **Cache** | `prefetch.rs` | 174 | `info!("Prefetched {:?}: {} chunks, {} bytes", file_id, chunks, bytes)` |
| **Cache** | `eviction.rs` | 51 | `info!("Evicted {} bytes from cache", bytes)` |
| **Cache** | `prefetch.rs` | 73 | `info!("Prefetch engine started (lookahead: {}, max_concurrent: {})")` (ADD) |

### DEBUG Level (60+ locations) - Operation Details

| Category | File | Line | Log Message |
|----------|------|------|-------------|
| **FUSE lookup** | `filesystem.rs` | 162,195,200 | Entry + result/miss |
| **FUSE getattr** | `filesystem.rs` | 203,230,233 | Entry + result/miss |
| **FUSE readdir** | `filesystem.rs` | 237,263,303 | Entry + result/miss |
| **FUSE read** | `filesystem.rs` | 325,338,362,364 | Entry + file_id + result/error |
| **Local origin** | `local.rs` | 51,68 | readdir entry + result |
| **Local origin** | `local.rs` | 88-91,112 | read entry + result |
| **SMB origin** | `smb.rs` | 86,93 | readdir/read entry + result |
| **NFS origin** | `nfs.rs` | 81,89 | readdir/read entry + result |
| **Failover** | `failover.rs` | 66,82,87 | Entry + trying origin + success |
| **Tree lookup** | `tree.rs` | 124,132 | Entry + result |
| **Metadata cache** | `metadata.rs` | 36,40 | lookup + is_fresh entry/result |
| **CAS store** | `store.rs` | 70,101 | put/get entry |
| **File reader** | `reader.rs` | 66,86 | manifest cache + read entry |
| **Search** | `ops/search.rs` | 107,141,182 | readdir_query + readlink + execute_query |
| **Search index** | `index.rs` | 98,174 | index_file + search entry |
| **Fetcher** | `fetcher.rs` | 54,61,121 | fetch_file entry + meta + ensure_cached |

**Key DEBUG fields**: `ino`, `parent`, `name`, `offset`, `size`, `bytes_read`, `origin_id`, `path`, `file_id`, `query`, `results_count`, `latency_ms`

### TRACE Level (100+ locations) - Fine-Grained Flow

| Category | File | Lines | What to Log |
|----------|------|-------|-------------|
| **Manifest cache** | `reader.rs` | 67-74 | Cache hit/miss decision |
| **Chunk iteration** | `reader.rs` | 107-127 | Each chunk: skip/read boundaries |
| **CAS dedup** | `store.rs` | 74-77 | Dedup hit decision |
| **CAS integrity** | `store.rs` | 121-134 | Verification result |
| **Tree lookup** | `tree.rs` | 118-129 | Path→inode + child lookup |
| **Tree parent** | `tree.rs` | 148-153 | Parent resolution path |
| **Prefetch event** | `prefetch.rs` | 91-120 | Event type match arms |
| **Prefetch semaphore** | `prefetch.rs` | 150-164 | In-flight check + acquire |
| **Delta scan** | `delta.rs` | 79-102 | Each file: cached/modified/unchanged/removed |
| **Delta entries** | `delta.rs` | 128-146 | Each entry: dir/audio/skip |
| **CDC chunking** | `cdc.rs` | 84-93 | Each chunk: offset/length/hash |
| **Failover origin** | `failover.rs` | 68-93 | Each origin attempt result |
| **Failover retry** | `failover.rs` | 107-122 | Each retry: attempt/success/delay |
| **Router select** | `router.rs` | 79-108 | Each candidate + selection reason |
| **FUSE node→attr** | `filesystem.rs` | 109-145 | Directory vs file conversion |
| **FUSE lookup** | `filesystem.rs` | 192-200 | Found/not found |
| **FUSE readdir** | `filesystem.rs` | 274-291 | Each child entry |
| **FUSE read** | `filesystem.rs` | 340-367 | file_id resolution + result |
| **Metadata tag** | `parser.rs` | 86-100 | Each tag extraction |
| **Health transition** | `health.rs` | 199-237 | State transition details |
| **Latency recording** | `router.rs` | 23-42 | Stats update per sample |

**Key TRACE patterns**:
- Every `match` arm: `trace!("match arm: {variant}")` 
- Every `if/else`: `trace!("branch: {condition}={value}")`
- Every loop iteration: `trace!("iteration {i}/{total}: ...")`
- Every cache lookup: `trace!("cache lookup key={key}, hit={hit}")`

---

## gRPC Handler Instrumentation (ADDED - Oracle Review)

**Gap identified**: 8/10 gRPC handlers had no logging.

### server.rs - All Handlers

| Handler | Line | Level | Log Message |
|---------|------|-------|-------------|
| `get_status()` | 209 | DEBUG | `debug!("gRPC get_status called")` |
| `get_cache_stats()` | 241 | DEBUG | `debug!("gRPC get_cache_stats called")` |
| `clear_cache()` | 278 | INFO | `info!("gRPC clear_cache: clearing {tier}")` |
| `prefetch()` | 296 | DEBUG | `debug!(file_count = paths.len(), "gRPC prefetch started")` |
| `list_origins()` | 322 | DEBUG | `debug!("gRPC list_origins called")` |
| `get_origin_health()` | 329 | DEBUG | `debug!(origin_id = %id, "gRPC get_origin_health")` |
| `rescan_origin()` | 337 | INFO | `info!(origin_id = %id, "gRPC rescan_origin started")` |
| `subscribe_events()` | 376 | INFO | `info!("gRPC subscribe_events: client connected")` |
| `shutdown()` | 402 | INFO | `info!(graceful = graceful, "gRPC shutdown requested")` |

### search_service.rs

| Handler | Line | Level | Log Message |
|---------|------|-------|-------------|
| `search()` | entry | DEBUG | `debug!(query = %q, limit = limit, "gRPC search")` |
| `search()` | result | DEBUG | `debug!(results = results.len(), "gRPC search completed")` |

### Pattern: Use `#[instrument]` on all handlers

```rust
#[tracing::instrument(level = "debug", skip(self, request), fields(method = "get_status"))]
async fn get_status(&self, request: Request<()>) -> Result<Response<StatusResponse>, Status> {
    // ...
}
```

---

## Async Task Spawn Instrumentation (ADDED - Oracle Review)

**Gap identified**: 14 `tokio::spawn` sites need correlation IDs and span propagation.

### Spawn Sites Requiring Instrumentation

| File | Line | Task | Instrumentation |
|------|------|------|-----------------|
| `server.rs` | 305 | prefetch stream | `spawn(async { ... }.instrument(info_span!("prefetch_stream")))` |
| `server.rs` | 354 | rescan stream | `spawn(async { ... }.instrument(info_span!("rescan_stream", origin_id = %id)))` |
| `server.rs` | 384 | subscribe events | `spawn(async { ... }.instrument(info_span!("event_subscriber")))` |
| `search_service.rs` | spawn | search task | `spawn(async { ... }.instrument(debug_span!("search_task", query = %q)))` |
| `indexer.rs` | spawn | indexer loop | `spawn(async { ... }.instrument(info_span!("indexer")))` |
| `prefetch.rs` | 87 | prefetch engine | `spawn(async { ... }.instrument(info_span!("prefetch_engine")))` |
| `prefetch.rs` | 169 | prefetch file | `spawn(async { ... }.instrument(debug_span!("prefetch_file", file_id = ?id)))` |
| `health.rs` | 154 | health monitor | `spawn(async { ... }.instrument(info_span!("health_monitor")))` |
| `watcher.rs` | 34 | file watcher | `spawn(async { ... }.instrument(info_span!("file_watcher", origin_id = %id)))` |
| `artwork.rs` | spawn | image decode | `spawn_blocking(|| { ... })` - add span before spawn |

### Pattern: Span Propagation

```rust
use tracing::Instrument;

// BEFORE (loses context)
tokio::spawn(async move {
    do_work().await;
});

// AFTER (preserves correlation)
let span = tracing::info_span!("task_name", task_id = %id);
tokio::spawn(async move {
    do_work().await;
}.instrument(span));
```

### Add to init_logging() for request IDs

```rust
// Generate request ID for correlation
use tracing::Span;
use uuid::Uuid;

fn with_request_id<F, R>(f: F) -> R
where F: FnOnce() -> R {
    let request_id = Uuid::new_v4();
    let span = tracing::info_span!("request", request_id = %request_id);
    span.in_scope(f)
}
```

---

## Database Operation Logging (ADDED - Oracle Review)

**Gap identified**: Zero logging for rusqlite operations in db.rs, collections.rs, patterns.rs, artwork.rs.

### db.rs - Core Database

| Function | Line | Level | Log Message |
|----------|------|-------|-------------|
| `open()` | entry | INFO | `info!(path = ?path, "Opening metadata database")` |
| `open()` | success | INFO | `info!(file_count = count, "Database opened")` |
| `upsert_file()` | entry | DEBUG | `debug!(file_id = ?id, path = %path, "Upserting file")` |
| `upsert_file()` | error | ERROR | `error!(file_id = ?id, error = %e, "Failed to upsert file")` |
| `get_file_by_id()` | miss | TRACE | `trace!(file_id = ?id, "File not found in db")` |
| `delete_file()` | entry | DEBUG | `debug!(file_id = ?id, "Deleting file from db")` |
| `list_files_by_origin()` | result | DEBUG | `debug!(origin_id = %id, count = files.len(), "Listed files")` |

### collections.rs

| Function | Line | Level | Log Message |
|----------|------|-------|-------------|
| `create()` | entry | INFO | `info!(name = %name, "Creating collection")` |
| `save()` | error | WARN | `warn!(name = %name, error = %e, "Failed to save collection")` |
| `delete()` | entry | INFO | `info!(name = %name, "Deleting collection")` |
| `list()` | result | DEBUG | `debug!(count = collections.len(), "Listed collections")` |

### patterns.rs - Access Patterns

| Function | Line | Level | Log Message |
|----------|------|-------|-------------|
| `record_access()` | entry | TRACE | `trace!(file_id = ?id, "Recording access pattern")` |
| `predict_next()` | result | DEBUG | `debug!(predictions = preds.len(), "Predicted next files")` |

### artwork.rs

| Function | Line | Level | Log Message |
|----------|------|-------|-------------|
| `store()` | entry | DEBUG | `debug!(file_id = ?id, size_bytes = data.len(), "Storing artwork")` |
| `get()` | hit/miss | TRACE | `trace!(file_id = ?id, found = found, "Artwork lookup")` |

### Pattern: Database Error Wrapper

```rust
// Add to musicfs-cache/src/db.rs
fn log_db_result<T>(op: &str, result: Result<T, rusqlite::Error>) -> Result<T, Error> {
    match result {
        Ok(v) => {
            tracing::trace!(op = op, "db operation succeeded");
            Ok(v)
        }
        Err(e) => {
            tracing::error!(op = op, error = %e, "db operation failed");
            Err(Error::Database(e.to_string()))
        }
    }
}
```

---

## Channel Operation Logging (ADDED - Oracle Review)

**Gap identified**: No logging for channel capacity, close, or broadcast lag.

### Channel Locations

| File | Type | Log Points |
|------|------|------------|
| `events.rs` | broadcast | Lag warning when receiver falls behind |
| `watcher.rs` | mpsc | Channel close on watcher shutdown |
| `server.rs` | mpsc | gRPC stream channel capacity |
| `indexer.rs` | mpsc | Event queue depth |
| `health.rs` | mpsc | Health check channel |

### Patterns

```rust
// Broadcast lag detection (events.rs)
match rx.recv().await {
    Ok(event) => { /* handle */ }
    Err(broadcast::error::RecvError::Lagged(n)) => {
        tracing::warn!(skipped = n, "Event subscriber lagged, skipped events");
    }
    Err(broadcast::error::RecvError::Closed) => {
        tracing::debug!("Event channel closed");
        break;
    }
}

// Channel capacity warning (before send)
if tx.capacity() < 10 {
    tracing::warn!(remaining = tx.capacity(), "Channel near capacity");
}

// Channel close
impl Drop for EventBus {
    fn drop(&mut self) {
        tracing::debug!("Event bus shutting down");
    }
}
```

---

## Drop Implementation Logging (ADDED - Oracle Review)

**Gap identified**: No logging in Drop impls for cleanup verification.

| File | Type | Log Message |
|------|------|-------------|
| `manager.rs:276` | `PluginManager` | `debug!("PluginManager dropping, unloading {} plugins", self.plugins.len())` |
| `watcher.rs:157` | `WatchHandle` | `trace!(origin_id = %self.origin_id, "WatchHandle dropped")` |
| `prefetch.rs` | `PrefetchEngine` | `debug!("PrefetchEngine dropping, {} in-flight", self.in_flight.len())` |
| `server.rs` | gRPC server | `info!("gRPC server shutting down")` |

### Pattern

```rust
impl Drop for PluginManager {
    fn drop(&mut self) {
        tracing::debug!(
            plugin_count = self.plugins.len(),
            "PluginManager dropping"
        );
        // existing cleanup...
    }
}
```

---

## Credential Loading (ADDED - Oracle Review)

**Gap identified**: No logging in credentials.rs::load().

| Function | Level | Log Message |
|----------|-------|-------------|
| `load()` entry | DEBUG | `debug!(origin_id = %origin_id, "Loading credentials")` |
| `load()` cache hit | TRACE | `trace!(origin_id = %origin_id, "Credential cache hit")` |
| `load()` success | INFO | `info!(origin_id = %origin_id, cred_type = %cred.type_name(), "Credential loaded")` |
| `load()` not found | DEBUG | `debug!(origin_id = %origin_id, "No credential found")` |
| `load()` error | WARN | `warn!(origin_id = %origin_id, error = %e, "Credential load failed")` |

**SECURITY**: Never log credential values. The existing Debug impl with redaction is correct.

---

## Security Considerations (ADDED - Oracle Review)

### Never Log These

| Data | Location | Mitigation |
|------|----------|------------|
| `WebhookConfig.secret` | webhook.rs | Add `#[serde(skip_serializing)]`, use custom Debug |
| Credential values | credentials.rs | Already redacted in Debug impl ✓ |
| Full file paths with usernames | everywhere | Sanitize `/home/{user}/` → `~/` |
| API keys/tokens | config.rs | Mark sensitive fields |

### Sanitization Helper

```rust
// Add to musicfs-core/src/lib.rs
pub fn sanitize_path(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        path.to_string_lossy()
            .replace(&home, "~")
            .to_string()
    } else {
        path.to_string_lossy().to_string()
    }
}

// Usage
debug!(path = %sanitize_path(&path), "Reading file");
```

### WebhookConfig Fix

```rust
// webhook.rs - add custom Debug
#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    #[serde(skip_serializing)]
    pub secret: Option<String>,  // Never serialize
    // ...
}

impl std::fmt::Debug for WebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookConfig")
            .field("url", &self.url)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}
```

---

## Performance Considerations (ADDED - Oracle Review)

### Hot Path Warnings

| Path | Risk | Mitigation |
|------|------|------------|
| `reader.rs` chunk loop | 100s of TRACE logs per seek | Log summary only: `trace!(chunks_read = n, "Read complete")` |
| `store.rs` put/get | 1000s during sync | Keep at DEBUG, not TRACE |
| `delta.rs` file scan | Log per file during full scan | Use TRACE, batch summaries at DEBUG |
| `parser.rs` tag extraction | Many TRACE per file | Sample: log every 100th file |

### Trace Sampling Config

```rust
// Add to LoggingConfig
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    // ... existing fields ...
    
    /// Sample rate for TRACE logs in hot paths (0.0-1.0, default 1.0)
    #[serde(default = "default_sample_rate")]
    pub trace_sample_rate: f32,
}

fn default_sample_rate() -> f32 { 1.0 }

// Usage in hot paths
if rand::random::<f32>() < config.trace_sample_rate {
    trace!(...);
}
```

### Rate-Limited Warnings

```rust
// For repeating warnings during outages (failover.rs)
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static LAST_FAILOVER_WARN: AtomicU64 = AtomicU64::new(0);

fn warn_rate_limited(origin_id: &str, error: &str) {
    let now = Instant::now().elapsed().as_secs();
    let last = LAST_FAILOVER_WARN.load(Ordering::Relaxed);
    if now - last >= 60 { // Max once per minute
        LAST_FAILOVER_WARN.store(now, Ordering::Relaxed);
        warn!(origin_id = %origin_id, error = %error, "Origin failover");
    }
}
```

---

## Standardized Field Names (ADDED - Oracle Review)

Use these consistently across all log statements:

| Field | Type | Usage |
|-------|------|-------|
| `origin_id` | String | Origin identifier (not `origin`) |
| `file_id` | FileId | File identifier |
| `path` | String | Virtual or real path (sanitized) |
| `size_bytes` | u64 | Size in bytes (not `size`, `bytes`, `len`) |
| `offset` | u64 | Read offset |
| `duration_ms` | u64 | Operation duration in milliseconds |
| `count` | usize | Generic count |
| `attempt` | u32 | Retry attempt number |
| `max_attempts` | u32 | Maximum retry attempts |
| `error` | impl Display | Error message (not `err`, `e`) |
| `request_id` | Uuid | Correlation ID for requests |

---

## Instrumentation Patterns (ADDED - Oracle Review)

### Use `#[instrument(err)]` for Automatic Error Logging

```rust
// BEFORE: Manual error logging
pub async fn read(&self, path: &Path) -> Result<Bytes> {
    match self.inner_read(path).await {
        Ok(data) => Ok(data),
        Err(e) => {
            error!(path = ?path, error = %e, "Read failed");
            Err(e)
        }
    }
}

// AFTER: Automatic with #[instrument]
#[tracing::instrument(level = "debug", skip(self), err)]
pub async fn read(&self, path: &Path) -> Result<Bytes> {
    self.inner_read(path).await
}
```

### Span Events vs Regular Logs

```rust
// Regular log - standalone event
info!("Operation completed");

// Span event - attached to current span context
tracing::Span::current().record("result", "success");

// Prefer span events for operation outcomes
#[instrument(fields(result))]
async fn operation() -> Result<()> {
    // ... work ...
    Span::current().record("result", "success");
    Ok(())
}
```

---

## Fixes: Incorrect Line References (ADDED - Oracle Review)

| File | Issue | Fix |
|------|-------|-----|
| `webhook.rs:43` | Uses `expect()` (panics) | Replace with `?` + error log |
| `webhook.rs:133` | Uses `expect()` (panics) | Replace with `?` + error log |

```rust
// webhook.rs - BEFORE
let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()
    .expect("Failed to create HTTP client");

// webhook.rs - AFTER  
let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()
    .map_err(|e| {
        error!(error = %e, "Failed to create webhook HTTP client");
        WebhookError::ClientInit(e.to_string())
    })?;
```

---

## Log Levels Guide

| Level | Use Case | Example |
|-------|----------|---------|
| `ERROR` | Unrecoverable failures | Mount failed, DB corruption |
| `WARN` | Recoverable issues | Origin timeout, retry needed |
| `INFO` | Lifecycle events | Service start/stop, health change |
| `DEBUG` | Operation details | Function entry, request params |
| `TRACE` | Fine-grained flow | Match arms, cache hit/miss |

---

## Testing Checklist

### Basic Functionality
- [ ] Log files created in configured directory
- [ ] Daily rotation creates new files at midnight
- [ ] JSON output parseable by `jq`
- [ ] `journalctl -t musicfs` shows logs
- [ ] `RUST_LOG=musicfs=trace` enables trace output
- [ ] WorkerGuard kept alive (logs flush on shutdown)
- [ ] Logrotate compresses old files

### Correlation & Context (NEW)
- [ ] Request IDs propagate through async tasks
- [ ] Spawned task logs include parent span context
- [ ] gRPC handler logs show method name in span

### Security (NEW)
- [ ] WebhookConfig.secret never appears in logs
- [ ] Credential values never appear in logs
- [ ] File paths with `/home/{user}` show as `~/`

### Performance (NEW)
- [ ] TRACE sampling respects `trace_sample_rate` config
- [ ] Hot path chunk loops log summary, not per-chunk
- [ ] Origin failover warnings are rate-limited (1/minute)
- [ ] Database operations log without blocking

### Database & Channels (NEW)
- [ ] Database open logs file count
- [ ] Channel capacity warnings appear when queue fills
- [ ] Broadcast lag warnings appear when subscriber falls behind
- [ ] Drop implementations log cleanup

---

## Summary

| Phase | Effort | Deliverables |
|-------|--------|--------------|
| 1. Config & Dependencies | 2h | LoggingConfig, init_logging(), logrotate, trace sampling |
| 2. Core instrumentation | 1h | tracing in musicfs-core, credentials, sanitization |
| 3. Hot path instrumentation | 4h | #[instrument] + trace! across 5 crates |
| 4. gRPC & async tasks | 2h | Handler instrumentation, spawn correlation |
| 5. Database & channels | 2h | rusqlite logging, channel capacity/close |
| 6. Production files | 1h | Updated systemd, example config |
| **Total** | **12h** | Full observability |

---

## Files to Modify

### Phase 1: Config & Dependencies
| File | Changes |
|------|---------|
| `Cargo.toml` (workspace) | Add tracing-appender, tracing-journald |
| `crates/musicfs-cli/Cargo.toml` | Add dependencies |
| `crates/musicfs-core/Cargo.toml` | Add tracing |
| `crates/musicfs-core/src/config.rs` | Add LoggingConfig with trace_sample_rate |
| `crates/musicfs-cli/src/main.rs` | Expand init_logging(), request ID helper |
| `crates/musicfs-core/src/lib.rs` | Add sanitize_path() helper |

### Phase 2: Core Instrumentation
| File | Changes |
|------|---------|
| `crates/musicfs-core/src/credentials.rs` | Add load() logging (redacted) |
| `crates/musicfs-core/src/events.rs` | Add broadcast lag detection |

### Phase 3: Hot Path Instrumentation
| File | Changes |
|------|---------|
| `crates/musicfs-fuse/src/filesystem.rs` | Add #[instrument], trace! |
| `crates/musicfs-origins/src/failover.rs` | Add #[instrument], trace!, rate-limited warn |
| `crates/musicfs-origins/src/health.rs` | Add state transition logging |
| `crates/musicfs-origins/src/router.rs` | Add selection logging |
| `crates/musicfs-cache/src/tree.rs` | Add mutation logging |
| `crates/musicfs-cache/src/metadata.rs` | Add hit/miss logging |
| `crates/musicfs-cas/src/reader.rs` | Add chunk assembly logging (summary, not per-chunk) |
| `crates/musicfs-cas/src/store.rs` | Add dedup logging |
| `crates/musicfs-sync/src/delta.rs` | Add change detection logging |

### Phase 4: gRPC & Async Tasks (NEW)
| File | Changes |
|------|---------|
| `crates/musicfs-grpc/src/server.rs` | Add #[instrument] to all 10 handlers, spawn correlation |
| `crates/musicfs-grpc/src/search_service.rs` | Add #[instrument], spawn instrumentation |
| `crates/musicfs-grpc/src/webhook.rs` | Fix expect() → error!, custom Debug for secret |
| `crates/musicfs-cache/src/prefetch.rs` | Add spawn instrumentation, Drop logging |
| `crates/musicfs-search/src/indexer.rs` | Add spawn instrumentation |
| `crates/musicfs-sync/src/watcher.rs` | Add spawn instrumentation, Drop logging |
| `crates/musicfs-plugins/src/manager.rs` | Add Drop logging |

### Phase 5: Database & Channels (NEW)
| File | Changes |
|------|---------|
| `crates/musicfs-cache/src/db.rs` | Add log_db_result() helper, open/upsert/query logging |
| `crates/musicfs-search/src/collections.rs` | Add CRUD operation logging |
| `crates/musicfs-cache/src/patterns.rs` | Add access pattern logging |
| `crates/musicfs-cache/src/artwork.rs` | Add store/get logging |

### Phase 6: Production Files
| File | Changes |
|------|---------|
| `dist/musicfs.service` | Add logging directives |
| `dist/logrotate.d/musicfs` | New file |
| `dist/config.example.toml` | Add logging section with trace_sample_rate |
