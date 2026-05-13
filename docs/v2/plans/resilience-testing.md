# MusicFS Resilience Testing Strategy

**Date**: 2026-05-13
**Status**: Proposal
**Prerequisites**: [resilience-fault-tolerance.md](resilience-fault-tolerance.md)

---

## 1. Current State

- **162 tests** across 43 files — all unit/integration, no fault injection
- **Zero chaos/resilience tests** — no error injection, no crash recovery, no signal handling
- **E2E tests are manual** — `tests/e2e/e2e_players.rs` is `#[ignore]`, requires pre-mounted FUSE
- **No mocking framework** — tests use real components with TempDir
- **No CI pipeline** — no GitHub Actions, tests run manually via `cargo test`
- **Test tools available** in Nix flake: `cargo-nextest`, `cargo-criterion`

---

## 2. Testing Layers

Three layers, from fast/cheap to slow/thorough:

```
Layer 1: Trait-based mocks + failpoints     (ms per test,  cargo test)
Layer 2: Fork-kill crash recovery           (seconds,      cargo test)
Layer 3: Toxiproxy + Docker integration     (seconds,      docker compose + cargo test)
```

**Rule**: Every resilience issue gets at least Layer 1 coverage. Critical issues get Layer 2. Network-specific issues get Layer 3.

---

## 3. Tooling

### 3.1 Required New Dependencies

```toml
# Cargo.toml [workspace.dependencies]
fail = "0.5"                    # TiKV failpoints — conditional fault injection
rlimit = "0.10"                 # Resource limit manipulation (fd, memory)
nix = "0.29"                    # Signal sending, process control

# Cargo.toml [workspace.features]
failpoints = ["fail/failpoints"]  # Zero-cost when disabled

# dev-dependencies only
wiremock = "0.6"                # HTTP mock server (for S3 origin tests)
assert_cmd = "2.0"              # CLI integration testing
```

### 3.2 Test Infrastructure to Build

**`crates/musicfs-test-utils/`** — new crate with shared test helpers:

```rust
// Faulty origin wrapper — implements Origin trait, injects failures
pub struct FaultyOrigin {
    inner: Arc<dyn Origin>,
    fail_mode: Arc<RwLock<FailMode>>,
}

pub enum FailMode {
    Healthy,
    FailEveryNth(usize),
    FailAfterN(usize),
    TimeoutMs(u64),
    PartialRead { max_bytes: usize },
    ReturnError(io::ErrorKind),
}

// Faulty CAS wrapper — injects disk errors
pub struct FaultyCasStore {
    inner: CasStore,
    inject_enospc: AtomicBool,
    inject_eio_on_read: AtomicBool,
    inject_corruption: AtomicBool,
}

// Shared test helpers (currently duplicated across 29 files)
pub fn make_file_meta(id: i64, vpath: &str, size: u64) -> FileMeta;
pub fn make_audio_meta(artist: &str, album: &str, title: &str) -> AudioMeta;
pub async fn setup_test_cas(dir: &Path) -> Arc<CasStore>;
pub fn setup_test_tree(files: &[FileMeta]) -> Arc<RwLock<VirtualTree>>;
```

---

## 4. Issue → Test Mapping

### Phase A: Stop Dying

#### 2.1 No Signal Handling (SIGTERM/SIGINT)

**Test approach**: Spawn daemon as child process, send signal, verify behavior.

```rust
// tests/resilience/signal_handling.rs

#[tokio::test]
async fn test_sigterm_triggers_shutdown() {
    // Spawn the daemon
    let mut child = Command::new(env!("CARGO_BIN_EXE_musicfs"))
        .args(["mount", "--origin", &test_dir, &mount_dir])
        .spawn().unwrap();

    // Wait for mount
    wait_for_mount(&mount_dir).await;

    // Send SIGTERM
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    ).unwrap();

    // Verify clean exit (not killed by signal)
    let status = tokio::time::timeout(
        Duration::from_secs(10), child.wait()
    ).await.unwrap().unwrap();
    assert!(status.success() || status.code() == Some(0));

    // Verify mountpoint is unmounted
    assert!(!is_mounted(&mount_dir));
}

#[tokio::test]
async fn test_sigint_triggers_shutdown() {
    // Same pattern with SIGINT
}

#[tokio::test]
async fn test_double_signal_forces_immediate_exit() {
    // Send SIGTERM, then SIGTERM again within 1s
    // Verify daemon exits immediately on second signal
}
```

