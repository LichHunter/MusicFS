# Phase A: Stop Dying — Implementation Plan

**Authors:** AI-assisted  
**Status:** Draft  
**Last Updated:** 2026-05-13  
**Reviewers:** TBD  
**Approvers:** TBD  
**Prerequisites:** [resilience-fault-tolerance.md](resilience-fault-tolerance.md), [resilience-testing.md](resilience-testing.md)  
**Estimated Effort:** ~5 days

---

[TOC]

---

## 1. Abstract

Implement the 6 most critical resilience fixes (issues 2.1, 2.2, 2.7, 2.9, 2.10, 3.7 from the [resilience audit](resilience-fault-tolerance.md)) that prevent MusicFS from dying on common operational events: signals, panics, lock poisoning, and systemd lifecycle.

Issues 2.3 (shutdown orchestration), 2.4 (cache integrity), 2.5 (sync recovery), 2.6 (task supervisor), 2.8 (disk space) are deferred to Phase B — they depend on Phase A infrastructure or on the [persistent state](persistent-state.md) work.

**Development flow** (TDD, per-issue):
1. Create stubs so the codebase compiles
2. Write RED tests that express the expected behavior
3. Implement the fix
4. Verify tests turn GREEN
5. Run full test suite — no regressions

---

## 2. Background

MusicFS currently dies on:
- Any signal (SIGTERM, SIGINT) — instant death, no cleanup
- Any panic in a writer thread — RwLock poisons, all FUSE ops crash
- systemd lifecycle — `Type=notify` but no `sd_notify`, ExecStop is a stub
- Crash leaves stale FUSE mount — users must manually `fusermount -u`

The [resilience test crate](../../musicfs/crates/musicfs-test-utils/) and RED tests are already in place. This plan implements the fixes to turn them GREEN.

---

## 3. Goals & Non-Goals

### 3.1 Goals

- Signal handler catches SIGTERM/SIGINT and initiates clean exit
- Panics are logged with full context before process terminates
- RwLock poisoning cannot cascade to kill FUSE operations
- systemd integration works (`sd_notify READY=1`, `ExecStopPost`)
- Stale FUSE mounts are detected and cleaned on startup
- All existing 162 tests continue to pass
- All Phase A RED tests turn GREEN

### 3.2 Non-Goals

- Graceful shutdown orchestration with ordered teardown (Phase B — needs CancellationToken plumbing through all components)
- Task supervisor for background task restart (Phase B)
- Cache integrity checks on startup (Phase B — needs persistent state)
- Disk space monitoring (Phase B)
- Interrupted sync recovery (Phase B — needs persistent state)

---

## 4. Proposed Design

### 4.1 Implementation Order

Dependencies determine the order. Each issue is independent except where noted.

```
4.2  RwLock poison fix        (no deps, instant win, unblocks safety)
 ↓
4.3  Panic hook               (no deps, complements RwLock fix)
 ↓
4.4  systemd ExecStopPost     (no deps, config-only change)
 ↓
4.5  sd_notify integration    (no deps, new crate dependency)
 ↓
4.6  Signal handling           (depends on: FUSE mount change to spawn_mount2)
 ↓
4.7  Stale mount detection    (depends on: signal handling for clean test)
```

### 4.2 Issue 2.9: RwLock Poison Fix

**Approach**: Replace `std::sync::RwLock` with `parking_lot::RwLock` in all production paths. `parking_lot` never poisons — a panic in a writer releases the lock and subsequent readers see the pre-panic state.

**Why parking_lot over poison recovery**: The codebase already uses `parking_lot` in `prefetch.rs` and `index.rs`. Using it everywhere is consistent. The alternative (`.unwrap_or_else(|p| p.into_inner())`) is verbose and error-prone — one missed call re-introduces the bug.

#### Step 1: Stubs (compile)

None needed — `parking_lot::RwLock` is a drop-in replacement (same API, no `PoisonError`).

#### Step 2: RED tests

