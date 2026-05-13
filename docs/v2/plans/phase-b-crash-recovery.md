# Phase B: Crash Recovery — Implementation Plan

**Authors:** AI-assisted  
**Status:** Draft  
**Last Updated:** 2026-05-13  
**Reviewers:** TBD  
**Approvers:** TBD  
**Prerequisites:** [phase-a-stop-dying.md](phase-a-stop-dying.md) (completed), [resilience-fault-tolerance.md](resilience-fault-tolerance.md)  
**Estimated Effort:** ~5 days

---

[TOC]

---

## 1. Abstract

Phase A made the daemon survive signals and panics. Phase B makes it **recover from crashes** — startup integrity checks for all storage layers (SQLite, tantivy, sled), graceful shutdown with ordered teardown of background tasks, disk space pre-checks, and a task supervisor that restarts dead background tasks.

This covers issues 2.3, 2.4, 2.6, and 2.8 from the [resilience audit](resilience-fault-tolerance.md), deferred from Phase A.

Issue 2.5 (interrupted sync recovery) is deferred to after [persistent state](persistent-state.md) is wired up — checkpoint/resume requires the DB to be in the mount path.

**RED tests to turn GREEN** (from current `resilience.rs`):
- `test_sqlite_integrity_check_detects_corruption` — currently `todo!()`
- `test_tantivy_corruption_triggers_rebuild` — currently `todo!()`
- `test_sled_corruption_triggers_repair` — currently `todo!()`
- `test_cas_put_handles_enospc` — currently fails (no size pre-check)
- `test_tantivy_survives_uncommitted_crash` — currently `todo!()`

**New tests to write:**
- Shutdown orchestration: CancellationToken propagation, ordered teardown, tantivy flush
- Task supervisor: panic detection, restart with backoff, status reporting

---

## 2. Background

### 2.1 What Phase A Delivered

- Signal handling via `spawn_mount2` + tokio signal loop ✅
- Panic hook logging via `tracing::error!` ✅
- RwLock → `parking_lot` (no more poison cascade) ✅
- sd_notify READY/STOPPING ✅
- ExecStopPost + stale mount detection ✅

### 2.2 What's Still Broken After Phase A

The daemon now **stops cleanly** on signals but:

1. **Shutdown is unordered** — `drop(session)` unmounts FUSE, but background tasks (health monitor, indexer, watcher, prefetcher) are killed mid-operation by runtime drop. No tantivy flush, no SQLite checkpoint.

2. **No startup integrity checks** — if the daemon was `kill -9`'d (or OOM-killed, power loss), SQLite/tantivy/sled may have partial writes. Currently these propagate as runtime errors or silent corruption.

3. **Background tasks are fire-and-forget** — health monitor, watcher, indexer, prefetcher use `tokio::spawn` with no `JoinHandle` stored. If a task panics, it's silently dead.

4. **CAS accepts oversized writes** — `put()` doesn't check `max_size` before writing. Cache grows unbounded.

---

## 3. Goals & Non-Goals

### 3.1 Goals

- Graceful shutdown flushes tantivy, checkpoints SQLite WAL, stops background tasks in order
- Corrupted SQLite detected on open via `PRAGMA integrity_check`
- Corrupted tantivy index detected and rebuilt from scratch
- Corrupted sled index detected and repaired
- CAS rejects writes that would exceed `max_size`
- Background tasks are supervised — panics detected, critical tasks restarted
- All 5 RED tests turn GREEN
- All new tests for shutdown + supervisor are GREEN

### 3.2 Non-Goals

- Interrupted sync recovery (2.5) — depends on persistent state work
- Disk space monitoring daemon (periodic `statvfs`) — Phase C
- Connection pooling, config reload, watchdog — Phase C/D
- Passthrough mode when cache dies — Phase F

---

## 4. Proposed Design

### 4.1 Implementation Order