**Layer**: 2 (fork-kill)
**Can test before implementation?**: No — signal handler must exist first. Write tests first as spec, they'll fail until implementation.

---

#### 2.2 No Panic Hook

**Test approach**: Trigger panic in background task, verify logging and continued operation.

```rust
#[tokio::test]
async fn test_panic_in_background_task_is_logged() {
    // Setup tracing subscriber that captures error events
    let (subscriber, logs) = test_subscriber();

    // Spawn a task that panics
    let handle = tokio::spawn(async {
        panic!("test panic in background task");
    });

    // Wait for task to complete
    let result = handle.await;
    assert!(result.is_err()); // JoinError with panic

    // Verify panic was logged (requires custom panic hook)
    assert!(logs.contains("test panic in background task"));
}

#[test]
fn test_panic_hook_includes_backtrace() {
    // Install custom panic hook
    install_panic_hook();

    let result = std::panic::catch_unwind(|| {
        panic!("deliberate test panic");
    });

    assert!(result.is_err());
    // Verify hook logged thread name + backtrace
}
```

**Layer**: 1 (unit)

---

#### 2.3 Graceful Shutdown Orchestration

**Test approach**: Start all components, trigger shutdown, verify ordered teardown.

```rust
#[tokio::test]
async fn test_shutdown_order() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));

    // Setup components with shutdown tracking
    let token = CancellationToken::new();

    // Spawn mock background tasks that log shutdown order
    let watcher_events = events.clone();
    let watcher_token = token.clone();
    tokio::spawn(async move {
        watcher_token.cancelled().await;
        watcher_events.lock().unwrap().push("watcher_stopped".into());
    });

    let indexer_events = events.clone();
    let indexer_token = token.clone();
    tokio::spawn(async move {
        indexer_token.cancelled().await;
        indexer_events.lock().unwrap().push("indexer_stopped".into());
    });

    // Trigger shutdown
    token.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let order = events.lock().unwrap();
    assert!(order.contains(&"watcher_stopped".to_string()));
    assert!(order.contains(&"indexer_stopped".to_string()));
}

#[tokio::test]
async fn test_shutdown_flushes_tantivy() {
    let dir = TempDir::new().unwrap();
    let index = SearchIndex::open(dir.path()).unwrap();

    // Add document without committing
    index.index_file(&make_file_meta(1, "/a.flac", 1000)).unwrap();

    // Trigger graceful shutdown (should commit)
    index.commit().unwrap();

    // Reopen and verify document survived
    let index2 = SearchIndex::open(dir.path()).unwrap();
    let results = index2.search("a", 10).unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn test_shutdown_with_inflight_reads() {
    // Start a slow read (simulated via FaultyOrigin with TimeoutMs)
    // Trigger shutdown
    // Verify: read completes or returns EIO (not hang)
    // Verify: daemon exits within drain_timeout
}
```

**Layer**: 1 (unit) + 2 (process-level for full integration)

---

#### 2.4 Cache Integrity on Startup

**Test approach**: Corrupt storage, reopen, verify detection and recovery.

