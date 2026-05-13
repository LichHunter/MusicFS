# MusicFS Resilience & Fault Tolerance Plan

**Date**: 2026-05-13
**Status**: Research Complete — Ready for Implementation
**Prerequisites**: [architecture.md](../architecture.md), [requirements.md](../requirements.md)
**Related Requirements**: NFR-7 (Availability), NFR-8 (Data Integrity), FR-25 (Resilience)

---

## 1. Audit Summary

MusicFS is designed as a critical filesystem daemon. Like any Linux filesystem, it must never "just die" — it must survive crashes, network failures, disk pressure, and signal interrupts with a clear recovery path for every failure mode.

### Current Resilience Posture

**Working well:**
- Origin failover with retry (100ms→500ms→2s) via `FailoverExecutor`
- NFS stale handle retry (`retry_on_stale` in nfs.rs)
- SMB disconnect retry (`retry_on_disconnect` in smb.rs)
- Webhook delivery retries with configurable count
- Health monitoring with consecutive failure tracking and degraded state
- SQLite WAL mode (`PRAGMA journal_mode = WAL`) — crash-safe
- SQLite NORMAL sync (`PRAGMA synchronous = NORMAL`) — good perf/safety tradeoff
- FUSE operations return proper error codes (ENOENT/EROFS/EIO), never panic
- Broadcast lag handling (RecvError::Lagged) in server/webhook/indexer
- systemd restart on failure (`Restart=on-failure`, `RestartSec=5`)

**Critical gaps identified:** 10 issues, detailed below.

---

## 2. Critical Gaps

### 2.1 No Signal Handling (SIGTERM/SIGINT/SIGHUP)

**Location**: `musicfs-cli/src/main.rs`

**Problem**: `main.rs` has no `tokio::signal::ctrl_c()` or `unix::signal(SIGTERM)`. The FUSE mount blocks the main thread — there's no way to trigger graceful cleanup. When systemd sends SIGTERM, the process just dies with no flush, no unmount, no DB sync.

**Impact**: Corrupted tantivy index, orphaned FUSE mount (users see "Transport endpoint is not connected"), dirty cache state.

**Required**:
- `tokio::signal::ctrl_c()` + `tokio::signal::unix::signal(SignalKind::terminate())` listener
- Signal triggers `CancellationToken` that propagates to all background tasks
- FUSE session unmount via `fuser::Session::unmount()` or `fusermount -u`
- Flush tantivy writer, close SQLite connections, stop health monitor/watcher/indexer
- Log clean shutdown with timing

**Architecture ref**: FR-17.5 (graceful shutdown with drain), FR-1.4 (release all resources on unmount)

---

### 2.2 No Panic Hook / catch_unwind

**Location**: None (completely absent)

**Problem**: No `std::panic::set_hook()` anywhere. A panic in any background task (health monitor, watcher, indexer) silently kills that task — the daemon continues in degraded state with zero notification. A panic in the FUSE thread kills the whole daemon instantly.

**Impact**: Silent degradation or instant death with no diagnostic output.

**Required**:
- Custom panic hook that logs the panic with `error!()` before default behavior
- Include thread name, backtrace, and panic payload in log
- For background tasks: `catch_unwind` wrapper that logs + triggers task restart
- For FUSE thread: panic hook should attempt emergency unmount before abort

---

### 2.3 No Graceful Shutdown Orchestration

**Location**: `musicfs-cli/src/main.rs`, all background task spawns

**Problem**: 
- `musicfs shutdown` CLI command prints "gRPC client integration pending" — it's a stub
- No `CancellationToken` or shutdown signal propagation to background tasks
- `WatchHandle::drop()` tries `try_send(())` but that's best-effort
- Health monitor, indexer, prefetcher loop forever with no shutdown path

**Impact**: On shutdown, background tasks are killed mid-operation. Partial writes, corrupt indexes.

**Required**:
- `tokio_util::sync::CancellationToken` shared across all components
- Each background task checks `token.cancelled()` in its loop
- Shutdown sequence with ordering:
  1. Stop accepting new FUSE operations (drain timeout from ShutdownRequest)
  2. Cancel background tasks (watcher → indexer → health monitor → prefetcher)
  3. Flush tantivy index writer
  4. Close SQLite connections (checkpoint WAL)
  5. Unmount FUSE
  6. Exit

---

### 2.4 No Cache Integrity Validation on Startup

**Location**: `musicfs-cache/src/db.rs` (`Database::open()`)

**Problem**: Architecture requires "The system SHALL validate cache integrity on startup" (FR-25.5). Currently `Database::open()` just opens and runs schema — no integrity check. No `PRAGMA integrity_check`, no tantivy index validation, no CAS chunk verification. If the process was killed during a write, corrupt data silently persists.

**Impact**: Corrupt metadata served to FUSE clients after crash recovery.

**Required**:
- On startup: `PRAGMA integrity_check` on SQLite (quick mode for large DBs)
- Validate tantivy index can be opened and searched
- Spot-check random CAS chunks (verify hash matches content)
- If corruption detected: log warning, offer `--repair` mode
- Repair mode: rebuild tantivy index from SQLite, re-verify CAS chunks

---

### 2.5 No Interrupted Sync Recovery

**Location**: `musicfs-sync/src/delta.rs`

**Problem**: Architecture requires "The system SHALL recover from interrupted synchronization" (NFR-8.3). `DeltaDetector::detect_changes()` has no checkpoint/resume mechanism. If killed during sync, next restart re-scans from scratch. No partial manifest tracking — partially cached files have no state.

**Impact**: Wasted bandwidth, slow restart after crash during large sync.

**Required**:
- Sync state table in SQLite: `sync_progress(origin_id, phase, last_path, files_processed, started_at)`
- Checkpoint after each batch of files processed
- On restart: check for incomplete sync, resume from last checkpoint
- Partial manifests: mark files as `sync_state = 'partial'` until all chunks cached
- On read of partial file: fetch remaining chunks on demand

---

### 2.6 Spawned Tasks Are Fire-and-Forget

**Location**: 13 `tokio::spawn()` calls across server.rs, search_service.rs, indexer.rs, health.rs, watcher.rs, prefetch.rs, artwork.rs

**Problem**: None of the spawned tasks have their `JoinHandle` stored for monitoring. If health monitor panics → no failover, origins silently become "unknown". If watcher panics → no change detection, stale data forever. If indexer panics → search silently stops updating.

**Impact**: Silent feature degradation, impossible to detect or recover.

**Required**:
- `TaskSupervisor` struct that stores `JoinHandle<()>` for each critical task
- Periodic check (every 30s): is the task still running?
- If task died: log error, attempt restart with backoff
- Critical tasks (must restart): health monitor, file watcher, search indexer
- Non-critical tasks (log and continue): prefetcher, webhook sender
- Expose task health via gRPC `GetStatus()` response

---

### 2.7 No FUSE Unmount on Crash

**Location**: `dist/musicfs.service`, `musicfs-cli/src/main.rs`

**Problem**: When the daemon dies, the FUSE mount becomes a dead mountpoint. `ls /mnt/music` hangs or returns "Transport endpoint is not connected". `ExecStop` calls `musicfs shutdown` which is a stub. No `fusermount -u` anywhere.

**Impact**: Users must manually `fusermount -u /mnt/music` after every crash.

**Required**:
- `ExecStopPost=/usr/bin/fusermount -u /mnt/music` in systemd service
- In signal handler: attempt `fuser::Session::unmount()` before exit
- On startup: check if mountpoint is already mounted (stale), auto-unmount if so
- Timeout on unmount attempt (5s), then force unmount

---

### 2.8 No Disk Space Handling

**Location**: `musicfs-cas/src/store.rs`

**Problem**: CAS `put()` writes files with no check for ENOSPC. If cache disk fills up, chunk writes fail silently or crash. No emergency eviction, no watermark monitoring.

**Impact**: Daemon crash or cache corruption when disk fills.

**Required**:
- Check available disk space before CAS write
- High watermark (90% full): trigger aggressive LRU eviction
- Critical watermark (95% full): stop prefetching, evict aggressively
- Emergency (99% full): reject new cache writes, serve only cached data
- Periodic disk space monitoring (every 60s) with metric export
- `statvfs()` for disk checks — cheap syscall

---

### 2.9 `.unwrap()` on RwLock in Production FUSE Paths

**Location**: `musicfs-fuse/src/filesystem.rs` (every FUSE operation), `musicfs-cas/src/reader.rs`, `musicfs-origins/src/registry.rs`

**Problem**: `self.tree.read().unwrap()` appears in every FUSE operation (lookup, getattr, readdir, open, read). `self.manifests.write().unwrap()` in ContentReader. `self.origins.read().unwrap()` in OriginRegistry. If any writer panics while holding a write lock, **every** subsequent FUSE operation panics → instant daemon death.

**Impact**: Single poisoned RwLock = total daemon crash. This is the #1 single-point-of-failure.