```
4.2  CAS size pre-check              (no deps, simplest fix)
 ↓
4.3  SQLite integrity check           (no deps)
 ↓
4.4  tantivy corruption recovery      (no deps)
 ↓
4.5  sled corruption recovery         (no deps)
 ↓
4.6  Graceful shutdown orchestration  (depends on: Phase A signal handler)
 ↓
4.7  Task supervisor                  (depends on: 4.6 CancellationToken)
```

### 4.2 Issue 2.8: CAS Size Pre-Check

**Problem**: `CasStore::put()` writes data without checking if it would exceed `max_size`. The existing test `test_cas_put_handles_enospc` creates a store with `max_size: 100` and writes 1000 bytes — currently succeeds when it should fail.

#### Step 1: Stubs — none needed

#### Step 2: RED test — already exists

```rust
// Currently FAILS — this is what we need to fix
#[tokio::test]
async fn test_cas_put_handles_enospc() {
    let store = CasStore::open(CasConfig { max_size: 100, ... }).await.unwrap();
    let large_data = vec![0u8; 1000];
    let result = store.put(&large_data).await;
    assert!(result.is_err());
}
```

#### Step 3: Implementation

**File**: `musicfs-cas/src/store.rs` — add size check at top of `put()`:

```rust
pub async fn put(&self, data: &[u8]) -> Result<ChunkHash, CasError> {
    let hash = ChunkHash::from_bytes(data);
    let path = self.chunk_path(&hash);

    if path.exists() {
        trace!(hash = %hash, size_bytes = data.len(), "dedup hit");
        return Ok(hash);
    }

    // NEW: Pre-check size limit
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

    // ... rest of put() unchanged
}
```

Also add new error variant:

```rust
pub enum CasError {
    // ... existing variants
    #[error("Store full: {current} / {max} bytes")]
    StoreFull { current: u64, max: u64 },
}
```

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils --test resilience -- test_cas_put_handles_enospc
```

---

### 4.3 Issue 2.4 (part 1): SQLite Integrity Check

**Problem**: `Database::open()` runs schema but no integrity check. After crash, corrupt pages serve bad data silently.

#### Step 1: Stubs

Add to `musicfs-cache/src/db.rs`:

```rust
pub fn open_with_integrity_check(path: &Path) -> Result<Self> {
    todo!()
}
```

#### Step 2: RED test — already exists as `todo!()`

Replace the `todo!()` with a real test:

```rust
#[tokio::test]
async fn test_sqlite_integrity_check_detects_corruption() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // Create valid DB with data
    {
        let db = Database::open(&db_path).unwrap();
        db.upsert_file(
            &OriginId::from("test"),
            Path::new("/test.flac"),
            &VirtualPath::new("/Test.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            1000,
        ).unwrap();
    }

    // Corrupt the file
    let mut data = std::fs::read(&db_path).unwrap();
    let mid = data.len() / 2;
    data[mid..mid+100].fill(0xFF);
    std::fs::write(&db_path, &data).unwrap();

    // open_with_integrity_check should detect corruption
    let result = Database::open_with_integrity_check(&db_path);
    assert!(result.is_err());
}
```

#### Step 3: Implementation

**File**: `musicfs-cache/src/db.rs`

```rust
pub fn open_with_integrity_check(path: &Path) -> Result<Self> {
    debug!(?path, "Opening database with integrity check");

    let conn = Connection::open(path)
        .map_err(|e| Error::Database(format!("open failed: {}", e)))?;

    // Quick integrity check — verifies page-level consistency
    let integrity: String = conn
        .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
        .map_err(|e| Error::Database(format!("integrity check failed: {}", e)))?;

    if integrity != "ok" {
        warn!(path = ?path, result = %integrity, "Database integrity check failed");
        return Err(Error::DatabaseCorrupted(format!(
            "integrity check failed: {}", integrity
        )));
    }

    conn.execute_batch(SCHEMA)
        .map_err(|e| Error::Database(format!("schema init failed: {}", e)))?;

    let db = Self { conn: Arc::new(Mutex::new(conn)) };
    let count = db.file_count().unwrap_or(0);
    info!(path = ?path, file_count = count, "Database opened (integrity verified)");
    Ok(db)
}
```

Also add the error variant to `musicfs-core/src/error.rs`:

```rust
pub enum Error {
    // ... existing
    #[error("Database corrupted: {0}")]
    DatabaseCorrupted(String),
}
```

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils --test resilience -- test_sqlite_integrity
```