```rust
#[tokio::test]
async fn test_sqlite_integrity_check_detects_corruption() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // Create valid DB
    {
        let db = Database::open(&db_path).unwrap();
        db.upsert_file(/* ... */).unwrap();
    }

    // Corrupt the file (overwrite middle bytes)
    let mut data = std::fs::read(&db_path).unwrap();
    if data.len() > 200 {
        data[100..200].fill(0xFF);
    }
    std::fs::write(&db_path, &data).unwrap();

    // Reopen — should detect corruption
    let result = Database::open_with_integrity_check(&db_path);
    assert!(matches!(result, Err(Error::DatabaseCorrupted(_))));
}

#[tokio::test]
async fn test_tantivy_corruption_triggers_rebuild() {
    let dir = TempDir::new().unwrap();

    // Create valid index
    {
        let index = SearchIndex::open(dir.path()).unwrap();
        index.index_file(&make_file_meta(1, "/a.flac", 1000)).unwrap();
        index.commit().unwrap();
    }

    // Corrupt meta.json
    std::fs::write(dir.path().join("meta.json"), b"corrupted").unwrap();

    // Reopen — should detect and recreate
    let index = SearchIndex::open_with_recovery(dir.path()).unwrap();
    // Index is empty (rebuilt) but functional
    let results = index.search("a", 10).unwrap();
    assert_eq!(results.len(), 0);
}

#[tokio::test]
async fn test_sled_corruption_triggers_repair() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig { chunks_dir: dir.path().join("chunks"), ..Default::default() };

    // Create valid store
    {
        let store = CasStore::open(config.clone()).await.unwrap();
        store.put(b"test data").await.unwrap();
    }

    // Corrupt sled files
    for entry in std::fs::read_dir(dir.path().join("chunks/index.sled")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some() {
            std::fs::write(entry.path(), b"corrupted").unwrap();
        }
    }

    // Reopen — should attempt repair
    let result = CasStore::open(config).await;
    // Either succeeds with repair, or returns clear error
    // (depends on implementation choice)
}
```

**Layer**: 1 (unit)

---

#### 2.5 Interrupted Sync Recovery

**Test approach**: Failpoints to crash mid-sync, verify resume on restart.

```rust
#[tokio::test]
#[cfg(feature = "failpoints")]
async fn test_sync_resumes_after_crash() {
    let dir = TempDir::new().unwrap();

    // Set failpoint: crash after processing 50 files
    fail::cfg("delta-sync-after-batch", "50*off->return").unwrap();

    let detector = DeltaDetector::new(dir.path());
    let result = detector.detect_changes(&origin).await;
    assert!(result.is_err()); // Crashed at file 50

    // Resume — should continue from checkpoint
    fail::remove("delta-sync-after-batch");
    let result = detector.detect_changes(&origin).await;
    assert!(result.is_ok());

    // Verify: processed file count = total (not total + 50 duplicates)
}
```

**Layer**: 1 (failpoints)

---

#### 2.6 Fire-and-Forget Tasks

**Test approach**: Spawn task that panics, verify supervisor detects and restarts.

```rust
#[tokio::test]
async fn test_task_supervisor_detects_panic() {
    let supervisor = TaskSupervisor::new();

    // Register a task that panics after 100ms
    supervisor.spawn_supervised("test_task", async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        panic!("deliberate task panic");
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify: supervisor detected the failure
    let status = supervisor.task_status("test_task");
    assert!(matches!(status, TaskStatus::Failed { .. }));
}

#[tokio::test]
async fn test_task_supervisor_restarts_critical_task() {
    let call_count = Arc::new(AtomicU32::new(0));
    let count = call_count.clone();

    let supervisor = TaskSupervisor::new();
    supervisor.spawn_critical("health_monitor", move || {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            if count.load(Ordering::SeqCst) == 1 {
                panic!("first run fails");
            }
            // Second run succeeds — loop forever
            loop { tokio::time::sleep(Duration::from_secs(60)).await; }
        }
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Task panicked once, was restarted, is now running
    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    assert!(matches!(supervisor.task_status("health_monitor"), TaskStatus::Running));
}
```

**Layer**: 1 (unit)

---

#### 2.7 FUSE Unmount on Crash

**Test approach**: Fork daemon, kill -9, verify mountpoint state.

```rust
#[tokio::test]
async fn test_stale_mount_detected_on_startup() {
    let mount_dir = TempDir::new().unwrap();

    // Simulate stale FUSE mount (create a marker that looks mounted)
    // In real test: spawn daemon, kill -9, check /proc/mounts

    // Verify: new mount attempt detects stale mount and cleans up
    // (fusermount -u or auto-unmount)
}

#[test]
fn test_systemd_service_has_execstoppost() {
    let service = std::fs::read_to_string("dist/musicfs.service").unwrap();
    assert!(service.contains("ExecStopPost"));
    assert!(service.contains("fusermount"));
}
```

**Layer**: 2 (fork-kill) + 1 (config validation)

---

#### 2.8 Disk Space Handling