Already exist in `tests/resilience.rs`:
- `test_poisoned_tree_lock_returns_eio_not_panic` — currently passes (demonstrates the problem)
- `test_parking_lot_rwlock_survives_panic` — currently passes (proves the fix works)

Additional test to add: verify FUSE filesystem survives a writer panic on the tree lock.

#### Step 3: Implementation

**Files to change:**

| File | Change |
|------|--------|
| `musicfs-fuse/src/filesystem.rs` | `use std::sync::RwLock` → `use parking_lot::RwLock`; remove all `.unwrap()` on lock calls (parking_lot returns guard directly, not `Result`) |
| `musicfs-cas/src/reader.rs` | Same change for `manifests: RwLock<HashMap<...>>` |
| `musicfs-cas/src/fetcher.rs` | Same change for `origins` and `file_meta` locks |
| `musicfs-origins/src/registry.rs` | Same change for `origins` and `watch_handles` locks |
| `musicfs-cache/src/eviction.rs` | Same change for `access_times` and `hash_to_time` locks |
| `musicfs-core/src/metrics.rs` | Same change for histogram locks |
| `musicfs-cache/src/tree.rs` | Same change for `last_refresh` lock |

**Pattern**: In each file:
```rust
// BEFORE
use std::sync::RwLock;
let guard = self.tree.read().unwrap();

// AFTER
use parking_lot::RwLock;
let guard = self.tree.read();  // No unwrap needed
```

For the `MusicFs` struct in `filesystem.rs`, the `tree` field is `Arc<RwLock<VirtualTree>>` — this is passed in from `main.rs`. Change `main.rs` to use `parking_lot::RwLock` there too.

#### Step 4: Verify

```bash
cargo test                          # All 162+ tests pass
cargo test -p musicfs-test-utils    # Resilience tests pass
cargo check                         # No warnings
```

---

### 4.3 Issue 2.2: Panic Hook

**Approach**: Install a custom panic hook at daemon startup that logs the panic with `tracing::error!` before the default behavior (abort/unwind). This ensures panics are captured in log files and journald.

#### Step 1: Stubs

Add to `musicfs-core/src/lib.rs`:
```rust
pub fn install_panic_hook() {
    // stub — will be implemented
}
```

#### Step 2: RED tests

Write in `tests/resilience.rs`:
```rust
#[test]
fn test_panic_hook_logs_to_tracing() {
    // Install hook with a test tracing subscriber
    // Trigger panic via catch_unwind
    // Verify error! log contains panic message + thread name
}
```

#### Step 3: Implementation

**File**: `musicfs-core/src/lib.rs` (or new `musicfs-core/src/panic.rs`)

```rust
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");

        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };

        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());

        tracing::error!(
            thread = thread_name,
            location = %location,
            "PANIC: {}",
            message
        );

        default_hook(info);
    }));
}
```

**Call site**: `musicfs-cli/src/main.rs`, at the very top of `main()`:
```rust
fn main() -> Result<()> {
    musicfs_core::install_panic_hook();
    let cli = Cli::parse();
    // ...
}
```

#### Step 4: Verify

```bash
cargo test -p musicfs-core          # Panic hook unit tests
cargo test -p musicfs-test-utils    # Resilience tests
```

---

### 4.4 Issue 3.7 + 2.7: systemd Service Fix + FUSE Cleanup

**Approach**: Fix the systemd service file and add stale mount detection on startup.

#### Step 1: No stubs needed (config change)

#### Step 2: RED tests

Already exists: `test_systemd_service_has_execstoppost` — currently fails because service file lacks `ExecStopPost`.

Add test for stale mount detection:
```rust
#[test]
fn test_stale_mount_check_function_exists() {
    // Verify the function signature exists
    // (actual mount test needs privileged environment)
}
```

#### Step 3: Implementation

**File**: `dist/musicfs.service`

