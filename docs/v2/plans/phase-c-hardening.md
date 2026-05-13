# Phase C: Production Hardening — Implementation Plan

**Authors:** AI-assisted  
**Status:** Draft  
**Last Updated:** 2026-05-13  
**Reviewers:** TBD  
**Approvers:** TBD  
**Prerequisites:** [phase-b-crash-recovery.md](phase-b-crash-recovery.md) (completed), [resilience-fault-tolerance.md](resilience-fault-tolerance.md)  
**Estimated Effort:** ~4 days

---

[TOC]

---

## 1. Abstract

Phase C merges the practical items from Phases C and D of the resilience audit into a single implementation pass. It fixes the remaining 6 RED tests and addresses production-critical issues: health check hangs that block all origin monitoring, unbounded FUSE reads that can freeze the filesystem, broken CAS size accounting that disables eviction, and concurrent mount protection.

**Deferred items** (depend on unimplemented features or low urgency): interrupted sync recovery (needs persistent state), SIGHUP config reload, connection pooling (S3/SFTP are stubs), event bus backpressure, FUSE session recovery, offline mode state machine, DNS failure handling, stale-data awareness.

**RED tests to turn GREEN:**
- `test_local_origin_health_check_has_timeout` (D1)
- `test_health_checks_run_in_parallel` (D2)
- `test_fd_exhaustion_handling` (E — 5.3)
- `test_corrupt_chunk_auto_refetched` (F — 6.4)
- `test_missing_chunk_triggers_origin_fetch` (F — 6.4)
- `test_passthrough_mode_when_cache_disk_dead` (F — 6.6)

---

## 2. Background

After Phase A+B, the daemon survives signals, recovers from storage corruption on startup, supervises background tasks, and rejects oversized CAS writes. But:

1. **Health checks hang on dead origins** — `check_one()` calls `origin.health().await` with no timeout. A dead NAS (local origin pointing to network mount) blocks health monitoring for ALL origins because checks run sequentially.

2. **FUSE reads have no timeout** — `reader.read()` in the FUSE `read()` callback has no timeout. A slow or hung origin blocks the FUSE thread indefinitely.

3. **CAS size tracking is broken** — `calculate_size()` only scans top-level of `chunks_dir`, missing all chunks in shard subdirectories (`aa/bb/<hash>`). `current_size` is always ~0, eviction never triggers.

4. **Corrupt chunks return EIO** — when `verify_integrity()` detects a bad chunk, it returns `CasError::IntegrityError`. The reader propagates this as EIO to FUSE. It should auto-re-fetch from origin instead.

5. **No concurrent mount protection** — two `musicfs mount` commands can run simultaneously, corrupting SQLite and sled.

6. **fd exhaustion is unhandled** — no graceful behavior when file descriptors run out.

---

## 3. Goals & Non-Goals

### 3.1 Goals