**Test approach**: rlimit to constrain file size, or fill TempDir.

```rust
#[tokio::test]
async fn test_cas_put_handles_enospc() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        max_size: 1024, // 1KB limit
        ..Default::default()
    };
    let store = CasStore::open(config).await.unwrap();

    // Fill the store past limit
    let big_data = vec![0u8; 2048];
    let result = store.put(&big_data).await;

    // Should fail gracefully, not panic
    assert!(result.is_err() || store.current_size() <= 1024);
}

#[tokio::test]
async fn test_eviction_triggers_at_watermark() {
    // Create store with small max_size
    // Fill to 90%
    // Verify: background eviction triggered
    // Write more data
    // Verify: still under limit
}
```

**Layer**: 1 (unit)

---

#### 2.9 RwLock Poison Recovery

**Test approach**: Poison a lock, verify FUSE operations survive.

```rust
#[test]
fn test_poisoned_tree_lock_returns_eio_not_panic() {
    let tree = Arc::new(std::sync::RwLock::new(VirtualTree::new()));

    // Poison the lock by panicking inside a write guard
    let tree_clone = tree.clone();
    let _ = std::thread::spawn(move || {
        let _guard = tree_clone.write().unwrap();
        panic!("poisoning the lock");
    }).join();

    // Verify: lock is poisoned
    assert!(tree.read().is_err());

    // After fix: should recover via unwrap_or_else(|p| p.into_inner())
    // OR: switch to parking_lot::RwLock which never poisons
}

#[test]
fn test_parking_lot_rwlock_survives_panic() {
    let tree = Arc::new(parking_lot::RwLock::new(VirtualTree::new()));

    let tree_clone = tree.clone();
    let _ = std::thread::spawn(move || {
        let _guard = tree_clone.write();
        panic!("writer panic");
    }).join();

    // parking_lot: lock is NOT poisoned, read succeeds
    let guard = tree.read();
    assert!(guard.get(ROOT_INODE).is_some());
}
```

**Layer**: 1 (unit)

---

#### 2.10 sd_notify Integration

**Test approach**: Mock the notify socket, verify messages.

```rust
#[test]
fn test_sd_notify_ready_sent() {
    // Create a Unix socket to capture sd_notify messages
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("notify.sock");

    std::env::set_var("NOTIFY_SOCKET", &socket_path);

    let listener = std::os::unix::net::UnixDatagram::bind(&socket_path).unwrap();

    // Call the notify function
    sd_notify::notify(false, &[sd_notify::NotifyState::Ready]).unwrap();

    // Verify message received
    let mut buf = [0u8; 256];
    let n = listener.recv(&mut buf).unwrap();
    let msg = std::str::from_utf8(&buf[..n]).unwrap();
    assert!(msg.contains("READY=1"));
}
```

**Layer**: 1 (unit)

---

### Phase C-D: Network & Origin Failures

#### Origin Failover Under Network Failure

**Test approach**: FaultyOrigin trait mock with configurable failures.

```rust
#[tokio::test]
async fn test_failover_on_primary_death() {
    let primary = Arc::new(FaultyOrigin::new(
        LocalOrigin::new("primary", &primary_dir),
        FailMode::ReturnError(io::ErrorKind::ConnectionRefused),
    ));
    let secondary = Arc::new(LocalOrigin::new("secondary", &secondary_dir));

    let registry = OriginRegistry::new(/* ... */);
    registry.register(primary, 1);
    registry.register(secondary, 2);

    let executor = FailoverExecutor::new(registry, RetryConfig::default());

    // Read should fail on primary, succeed on secondary
    let result = executor.read_with_failover(&path, 0, 100).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_all_origins_dead_returns_cached() {
    // All origins return errors
    // CAS has cached chunks
    // Verify: reads from CAS succeed
    // Verify: AllOriginsUnhealthy event emitted
}

#[tokio::test]
async fn test_origin_recovery_resumes_routing() {
    let origin = Arc::new(FaultyOrigin::new(
        LocalOrigin::new("test", &dir),
        FailMode::FailAfterN(0), // Starts dead
    ));

    // Register and verify unhealthy
    monitor.add_origin(origin.clone());
    monitor.check_now(&id).await;
    assert!(monitor.snapshot().is_unhealthy(&id));

    // "Recover" — switch to healthy mode
    origin.set_mode(FailMode::Healthy);
    monitor.check_now(&id).await;
    assert!(monitor.snapshot().is_healthy(&id));
}
```