```diff
 ExecStop=/usr/bin/musicfs shutdown
+ExecStopPost=/usr/bin/fusermount -uz %h/music || true
 Restart=on-failure
```

Note: `fusermount -uz` is "lazy unmount" — always succeeds even if mount is busy. The `|| true` prevents systemd from treating cleanup failure as a service failure.

**File**: `musicfs-cli/src/main.rs` — add stale mount check before mounting:

```rust
fn check_stale_mount(mountpoint: &Path) -> Result<()> {
    // Check /proc/mounts for existing mount at this path
    if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            if line.contains(&mountpoint.to_string_lossy().as_ref()) && line.contains("fuse") {
                warn!("Stale FUSE mount detected at {:?}, attempting cleanup", mountpoint);
                let status = std::process::Command::new("fusermount")
                    .args(["-uz", &mountpoint.to_string_lossy()])
                    .status();
                match status {
                    Ok(s) if s.success() => info!("Stale mount cleaned up"),
                    Ok(s) => warn!("fusermount exited with: {}", s),
                    Err(e) => warn!("Failed to run fusermount: {}", e),
                }
            }
        }
    }
    Ok(())
}
```

Also fix the `test_systemd_service_has_execstoppost` test path — currently points to wrong location.

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils -- test_systemd  # Service file test
```

---

### 4.5 Issue 2.10: sd_notify Integration

**Approach**: Add `sd-notify` crate, call `READY=1` after mount, `STOPPING` on shutdown.

#### Step 1: Stubs

Add dependency to `musicfs-cli/Cargo.toml`:
```toml
sd-notify = "0.4"
```

#### Step 2: RED tests

Write test that mocks the notify socket:
```rust
#[test]
fn test_sd_notify_ready_sent() {
    // Create Unix datagram socket at $NOTIFY_SOCKET
    // Call sd_notify::notify(READY=1)
    // Verify message received on socket
}
```

#### Step 3: Implementation

**File**: `musicfs-cli/src/main.rs`

After `fs.mount()` succeeds (or more precisely, after `spawn_mount2` — see 4.6):
```rust
// Notify systemd we're ready
if let Err(e) = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]) {
    debug!("sd_notify not available (not running under systemd): {}", e);
}
```

On shutdown path:
```rust
let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
```

#### Step 4: Verify

```bash
cargo test -p musicfs-test-utils -- test_sd_notify
cargo build -p musicfs-cli  # Verify it compiles with new dep
```

---

### 4.6 Issue 2.1: Signal Handling

**Approach**: Switch from `fuser::mount2` (blocking) to `fuser::spawn_mount2` (background), then listen for signals on the main thread.

This is the most complex change in Phase A. It restructures the daemon's main loop.

#### Step 1: Stubs

Change `MusicFs::mount()` signature to return a session handle:

```rust
// BEFORE
pub fn mount(self, mountpoint: &Path) -> Result<()> {
    fuser::mount2(self, mountpoint, &options)?;
    Ok(())
}