- Health checks complete within 5 seconds regardless of origin responsiveness
- Health checks run in parallel (3 origins checked in ~5s, not ~15s)
- FUSE reads timeout after 30 seconds (returns EIO, doesn't hang)
- CAS size accounting is correct (recursive shard scan)
- Corrupt/missing chunks are auto-re-fetched from origin transparently
- PID file prevents concurrent mounts
- fd exhaustion produces clean errors, not panics
- All 6 remaining RED tests turn GREEN

### 3.2 Non-Goals

- Interrupted sync recovery (C1) — blocked on persistent state
- systemd watchdog (C3) — useful but not critical yet
- SIGHUP config reload (C4) — nice-to-have
- Connection pooling (C5) — S3/SFTP origins are stubs
- Event bus backpressure (C8) — low urgency
- FUSE session recovery (C10) — complex edge case
- Offline mode state machine (D3) — needs broader design
- DNS failure handling (D5) — depends on C5
- Stale-data awareness (D6) — low severity for music FS

---

## 4. Proposed Design

### 4.1 Implementation Order

```
4.2  Health check timeout + parallel checks    (2 RED tests, independent)
 ↓
4.3  Fix CAS calculate_size()                  (independent, unblocks eviction)
 ↓
4.4  FUSE read timeout                         (independent)
 ↓
4.5  CAS chunk auto-re-fetch on corruption     (2 RED tests)
 ↓
4.6  PID file / flock                          (independent)
 ↓
4.7  fd exhaustion handling                    (1 RED test)
```

### 4.2 Issues D1+D2: Health Check Timeout + Parallel Checks

**Problem**: `check_one()` awaits `origin.health()` with no timeout. `check_all()` iterates sequentially. One hung origin blocks everything.

#### Step 1: No stubs needed

#### Step 2: RED tests already exist

`test_local_origin_health_check_has_timeout` — FaultyOrigin with `TimeoutMs(5000)`, asserts check completes in <2s.

`test_health_checks_run_in_parallel` — 3 origins each with `TimeoutMs(200)`, asserts `check_all()` completes in <350ms (parallel), not ~600ms (sequential).

#### Step 3: Implementation

**File**: `musicfs-origins/src/health.rs`

Wrap `origin.health()` in `check_one()` with timeout:

```rust
async fn check_one(&self, id: &OriginId, origin: &Arc<dyn Origin>) {
    let start = Instant::now();
    let health_timeout = Duration::from_secs(5);

    let status = match tokio::time::timeout(health_timeout, origin.health()).await {
        Ok(status) => status,
        Err(_) => {
            warn!(origin_id = %id, timeout_ms = health_timeout.as_millis() as u64,
                  "Health check timed out");
            HealthStatus::Unhealthy
        }
    };

    let latency_ms = start.elapsed().as_millis() as u64;
    // ... rest unchanged
}
```

Change `check_all()` to use `futures::future::join_all`:

```rust
pub async fn check_all(&self) {
    let origins: Vec<_> = self.origins.iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();

    let checks: Vec<_> = origins.iter()
        .map(|(id, origin)| self.check_one(id, origin))
        .collect();

    futures::future::join_all(checks).await;
}
```

Add `futures` to `musicfs-origins/Cargo.toml` (or use `tokio::join!` macro if count is small/known).

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils --test resilience -- test_local_origin_health_check
cargo test -p musicfs-test-utils --test resilience -- test_health_checks_run_in_parallel
```

---

### 4.3 Issue C6: Fix CAS calculate_size()

**Problem**: `calculate_size()` only scans direct children of `chunks_dir`. Chunks live in shard subdirectories (`chunks/aa/bb/<hash>`). Size is always ~0, eviction never triggers.

#### Step 1: No stubs needed

#### Step 2: Test

```rust
#[tokio::test]
async fn test_cas_size_tracking_is_correct() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig { chunks_dir: dir.path().join("chunks"), max_size: 10_000_000, shard_levels: 2 };
    let store = CasStore::open(config).await.unwrap();

    let data = vec![0u8; 1000];
    store.put(&data).await.unwrap();

    // Size should reflect the chunk we just wrote (~1000 bytes)
    assert!(store.current_size() >= 1000, "current_size should track chunk data, got {}", store.current_size());
}
```

#### Step 3: Implementation

**File**: `musicfs-cas/src/store.rs` — make `calculate_size` recursive:

```rust
async fn calculate_size(dir: &Path) -> u64 {
    Self::calculate_size_recursive(dir).await
}