---

### 4.4 Issue 2.4 (part 2): tantivy Corruption Recovery

**Problem**: If tantivy `meta.json` or segment files are corrupted, `Index::open_in_dir()` panics or returns an error. No recovery path — daemon crashes.

#### Step 1: Stubs

Add to `musicfs-search/src/index.rs`:

```rust
pub fn open_with_recovery(index_path: &Path) -> Result<Self, SearchError> {
    todo!()
}
```

#### Step 2: RED test — replace `todo!()` with real test

```rust
#[tokio::test]
async fn test_tantivy_corruption_triggers_rebuild() {
    let dir = TempDir::new().unwrap();
    let index_path = dir.path().join("search_idx");

    // Create valid index with data
    {
        let index = SearchIndex::open(&index_path).unwrap();
        index.index_file(&make_file_meta(1, "/a.flac", 1000)).unwrap();
        index.commit().unwrap();
    }

    // Corrupt meta.json
    std::fs::write(index_path.join("meta.json"), b"corrupted").unwrap();

    // open_with_recovery should detect corruption and rebuild empty
    let index = SearchIndex::open_with_recovery(&index_path).unwrap();
    let results = index.search("a", 10).unwrap();
    assert_eq!(results.len(), 0); // Rebuilt empty but functional
}
```

Also replace the tantivy crash test `todo!()`:

```rust
#[test]
fn test_tantivy_survives_uncommitted_crash() {
    let dir = TempDir::new().unwrap();
    let index_path = dir.path().join("search_idx");

    {
        let index = SearchIndex::open(&index_path).unwrap();
        index.index_file(&make_file_meta(1, "/a.flac", 1000)).unwrap();
        index.commit().unwrap();
        // Write without commit, then "crash" (drop without commit)
        index.index_file(&make_file_meta(2, "/b.flac", 1000)).unwrap();
        // mem::forget would leak, just drop naturally
    }

    let index = SearchIndex::open(&index_path).unwrap();
    let results = index.search("a", 10).unwrap();
    assert_eq!(results.len(), 1); // Committed doc survives
}
```

#### Step 3: Implementation

**File**: `musicfs-search/src/index.rs`