// AFTER (stub — returns BackgroundSession)
pub fn spawn_mount(self, mountpoint: &Path) -> Result<fuser::BackgroundSession> {
    let session = fuser::spawn_mount2(self, mountpoint, &options)?;
    Ok(session)
}
```

Keep old `mount()` temporarily for compatibility.

#### Step 2: RED tests

Write in `tests/resilience.rs`:
```rust
#[tokio::test]
async fn test_sigterm_triggers_shutdown() {
    // Spawn daemon as child process
    // Wait for mount
    // Send SIGTERM
    // Verify clean exit within 10s
    // Verify mountpoint is unmounted
}
```

This test requires the signal handler to exist. It will be RED until implementation.

#### Step 3: Implementation

**File**: `musicfs-cli/src/main.rs` — rewrite `run_mount()`:

```rust
fn run_mount(mountpoint: PathBuf, origin_path: Option<PathBuf>, cache_dir: Option<PathBuf>) -> Result<()> {
    let origin_path = origin_path.context("--origin is required")?;
    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.handle().clone();

    let (tree, reader) = runtime.block_on(async {
        // ... existing setup code (unchanged) ...
        Ok::<_, anyhow::Error>((tree, reader))
    })?;

    // Check for stale mount before mounting
    check_stale_mount(&mountpoint)?;

    let fs = MusicFs::with_reader(tree, reader, handle.clone());
    info!("Mounting filesystem at {:?}", mountpoint);

    // spawn_mount2 returns immediately — FUSE runs in background
    let session = fs.spawn_mount(&mountpoint)
        .context("Failed to mount filesystem")?;

    // Notify systemd
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
    info!("MusicFS ready, PID {}", std::process::id());

    // Block on signal
    runtime.block_on(async {
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate()
        )?;
        let mut sigint = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::interrupt()
        )?;

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down");
            }
        }

        Ok::<_, anyhow::Error>(())
    })?;

    // Shutdown sequence
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
    info!("Unmounting filesystem");
    drop(session); // BackgroundSession::drop() calls unmount
    info!("Shutdown complete");

    Ok(())
}
```

**File**: `musicfs-fuse/src/filesystem.rs` — add `spawn_mount()`:

```rust
pub fn spawn_mount(self, mountpoint: &Path) -> Result<fuser::BackgroundSession> {
    info!("Mounting MusicFS at {:?}", mountpoint);
    let options = vec![
        fuser::MountOption::RO,
        fuser::MountOption::FSName("musicfs".to_string()),
        fuser::MountOption::AutoUnmount,
        fuser::MountOption::AllowOther,
    ];
    let session = fuser::spawn_mount2(self, mountpoint, &options)
        .map_err(musicfs_core::Error::Io)?;
    Ok(session)
}
```

#### Step 4: Verify

```bash
cargo build -p musicfs-cli
cargo test -p musicfs-test-utils -- test_sigterm  # Process-level test
cargo test                                         # No regressions
```

---

## 5. Cross-Cutting Concerns

### 5.1 Security & Privacy

- No new attack surface — changes are internal lifecycle management
- Panic hook does NOT log sensitive data (only panic message, thread name, location)
- `sd_notify` uses existing systemd socket — no new IPC

### 5.2 Observability

- Panic hook ensures all panics are captured in logs/journald
- Signal handling logs which signal triggered shutdown
- sd_notify gives systemd accurate service state
- Stale mount detection logs cleanup attempts

### 5.3 Testing

All changes follow the TDD flow:
1. Stubs compile
2. RED tests document expected behavior
3. Implementation turns tests GREEN
4. Full suite passes (no regressions)

---

## 6. Alternatives Considered

### 6.1 Poison Recovery Instead of parking_lot

**Alternative**: Keep `std::sync::RwLock`, add `.unwrap_or_else(|p| p.into_inner())` to every lock call.

**Rejected**: 30+ call sites to change, easy to miss one, and the pattern is verbose. `parking_lot` is already a dependency and is strictly better for this use case (faster, no poison, correct API).

### 6.2 Keep mount2 (blocking) with Signal Thread

**Alternative**: Keep `fuser::mount2`, spawn a separate thread for signal handling, use a channel to communicate shutdown.

**Rejected**: `mount2` consumes `self` and blocks — there's no clean way to interrupt it from another thread. `spawn_mount2` is the canonical solution from the `fuser` crate.

### 6.3 Defer sd_notify Until Full Shutdown Orchestration

**Alternative**: Implement sd_notify only after CancellationToken + graceful shutdown are in place.

**Rejected**: sd_notify `READY=1` is critical now — without it, `Type=notify` in the service file means systemd will timeout and kill the daemon on every start. The shutdown `STOPPING` notification is a bonus but not required for Phase A.

---

## 7. Implementation Plan

### 7.1 Task Sequence

| Day | Task | Issue | Effort | Test Approach |
|-----|------|-------|--------|---------------|
| 1 (morning) | RwLock → parking_lot migration | 2.9 | 2h | Existing GREEN test validates; verify no `.unwrap()` on locks |
| 1 (afternoon) | Panic hook | 2.2 | 2h | New test: panic → verify tracing output |
| 2 (morning) | systemd ExecStopPost + stale mount check | 3.7 + 2.7 | 2h | Existing RED test → GREEN; new stale mount test |
| 2 (afternoon) | sd_notify integration | 2.10 | 2h | New test: mock socket → verify READY=1 |
| 3 | Signal handling (spawn_mount2 + signal loop) | 2.1 | 4h | Fork daemon → send SIGTERM → verify exit |
| 4 | Integration + regression testing | — | 4h | Full `cargo test`, manual FUSE mount test |
| 5 | Buffer for issues found during integration | — | 4h | — |

### 7.2 Verification Checklist

After all tasks complete:

- [ ] `cargo check` — zero errors, zero warnings
- [ ] `cargo test` — all 162+ existing tests pass
- [ ] `cargo test -p musicfs-test-utils` — all resilience tests pass
- [ ] `cargo clippy` — no new warnings
- [ ] `grep -r '\.read()\.unwrap()\|\.write()\.unwrap()' crates/` — zero hits in production code (test code is OK)
- [ ] `dist/musicfs.service` contains `ExecStopPost`
- [ ] Manual test: `musicfs mount`, then `kill -TERM <pid>`, verify clean exit + mount gone
- [ ] Manual test: `kill -9 <pid>`, then `musicfs mount` again — no "already mounted" error

---

## 8. Files Changed

| File | Change | Issue |
|------|--------|-------|
| `musicfs-fuse/src/filesystem.rs` | `std::sync::RwLock` → `parking_lot::RwLock`; add `spawn_mount()` | 2.9, 2.1 |
| `musicfs-cas/src/reader.rs` | `std::sync::RwLock` → `parking_lot::RwLock` | 2.9 |
| `musicfs-cas/src/fetcher.rs` | `std::sync::RwLock` → `parking_lot::RwLock` | 2.9 |
| `musicfs-origins/src/registry.rs` | `std::sync::RwLock` → `parking_lot::RwLock` | 2.9 |
| `musicfs-cache/src/eviction.rs` | `std::sync::RwLock` → `parking_lot::RwLock` | 2.9 |
| `musicfs-cache/src/tree.rs` | `std::sync::RwLock` → `parking_lot::RwLock` | 2.9 |
| `musicfs-core/src/metrics.rs` | `std::sync::RwLock` → `parking_lot::RwLock` | 2.9 |
| `musicfs-core/src/lib.rs` | Add `install_panic_hook()` | 2.2 |
| `musicfs-cli/src/main.rs` | Panic hook, signal handler, spawn_mount2, sd_notify, stale mount check | 2.1, 2.2, 2.7, 2.10 |
| `musicfs-cli/Cargo.toml` | Add `sd-notify`, `tokio-util` deps | 2.10, 2.1 |
| `dist/musicfs.service` | Add `ExecStopPost`, fix `ExecStop` | 3.7 |
| `tests/resilience.rs` | Update/add tests for signal, panic hook, sd_notify | all |

---

## 9. Glossary / References

| Term | Definition |
|------|------------|
| **parking_lot** | Fast, poison-free lock implementation. Already a project dependency. |
| **spawn_mount2** | `fuser` API that mounts FUSE in a background thread, returning a `BackgroundSession` handle |
| **sd_notify** | systemd notification protocol. `READY=1` signals service started, `STOPPING` signals shutdown. |
| **BackgroundSession** | Handle returned by `spawn_mount2`. Dropping it unmounts the filesystem. |

| Document | Path |
|----------|------|
| Resilience audit | [resilience-fault-tolerance.md](resilience-fault-tolerance.md) |
| Resilience testing | [resilience-testing.md](resilience-testing.md) |
| Architecture | [architecture.md](../architecture.md) |