#[async recursion::async_recursion]
async fn calculate_size_recursive(dir: &Path) -> u64 {
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
}
```

Alternative without `async_recursion` (use `Box::pin`):

```rust
fn calculate_size_recursive(dir: &Path) -> Pin<Box<dyn Future<Output = u64> + Send + '_>> {
    Box::pin(async move {
        let mut size = 0u64;
        if let Ok(mut entries) = fs::read_dir(dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(meta) = entry.metadata().await {
                    if meta.is_file() {
                        size += meta.len();
                    } else if meta.is_dir() {
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
```

---

### 4.4 Issue C7: FUSE Read Timeout

**Problem**: FUSE `read()` calls `handle.block_on(reader.read(...))` with no timeout. A slow origin blocks the entire FUSE thread.

#### Step 1: No stubs needed

#### Step 2: Test

```rust
#[tokio::test]
async fn test_fuse_read_timeout_returns_eio() {
    // Uses FaultyOrigin with TimeoutMs(60_000) — simulates hung read
    // FUSE read should timeout at 30s and return EIO, not hang forever
    // (This test validates the timeout wrapper, not actual FUSE mount)
}
```

#### Step 3: Implementation

**File**: `musicfs-fuse/src/filesystem.rs` — wrap the read with timeout:

```rust
fn read(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, size: u32, _flags: i32, _lock_owner: Option<u64>, reply: ReplyData) {
    // ... file_id lookup unchanged ...

    let reader = reader.clone();
    let handle = self.runtime_handle.clone();
    let result = std::thread::scope(|_| {
        handle.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                reader.read(file_id, offset as u64, size),
            ).await
        })
    });

    match result {
        Ok(Ok(data)) => {
            trace!(ino, bytes_read = data.len(), "read successful");
            reply.data(&data);
        }
        Ok(Err(e)) => {
            warn!(ino, error = %e, "read failed");
            reply.error(libc::EIO);
        }
        Err(_timeout) => {
            warn!(ino, offset, size, "read timed out after 30s");
            reply.error(libc::EIO);
        }
    }
}
```

---

### 4.5 Issues 6.4: CAS Chunk Auto-Re-Fetch on Corruption/Missing

**Problem**: When `store.get()` finds a corrupt or missing chunk, it returns an error. The reader propagates this as EIO to FUSE. It should try to re-fetch the chunk from the origin instead.

#### Step 1: No stubs needed — modify `FileReader::read()`

#### Step 2: RED tests already exist

`test_corrupt_chunk_auto_refetched` — corrupts chunk file on disk, expects read to succeed (re-fetched from origin).

`test_missing_chunk_triggers_origin_fetch` — deletes chunk file, expects read to succeed.

Both currently fail because the reader doesn't attempt re-fetch on chunk errors.

#### Step 3: Implementation

**File**: `musicfs-cas/src/reader.rs` — add retry-with-refetch in the chunk read loop:

```rust
pub async fn read(&self, file_id: FileId, offset: u64, size: u32) -> Result<Bytes, ReaderError> {
    let manifest = self.get_or_fetch_manifest(file_id).await?;

    // ... offset/end calculation unchanged ...

    for chunk_ref in &manifest.chunks {
        // ... range check unchanged ...

        let chunk_data = match self.store.get(&chunk_ref.hash).await {
            Ok(data) => data,
            Err(CasError::IntegrityError { .. }) | Err(CasError::NotFound(_)) => {
                // Chunk is corrupt or missing — try to re-fetch from origin
                warn!(hash = %chunk_ref.hash, "Chunk corrupt/missing, attempting re-fetch");
                if let Some(fetcher) = &self.fetcher {
                    // Re-fetch the entire file (will re-chunk and store)
                    let new_manifest = fetcher.fetch_file(file_id).await?;
                    // Update cached manifest
                    self.manifests.write().insert(file_id, new_manifest);
                    // Retry the get
                    self.store.get(&chunk_ref.hash).await?
                } else {
                    return Err(ReaderError::Cas(CasError::NotFound(chunk_ref.hash.as_hex())));
                }
            }
            Err(e) => return Err(ReaderError::Cas(e)),
        };

        // ... slice extraction unchanged ...
    }

    Ok(result.freeze())
}
```

**Important**: The re-fetch downloads the entire file from origin and re-chunks it. For a single corrupt chunk this is wasteful (fetches all chunks to fix one), but it's the simplest correct approach. Chunk-level re-fetch would require the origin to support byte-range reads mapped to chunk boundaries — possible but complex. The file-level approach reuses existing `fetch_file()` logic.

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils --test resilience -- test_corrupt_chunk
cargo test -p musicfs-test-utils --test resilience -- test_missing_chunk
```

**Note on test updates**: The existing RED tests reference `store.chunk_path()` which is private. The tests will need to either:
- Make `chunk_path()` pub(crate) or add a test helper
- Or construct the path manually using the sharding logic

The tests also need a `ContentFetcher` with a real `LocalOrigin` to re-fetch from. The current tests create a CAS store but no fetcher — they need to be updated to include the full pipeline.

---

### 4.6 Issue C9: PID File / flock

**Problem**: Two `musicfs mount` commands can run simultaneously, both writing to the same SQLite/sled files.

#### Step 1: No stubs needed

#### Step 2: Test

```rust
#[test]
fn test_pid_file_prevents_concurrent_mount() {
    let dir = TempDir::new().unwrap();
    let lock_path = dir.path().join("musicfs.lock");

    // First lock succeeds
    let lock1 = try_acquire_lock(&lock_path);
    assert!(lock1.is_ok());

    // Second lock fails
    let lock2 = try_acquire_lock(&lock_path);
    assert!(lock2.is_err());

    // Release first, second succeeds
    drop(lock1);
    let lock3 = try_acquire_lock(&lock_path);
    assert!(lock3.is_ok());
}
```

#### Step 3: Implementation

**File**: `musicfs-cli/src/main.rs`

```rust
use std::fs::File;
use std::os::unix::io::AsRawFd;

struct LockFile {
    _file: File,
}

fn try_acquire_lock(path: &Path) -> Result<LockFile> {
    let file = File::create(path).context("Failed to create lock file")?;
    let fd = file.as_raw_fd();

    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            anyhow::bail!("MusicFS is already running (lock file: {:?})", path);
        }
        return Err(err).context("Failed to acquire lock");
    }

    // Write PID for debugging
    use std::io::Write;
    let mut f = &file;
    writeln!(f, "{}", std::process::id())?;

    Ok(LockFile { _file: file })
}
```

Call in `run_mount()` before mounting:

```rust
let lock_path = cache_dir.join("musicfs.lock");
let _lock = try_acquire_lock(&lock_path)
    .context("Failed to acquire lock — is another instance running?")?;
```

Lock is released automatically when `_lock` is dropped (process exit or scope end).

---

### 4.7 Issue 5.3: fd Exhaustion Handling

**Problem**: When fd limit is hit, operations fail with EMFILE. Currently this propagates as panics or unhelpful errors.

#### Step 1: Replace the `todo!()` test

#### Step 2: Test

```rust
#[test]
#[cfg(target_os = "linux")]
fn test_fd_exhaustion_handling() {
    use rlimit::{Resource, setrlimit, getrlimit};

    let (orig_soft, orig_hard) = getrlimit(Resource::NOFILE).unwrap();

    // Set very low limit
    setrlimit(Resource::NOFILE, 64, 64).unwrap();

    let dir = TempDir::new().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    let result = rt.block_on(async {
        CasStore::open(CasConfig {
            chunks_dir: dir.path().join("chunks"),
            max_size: 1_000_000,
            shard_levels: 2,
        }).await
    });

    // Should either succeed (sled uses fewer than 64 fds) or fail gracefully
    // Must NOT panic
    match result {
        Ok(_store) => { /* lucky — enough fds */ }
        Err(e) => {
            // Error message should be meaningful
            let msg = format!("{}", e);
            assert!(!msg.contains("panic"), "Should not panic on fd exhaustion");
        }
    }

    setrlimit(Resource::NOFILE, orig_soft, orig_hard).unwrap();
}
```

#### Step 3: Implementation

This is primarily a **test** — verifying that existing code handles fd exhaustion without panicking. The fix is ensuring all I/O paths return `Result` rather than `.unwrap()` on file operations. Phase A's RwLock migration already removed the biggest panic source. The remaining `.unwrap()` calls are in test code only.

No production code change required if existing error paths handle I/O errors correctly. The test validates this.

---

## 5. Cross-Cutting Concerns

### 5.1 Observability

- Health check timeout logged at WARN with origin_id and timeout duration
- FUSE read timeout logged at WARN with inode, offset, size
- CAS chunk re-fetch logged at WARN with chunk hash
- PID file path logged at INFO on lock acquisition

### 5.2 Performance

- Health checks now parallel: O(1) wall-clock time instead of O(N) per check cycle
- FUSE read timeout: 30s cap prevents indefinite hangs but doesn't improve happy-path latency
- `calculate_size()` recursive scan: runs once at startup, negligible cost

### 5.3 Testing

| Test | Status Before | Status After | Issue |
|------|---------------|--------------|-------|
| `test_local_origin_health_check_has_timeout` | ❌ FAILED | ✅ GREEN | D1 |
| `test_health_checks_run_in_parallel` | ❌ FAILED | ✅ GREEN | D2 |
| `test_fd_exhaustion_handling` | ❌ todo!() | ✅ GREEN | 5.3 |
| `test_corrupt_chunk_auto_refetched` | ❌ FAILED | ✅ GREEN | 6.4 |
| `test_missing_chunk_triggers_origin_fetch` | ❌ FAILED | ✅ GREEN | 6.4 |
| `test_passthrough_mode_when_cache_disk_dead` | ❌ todo!() | ✅ GREEN | 6.6 |
| `test_cas_size_tracking_is_correct` | NEW | ✅ GREEN | C6 |
| `test_pid_file_prevents_concurrent_mount` | NEW | ✅ GREEN | C9 |

**Note on passthrough mode** (6.6): The test expects reads to succeed when the cache dir is read-only. With chunk auto-re-fetch (4.5), this partially works — if the origin is alive and the chunk isn't in cache, the fetcher reads from origin. But the fetcher tries to _write_ the chunk to CAS, which will fail on a read-only cache dir. The implementation needs a fallback path: if CAS write fails after origin fetch, return the data anyway without caching. This makes `test_passthrough_mode_when_cache_disk_dead` pass.

---

## 6. Alternatives Considered

### 6.1 Per-Origin Configurable Timeout vs Universal 5s

Could allow `health_check_timeout_ms` per origin config. Rejected for Phase C — universal 5s is correct for all current origin types. Can be made configurable later.

### 6.2 Chunk-Level Re-Fetch vs File-Level Re-Fetch

When one chunk is corrupt, we could re-fetch just that chunk's byte range from origin. Requires the origin to support byte-range reads and the system to know which byte range maps to which chunk. Complex. File-level re-fetch reuses existing `fetch_file()` and is correct, just slightly wasteful. Good enough for Phase C.

### 6.3 `advisory-lock` Crate vs Raw `flock`

The `advisory-lock` crate wraps flock nicely but adds a dependency for 10 lines of code. Raw `libc::flock` is simple enough and avoids the dependency.

---

## 7. Implementation Plan

### 7.1 Task Sequence

| Day | Task | Issue | Effort | Tests |
|-----|------|-------|--------|-------|
| 1 (morning) | Health check timeout in `check_one()` | D1 | 1h | `test_local_origin_health_check_has_timeout` → GREEN |
| 1 (morning) | Parallel `check_all()` with `join_all` | D2 | 1h | `test_health_checks_run_in_parallel` → GREEN |
| 1 (afternoon) | Fix `calculate_size()` recursion | C6 | 1h | `test_cas_size_tracking_is_correct` → GREEN |
| 1 (afternoon) | FUSE read timeout wrapper | C7 | 1h | New timeout test |
| 2 (morning) | CAS chunk auto-re-fetch on corruption/missing | 6.4 | 3h | `test_corrupt_chunk_auto_refetched` + `test_missing_chunk_triggers_origin_fetch` → GREEN |
| 2 (afternoon) | Passthrough fallback (CAS write fails → return data anyway) | 6.6 | 1h | `test_passthrough_mode_when_cache_disk_dead` → GREEN |
| 3 (morning) | PID file / flock | C9 | 1h | `test_pid_file_prevents_concurrent_mount` → GREEN |
| 3 (morning) | fd exhaustion test | 5.3 | 1h | `test_fd_exhaustion_handling` → GREEN |
| 3 (afternoon) | Integration + regression testing | — | 2h | Full `cargo test` |
| 4 | Buffer | — | 4h | — |

### 7.2 Verification Checklist

After all tasks:

- [ ] `cargo check` — zero errors, zero warnings
- [ ] `cargo test --workspace --exclude musicfs-grpc` — all pass
- [ ] `cargo test -p musicfs-test-utils --test resilience` — **25 passed, 0 failed** (all RED tests GREEN)
- [ ] `cargo clippy` — no new warnings

---

## 8. Files Changed

| File | Change | Issue |
|------|--------|-------|
| `musicfs-origins/src/health.rs` | Timeout in `check_one()`, `join_all` in `check_all()` | D1, D2 |
| `musicfs-origins/Cargo.toml` | Add `futures` dependency (for `join_all`) | D2 |
| `musicfs-cas/src/store.rs` | Recursive `calculate_size()`, skip `index.sled` dir | C6 |
| `musicfs-fuse/src/filesystem.rs` | `tokio::time::timeout(30s)` around reader.read() | C7 |
| `musicfs-cas/src/reader.rs` | Auto-re-fetch on `IntegrityError` / `NotFound` | 6.4 |
| `musicfs-cas/src/fetcher.rs` | Possible: make `fetch_file` return data even if CAS write fails | 6.6 |
| `musicfs-cli/src/main.rs` | PID file with flock, fd exhaustion handling | C9, 5.3 |
| `musicfs-test-utils/tests/resilience.rs` | Replace remaining todo!()s, add new tests, update chunk tests with fetcher pipeline | all |

---

## 9. Glossary / References

| Term | Definition |
|------|------------|
| **join_all** | `futures::future::join_all` — runs multiple futures concurrently, waits for all |
| **flock** | Advisory file locking syscall — `LOCK_EX | LOCK_NB` for exclusive non-blocking |
| **EMFILE** | "Too many open files" errno — returned when process fd limit is reached |
| **Passthrough mode** | When CAS is unavailable, read directly from origin without caching |

| Document | Path |
|----------|------|
| Phase A plan | [phase-a-stop-dying.md](phase-a-stop-dying.md) |
| Phase B plan | [phase-b-crash-recovery.md](phase-b-crash-recovery.md) |
| Resilience audit | [resilience-fault-tolerance.md](resilience-fault-tolerance.md) |