**Layer**: 1 (trait mocks)

---

#### Health Check Timeout (Gap 4.2.1)

```rust
#[tokio::test]
async fn test_local_origin_health_check_has_timeout() {
    // Create origin pointing to path that will hang on stat()
    // (e.g., an NFS mount to a dead server — simulated via FaultyOrigin with TimeoutMs)
    let origin = Arc::new(FaultyOrigin::new(
        LocalOrigin::new("slow", &dir),
        FailMode::TimeoutMs(30_000), // 30s hang
    ));

    let monitor = HealthMonitor::new(Duration::from_secs(30));
    monitor.add_origin(origin);

    // Health check should complete within 5s (timeout), not 30s
    let start = Instant::now();
    monitor.check_now(&OriginId::from("slow")).await;
    assert!(start.elapsed() < Duration::from_secs(10));

    // Origin should be marked unhealthy
    assert!(monitor.snapshot().is_unhealthy(&OriginId::from("slow")));
}
```

**Layer**: 1 (unit)

---

#### Sequential Health Check Blocking (Gap 4.2.2)

```rust
#[tokio::test]
async fn test_health_checks_run_in_parallel() {
    // 3 origins: 2 healthy (instant), 1 dead (5s timeout)
    // check_all() should complete in ~5s, not ~15s

    let monitor = HealthMonitor::new(Duration::from_secs(30));
    monitor.add_origin(healthy_origin_1);
    monitor.add_origin(healthy_origin_2);
    monitor.add_origin(dead_origin); // 5s timeout

    let start = Instant::now();
    monitor.check_all().await;
    let elapsed = start.elapsed();

    // Parallel: ~5s. Sequential: ~15s.
    assert!(elapsed < Duration::from_secs(8));

    // Both healthy origins should be healthy (not delayed by dead one)
    let snapshot = monitor.snapshot();
    assert!(snapshot.is_healthy(&healthy_1_id));
    assert!(snapshot.is_healthy(&healthy_2_id));
}
```

**Layer**: 1 (unit)

---

### Phase E: Runtime Robustness

#### FUSE↔tokio Deadlock (Gap 5.1)

```rust
#[tokio::test]
async fn test_concurrent_fuse_reads_dont_deadlock() {
    // Mount FUSE with spawn_mount2
    // Spawn 100 concurrent reads via tokio::fs::read
    // Set timeout of 30s
    // If any read doesn't complete → deadlock detected

    let mount_dir = TempDir::new().unwrap();
    let session = spawn_test_mount(mount_dir.path()).await;

    let handles: Vec<_> = (0..100).map(|i| {
        let path = mount_dir.path().join(format!("Artist/Album/{:02} - Track.flac", i));
        tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                tokio::fs::read(&path),
            ).await
        })
    }).collect();

    for handle in handles {
        let result = handle.await.unwrap();
        // Should complete (Ok or Err), never timeout
        assert!(result.is_ok(), "read timed out — possible deadlock");
    }

    drop(session);
}
```

**Layer**: 2 (requires FUSE mount — may need Docker/privileged in CI)

---

#### tantivy Crash Recovery (Gap 5.2)

```rust
#[test]
fn test_tantivy_survives_uncommitted_crash() {
    let dir = TempDir::new().unwrap();

    // Phase 1: write + commit, then write WITHOUT commit
    {
        let index = SearchIndex::open(dir.path()).unwrap();
        index.index_file(&make_file_meta(1, "/a.flac", 1000)).unwrap();
        index.commit().unwrap();

        // This document is NOT committed
        index.index_file(&make_file_meta(2, "/b.flac", 1000)).unwrap();

        // Simulate crash — drop without commit
        std::mem::forget(index);
    }

    // Phase 2: reopen and verify
    let index = SearchIndex::open(dir.path()).unwrap();
    let results = index.search("a", 10).unwrap();
    assert_eq!(results.len(), 1); // Committed doc survives

    let results = index.search("b", 10).unwrap();
    assert_eq!(results.len(), 0); // Uncommitted doc lost — expected
}
```