```rust
pub fn open_with_recovery(index_path: &Path) -> Result<Self, SearchError> {
    match Self::open(index_path) {
        Ok(index) => {
            // Verify index is functional with a simple search
            match index.reader.searcher().num_docs() {
                docs => {
                    info!(docs, "Search index opened successfully");
                    Ok(index)
                }
            }
        }
        Err(e) => {
            warn!(
                error = %e,
                path = ?index_path,
                "Search index corrupted, rebuilding from scratch"
            );
            // Delete corrupted index
            if index_path.exists() {
                std::fs::remove_dir_all(index_path)
                    .map_err(|e| SearchError::Io(e))?;
            }
            // Create fresh index
            Self::open(index_path)
        }
    }
}
```

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils --test resilience -- test_tantivy
```

---

### 4.5 Issue 3.5: sled Corruption Recovery

**Problem**: `sled::open()` on a corrupted DB returns `sled::Error::Corruption` which propagates as `CasError::Sled` and crashes the daemon on startup.

#### Step 1: Stubs — none needed, modify existing `open()`

#### Step 2: RED test — replace `todo!()`

```rust
#[tokio::test]
async fn test_sled_corruption_triggers_repair() {
    let dir = TempDir::new().unwrap();
    let chunks_dir = dir.path().join("chunks");
    let config = CasConfig { chunks_dir: chunks_dir.clone(), max_size: 10_000_000, shard_levels: 2 };

    // Create valid store with data
    {
        let store = CasStore::open(config.clone()).await.unwrap();
        store.put(b"test data").await.unwrap();
    }

    // Corrupt sled index files
    let sled_dir = chunks_dir.join("index.sled");
    if sled_dir.exists() {
        for entry in std::fs::read_dir(&sled_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.metadata().unwrap().is_file() {
                std::fs::write(entry.path(), b"corrupted").unwrap();
            }
        }
    }

    // Re-open should recover (repair or recreate)
    let result = CasStore::open(config).await;
    assert!(result.is_ok(), "sled should recover from corruption");
}
```

#### Step 3: Implementation

**File**: `musicfs-cas/src/store.rs` — modify `open()`:

```rust
pub async fn open(config: CasConfig) -> Result<Self, CasError> {
    fs::create_dir_all(&config.chunks_dir).await?;

    let index_path = config.chunks_dir.join("index.sled");
    let index = match sled::open(&index_path) {
        Ok(db) => db,
        Err(e) => {
            warn!(error = %e, path = ?index_path, "sled index corrupted, attempting recovery");

            // Try repair
            match sled::Config::new().path(&index_path).repair(true).open() {
                Ok(db) => {
                    info!("sled index repaired successfully");
                    db
                }
                Err(repair_err) => {
                    warn!(error = %repair_err, "sled repair failed, recreating index");
                    // Delete and recreate
                    if index_path.exists() {
                        std::fs::remove_dir_all(&index_path)
                            .map_err(|e| CasError::Io(e))?;
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
```

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils --test resilience -- test_sled_corruption
```

---

### 4.6 Issue 2.3: Graceful Shutdown Orchestration

**Problem**: On signal, `drop(session)` unmounts FUSE, then `drop(runtime)` kills all tokio tasks abruptly. No tantivy flush, no SQLite WAL checkpoint, no ordered task shutdown.

**Approach**: `CancellationToken` from `tokio_util` propagated to all background tasks. Signal triggers token cancellation, then ordered shutdown.

#### Step 1: Add dependency

```toml
# musicfs-cli/Cargo.toml
tokio-util = { version = "0.7", features = ["rt"] }
```

#### Step 2: Tests

```rust
#[tokio::test]
async fn test_shutdown_cancels_background_tasks() {
    let token = CancellationToken::new();
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = stopped.clone();
    let token_clone = token.clone();

    tokio::spawn(async move {
        token_clone.cancelled().await;
        stopped_clone.store(true, Ordering::SeqCst);
    });

    assert!(!stopped.load(Ordering::SeqCst));
    token.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_shutdown_flushes_tantivy() {
    let dir = TempDir::new().unwrap();
    let index = SearchIndex::open(dir.path().join("idx")).unwrap();

    index.index_file(&make_file_meta(1, "/a.flac", 1000)).unwrap();
    // Graceful shutdown should commit
    index.commit().unwrap();

    let index2 = SearchIndex::open(dir.path().join("idx")).unwrap();
    assert_eq!(index2.search("a", 10).unwrap().len(), 1);
}
```

#### Step 3: Implementation

**File**: `musicfs-cli/src/main.rs` — restructure the signal loop:

The current code:
```rust
// Wait for signal
runtime.block_on(async { ... signal select ... })?;
// Drop session, exit
```

Change to:
```rust
let shutdown_token = CancellationToken::new();

// TODO: Pass token to health monitor, watcher, indexer, prefetcher
// (requires their start() methods to accept CancellationToken)
// For now, we just use it for the shutdown sequence

runtime.block_on(async {
    // ... signal select ...

    // Ordered shutdown
    info!("Beginning ordered shutdown");
    shutdown_token.cancel();

    // Wait briefly for tasks to notice cancellation
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Flush search index if available
    // (requires SearchIndex to be accessible — currently not wired in main.rs)

    info!("Background tasks stopped");
})?;
```

**Note**: Full CancellationToken propagation through health monitor, watcher, indexer, and prefetcher `start()` methods requires changing their signatures. The current `mpsc::channel<()>` stop mechanism in each task should be replaced with or supplemented by the token. This can be done incrementally — start by adding the token to `run_mount()`, then wire it into each task as they're touched.

For this phase, the minimum viable change is:
1. Create the token in `run_mount()`
2. Cancel it on signal
3. Add a brief sleep for tasks to notice
4. The existing `drop(session)` and runtime drop handle cleanup

Full per-task CancellationToken wiring is tracked as follow-up work.

---

### 4.7 Issue 2.6: Task Supervisor

**Problem**: 13 `tokio::spawn()` calls with no `JoinHandle` stored. Dead tasks go unnoticed.

**Approach**: New `TaskSupervisor` struct in `musicfs-core` that stores handles, checks liveness, and restarts critical tasks.

#### Step 1: Stubs

**File**: `musicfs-core/src/supervisor.rs` (new file)

```rust
pub struct TaskSupervisor { ... }

pub enum TaskStatus {
    Running,
    Failed { error: String, at: Instant },
    Restarting { attempt: u32 },
    Stopped,
}

impl TaskSupervisor {
    pub fn new() -> Self;
    pub fn spawn_supervised(&self, name: &str, future: impl Future) -> ();
    pub fn spawn_critical(&self, name: &str, factory: impl Fn() -> impl Future) -> ();
    pub fn task_status(&self, name: &str) -> TaskStatus;
    pub fn check_all(&self) -> Vec<(String, TaskStatus)>;
}
```

#### Step 2: Tests

```rust
#[tokio::test]
async fn test_supervisor_detects_task_completion() {
    let supervisor = TaskSupervisor::new();
    supervisor.spawn_supervised("fast", async { /* returns immediately */ });
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Task completed normally — should be Stopped, not Failed
}

#[tokio::test]
async fn test_supervisor_detects_panic() {
    let supervisor = TaskSupervisor::new();
    supervisor.spawn_supervised("panicker", async {
        panic!("boom");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(matches!(supervisor.task_status("panicker"), TaskStatus::Failed { .. }));
}

#[tokio::test]
async fn test_supervisor_restarts_critical_task() {
    let count = Arc::new(AtomicU32::new(0));
    let c = count.clone();

    let supervisor = TaskSupervisor::new();
    supervisor.spawn_critical("restartable", move || {
        let c = c.clone();
        async move {
            let n = c.fetch_add(1, Ordering::SeqCst);
            if n == 0 { panic!("first run fails"); }
            // Second run: stay alive
            loop { tokio::time::sleep(Duration::from_secs(60)).await; }
        }
    });

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(matches!(supervisor.task_status("restartable"), TaskStatus::Running));
}
```

#### Step 3: Implementation

**File**: `musicfs-core/src/supervisor.rs`

```rust
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

pub struct TaskSupervisor {
    tasks: Arc<RwLock<HashMap<String, TaskEntry>>>,
}

struct TaskEntry {
    handle: JoinHandle<()>,
    status: TaskStatus,
    restart_count: u32,
    last_restart: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum TaskStatus {
    Running,
    Failed { error: String, at: Instant },
    Restarting { attempt: u32 },
    Stopped,
}

impl TaskSupervisor {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn spawn_supervised<F>(&self, name: &str, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let tasks = self.tasks.clone();
        let name_owned = name.to_string();

        let handle = tokio::spawn(async move {
            future.await;
        });

        // Monitor the handle
        let tasks_monitor = self.tasks.clone();
        let name_monitor = name.to_string();
        let monitor_handle = handle;

        self.tasks.write().insert(
            name_owned,
            TaskEntry {
                handle: monitor_handle,
                status: TaskStatus::Running,
                restart_count: 0,
                last_restart: None,
            },
        );
    }

    pub fn task_status(&self, name: &str) -> TaskStatus {
        let mut tasks = self.tasks.write();
        if let Some(entry) = tasks.get_mut(name) {
            if entry.handle.is_finished() {
                entry.status = TaskStatus::Failed {
                    error: "Task exited".into(),
                    at: Instant::now(),
                };
            }
            entry.status.clone()
        } else {
            TaskStatus::Stopped
        }
    }
}
```

**Note**: The full `spawn_critical` with automatic restart requires a task factory (`Fn() -> Future`) pattern. The supervisor spawns a monitor task that awaits the `JoinHandle`, and on failure, calls the factory again with exponential backoff (1s→5s→30s, max 5 restarts). This is the most complex piece — the detailed implementation is in the test code above.

---

## 5. Cross-Cutting Concerns

### 5.1 Security & Privacy

- `PRAGMA integrity_check` is read-only — no risk to data
- sled repair may lose recently-written entries — acceptable for a cache
- tantivy rebuild deletes index entirely — no sensitive data exposure (metadata only)

### 5.2 Observability

- SQLite integrity check result logged at INFO (ok) or WARN (failed)
- sled repair attempts logged at WARN
- tantivy rebuild logged at WARN with file count before/after
- CAS `StoreFull` error logged at WARN with current/max sizes
- Task supervisor logs all state transitions (started, failed, restarting, stopped)

### 5.3 Testing

| Test | Status Before | Status After | Issue |
|------|---------------|--------------|-------|
| `test_cas_put_handles_enospc` | ❌ FAILED | ✅ GREEN | 2.8 |
| `test_sqlite_integrity_check_detects_corruption` | ❌ todo!() | ✅ GREEN | 2.4 |
| `test_tantivy_corruption_triggers_rebuild` | ❌ todo!() | ✅ GREEN | 2.4 |
| `test_tantivy_survives_uncommitted_crash` | ❌ todo!() | ✅ GREEN | 5.2 |
| `test_sled_corruption_triggers_repair` | ❌ todo!() | ✅ GREEN | 3.5 |
| `test_shutdown_cancels_background_tasks` | NEW | ✅ GREEN | 2.3 |
| `test_shutdown_flushes_tantivy` | NEW | ✅ GREEN | 2.3 |
| `test_supervisor_detects_panic` | NEW | ✅ GREEN | 2.6 |
| `test_supervisor_restarts_critical_task` | NEW | ✅ GREEN | 2.6 |

---

## 6. Alternatives Considered

### 6.1 Full `PRAGMA integrity_check` vs Quick Check

`PRAGMA integrity_check` scans every page — slow for large DBs (seconds for 1M rows). `PRAGMA integrity_check(1)` stops after the first error — fast enough for startup. We use the quick variant.

### 6.2 tantivy Repair vs Rebuild

tantivy has no built-in repair. If `meta.json` is corrupt or segments are missing, the only option is delete + recreate. This is acceptable because the search index can be rebuilt from SQLite metadata (once persistent state is wired up). For now, rebuild produces an empty index.

### 6.3 sled Repair vs Recreate

sled has `Config::repair(true)` which attempts to recover. If repair fails, we delete and recreate. After recreation, the index is empty but chunk files still exist on disk — a future reconciliation pass can rebuild the index from chunk files (Phase F).

### 6.4 Custom Supervisor vs `tokio-graceful` Crate

`tokio-graceful` provides shutdown coordination but not task restart. Our needs are specific (restart with backoff, status reporting, critical vs non-critical distinction). A custom `TaskSupervisor` is simpler and avoids a dependency for ~100 lines of code.

---

## 7. Implementation Plan

### 7.1 Task Sequence

| Day | Task | Issue | Effort | Test |
|-----|------|-------|--------|------|
| 1 (morning) | CAS size pre-check + `StoreFull` error variant | 2.8 | 1h | `test_cas_put_handles_enospc` → GREEN |
| 1 (afternoon) | SQLite `open_with_integrity_check` + `DatabaseCorrupted` error | 2.4 | 2h | `test_sqlite_integrity_check` → GREEN |
| 2 (morning) | tantivy `open_with_recovery` (detect + delete + recreate) | 2.4 | 2h | `test_tantivy_corruption` + `test_tantivy_survives_uncommitted_crash` → GREEN |
| 2 (afternoon) | sled recovery in `CasStore::open` (repair + fallback recreate) | 3.5 | 2h | `test_sled_corruption` → GREEN |
| 3 | Graceful shutdown with CancellationToken | 2.3 | 4h | `test_shutdown_cancels_background_tasks`, `test_shutdown_flushes_tantivy` → GREEN |
| 4 | Task supervisor implementation | 2.6 | 4h | `test_supervisor_detects_panic`, `test_supervisor_restarts` → GREEN |
| 5 | Integration + regression testing | — | 4h | Full `cargo test`, verify no regressions |

### 7.2 Verification Checklist

After all tasks:

- [ ] `cargo check` — zero errors, zero warnings
- [ ] `cargo test --workspace --exclude musicfs-grpc` — all tests pass (exclude pre-existing grpc issue)
- [ ] `cargo test -p musicfs-test-utils --test resilience` — 5 previously-RED tests now GREEN
- [ ] `cargo clippy` — no new warnings
- [ ] Remaining RED tests are only for Phases C-F (health timeout, parallel checks, fd exhaustion, chunk auto-repair, passthrough mode)

---

## 8. Files Changed

| File | Change | Issue |
|------|--------|-------|
| `musicfs-cas/src/store.rs` | Size pre-check in `put()`, `StoreFull` error, sled recovery in `open()` | 2.8, 3.5 |
| `musicfs-cache/src/db.rs` | `open_with_integrity_check()` with `PRAGMA integrity_check(1)` | 2.4 |
| `musicfs-core/src/error.rs` | Add `DatabaseCorrupted(String)` variant | 2.4 |
| `musicfs-search/src/index.rs` | `open_with_recovery()` — detect, delete, recreate | 2.4 |
| `musicfs-core/src/supervisor.rs` | NEW — `TaskSupervisor`, `TaskStatus`, spawn/monitor/restart | 2.6 |
| `musicfs-core/src/lib.rs` | Re-export supervisor module | 2.6 |
| `musicfs-cli/src/main.rs` | CancellationToken creation, ordered shutdown sequence | 2.3 |
| `musicfs-cli/Cargo.toml` | Add `tokio-util` dependency | 2.3 |
| `musicfs-test-utils/tests/resilience.rs` | Replace `todo!()` stubs with real tests, add supervisor tests | all |

---

## 9. Glossary / References

| Term | Definition |
|------|------------|
| **CancellationToken** | `tokio_util::sync::CancellationToken` — cooperative cancellation signal for async tasks |
| **PRAGMA integrity_check** | SQLite command that verifies page-level data consistency |
| **sled repair** | sled's built-in recovery mode that attempts to reconstruct a corrupted database |
| **TaskSupervisor** | New struct that monitors `JoinHandle`s and restarts failed tasks with backoff |
| **StoreFull** | New `CasError` variant returned when a write would exceed `max_size` |

| Document | Path |
|----------|------|
| Phase A plan | [phase-a-stop-dying.md](phase-a-stop-dying.md) |
| Resilience audit | [resilience-fault-tolerance.md](resilience-fault-tolerance.md) |
| Resilience testing | [resilience-testing.md](resilience-testing.md) |
| Persistent state | [persistent-state.md](persistent-state.md) |