**Required**:
- Replace `.unwrap()` with `.read().unwrap_or_else(|poisoned| poisoned.into_inner())` for read locks (safe: readers don't mutate)
- For write locks: log error + return EIO to FUSE caller
- Alternative: use `parking_lot::RwLock` which doesn't poison on panic
- Audit all 30+ `.unwrap()` calls on locks in production paths

---

### 2.10 No sd_notify Integration

**Location**: `musicfs-cli/src/main.rs`, `dist/musicfs.service`

**Problem**: systemd service has `Type=notify` but no code calls `sd_notify(READY=1)`. systemd will think the service never started and kill it after `TimeoutStartSec` (default 90s).

**Impact**: Service fails to start under systemd.

**Required**:
- Add `sd-notify` crate dependency
- Call `sd_notify::notify(false, &[NotifyState::Ready])` after FUSE mount succeeds
- Call `sd_notify::notify(false, &[NotifyState::Stopping])` on shutdown
- Call `sd_notify::notify(false, &[NotifyState::Status("Serving N files")])` periodically
- If `WatchdogSec` configured: periodic `sd_notify::notify(false, &[NotifyState::Watchdog])`

---

## 3. Medium Gaps

### 3.1 No systemd Watchdog Integration

**Priority**: Medium
**Location**: `dist/musicfs.service`, `musicfs-cli/src/main.rs`

**Problem**: The systemd service has no `WatchdogSec` directive and no code sends periodic keepalive pings. systemd has a built-in watchdog mechanism: if a service declares `WatchdogSec=30s`, systemd expects `sd_notify(WATCHDOG=1)` every 15 seconds (half the interval). If the daemon hangs (deadlock, infinite loop, blocked on I/O), systemd detects the silence and restarts it.

Currently, if MusicFS deadlocks (e.g., a poisoned RwLock cascading, a stuck `block_on()` in the FUSE thread, or a sled compaction blocking the tokio runtime), the process stays alive but completely unresponsive. Users see hung `ls` commands, and systemd thinks everything is fine because the process PID still exists.

**Current code**: No watchdog-related code exists anywhere. The systemd unit has `Restart=on-failure` but that only triggers on process death, not hangs.

**Impact**: Daemon can hang indefinitely with no automatic recovery. Users must manually `kill -9` the process.

**Required**:
- Add `WatchdogSec=30s` to `dist/musicfs.service`
- Spawn a dedicated watchdog task in `main.rs` that sends `sd_notify(WATCHDOG=1)` every 15s
- The watchdog task should also perform a lightweight health check before sending:
  - Can we acquire a read lock on the virtual tree? (proves FUSE path isn't deadlocked)
  - Is the tokio runtime responsive? (proves async tasks can run)
  - Are critical background tasks still alive? (proves supervisor is working)
- If any check fails: log error, skip the watchdog ping → systemd kills and restarts
- Depends on: sd_notify integration (2.10), task supervisor (2.6)

**Architecture ref**: NFR-10.3 (health check endpoint/signal)

**Files**: `dist/musicfs.service`, `musicfs-cli/src/main.rs`

---

### 3.2 No Connection Pooling for Remote Origins

**Priority**: Medium
**Location**: `musicfs-origins/src/s3.rs`, `musicfs-origins/src/sftp.rs`

**Problem**: S3 and SFTP origins are currently feature-gated stubs. The SFTP stub comments explicitly note "Use deadpool connection pool, not `Arc<Mutex<SftpSession>>`" as an Oracle fix. When these origins are implemented, each read operation will establish a new connection — SSH handshake (SFTP) or HTTPS/TLS negotiation (S3). For a music player seeking through a file, this means dozens of connections per second.

**Current code**:
- `s3.rs`: 51-line stub with commented implementation showing raw per-request `get_object()` calls
- `sftp.rs`: 12-line stub noting `deadpool` connection pool requirement
- No connection pool crate in workspace `Cargo.toml`
- NFS and SMB origins delegate to local filesystem operations (no network connection to pool)

**Impact**: 
- SFTP: Each `read()` = SSH handshake (200-500ms). Seeking in a file = unusable latency.
- S3: AWS SDK has internal connection pooling via hyper, but without explicit pool management, connection limits aren't enforced and idle connections aren't cleaned up.
- Under load (10+ concurrent readers from remote origins), connection exhaustion is likely.

**Required**:
- SFTP: Use `deadpool-russh` or custom pool with `deadpool` generic pool
  - Pool size: configurable, default 4 per origin
  - Connection health check: send keepalive before returning from pool
  - Idle timeout: close connections idle >60s
  - Connection recovery: if SSH session drops, create new session transparently
- S3: Configure hyper connection pool settings explicitly
  - `pool_max_idle_per_host`: 4 (default is unlimited)
  - `pool_idle_timeout`: 90s
  - Request timeout: 30s (as noted in Oracle fixes in s3.rs comments)
- All remote origins: wrap operations with `tokio::time::timeout(30s)` to prevent hung connections from blocking indefinitely
- Add `deadpool` to workspace dependencies

**Architecture ref**: NFR-6.2 (connection pooling for remote origins)

**Files**: `musicfs-origins/src/sftp.rs`, `musicfs-origins/src/s3.rs`, `musicfs-origins/Cargo.toml`

---

### 3.3 No Backpressure on Event Bus

**Priority**: Low
**Location**: `musicfs-core/src/events.rs`

**Problem**: The `EventBus` uses `tokio::sync::broadcast::channel(1024)`. When the channel is full (1024 events buffered), `broadcast` silently drops the oldest events for slow receivers. The current `publish()` method only logs when there are zero receivers — it has no detection for slow-receiver drops.

On the receiver side, we recently added `RecvError::Lagged(n)` handling in server.rs, webhook.rs, and indexer.rs — those log a warning and continue. But the publish side has no awareness that events are being dropped.

**Current code** (`events.rs`):
```rust
pub fn publish(&self, event: Event) {
    trace!(event = ?event, "Publishing event");
    let receiver_count = self.sender.receiver_count();
    if self.sender.send(event).is_err() && receiver_count > 0 {
        debug!(receiver_count = receiver_count, "Event dropped, no active receivers");
    }
}
```

The `send()` return value is `Result<usize, SendError>` — the `Err` case means zero receivers. It does NOT indicate channel-full drops. Those happen silently on the receiver side via `Lagged`.

**Impact**:
- During heavy file change events (large origin rescan), the watcher may publish faster than the indexer can consume
- Search index falls behind, webhook notifications are missed, gRPC event streams have gaps
- No metric to detect this is happening — silent data loss

**Required**:
- Add a metric counter: `musicfs_events_lagged_total` incremented each time a receiver sees `Lagged(n)`, with the lag count added
- Add channel capacity to `GetStatus()` response so operators can tune it
- Make channel capacity configurable (currently hardcoded 1024 in `Default::default()`)
- Consider: if lag count exceeds threshold (e.g., 1000 events in 60s), temporarily pause the publisher (watcher) to let consumers catch up
- Alternative: switch to `tokio::sync::mpsc` per-subscriber with bounded channels and explicit backpressure (more complex but no silent drops)

**Architecture ref**: FR-18.1 (emit events for file access — events must not be silently lost)

**Files**: `musicfs-core/src/events.rs`, `musicfs-core/src/config.rs` (add event_bus_capacity config)

---

### 3.4 No FUSE Session Recovery

**Priority**: Low
**Location**: `musicfs-fuse/src/filesystem.rs`

**Problem**: The FUSE mount is established via `fuser::mount2(self, mountpoint, &options)` which blocks the calling thread until the filesystem is unmounted. If the FUSE kernel module encounters issues (e.g., kernel memory pressure, `/dev/fuse` fd becomes invalid, or the FUSE connection is interrupted by a kernel update), the mount becomes unusable with no recovery path.

**Current code** (`filesystem.rs`):
```rust
pub fn mount(self, mountpoint: &Path) -> musicfs_core::Result<()> {
    let options = vec![
        fuser::MountOption::RO,
        fuser::MountOption::FSName("musicfs".to_string()),
        fuser::MountOption::AutoUnmount,
        fuser::MountOption::AllowOther,
    ];
    fuser::mount2(self, mountpoint, &options).map_err(musicfs_core::Error::Io)?;
    Ok(())
}
```

Note: `MountOption::AutoUnmount` is set, which means the kernel will auto-unmount if the process dies. But this doesn't help with:
- FUSE connection interruption while process is alive
- Kernel-side FUSE abort (e.g., `echo 1 > /sys/fs/fuse/connections/<N>/abort`)
- `/dev/fuse` errors during memory pressure

**Impact**: If the FUSE connection drops, the daemon is alive but the mount is dead. No recovery without full restart.

**Required**:
- Detect FUSE connection loss (the `mount2` call returns with an error or the `destroy()` callback is invoked unexpectedly)
- On unexpected FUSE disconnect: log error, attempt remount after brief delay
- Maximum remount attempts: 3, with exponential backoff (1s, 5s, 15s)
- If remount fails: log critical error, trigger clean shutdown
- Consider using `fuser::spawn_mount2()` instead of `mount2()` — returns a `BackgroundSession` that can be monitored and re-established
- Note: remounting requires rebuilding the virtual tree (or keeping it alive separately from the FUSE session)

**Architecture ref**: FR-1.1 (mount as FUSE filesystem), NFR-7.2 (graceful degradation)

**Files**: `musicfs-fuse/src/filesystem.rs`, `musicfs-cli/src/main.rs`

---

### 3.5 sled Crash Recovery Not Verified

**Priority**: Medium
**Location**: `musicfs-cas/src/store.rs`

**Problem**: sled (used as CAS chunk index mapping `ChunkHash → ChunkLocation`) has built-in crash recovery — it uses a log-structured merge tree with write-ahead logging. However, MusicFS never verifies that sled's recovery succeeded or that the index is consistent with the actual chunk files on disk.

**Current code** (`store.rs`):
```rust
pub async fn open(config: CasConfig) -> Result<Self, CasError> {
    fs::create_dir_all(&config.chunks_dir).await?;
    let index_path = config.chunks_dir.join("index.sled");
    let index = sled::open(&index_path)?;  // No recovery verification
    let current_size = Self::calculate_size(&config.chunks_dir).await;
    // ...
}
```

Failure scenarios:
1. **Chunk file written, sled index not updated** (crash between fs::write and index.insert): Orphaned chunk file on disk, invisible to the system. Wastes disk space.
2. **sled index updated, chunk file not written** (crash between index.insert and fs::write — unlikely due to ordering but possible with async I/O): Index points to nonexistent chunk. `get()` will fail with `CasError::NotFound`.
3. **sled corruption**: sled can fail to open with `sled::Error::Corruption`. Currently this propagates as `CasError::Sled` and crashes the daemon.
4. **Size accounting drift**: `current_size` is calculated by scanning files in `chunks_dir`, but only at the top level (`read_dir` without recursion). Since chunks are stored in sharded subdirectories (e.g., `aa/bb/aabb...`), `calculate_size()` misses all chunk files. The `current_size` is always ~0.

**Specific bug found**: `calculate_size()` only counts files directly in `chunks_dir`, but chunks are stored in `chunks_dir/aa/bb/<hash>` (2 levels deep per `shard_levels: 2`). This means `current_size` is always wrong, and cache size enforcement/eviction never works correctly.

**Impact**: 
- Cache size tracking is broken (always reports ~0 bytes)
- Eviction never triggers (cache grows unbounded until disk fills)
- After crash: orphaned chunks waste disk, missing chunks cause read errors
- sled corruption = daemon won't start

**Required**:
- Fix `calculate_size()`: recursively scan shard directories, or compute size from sled index entries
- On startup: verify sled opens cleanly; if `sled::Error::Corruption`, attempt `sled::Config::repair()`
- Consistency check (optional, `--verify-cas` flag):
  - For each entry in sled index: verify chunk file exists on disk
  - For each chunk file on disk: verify entry exists in sled index
  - Report orphaned files and missing chunks
  - Option to auto-repair: delete orphaned files, remove dangling index entries
- Consider atomic write pattern: write chunk to `.tmp` file, `rename()` to final path, then update sled index — `rename()` is atomic on Linux

**Architecture ref**: NFR-8.1 (verify chunk integrity via checksums), NFR-8.4 (detect and report cache corruption)

**Files**: `musicfs-cas/src/store.rs`

---

### 3.6 No Config Reload (SIGHUP)

**Priority**: Low
**Location**: `musicfs-core/src/config.rs`, `musicfs-cli/src/main.rs`

**Problem**: The architecture requires "The system SHALL support runtime configuration changes" (FR-17.4). Currently, configuration is loaded once at startup from TOML file and CLI args. There is no way to change configuration without restarting the daemon.

**Current code**: `Config` struct in `config.rs` has fields for origins, cache, logging, template, search, and prefetch. All are set once. The prefetch engine has `update_config()` method but it's never called at runtime.

Common operations that should be hot-reloadable:
- Adding/removing origins (e.g., plugging in an external drive)
- Changing cache size limits
- Adjusting log level
- Enabling/disabling prefetching
- Updating path template

Operations that require restart:
- Mount point change
- FUSE options
- gRPC socket path

**Impact**: Any config change requires daemon restart → FUSE unmount → all in-flight reads fail → media players stop playback.

**Required**:
- Register SIGHUP handler via `tokio::signal::unix::signal(SignalKind::hangup())`
- On SIGHUP: re-read config file, diff against current config
- Hot-reloadable fields: log level, cache limits, prefetch config, origin list
- Cold fields (require restart): mount point, FUSE options, socket path
- Emit `ConfigReloaded` event on successful reload
- Log what changed: `info!(changed_fields = ?diff, "Configuration reloaded")`
- Validate new config before applying (don't break on invalid TOML)
- Expose via gRPC: `ReloadConfig()` RPC for programmatic reload

**Architecture ref**: FR-17.4 (runtime configuration changes)

**Files**: `musicfs-core/src/config.rs`, `musicfs-cli/src/main.rs`, `musicfs-grpc/src/server.rs`

---

### 3.7 ExecStop Is a Stub

**Priority**: High (but overlaps with 2.3 and 2.7)
**Location**: `dist/musicfs.service`, `musicfs-cli/src/main.rs`

**Problem**: The systemd service has `ExecStop=/usr/bin/musicfs shutdown` but the `run_shutdown()` function in `main.rs` just prints a message and exits:

```rust
fn run_shutdown(graceful: bool, timeout: u32) -> Result<()> {
    println!("Shutdown requested (graceful: {}, timeout: {}s)", graceful, timeout);
    println!("gRPC client integration pending");
    Ok(())
}
```

When systemd stops the service, `ExecStop` runs first. Since it does nothing, systemd then sends SIGTERM. Since there's no signal handler (gap 2.1), the daemon is killed immediately. This is a triple failure: ExecStop is a no-op, SIGTERM has no handler, and there's no ExecStopPost to clean up.

**Current flow on `systemctl stop musicfs`**:
1. systemd runs `ExecStop=/usr/bin/musicfs shutdown` → prints message, exits 0
2. systemd sends SIGTERM to main daemon PID → daemon dies instantly
3. FUSE mount becomes stale (no `fusermount -u`)
4. No WAL checkpoint, no tantivy flush, no sled flush

**Impact**: Every `systemctl stop musicfs` leaves a stale mount and risks data corruption.

**Required** (this is solved by combining fixes from 2.1, 2.3, and 2.7):
- Short term: Change `ExecStop` to `ExecStop=/usr/bin/fusermount -u /mnt/music` (at least unmounts cleanly)
- Add `ExecStopPost=/usr/bin/fusermount -uz /mnt/music` as safety net (lazy unmount, always succeeds)
- Medium term: Implement gRPC shutdown RPC, make `musicfs shutdown` actually connect and send `ShutdownRequest`
- Long term: Signal handler catches SIGTERM, initiates graceful shutdown sequence, `ExecStop` becomes optional

**Architecture ref**: FR-17.5 (graceful shutdown with drain), FR-1.4 (release all resources on unmount)

**Files**: `dist/musicfs.service`, `musicfs-cli/src/main.rs`, `musicfs-grpc/src/server.rs`

---

### 3.8 No Timeout on Origin Operations in FUSE Path

**Priority**: Medium
**Location**: `musicfs-fuse/src/filesystem.rs`, `musicfs-cas/src/fetcher.rs`

**Problem**: When a FUSE `read()` triggers a cache miss, the request flows through `FileReader → ContentFetcher → Origin.read()`. For remote origins, this can block for an unbounded duration — the failover retry config has delays (100ms, 500ms, 2s) but no overall timeout. If an origin is responding but extremely slowly (trickle attack, network congestion), all 3 retry attempts could each take minutes.

**Current code** (`filesystem.rs` read path):
```rust
let result = std::thread::scope(|_| {
    handle.block_on(async {
        reader.read(file_id, offset as u64, size).await
    })
});
```

This `block_on` has no timeout. A hung origin blocks the FUSE thread. Since fuser processes FUSE requests sequentially (single-threaded filesystem impl), one hung read blocks ALL FUSE operations — `ls`, `stat`, everything.

**Impact**: One slow origin request can freeze the entire filesystem for all users. Media players hang, file managers become unresponsive, and the daemon appears dead even though it's technically alive.

**Required**:
- Wrap the FUSE read path with `tokio::time::timeout(Duration::from_secs(30), reader.read(...))`
- On timeout: return `EIO` to FUSE, log warning with origin and path
- Add per-origin timeout configuration (local: 5s, remote: 30s)
- The S3 origin stub already notes this requirement: "Wrap all remote calls with `tokio::time::timeout(30s)`"
- Consider: FUSE `read()` has a kernel-side timeout too (usually 30s), but relying on kernel timeout gives poor error messages

**Architecture ref**: NFR-1.6 (read cache miss remote: max 1000ms — current code has no enforcement)

**Files**: `musicfs-fuse/src/filesystem.rs`, `musicfs-cas/src/fetcher.rs`, `musicfs-cas/src/reader.rs`

---

### 3.9 No Protection Against Concurrent Mount Attempts

**Priority**: Low
**Location**: `musicfs-cli/src/main.rs`

**Problem**: Nothing prevents two instances of `musicfs mount /mnt/music` from running simultaneously. The second instance would try to mount on the same mountpoint, potentially succeeding (FUSE allows it on some kernels) or failing with confusing errors. Two daemons writing to the same SQLite database and sled index would cause corruption.

**Current code**: No PID file, no flock, no socket check.

**Impact**: Accidental double-start corrupts cache databases.

**Required**:
- Create a lock file at `{cache_dir}/musicfs.lock` using `flock(LOCK_EX | LOCK_NB)`
- If lock fails: print "MusicFS is already running (PID: N)" and exit 1
- Write current PID to lock file for debugging
- Lock is automatically released on process death (kernel flock semantics)
- Alternative: check if gRPC socket exists and is responsive before mounting

**Files**: `musicfs-cli/src/main.rs`

---

### 3.10 Eviction System Has Broken Size Accounting

**Priority**: Medium (closely related to 3.5)
**Location**: `musicfs-cache/src/eviction.rs`, `musicfs-cas/src/store.rs`

**Problem**: The LRU eviction system depends on `CasStore::current_size()` to know when to evict. But as identified in 3.5, `calculate_size()` only scans the top level of `chunks_dir`, missing all actual chunks stored in shard subdirectories. The `current_size` is effectively always ~0.

Additionally, the eviction system operates in-memory only — `LruEviction` stores access times in a `BTreeMap<Instant, ChunkHash>`. On daemon restart, all access history is lost. Every chunk has equal eviction priority, and the most recently accessed (hot) chunks are just as likely to be evicted as cold ones.

**Current code** (`store.rs`):
```rust
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
```

This only reads direct children of `chunks_dir`. Chunks are stored as `chunks_dir/aa/bb/<hash>` (2 shard levels deep). So `calculate_size` returns the size of `index.sled` files at best.

**Impact**: 
- Cache grows unbounded — eviction never triggers because size appears to be ~0
- Disk fills up → CAS writes fail → FUSE read errors
- After restart, hot data has no protection from eviction

**Required**:
- Fix `calculate_size()`: recursive walk through shard directories, or calculate from sled index (sum of all `ChunkLocation.size`)
- Better: maintain size atomically during put/delete (current code does `fetch_add`/`fetch_sub` but seeds from broken `calculate_size`)
- Persist access times: add `last_accessed` column to sled index entries, or a separate SQLite table
- On startup: reconstruct LRU order from persisted access times
- Trigger eviction proactively: when `current_size > 0.9 * max_size`, start background eviction

**Files**: `musicfs-cas/src/store.rs`, `musicfs-cache/src/eviction.rs`

---

## 4. Network Fault Tolerance Analysis

### 4.1 Scenario: Source Machine Dies

**Full failure chain analysis:**

When the machine hosting origin storage (NFS server, SMB share, S3 bucket, SFTP host) dies:

| Phase | Timing | Current Behavior | Gap? |
|-------|--------|-----------------|------|
| **Immediate** (0-5s) | First read attempt | `Origin.read()` hangs or returns error | ⚠️ No timeout on FUSE read path (gap 3.8) |
| **Detection** (5-90s) | Health check cycle | NFS/SMB: 5s timeout per check, threshold=3 → marked Unhealthy after 3 intervals | ✅ Works |
| **Failover** (0-3s) | On next read | FailoverExecutor tries next origin, retries 100ms→500ms→2s | ✅ Works |
| **Degraded mode** | Ongoing | Cache-first: CAS serves cached chunks, ENOENT for uncached | ⚠️ Partial |
| **Recovery** | Origin comes back | Health monitor detects healthy, router re-enables | ✅ Works |

**What works well:**
- **Multi-origin failover**: `FailoverExecutor.read_with_failover()` iterates all origins by priority, falls through on failure
- **Health monitoring**: `HealthMonitor.check_one()` with 5s timeout on health checks, consecutive failure tracking, per-type thresholds (Local=1, Remote=3)
- **Graceful degradation**: `Router.select_with_fallback()` falls through Healthy→Degraded→least-bad Unhealthy
- **Event notification**: `AllOriginsUnhealthy` event emitted when all origins are down, `OriginHealthChanged` on transitions
- **NFS-specific**: `retry_on_stale()` handles ESTALE (stale NFS file handle) with retry
- **SMB-specific**: `retry_on_disconnect()` handles ENOTCONN (SMB session drop) with retry

### 4.2 Network-Specific Gaps Not Yet Covered

#### 4.2.1 No Health Check Timeout on Local Origin

**Priority**: Medium
**Location**: `musicfs-origins/src/local.rs`

**Problem**: Local origin health check uses `fs::try_exists(&self.root)` with NO timeout. If the local path is actually an NFS/CIFS automount (common in NAS setups), this can hang indefinitely when the remote server dies. NFS and SMB origins wrap their health checks in `tokio::time::timeout(5s)` — local origin does not.

**Current code**:
```rust
// local.rs - NO timeout
async fn health(&self) -> HealthStatus {
    match fs::try_exists(&self.root).await {
        Ok(true) => HealthStatus::Healthy,
        Ok(false) => HealthStatus::Unhealthy,
        Err(_) => HealthStatus::Unhealthy,
    }
}

// nfs.rs - HAS 5s timeout
async fn health(&self) -> HealthStatus {
    let health_timeout = Duration::from_secs(5);
    match tokio::time::timeout(health_timeout, self.inner.stat(Path::new("/"))).await {
        Ok(Ok(_)) => HealthStatus::Healthy,
        Ok(Err(_)) | Err(_) => HealthStatus::Unhealthy,
    }
}
```

**Impact**: If a "local" origin is actually a mounted network share (extremely common — `/mnt/nas/music`), the health check hangs forever when the NAS dies. The health monitor task blocks on this one check and can't check any other origins either (checks are sequential in `check_all()`).

**Required**:
- Add timeout to local origin health check: `tokio::time::timeout(Duration::from_secs(5), fs::try_exists(...))`
- Better: move the timeout into `HealthMonitor.check_one()` so ALL origin types get a universal timeout regardless of their implementation
- Make health checks parallel (currently sequential `for origin in origins { check_one(...).await }`)

---

#### 4.2.2 Sequential Health Checks Block on Dead Origins

**Priority**: Medium  
**Location**: `musicfs-origins/src/health.rs`

**Problem**: `check_all()` checks origins sequentially:
```rust
async fn check_all(&self) {
    let origins: Vec<_> = self.origins.iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();
    for (id, origin) in origins {
        self.check_one(&id, &origin).await;  // Sequential!
    }
}
```

If 3 origins are configured and the first one's health check hangs (network timeout), the other 2 origins won't be checked until the first one finishes/times out. With a 5s timeout per check and 3 origins, a single dead origin delays all health updates by 5s.

**Impact**: Health detection for all origins is delayed by the slowest (dead) origin. With check_interval=30s and 3 origins, worst case: healthy origin shows stale state for 30s + 5s×3 = 45s.

**Required**:
- Change `check_all()` to use `futures::future::join_all()` or `tokio::spawn` per origin
- Each check runs concurrently with its own timeout
- All origins checked within one timeout window (5s), not N×5s

---

#### 4.2.3 No "Offline Mode" State Machine

**Priority**: Medium
**Location**: Architecture gap (no current implementation)

**Problem**: When ALL origins are down and all cached data has been served, the daemon has no concept of "offline mode". It continues trying reads, getting errors, retrying — wasting resources. There's no:
- Backoff on health checks when all origins are down (still checks every 30s)
- User-visible state indicating "all origins offline, serving from cache only"
- Suppression of repeated error logs (every failed read logs warnings)
- Proactive notification that data may be stale

The gRPC `MountState` enum already has `MOUNT_STATE_DEGRADED` ("Some origins unavailable") but no code sets it.

**Impact**: Log spam during extended outage, wasted CPU on retries, no clear signal to monitoring systems.

**Required**:
- Track global mount state: Ready → Degraded (some origins down) → Offline (all origins down)
- In Offline mode: increase health check interval to 60s (reduce wasted probes)
- In Offline mode: suppress per-read error logging, emit periodic summary instead ("Still offline, N reads served from cache in last 60s, M reads failed")
- Set `MountState` in gRPC `StatusResponse` based on health snapshot
- Emit event: `MountStateChanged { from, to }` for monitoring integration
- When first origin recovers: log "Origin {id} recovered, exiting offline mode", trigger immediate sync to catch up

---

#### 4.2.4 No Automatic Origin Re-registration After Transient Failure

**Priority**: Low  
**Location**: `musicfs-origins/src/registry.rs`, `musicfs-sync/src/watcher.rs`

**Problem**: When a local origin's directory is temporarily unavailable (NAS reboot, USB drive unmounted briefly), the inotify watcher in `watcher.rs` may die with an error. The `OriginWatcher` logs the error and the task ends:
```rust
if let Err(e) = Self::watch_loop(&origin_id, &root, &event_bus, &mut stop_rx).await {
    error!("Watcher error: {}", e);
}
// Task exits silently — no restart
```

When the origin comes back, the watcher is dead. No new file change events are detected until the daemon is restarted.

Similarly, if an NFS mount is re-established, the watcher created with the old file descriptors won't work on the new mount.

**Impact**: After origin recovery, file changes are not detected. Users see stale data until manual restart.

**Required**:
- Watcher should auto-restart on failure (ties into task supervisor from gap 2.6)
- On origin health transition Unhealthy→Healthy: restart watcher for that origin
- On watcher failure: retry with backoff (1s, 5s, 30s), max 5 attempts
- Log state: "Watcher for origin {id} failed, will retry in {delay}s"

---

#### 4.2.5 No DNS Resolution Failure Handling

**Priority**: Low  
**Location**: Future S3/SFTP implementations

**Problem**: Remote origins (S3, SFTP) depend on DNS resolution. DNS failures are a common transient network issue. The health check may fail not because the origin is down, but because DNS is temporarily unavailable. Currently, DNS failure = origin marked Unhealthy with same threshold as actual origin death.

**Impact**: Transient DNS glitch causes unnecessary failover, cache misses, and degraded experience for 90+ seconds (3 failures × 30s check interval).

**Required**:
- Distinguish DNS errors from connection errors in health checks
- DNS failure → Degraded (not Unhealthy), with faster re-check (5s instead of 30s)
- Cache DNS results internally (TTL 60s) to survive brief DNS outages
- Log DNS failures separately: `warn!("DNS resolution failed for {origin}, using cached IP")`
- Note: NFS and SMB origins (mounted locally) don't have this issue — DNS is resolved at mount time by the kernel

---

#### 4.2.6 No Network Partition Detection (Split-Brain)

**Priority**: Low  
**Location**: Architecture gap

**Problem**: If the daemon can reach some origins but not others (network partition), it may serve inconsistent data — e.g., origin A has version 1 of a file, origin B has version 2, and only B is reachable. The daemon happily serves version 2 without noting that the file's origin of record (A, higher priority) is unavailable.

Currently, `FailoverExecutor` just tries origins in priority order and uses whoever responds first. There's no concept of "this file's authoritative origin is A, and A is down — we're serving from backup B which may be stale."

**Impact**: Subtle inconsistency — user may hear an old version of a re-tagged file without knowing it. Low severity for a music filesystem, but matters for correctness.

**Required**:
- Track per-file "authoritative origin" (the origin with highest priority that has the file)
- When serving from non-authoritative origin: set a flag, log at debug level
- When authoritative origin recovers: trigger delta sync for files served from backup
- Optional: expose "served from backup" as extended attribute or in gRPC events
- This is P3 / nice-to-have — the read-only nature of MusicFS makes this low-risk

---

### 4.3 Network Fault Summary

| Failure Type | Detection Time | Recovery | Gaps |
|---|---|---|---|
| **Source machine death** | 15-90s (health check cycles) | Automatic failover to backup origin | Health check on local origin has no timeout; checks are sequential |
| **Network partition** | 5-15s (first failed read + health) | Failover to reachable origin | No stale-data awareness for files served from backup |
| **Transient NFS stale handle** | Immediate (on read attempt) | Automatic retry in NFS origin | ✅ Handled |
| **SMB session drop** | Immediate (on read attempt) | Automatic retry in SMB origin | ✅ Handled |
| **All origins down** | 15-90s | Serve from cache (CAS) | No offline mode state machine, log spam |
| **Origin recovery** | 30s (next health check) | Auto-detected, routing restored | Watcher not restarted, no catch-up sync |
| **DNS failure** | 5-15s (health check timeout) | None — treated as origin death | No distinction from real failure |
| **Slow network (not dead)** | Not detected | Reads succeed but slowly | No latency-based degradation threshold |

---

## 5. Additional Critical Issues

These are failure modes not covered by the network, shutdown, or crash-recovery categories above. They deal with resource exhaustion, runtime deadlocks, and data loss scenarios specific to a FUSE daemon.

### 5.1 FUSE↔Tokio Deadlock Risk (block_on inside sync callback)

**Priority**: Critical
**Location**: `musicfs-fuse/src/filesystem.rs` (read method)

**Problem**: The `fuser` crate requires the `Filesystem` trait to be implemented synchronously — all callbacks (`lookup`, `getattr`, `readdir`, `read`) run on fuser's internal thread. But all of MusicFS's I/O is async (tokio). The current bridge is:

```rust
fn read(&mut self, ...) {
    let result = std::thread::scope(|_| {
        handle.block_on(async {
            reader.read(file_id, offset as u64, size).await
        })
    });
}
```

`handle.block_on()` from inside a non-tokio thread blocks that thread until the future completes. This is generally fine. **But** if the tokio runtime's thread pool is saturated (all worker threads are busy), the `block_on` call will deadlock — it's waiting for a tokio worker to pick up the task, but all workers are busy (possibly also doing `block_on` calls from other FUSE requests if `fuser` uses multiple threads internally, or doing heavy CAS I/O).

Specific deadlock scenario:
1. Multiple FUSE reads arrive simultaneously (Plex scanning library)
2. Each calls `handle.block_on()` which enqueues work on the tokio runtime
3. The tokio runtime workers are busy with CAS I/O, health checks, prefetching, watcher events
4. `block_on` waits for a free worker → FUSE thread blocks
5. If fuser processes requests on a single thread (which `mount2` does by default): **all FUSE operations hang**
6. Even `ls` and `stat` are blocked because they share the same fuser thread

**Impact**: Complete filesystem hang under moderate load. Users see `ls /mnt/music` hang indefinitely. The daemon is alive, systemd thinks it's fine, but the filesystem is frozen.

**Required**:
- Use `fuser::spawn_mount2()` instead of `mount2()` — this runs FUSE in a background thread and returns a `BackgroundSession`, freeing the main thread for async work
- Consider using `tokio::task::spawn_blocking()` for FUSE reads instead of `std::thread::scope` + `block_on` — this uses tokio's dedicated blocking thread pool which auto-grows
- Set `tokio::runtime::Builder::max_blocking_threads()` appropriately (default 512, should be sufficient)
- Add metrics: track FUSE callback latency, tokio task queue depth
- Alternatively: use `fuser`'s `Session::run_custom()` with a custom thread pool, or implement `Filesystem` with async support if fuser supports it

**Architecture ref**: NFR-2.4 (>1000 concurrent file handles), NFR-1.3 (<5ms open cached)

---

### 5.2 Tantivy Index Corruption on Crash

**Priority**: High
**Location**: `musicfs-search/src/index.rs`, `musicfs-search/src/indexer.rs`

**Problem**: The tantivy `IndexWriter` buffers documents in memory and only flushes to disk on `commit()`. The indexer commits every 5 seconds (via `commit_timer`). If the daemon crashes between commits, all indexed documents since the last commit are lost.

Worse: if a crash occurs **during** a `commit()` call, the tantivy index files on disk may be in an inconsistent state. Tantivy uses a segment-based architecture — a commit writes new segment files and updates a `meta.json` manifest. If the process dies between writing segments and updating the manifest, the index may reference files that don't exist or miss files that do.

**Current code** (indexer.rs):
```rust
_ = commit_timer.tick() => {
    if pending_commit {
        if let Err(e) = self.index.commit() {
            error!("Index commit error: {}", e);
        }
        pending_commit = false;
    }
}
```

The `IndexWriter` is allocated with 50MB heap (`index.writer(50_000_000)`). In a heavy indexing scenario (origin rescan of 100K files), up to 50MB of uncommitted document data can be lost.

On the `index.commit()` error path: the indexer logs the error and continues. But a failed commit may leave the writer in an inconsistent state — subsequent `add_document` or `commit` calls may also fail.

**Impact**: After crash recovery, search results are incomplete or empty. Users search for a song they know exists and get no results.

**Required**:
- On startup: attempt to open tantivy index. If `Index::open_in_dir()` fails with corruption, delete the index directory and rebuild from SQLite metadata
- Add a "rebuild search index" CLI command: `musicfs search rebuild`
- Reduce commit interval to 1-2 seconds for lower data loss window (tradeoff: more I/O)
- On `commit()` failure: try `writer.rollback()` to restore consistent state, then retry
- On persistent commit failures: stop the indexer, log critical error, flag for rebuild on restart
- Add integrity check on startup: run a simple search query — if it panics or errors, rebuild

**Architecture ref**: FR-14.1 (index metadata for full-text search — index must be recoverable)

---

### 5.3 File Descriptor Exhaustion

**Priority**: High
**Location**: System-wide, `dist/musicfs.service`

**Problem**: MusicFS holds open many file descriptors simultaneously:
- 1 for FUSE `/dev/fuse`
- 1 for SQLite database (+ WAL + SHM = 3 total)
- 1 for sled (multiple internal files, typically 5-10)
- 1 per tantivy segment (grows with index size, typically 10-50)
- 1 per inotify watch (1 per watched directory — can be thousands)
- N for CAS chunk reads during cache misses (concurrent fetcher operations)
- N for gRPC connections (1 per connected client)
- N for origin file reads (local origin opens files via tokio::fs)

The default Linux `ulimit -n` is 1024. A music library with 10K directories being watched could exhaust this easily (inotify allocates one fd per watch on the directory, plus the inotify fd itself).

The systemd service has **no `LimitNOFILE` directive**.

**Impact**: Once fd limit is hit, every operation fails: FUSE reads return EIO, SQLite queries fail, new inotify watches fail silently, gRPC connections are rejected. The daemon is technically alive but completely non-functional.

**Required**:
- Add `LimitNOFILE=65536` to `dist/musicfs.service`
- Track open fd count via `/proc/self/fd` periodically (every 60s), export as metric
- Set high/critical watermarks: at 80% of limit, log warning; at 95%, stop accepting new gRPC connections and pause prefetching
- For inotify specifically: Linux has `fs.inotify.max_user_watches` (default 8192 on some distros, 524288 on others). Document the requirement: `sysctl fs.inotify.max_user_watches=524288`
- Consider: for very large libraries (100K+ directories), inotify is not viable — switch to polling-based change detection (already mentioned in architecture for remote origins, but needed for large local origins too)

**Architecture ref**: NFR-3.1 (handle 1M+ files), NFR-3.2 (handle 100K+ directory entries)

---

### 5.4 inotify Unreliable for NFS/SMB Watches

**Priority**: Medium
**Location**: `musicfs-sync/src/watcher.rs`, `musicfs-origins/src/nfs.rs`, `musicfs-origins/src/smb.rs`

**Problem**: The `OriginWatcher` uses `notify::RecommendedWatcher` (which uses inotify on Linux) for ALL origin types. But inotify does NOT work across NFS or SMB mounts — the NFS/SMB server doesn't send change notifications to the client kernel. The code already acknowledges this:

```rust
// nfs.rs
debug!("NFS watch - inotify may be unreliable over NFS, consider polling");

// smb.rs
warn!("SMB watch using inotify - may be unreliable. Consider polling for remote mounts.");
```

But then proceeds to set up inotify anyway. Changes made on the NFS/SMB server (or by other clients) will NEVER be detected by the watcher.

**Impact**: Files added/modified/deleted on the NFS/SMB server are invisible to MusicFS until manual rescan. Users add music to their NAS and wonder why it doesn't appear.

**Required**:
- Implement polling-based watcher for remote origin types (NFS, SMB, S3, SFTP)
- Polling interval: configurable per origin, default 300s (5 minutes)
- Polling strategy: walk directory tree, compare mtime against cached mtime
- Optimization: only walk directories whose parent mtime changed (directory mtime changes when files are added/removed)
- Keep inotify for local origins (reliable and instant)
- Hybrid mode for "local" origins that might be network mounts: start with inotify, fall back to polling if no events detected after initial changes

**Architecture ref**: FR-10.3 (use polling for remote origins without push support)

---

### 5.5 Memory Growth from Virtual Tree

**Priority**: Medium
**Location**: `musicfs-cache/src/tree.rs`

**Problem**: The `VirtualTree` holds the entire directory structure in memory — every directory node, every file node, the inode map, and the path map. For 1M files with average path length of 100 bytes:
- `inode_map`: 1M entries × ~100 bytes = ~100MB
- `path_map`: 1M entries × ~150 bytes (path + overhead) = ~150MB
- `DirNode.children`: BTreeMap overhead per directory
- Total: ~300-400MB for 1M files, approaching the NFR-4.3 peak limit of 500MB

The tree is wrapped in `Arc<RwLock<VirtualTree>>` and kept fully in memory for the entire daemon lifetime. There's no pagination, no lazy loading of deep subtrees, and no eviction of rarely-accessed branches.

**Current code**: `TreeBuilder::build()` constructs the entire tree upfront during mount. For 10M files (stretch goal NFR-3.5), this would require 3-4GB of RAM — well beyond limits.

**Impact**: Memory usage scales linearly with library size. At 10M files, the daemon either OOMs or is killed by systemd MemoryMax.

**Required**:
- Short term: add `MemoryMax=2G` to systemd service as safety net (prevents OOM-killing other services)
- Short term: track RSS via `/proc/self/statm`, export as metric, warn at 80% of limit
- Medium term: lazy subtree loading — only load the first 2 levels of the tree on mount, load deeper levels on first `readdir()`
- Medium term: evict cold subtrees after configurable timeout (30 minutes no access)
- Long term: move tree to SQLite/sled-backed structure with in-memory LRU cache for hot paths — this is a significant architectural change

**Architecture ref**: NFR-4.1 (idle <50MB), NFR-4.3 (peak <500MB), NFR-3.5 (10M files stretch goal)

---

### 5.6 System Clock Jump Breaks Mtime Comparison

**Priority**: Low
**Location**: `musicfs-sync/src/delta.rs`, `musicfs-cache/src/db.rs`

**Problem**: Delta detection compares `origin_mtime` (stored as unix timestamp in SQLite) against the current file's mtime. If the system clock jumps (NTP correction, VM suspend/resume, manual adjustment), files may appear changed (clock jumped forward — everything looks "modified") or unchanged (clock jumped backward — new files look "old").

Additionally, `last_sync` in the database uses `strftime('%s', 'now')` which is based on wall-clock time. A clock jump can make sync timing calculations wrong — e.g., "sync all files changed in the last hour" could miss files if the clock jumped forward.

**Impact**: Unnecessary full re-sync after NTP correction (wastes bandwidth), or missed changes after backward clock jump (stale data served).

**Required**:
- Use monotonic clock (`Instant`) for internal timing (health checks, intervals) — already done in health.rs
- For mtime comparison: use content hash as secondary check when mtime is "suspicious" (within 5 seconds of a known clock jump)
- Track clock jumps: compare `SystemTime::now()` against monotonic progression, log if jump >5s detected
- For `last_sync`: store both wall-clock time and a monotonic sequence number
- Note: this is inherent to any mtime-based system. Even git has this problem. Low priority because NTP corrections are typically <1s on well-configured systems

---

### 5.7 CAS Chunk Write Not Atomic

**Priority**: Medium
**Location**: `musicfs-cas/src/store.rs`

**Problem**: CAS `put()` writes a chunk in two steps:
```rust
fs::write(&path, data).await?;     // Step 1: write chunk file
self.index.insert(hash, location)?; // Step 2: update sled index
```

If the process crashes between step 1 and step 2: orphaned chunk file on disk (wastes space, but harmless). If the process crashes during step 1: partially written chunk file on disk. On next startup, `calculate_size()` counts this partial file, and if someone tries to read it, `verify_integrity()` will catch the hash mismatch — but only on read, not proactively.

More subtle: `fs::write()` in tokio is NOT atomic. It calls `write_all()` which may do multiple syscalls. If the kernel OOM-kills the process or power is lost during write, the file contains partial data.

**Impact**: After crash: orphaned or partial chunk files. Partial chunks cause integrity errors on read, which currently propagate as `CasError::IntegrityError` and cause FUSE to return EIO.

**Required**:
- Write to temporary file first: `{path}.tmp`
- Call `fsync()` on the temporary file (ensures data is on disk, not just in page cache)
- Rename temporary to final path: `rename()` is atomic on Linux for same-filesystem renames
- Then update sled index
- This guarantees: either the chunk is fully written and indexed, or it doesn't exist
- On startup: scan for `.tmp` files in chunks directory, delete them (incomplete writes from previous crash)
- Cost: one extra `rename()` syscall per chunk write — negligible

---

### 5.8 No Resource Limits in systemd Service

**Priority**: Medium
**Location**: `dist/musicfs.service`

**Problem**: The systemd service has security hardening (`NoNewPrivileges`, `ProtectSystem`, `PrivateTmp`) but no resource limits. A bug causing infinite allocation (memory leak, unbounded cache, runaway indexing) will consume all system resources before anything stops it.

**Current service has NO**:
- `LimitNOFILE` — fd limit (default 1024, way too low)
- `MemoryMax` — memory ceiling
- `MemoryHigh` — memory pressure notification threshold
- `TasksMax` — thread/task limit
- `CPUQuota` — CPU limit (prevents background tasks from starving other services)
- `IOWeight` — I/O priority
- `WatchdogSec` — liveness check (covered in gap 3.1)

**Impact**: Resource leak → system destabilization. OOM killer picks random victim (might kill sshd or Plex instead of musicfs).

**Required**:
```ini
# Resource limits
LimitNOFILE=65536
MemoryMax=4G
MemoryHigh=2G
TasksMax=4096
CPUQuota=200%

# I/O priority (lower than media playback, higher than backups)
IOSchedulingClass=best-effort
IOSchedulingPriority=4

# OOM handling - prefer killing musicfs over other services
OOMScoreAdjust=200
```

- `MemoryHigh=2G` triggers kernel memory pressure reclaim before hitting hard limit — gives the daemon a chance to evict cache
- `MemoryMax=4G` is the hard kill limit
- `TasksMax=4096` prevents thread/task bomb from runaway spawn loops
- `OOMScoreAdjust=200` makes the kernel prefer killing musicfs over other daemons (it can recover via restart, others may not)

---

## 6. Cache/Database Sudden Death Analysis

### 6.1 Data Flow Map: What Touches What

Understanding which storage layer each operation depends on is critical for failure analysis:

```
FUSE hot path (every file access):
  lookup/getattr/readdir/open  →  VirtualTree (in-memory only)     ← NO disk dependency
  read (cache hit)             →  CasStore.get()                   ← sled index + chunk files
  read (cache miss)            →  ContentFetcher → Origin.read()   ← sled + chunk files + origin

Background tasks:
  Search indexer                →  tantivy index (disk)
  Pattern recording             →  PatternStore (SQLite, separate DB)
  Collection queries            →  CollectionStore (SQLite, separate DB)
  Health monitor                →  in-memory only (DashMap)
  File watcher                  →  in-memory + EventBus

Startup only (not runtime):
  scan_music_files()            →  origin filesystem
  TreeBuilder::build()          →  builds in-memory VirtualTree
  Database is used for metadata caching but NOT in FUSE hot path currently
```

### 6.2 Scenario: SQLite Metadata Database Dies

**How it can die**: File deleted by user/script, filesystem corruption, disk bad sector, `rm ~/.cache/musicfs/metadata.db` by mistake, permissions changed.

**What happens NOW**:
- **FUSE browsing (lookup/readdir/stat)**: **Unaffected** — VirtualTree is entirely in-memory. Users can browse and see all files.
- **FUSE read**: **Unaffected** — FileReader uses in-memory manifests + CAS. SQLite is not in the read path.
- **Search indexer**: **Unaffected** — uses tantivy, not SQLite.
- **Pattern recording**: **FAILS** — PatternStore has its own SQLite connection. If the pattern DB file is deleted, `record()` returns `PatternError::Database`. The prefetch engine catches this: `warn!("Failed to record access pattern: {}")` and continues. **Gracefully degraded**.
- **Collection queries**: **FAILS** — CollectionStore operations fail with `Error::Database`. Smart collections stop working.
- **Delta sync**: **FAILS** — DeltaDetector queries SQLite for mtime comparisons. Sync operations fail.
- **On restart**: **FATAL** — `Database::open()` re-creates the schema on empty DB, but all metadata is lost. The initial scan repopulates from origin, but this means O(N) startup again + complete re-index.

**Gap**: No detection of SQLite corruption during runtime. No mechanism to reconstruct SQLite from origin files without full restart. No backup/snapshot of metadata DB.

**Required**:
- Periodic SQLite health check: `PRAGMA quick_check` every 5 minutes (lightweight, checks page integrity)
- If SQLite becomes inaccessible during runtime: log error, flag for rebuild on restart, continue serving from in-memory tree + CAS
- On startup with missing/corrupt DB: auto-trigger full rescan from origins (already happens implicitly since scan_music_files doesn't use DB, but should log clearly)
- Consider: periodic SQLite backup via `VACUUM INTO '/path/metadata.db.bak'` (atomic backup while DB is open, available since SQLite 3.27)
- Document: `metadata.db` can always be rebuilt from origins — it's a cache, not source of truth

---

### 6.3 Scenario: sled Chunk Index Dies

**How it can die**: Disk corruption, `rm -rf ~/.cache/musicfs/chunks/index.sled/`, sled internal corruption (rare but documented), unclean shutdown leaving sled in bad state.

**What happens NOW**:
- **sled::open() on startup**: Returns `sled::Error::Corruption` → propagated as `CasError::Sled` → daemon **crashes on startup**. There is no recovery attempt.
- **sled operation during runtime** (if files deleted under sled): sled will panic or return errors. `CasStore.get()` calls `self.index.insert()` / `self.index.get()` — these errors propagate to `ReaderError::Cas` → FUSE returns EIO.
- **Orphaned chunks**: If sled index is gone but chunk files remain on disk, chunks are invisible. They waste disk space but aren't harmful.
- **Missing chunks with valid index**: If chunk files are deleted but sled still has entries, `CasStore.get()` reads the file → `CasError::Io(NotFound)` → EIO.

**Critical issue**: sled corruption = **daemon cannot start**. No recovery, no repair attempt.

**Current code** (store.rs):
```rust
let index = sled::open(&index_path)?;  // Panics or errors on corruption
```

**Required**:
- On `sled::open()` failure: attempt `sled::Config::new().path(&index_path).repair(true).open()` — sled has built-in repair mode
- If repair fails: delete the sled directory, recreate empty index, and rebuild by scanning chunk files on disk (walk shard directories, compute hash of each file, re-insert into index)
- During runtime: catch sled errors in `put()`/`get()` paths, don't propagate as panics
- Add CLI command: `musicfs cache repair` — rebuilds sled index from chunk files

---

### 6.4 Scenario: CAS Chunk Files Deleted/Corrupted

**How it can die**: User deletes chunks directory, disk failure, bitrot on cache drive, filesystem corruption, `rm -rf ~/.cache/musicfs/chunks/` by mistake.

**What happens NOW**:
- **All chunks deleted**: Every `CasStore.get()` returns `CasError::NotFound`. Every FUSE `read()` returns EIO. The filesystem is "browsable" (tree is in memory) but no file can be read.
- **Some chunks deleted**: Affected files return EIO on read. Other files work fine. **No detection** — corruption is only discovered when a specific chunk is requested.
- **Corrupted chunk** (bitrot): `verify_integrity()` catches hash mismatch → `CasError::IntegrityError` → EIO. The corrupted chunk is NOT auto-deleted or re-fetched.
- **Chunk directory permissions changed**: `fs::read()` returns permission error → `CasError::Io` → EIO.

**Critical gaps**:
1. **No automatic re-fetch on integrity error**: When `verify_integrity()` fails, the daemon returns EIO but doesn't try to re-fetch the chunk from origin. The user is stuck with a corrupt chunk until cache is cleared.
2. **No proactive corruption scanning**: Bitrot can sit undetected for months until a specific file is played.
3. **No distinction between "chunk missing" and "origin down"**: When a read fails, the user sees EIO either way. No hint about whether clearing cache would fix it.
4. **Size tracking is wrong**: (as noted in 3.10) — `current_size` doesn't reflect reality, so eviction doesn't work.

**Required**:
- On `CasError::IntegrityError`: delete the corrupt chunk, re-fetch from origin automatically, return data to FUSE caller (transparent repair)
- On `CasError::NotFound` with fetcher available: attempt to fetch from origin before returning EIO (this may already work via `get_or_fetch_manifest` but not for individual chunks — the manifest is fetched, but if a chunk file was deleted after manifest creation, only EIO is returned)
- Background scrubber: periodically (daily, configurable) verify N random chunks' integrity. Report corruption rate. If >1% corrupt, trigger full scan.
- On startup with empty/missing chunks directory: create it, log warning, treat all files as cache misses (origin fetch on demand)
- `musicfs cache verify` CLI command: full integrity scan with progress and repair option

---

### 6.5 Scenario: tantivy Search Index Dies

**How it can die**: Disk corruption, directory deleted, crash during `commit()` (as discussed in 5.2), `meta.json` corrupted, segment files truncated.

**What happens NOW**:
- **Index deleted/corrupt on startup**: `SearchIndex::open()` calls `Index::open_in_dir()` → tantivy returns error → `SearchError::Tantivy` → daemon crashes (if search is required) or search is unavailable.
- **Current open logic** (index.rs):
```rust
let index = if index_path.exists() && index_path.join("meta.json").exists() {
    Index::open_in_dir(index_path)?  // Can fail with corruption
} else {
    std::fs::create_dir_all(index_path)?;
    Index::create_in_dir(index_path, schema_obj.schema.clone())?
};
```
- **Commit failure during runtime**: Indexer logs error, sets `pending_commit = false`, continues. But uncommitted documents are lost, and the writer may be in an inconsistent state.
- **Reader reload failure**: After a bad commit, `self.reader.reload()` may fail → subsequent searches return stale results or errors.

**Impact**: 
- Startup crash if index is corrupt and code doesn't handle the error
- Search returns no results or stale results after crash recovery
- `/.search/` virtual directory is broken

**Required**:
- On `Index::open_in_dir()` failure: log error, delete index directory, create fresh index, trigger re-index from SQLite metadata or in-memory tree
- On `commit()` failure: attempt `writer.rollback()`, log error, schedule retry
- On persistent commit failures (3+ consecutive): mark indexer as degraded, stop attempting commits, flag for rebuild
- Re-index capability: `musicfs search rebuild` CLI command
- On startup: verify index health with simple query before declaring ready

---

### 6.6 Scenario: Cache Disk Hardware Failure

**How it can die**: SSD wear-out, HDD bad sectors, NVMe controller failure, filesystem goes read-only (ext4 remounts read-only on errors).

**What happens NOW**:
- **Disk goes read-only**: All writes fail (CAS put, sled insert, SQLite upsert, tantivy commit). Reads continue working for cached data. No detection — each component reports IO errors independently with no correlation.
- **Disk completely dead**: All cache operations fail. The daemon is effectively a broken pipe — tree in memory but every read() returns EIO.
- **Partial failure (bad sectors)**: Random IO errors on specific files. Some chunks work, others don't. Unpredictable behavior.

**Critical gap**: There is no centralized "cache health" check. Each component (SQLite, sled, CAS, tantivy) handles IO errors independently. There's no detection of "the entire cache disk is gone."

**Required**:
- Centralized cache health monitor:
  - Periodically (every 60s): attempt to write a small test file to cache directory, read it back, delete it
  - If write fails: cache disk is read-only or dead → enter "passthrough mode"
  - Track consecutive IO errors across all components → if >N in M seconds, declare cache unhealthy
- **Passthrough mode** (cache disk dead, origins still alive):
  - Serve reads directly from origin (bypass CAS entirely)
  - Disable prefetching, pattern recording, search indexing
  - Log: `error!("Cache disk failure detected, operating in passthrough mode")`
  - Set MountState to Degraded
  - This is the "graceful degradation" the architecture requires (NFR-7.2)
- **Recovery**: When cache disk comes back (e.g., ext4 remount-rw after fsck):
  - Detect via periodic health check
  - Run integrity checks on all stores
  - Resume normal operation
  - Log: `info!("Cache disk recovered, resuming cached operation")`

---

### 6.7 Scenario: Cache Directory Permissions Changed

**How it can die**: Security hardening script, SELinux/AppArmor policy change, user accidentally `chmod 000 ~/.cache/musicfs/`, ownership change.

**What happens NOW**: Every cache operation fails with permission denied. Each component logs its own error. No centralized detection. The daemon appears to work (tree in memory) but every `read()` fails.

**Required**:
- On startup: verify write permissions on cache directory, chunks directory, and DB files
- If permissions are wrong: log clear error message with exact path and expected permissions
- During runtime: permission errors should trigger the same cache health check as disk failure → enter passthrough mode if origins are available
- systemd service already has `ReadWritePaths=/var/cache/musicfs` — but this doesn't help if permissions on the directory itself are wrong

---

### 6.8 Cache Failure Summary

| Component | Dies on Startup | Dies During Runtime | Recovery |
|---|---|---|---|
| **SQLite metadata.db** | Recreates empty DB, full rescan needed | In-memory tree + CAS unaffected, patterns/collections fail | Rebuild from origin rescan |
| **sled chunk index** | **DAEMON CRASHES** — no repair attempt | Chunk reads fail (EIO) | Repair mode or rebuild from chunk files |
| **CAS chunk files** | Cache dir recreated, all files are cache misses | Affected reads fail (EIO), no auto re-fetch | Re-fetch from origins on demand |
| **tantivy index** | May crash or create empty index | Search returns stale/no results | Rebuild from SQLite/tree metadata |
| **Pattern DB** | Recreated empty, predictions reset | Prefetch degrades gracefully (warn + continue) | Naturally repopulates from access patterns |
| **Cache disk (hardware)** | Daemon cannot start | All cache ops fail, EIO on reads | Passthrough mode (serve from origins) |

**The biggest gap**: No "passthrough mode." If the cache disk dies but origins are alive, MusicFS should still serve files. Currently it just returns EIO everywhere. This violates NFR-7.2 (graceful degradation) — the cache is supposed to be an optimization, not a hard dependency.

---

## 7. Critical Architecture Gap: No Persistent State Used on Restart

**Full analysis moved to**: [persistent-state.md](persistent-state.md)

**Summary**: Every mount is a full cold start — O(N × origin_latency). SQLite, tantivy, patterns, and manifests all persist on disk but none are opened during mount. The 4 critical in-memory structures (VirtualTree ~400MB, ContentFetcher.file_meta ~200MB, FileReader.manifests ~100MB, LruEviction ~50MB) are rebuilt from scratch on every restart. This violates G1 (O(1) mount time), NFR-1.7 (<500ms mount), and FR-7.1 (cache persists across restarts).

**This blocks all resilience work** — persistent state must be wired up before graceful shutdown, crash recovery, or cache integrity checks have meaning.

---

## 8. Requirements Coverage

| Requirement | Description | Status |
|-------------|-------------|--------|
| NFR-7.1 | Serve cached data when origin unavailable | ✅ Via failover |
| NFR-7.2 | Graceful degradation on network failure | ⚠️ Partial (failover yes, no graceful shutdown) |
| NFR-7.3 | Retry with exponential backoff (100ms, 500ms, 2s) | ✅ In failover.rs |
| NFR-7.4 | Don't crash on malformed audio | ✅ parse_file returns Result |
| NFR-8.1 | Verify chunk integrity via checksums | ❌ Missing |
| NFR-8.2 | ACID transactions for cache DB | ✅ SQLite WAL |
| NFR-8.3 | Recover from interrupted synchronization | ❌ Missing |
| NFR-8.4 | Detect and report cache corruption | ❌ Missing |
| FR-1.4 | Release all resources on unmount | ❌ No graceful unmount |
| FR-17.5 | Graceful shutdown with drain | ❌ Stub only |
| FR-25.3 | Zero-downtime upgrades | ❌ Missing |
| FR-25.5 | Validate cache integrity on startup | ❌ Missing |

---

## 5. Implementation Priority

### Phase 0: Wire Up Persistent State (Foundational — Unblocks Everything)

**See [persistent-state.md](persistent-state.md)** — ~8 days, storage engine decision pending.

Must be completed before Phase A. Without persistent state, graceful shutdown has nothing to flush, crash recovery has nothing to recover, and integrity checks have nothing to check.

### Phase A: Stop Dying (Critical — Must Ship First)

| # | Task | Effort | Blocks |
|---|------|--------|--------|
| A1 | Signal handling (SIGTERM/SIGINT) + CancellationToken | 1 day | Everything |
| A2 | Graceful shutdown orchestration (ordered teardown) | 1 day | A1 |
| A3 | Panic hook (log before death) | 0.5 day | — |
| A4 | RwLock poison recovery (or switch to parking_lot) | 0.5 day | — |
| A5 | FUSE cleanup on exit + ExecStopPost in systemd | 0.5 day | A1, A2 |
| A6 | sd_notify integration (READY/STOPPING/WATCHDOG) | 0.5 day | A1 |

### Phase B: Recover From Crashes (High — Required for Production)

| # | Task | Effort | Blocks |
|---|------|--------|--------|
| B1 | Task supervisor (monitor + restart background tasks) | 1 day | A1 |
| B2 | Startup integrity checks (SQLite + tantivy + CAS) | 1 day | — |
| B3 | Stale mountpoint detection + auto-cleanup on startup | 0.5 day | — |
| B4 | Disk space monitoring + watermark eviction | 1 day | — |

### Phase C: Resilient Operations (Medium — Production Hardening)

| # | Task | Effort | Blocks |
|---|------|--------|--------|
| C1 | Interrupted sync recovery (checkpoint/resume) | 1.5 days | — |
| C2 | CAS chunk integrity verification + sled recovery check | 1 day | — |
| C3 | systemd watchdog integration | 0.5 day | A6 |
| C4 | SIGHUP config reload | 1 day | A1 |
| C5 | Connection pooling for remote origins (SFTP/S3) | 1 day | — |
| C6 | Fix CAS size accounting + persistent eviction LRU | 1 day | — |
| C7 | FUSE read timeout enforcement | 0.5 day | — |
| C8 | Event bus backpressure + capacity config | 0.5 day | — |
| C9 | PID file / flock to prevent concurrent mounts | 0.5 day | — |
| C10 | FUSE session recovery (detect disconnect + remount) | 1 day | A1, A2 |

### Phase D: Network Resilience (Medium — Hardening for Real-World Networks)

| # | Task | Effort | Blocks |
|---|------|--------|--------|
| D1 | Add timeout to local origin health check | 0.25 day | — |
| D2 | Parallelize health checks (join_all instead of sequential) | 0.5 day | — |
| D3 | Offline mode state machine (Ready→Degraded→Offline) | 1 day | — |
| D4 | Auto-restart watcher on origin recovery (Unhealthy→Healthy) | 0.5 day | B1 |
| D5 | DNS failure handling for remote origins | 0.5 day | C5 |
| D6 | Network partition / stale-data awareness | 0.5 day | — |

### Phase E: Runtime Robustness (High/Medium — Prevents Silent Degradation Under Load)

| # | Task | Effort | Blocks |
|---|------|--------|--------|
| E1 | Fix FUSE↔tokio deadlock: switch to spawn_mount2 + spawn_blocking | 1 day | — |
| E2 | Tantivy crash recovery: detect corruption, rebuild from SQLite | 1 day | — |
| E3 | Atomic CAS chunk writes (write-to-tmp + rename + fsync) | 0.5 day | — |
| E4 | systemd resource limits (LimitNOFILE, MemoryMax, TasksMax, OOM) | 0.25 day | — |
| E5 | fd exhaustion monitoring + inotify watch limit documentation | 0.5 day | — |
| E6 | Polling-based watcher for NFS/SMB origins | 1.5 days | — |
| E7 | Memory tracking + metrics for virtual tree growth | 0.5 day | — |

### Phase F: Cache Resilience (High — Prevents Total Failure on Cache Corruption)

| # | Task | Effort | Blocks |
|---|------|--------|--------|
| F1 | sled corruption recovery (repair mode + rebuild from chunk files) | 1 day | — |
| F2 | CAS auto re-fetch on integrity error (transparent repair) | 0.5 day | — |
| F3 | Passthrough mode (bypass cache, serve from origins when cache disk dies) | 1.5 days | — |
| F4 | Centralized cache health monitor (write test + IO error correlation) | 1 day | — |
| F5 | tantivy index corruption recovery (detect + rebuild from metadata) | 1 day | E2 |
| F6 | Background chunk scrubber (periodic integrity verification) | 0.5 day | — |
| F7 | SQLite periodic backup (VACUUM INTO) + startup permission check | 0.5 day | — |
| F8 | `musicfs cache verify` + `musicfs cache repair` + `musicfs search rebuild` CLI | 1 day | F1, F2, F5 |

**Total estimate**: ~30.5 days across phases A-F (Phase 0 tracked separately in [persistent-state.md](persistent-state.md) — ~8 days, storage decision pending)

---

## 6. Key Design Decisions Needed

1. **parking_lot vs std RwLock**: `parking_lot::RwLock` never poisons (simpler), but loses panic detection. Recommended: use `parking_lot` — panics are caught by the task supervisor, not by lock poisoning.

2. **CancellationToken propagation**: Every component that spawns tasks needs access to the token. Options: (a) pass through constructors, (b) global static. Recommended: pass through constructors for explicit dependency.

3. **Integrity check depth on startup**: Full check (verify every chunk hash) vs quick check (SQLite integrity + spot-check 100 random chunks). Recommended: quick check by default, `--full-integrity-check` flag for thorough mode.

4. **Task restart policy**: Immediate restart vs exponential backoff. Recommended: immediate first restart, then 1s→5s→30s backoff, max 5 restarts before marking task as permanently failed.

---

## 7. Files That Need Changes

### Phase 0 (Foundational) — see [persistent-state.md](persistent-state.md)

### Phase A (Critical)
- `musicfs-cli/src/main.rs` — Signal handling, shutdown orchestration, sd_notify
- `musicfs-cli/Cargo.toml` — Add `tokio-util`, `sd-notify` deps
- `musicfs-fuse/src/filesystem.rs` — RwLock poison recovery
- `musicfs-cas/src/reader.rs` — RwLock poison recovery
- `musicfs-origins/src/registry.rs` — RwLock poison recovery
- `musicfs-cas/src/fetcher.rs` — RwLock poison recovery
- `musicfs-cache/src/eviction.rs` — RwLock poison recovery
- `musicfs-core/src/metrics.rs` — RwLock poison recovery
- `dist/musicfs.service` — ExecStopPost, WatchdogSec

### Phase B (High)
- `musicfs-core/src/lib.rs` — TaskSupervisor, new module
- `musicfs-cache/src/db.rs` — Integrity check on open
- `musicfs-cas/src/store.rs` — Disk space checks
- `musicfs-cli/src/main.rs` — Stale mount detection

### Phase C (Medium)
- `musicfs-sync/src/delta.rs` — Checkpoint/resume
- `musicfs-cache/src/schema.sql` — sync_progress table
- `musicfs-core/src/config.rs` — Config reload support, event_bus_capacity
- `musicfs-cas/src/store.rs` — Fix calculate_size() recursion, sled recovery verification
- `musicfs-cache/src/eviction.rs` — Persistent LRU access times
- `musicfs-core/src/events.rs` — Lag metrics, configurable capacity
- `musicfs-fuse/src/filesystem.rs` — Read timeout, FUSE session recovery
- `musicfs-origins/src/sftp.rs` — Connection pool (deadpool)
- `musicfs-origins/src/s3.rs` — Explicit hyper pool config, request timeouts
- `musicfs-origins/Cargo.toml` — Add deadpool dependency

### Phase D (Network Resilience)
- `musicfs-origins/src/local.rs` — Add timeout to health check
- `musicfs-origins/src/health.rs` — Parallel health checks, universal timeout wrapper
- `musicfs-core/src/lib.rs` — MountState enum, offline mode state machine
- `musicfs-origins/src/registry.rs` — Watcher restart on origin recovery
- `musicfs-sync/src/watcher.rs` — Auto-restart support
- `musicfs-grpc/src/server.rs` — MountState in StatusResponse

### Phase E (Runtime Robustness)
- `musicfs-fuse/src/filesystem.rs` — Switch to spawn_mount2, use spawn_blocking for reads
- `musicfs-search/src/index.rs` — Corruption detection, rebuild capability
- `musicfs-search/src/indexer.rs` — Commit failure recovery (rollback + retry)
- `musicfs-cas/src/store.rs` — Atomic write (tmp + rename + fsync), .tmp cleanup on startup
- `musicfs-sync/src/watcher.rs` — Polling-based watcher variant for remote origins
- `musicfs-cache/src/tree.rs` — Memory tracking, lazy subtree loading (future)
- `dist/musicfs.service` — LimitNOFILE, MemoryMax, MemoryHigh, TasksMax, OOMScoreAdjust, IOSchedulingClass

### Phase F (Cache Resilience)
- `musicfs-cas/src/store.rs` — sled repair on open failure, rebuild from chunk scan, integrity re-fetch, passthrough mode
- `musicfs-cas/src/reader.rs` — Auto re-fetch on chunk integrity error instead of returning EIO
- `musicfs-search/src/index.rs` — Corruption detection, delete + recreate on open failure
- `musicfs-cache/src/db.rs` — PRAGMA quick_check, VACUUM INTO backup, permission check on open
- `musicfs-core/src/lib.rs` — CacheHealthMonitor, passthrough mode flag
- `musicfs-cli/src/main.rs` — `cache verify`, `cache repair`, `search rebuild` CLI commands
- `musicfs-fuse/src/filesystem.rs` — Passthrough read path (bypass CAS, go to origin directly)