**Layer**: 1 (unit)

---

#### File Descriptor Exhaustion (Gap 5.3)

```rust
#[test]
#[cfg(target_os = "linux")]
fn test_fd_exhaustion_handling() {
    use rlimit::{Resource, setrlimit, getrlimit};

    let (orig_soft, orig_hard) = getrlimit(Resource::NOFILE).unwrap();

    // Set very low fd limit
    setrlimit(Resource::NOFILE, 64, 64).unwrap();

    // Try to open CAS store (uses sled + chunk files)
    let dir = TempDir::new().unwrap();
    let result = /* attempt CAS operations */;

    // Should fail gracefully, not panic
    // Restore limits
    setrlimit(Resource::NOFILE, orig_soft, orig_hard).unwrap();
}
```

**Layer**: 1 (unit, Linux-only)

---

### Phase F: Cache Resilience

#### CAS Chunk Corruption Auto-Repair (Gap 6.4)

```rust
#[tokio::test]
async fn test_corrupt_chunk_auto_refetched() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(CasStore::open(/* ... */).await.unwrap());

    // Store a valid chunk
    let data = b"valid audio data";
    let hash = store.put(data).await.unwrap();

    // Corrupt the chunk file on disk
    let chunk_path = store.chunk_path(&hash);
    std::fs::write(&chunk_path, b"corrupted garbage").unwrap();

    // Create reader with fetcher (has origin backup)
    let reader = FileReader::with_fetcher(store, fetcher);

    // Read should detect corruption, re-fetch from origin, and succeed
    let result = reader.read(file_id, 0, data.len() as u32).await;
    assert!(result.is_ok());
    assert_eq!(&result.unwrap()[..], data);
}

#[tokio::test]
async fn test_missing_chunk_triggers_origin_fetch() {
    // Store chunk, then delete the file
    let hash = store.put(b"data").await.unwrap();
    let chunk_path = store.chunk_path(&hash);
    std::fs::remove_file(&chunk_path).unwrap();

    // Read should fetch from origin instead of returning EIO
    let result = reader.read(file_id, 0, 4).await;
    assert!(result.is_ok());
}
```

**Layer**: 1 (unit)

---

#### Passthrough Mode (Gap 6.6)

```rust
#[tokio::test]
async fn test_passthrough_mode_when_cache_disk_dead() {
    let cache_dir = TempDir::new().unwrap();
    let origin_dir = TempDir::new().unwrap();
    std::fs::write(origin_dir.path().join("test.flac"), b"audio data").unwrap();

    // Setup system, then make cache dir read-only
    let store = CasStore::open(/* cache_dir */).await.unwrap();

    // Simulate cache disk failure
    std::fs::set_permissions(
        cache_dir.path(),
        std::fs::Permissions::from_mode(0o444),
    ).unwrap();

    // CAS writes fail — system should enter passthrough mode
    // Reads should go directly to origin
    let result = reader.read(file_id, 0, 10).await;
    assert!(result.is_ok());
    assert_eq!(&result.unwrap()[..], b"audio data");

    // Restore permissions for cleanup
    std::fs::set_permissions(
        cache_dir.path(),
        std::fs::Permissions::from_mode(0o755),
    ).unwrap();
}
```

**Layer**: 1 (unit)

---

## 5. Failpoints Instrumentation Map

These are the production code locations that need `fail_point!` macros for testing:

```rust
// musicfs-cas/src/store.rs
pub async fn put(&self, data: &[u8]) -> Result<ChunkHash, CasError> {
    fail_point!("cas-put-before-write", |_| Err(CasError::Io(io::Error::new(
        io::ErrorKind::Other, "ENOSPC"
    ))));

    fs::write(&path, data).await?;

    fail_point!("cas-put-after-write-before-index", |_| Err(CasError::Io(io::Error::new(
        io::ErrorKind::Other, "sled crash"
    ))));

    self.index.insert(hash, location)?;
    Ok(hash)
}

// musicfs-cas/src/reader.rs
async fn get_or_fetch_manifest(&self, file_id: FileId) -> Result<ChunkManifest, ReaderError> {
    fail_point!("reader-manifest-fetch", |_| Err(ReaderError::ManifestNotFound(file_id)));
    // ...
}

// musicfs-sync/src/delta.rs
pub async fn detect_changes(&self, origin: &dyn Origin) -> Result<ChangeSet> {
    fail_point!("delta-sync-after-batch", |_| Err(Error::SyncInterrupted));
    // ...
}

// musicfs-origins/src/health.rs
async fn check_one(&self, id: &OriginId, origin: &Arc<dyn Origin>) {
    fail_point!("health-check-hang", |_| {
        std::thread::sleep(Duration::from_secs(60)); // Simulate hang
    });
    // ...
}

// musicfs-cache/src/db.rs
pub fn open(path: &Path) -> Result<Self> {
    fail_point!("db-open-corrupt", |_| Err(Error::DatabaseCorrupted(
        "injected corruption".into()
    )));
    // ...
}
```

---

## 6. Integration Test Setup with Toxiproxy

For network fault tolerance tests that need real protocol behavior:

```yaml
# tests/docker-compose.yml
services:
  toxiproxy:
    image: ghcr.io/shopify/toxiproxy:2.9.0
    ports:
      - "8474:8474"
      - "20000-20010:20000-20010"

  minio:
    image: minio/minio
    command: server /data
    ports:
      - "9000:9000"
    environment:
      MINIO_ROOT_USER: test
      MINIO_ROOT_PASSWORD: testtest

  sftp:
    image: atmoz/sftp
    ports:
      - "2222:22"
    command: test:test:::music
```

```rust
// tests/integration/network_faults.rs

#[tokio::test]
#[ignore] // Requires docker-compose up
async fn test_s3_origin_survives_latency_spike() {
    let toxi = noxious_client::Client::new("http://localhost:8474");

    // Create proxy: client → toxiproxy:20000 → minio:9000
    let proxy = toxi.create_proxy("minio", "0.0.0.0:20000", "minio:9000").await.unwrap();

    // Normal operation
    let origin = S3Origin::new("http://localhost:20000", "test-bucket");
    let data = origin.read(Path::new("/test.flac"), 0, 100).await.unwrap();
    assert!(!data.is_empty());

    // Inject 5s latency
    proxy.add_toxic(&Toxic {
        name: "latency".into(),
        kind: ToxicKind::Latency { latency: 5000, jitter: 0 },
        direction: StreamDirection::Downstream,
        toxicity: 1.0,
    }).await.unwrap();

    // Read should timeout (if 30s timeout implemented) or succeed slowly
    let start = Instant::now();
    let result = origin.read(Path::new("/test.flac"), 0, 100).await;
    // Should not take >35s (timeout + buffer)
    assert!(start.elapsed() < Duration::from_secs(35));

    // Remove toxic and verify recovery
    proxy.remove_toxic("latency").await.unwrap();
    let data = origin.read(Path::new("/test.flac"), 0, 100).await.unwrap();
    assert!(!data.is_empty());
}

#[tokio::test]
#[ignore]
async fn test_origin_connection_drop_triggers_failover() {
    // Setup toxiproxy for primary origin
    // Inject "down" toxic → connection refused
    // Verify: requests routed to secondary origin
    // Remove toxic → verify: primary re-enabled on next health check
}
```

---

## 7. Test Organization

```
musicfs/
├── crates/
│   └── musicfs-test-utils/       # NEW — shared test helpers
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── faulty_origin.rs   # FaultyOrigin with FailMode
│           ├── faulty_cas.rs      # FaultyCasStore
│           ├── fixtures.rs        # make_file_meta, setup_test_cas, etc.
│           └── assertions.rs      # Custom assertions
├── tests/
│   ├── resilience/                # NEW — resilience test suite
│   │   ├── mod.rs
│   │   ├── signal_handling.rs     # SIGTERM/SIGINT tests
│   │   ├── crash_recovery.rs      # Fork-kill + state verification
│   │   ├── cache_corruption.rs    # SQLite/sled/tantivy/CAS corruption
│   │   ├── disk_failure.rs        # ENOSPC, permissions, passthrough
│   │   ├── resource_limits.rs     # fd exhaustion, memory limits
│   │   └── lock_poisoning.rs      # RwLock poison recovery
│   ├── failpoints/                # NEW — failpoint-gated tests
│   │   ├── mod.rs
│   │   ├── origin_failures.rs     # Injected origin errors
│   │   ├── sync_interruption.rs   # Delta sync crash/resume
│   │   └── cas_failures.rs        # CAS write failures
│   ├── integration/               # NEW — network integration (Docker)
│   │   ├── docker-compose.yml
│   │   ├── network_faults.rs      # Toxiproxy tests
│   │   └── origin_failover.rs     # Multi-origin integration
│   └── e2e/
│       └── e2e_players.rs         # Existing (unchanged)
```

**Running**:
```bash
# Fast unit + resilience tests (no Docker, no FUSE)
cargo test --lib --tests resilience

# Failpoint tests (sequential, feature-gated)
cargo test --features failpoints --test failpoints -- --test-threads 1

# Network integration (requires docker-compose up)
cargo test --test integration -- --ignored

# Everything
cargo nextest run --features failpoints
```

---

## 8. Coverage Matrix

| Resilience Issue | Layer | Test Type | Blocks On |
|---|---|---|---|
| **2.1** Signal handling | 2 | Fork + signal | Implementation |
| **2.2** Panic hook | 1 | Unit | Implementation |
| **2.3** Shutdown orchestration | 1+2 | Unit + fork | 2.1 |
| **2.4** Cache integrity | 1 | Corruption + reopen | — |
| **2.5** Sync recovery | 1 | Failpoints | Implementation |
| **2.6** Task supervisor | 1 | Spawn + panic + verify | Implementation |
| **2.7** FUSE unmount | 2 | Fork + kill -9 | 2.1 |
| **2.8** Disk space | 1 | rlimit / small max_size | — |
| **2.9** RwLock poison | 1 | Deliberate poison | — |
| **2.10** sd_notify | 1 | Mock socket | Implementation |
| **3.1** Watchdog | 1 | Mock sd_notify | 2.10 |
| **3.5** sled recovery | 1 | Corrupt + reopen | — |
| **3.7** ExecStop | 1 | Service file assertion | — |
| **3.8** FUSE read timeout | 1 | FaultyOrigin + timeout | — |
| **4.2.1** Health timeout | 1 | FaultyOrigin + timer | — |
| **4.2.2** Parallel checks | 1 | Multiple origins + timer | — |
| **4.2.3** Offline mode | 1 | All origins fail + state check | Implementation |
| **5.1** FUSE↔tokio deadlock | 2 | 100 concurrent reads | FUSE mount |
| **5.2** tantivy crash | 1 | Write + forget + reopen | — |
| **5.3** fd exhaustion | 1 | rlimit | Linux only |
| **5.7** CAS atomic write | 1 | Failpoint between write + index | — |
| **6.3** sled dies | 1 | Corrupt + reopen | — |
| **6.4** CAS corruption | 1 | Corrupt chunk + read | — |
| **6.6** Passthrough mode | 1 | Read-only cache dir | Implementation |
| **Network failover** | 1+3 | FaultyOrigin + Toxiproxy | Docker for Layer 3 |

**Tests that can be written NOW** (before implementation): 2.4, 2.9, 3.5, 3.7, 3.8, 4.2.1, 4.2.2, 5.2, 5.3, 6.3, 6.4

**Tests that need implementation first**: 2.1, 2.2, 2.3, 2.5, 2.6, 2.7, 2.10, 4.2.3, 6.6

---

## 9. Estimated Effort

| Task | Effort |
|---|---|
| Create `musicfs-test-utils` crate (FaultyOrigin, fixtures) | 1.5 days |
| Add `fail` crate + instrument 10 failpoints | 1 day |
| Write Layer 1 resilience tests (~25 tests) | 3 days |
| Write Layer 2 fork-kill tests (~5 tests) | 1 day |
| Setup Docker Compose + Toxiproxy integration | 1 day |
| Write Layer 3 network tests (~5 tests) | 1.5 days |
| **Total** | **~9 days** |
