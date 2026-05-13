# MusicFS Resilience Testing: Design Doc

**Authors:** AI-assisted  
**Status:** Draft  
**Last Updated:** 2026-05-13  
**Reviewers:** TBD  
**Approvers:** TBD  
**Prerequisites:** [resilience-fault-tolerance.md](resilience-fault-tolerance.md), [architecture.md](../architecture.md)

---

[TOC]

---

## 1. Abstract

MusicFS has 162 unit/integration tests but zero fault injection, crash recovery, or resilience tests. This design doc defines the test infrastructure, tooling, and test cases needed to verify that MusicFS survives the 34 failure modes identified in the [resilience audit](resilience-fault-tolerance.md).

The approach uses three testing layers: trait-based mocks with failpoints for fast unit-level verification, fork-kill process tests for crash and signal recovery, and Toxiproxy with Docker for real-protocol network fault injection. A new `musicfs-test-utils` crate centralizes shared test helpers that are currently duplicated across 29 files.

---

## 2. Background

### 2.1 Current Test State

| Metric | Value |
|--------|-------|
| Total tests | 162 |
| Test files with `#[cfg(test)]` | 43 |
| Async tests (`#[tokio::test]`) | 44 |
| Fault injection tests | 0 |
| Crash recovery tests | 0 |
| Signal handling tests | 0 |
| CI pipeline | None |
| Mocking framework | None (real components + TempDir) |

### 2.2 What Exists

- **Unit tests**: Per-crate `#[cfg(test)]` modules using real implementations with `TempDir` isolation
- **Integration tests**: `crates/musicfs-cas/tests/integration.rs` — CAS + fetcher + reader pipeline
- **E2E tests**: `tests/e2e/e2e_players.rs` — mpv/VLC playback over mounted FUSE (`#[ignore]`, manual)
- **Test helpers**: `make_file_meta()`, `mock_health()` — duplicated across modules, not centralized
- **Test tooling**: `cargo-nextest` and `cargo-criterion` available in Nix flake

### 2.3 What's Missing

The [resilience audit](resilience-fault-tolerance.md) identified 34 failure modes across 6 phases. None have test coverage. The audit covers:
- Signal handling and graceful shutdown (Phase A)
- Crash recovery and cache integrity (Phase B)
- Network fault tolerance and origin failover (Phase C-D)
- Runtime deadlocks and resource exhaustion (Phase E)
- Cache/database sudden death and passthrough mode (Phase F)

### 2.4 Why "Doing Nothing" Is Not an Option

MusicFS is designed as a critical filesystem daemon. Untested failure paths mean:
- Crashes that corrupt SQLite, sled, or tantivy go undetected until production
- Signal handling code (once implemented) has no regression tests
- Origin failover logic is tested for correctness but not for actual failure scenarios
- No confidence that the daemon survives real-world conditions (disk full, NAS reboot, OOM)

---

## 3. Goals & Non-Goals

### 3.1 Goals

- **Every resilience issue gets a test** — all 34 failure modes from the audit mapped to concrete test cases
- **Tests run without root** — no kernel modules, no privileged containers for Layer 1 and Layer 2
- **Tests run fast** — Layer 1 tests complete in <1 second each; full resilience suite in <60 seconds
- **Failpoints are zero-cost** — conditional compilation via Cargo features; no runtime overhead in release builds
- **Test helpers are centralized** — `musicfs-test-utils` crate eliminates duplication across 29 files

### 3.2 Non-Goals

- **Full chaos engineering platform** — this is not Jepsen; we test known failure modes, not random exploration
- **Performance benchmarking** — covered separately by `cargo-criterion`; this doc is about correctness under failure
- **CI pipeline setup** — pipeline configuration (GitHub Actions, Nix CI) is a separate task; this doc defines what to run, not where
- **FUSE kernel-level testing** — testing kernel FUSE module behavior or `/dev/fuse` edge cases is out of scope

---

## 4. Proposed Design

### 4.1 Testing Layers

```
┌─────────────────────────────────────────────────────────────┐
│ Layer 3: Toxiproxy + Docker                                  │
│ Real protocols, real latency, real connection drops           │
│ ~5 tests, seconds each, requires docker-compose              │
├─────────────────────────────────────────────────────────────┤
│ Layer 2: Fork-Kill Process Tests                             │
│ Spawn daemon, send signals, kill -9, verify recovery         │
│ ~5 tests, seconds each, cargo test                           │
├─────────────────────────────────────────────────────────────┤
│ Layer 1: Trait Mocks + Failpoints                            │
│ FaultyOrigin, FaultyCasStore, fail_point! macros             │
│ ~25 tests, milliseconds each, cargo test                     │
└─────────────────────────────────────────────────────────────┘
```

**Rule**: Every resilience issue gets Layer 1 coverage at minimum. Critical issues (signal handling, crash recovery, FUSE unmount) additionally get Layer 2. Network-specific issues (origin failover, latency, connection drops) additionally get Layer 3.

### 4.2 New Dependencies

```toml
# Cargo.toml [workspace.dependencies]
fail = "0.5"                    # TiKV failpoints — conditional fault injection
rlimit = "0.10"                 # Resource limit manipulation (fd, memory)
nix = "0.29"                    # Signal sending, process control

# Cargo.toml [workspace.features]
failpoints = ["fail/failpoints"]  # Zero-cost when disabled

# dev-dependencies only (not shipped in release binary)
wiremock = "0.6"                # HTTP mock server (S3 origin tests)
assert_cmd = "2.0"              # CLI integration testing
```

**Why these choices:**
- **`fail`** (TiKV failpoints): Production-proven by TiKV (distributed KV store). Zero overhead when `failpoints` feature is disabled. Supports deterministic failure injection with counter/probability controls.
- **`rlimit`**: Test fd exhaustion and memory limits without root. Wraps `setrlimit`/`getrlimit` syscalls.
- **`nix`**: Send signals to child processes (`kill(pid, SIGTERM)`). Already a transitive dependency via `fuser`.
- **`wiremock`**: Pure-Rust HTTP mock server for S3 origin testing. No external process needed.

### 4.3 Test Infrastructure Crate

**`crates/musicfs-test-utils/`** — new workspace crate providing shared test helpers.

#### 4.3.1 FaultyOrigin

Wraps any `Origin` implementation with configurable failure injection:

```rust
pub struct FaultyOrigin {
    inner: Arc<dyn Origin>,
    fail_mode: Arc<RwLock<FailMode>>,
    call_count: AtomicUsize,
}

pub enum FailMode {
    Healthy,                           // Pass through to inner
    FailEveryNth(usize),               // Fail on every Nth call
    FailAfterN(usize),                 // Succeed N times, then always fail
    TimeoutMs(u64),                    // Sleep then fail (simulates hung NFS)
    PartialRead { max_bytes: usize },  // Return truncated data
    ReturnError(io::ErrorKind),        // Return specific error
}
```

Implements `Origin` trait. `fail_mode` is `Arc<RwLock<>>` so tests can change behavior mid-test (e.g., origin "recovers" after health check).

#### 4.3.2 FaultyCasStore

Wraps `CasStore` with injectable disk errors:

```rust
pub struct FaultyCasStore {
    inner: CasStore,
    inject_enospc: AtomicBool,       // put() fails with ENOSPC
    inject_eio_on_read: AtomicBool,  // get() fails with EIO
    inject_corruption: AtomicBool,   // get() returns bad data
}
```

#### 4.3.3 Centralized Fixtures

Currently duplicated across 29 test modules:

```rust
pub fn make_file_meta(id: i64, vpath: &str, size: u64) -> FileMeta;
pub fn make_audio_meta(artist: &str, album: &str, title: &str) -> AudioMeta;
pub async fn setup_test_cas(dir: &Path) -> Arc<CasStore>;
pub fn setup_test_tree(files: &[FileMeta]) -> Arc<RwLock<VirtualTree>>;
```

### 4.4 Failpoints Instrumentation

Production code locations that need `fail_point!` macros:

| Location | Failpoint Name | Simulates |
|----------|---------------|-----------|
| `musicfs-cas/src/store.rs` `put()` | `cas-put-before-write` | ENOSPC before chunk write |
| `musicfs-cas/src/store.rs` `put()` | `cas-put-after-write-before-index` | Crash between write and sled insert |
| `musicfs-cas/src/reader.rs` `get_or_fetch_manifest()` | `reader-manifest-fetch` | Manifest fetch failure |
| `musicfs-sync/src/delta.rs` `detect_changes()` | `delta-sync-after-batch` | Crash mid-sync |
| `musicfs-origins/src/health.rs` `check_one()` | `health-check-hang` | Health check hangs forever |
| `musicfs-cache/src/db.rs` `open()` | `db-open-corrupt` | Database corruption on open |

All guarded by `#[cfg(feature = "failpoints")]` — zero-cost in release builds.

### 4.5 Test File Organization

```
musicfs/
├── crates/
│   └── musicfs-test-utils/          # NEW — shared test helpers
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── faulty_origin.rs     # FaultyOrigin with FailMode
│           ├── faulty_cas.rs        # FaultyCasStore
│           ├── fixtures.rs          # make_file_meta, setup_test_cas, etc.
│           └── assertions.rs        # Custom assertions
├── tests/
│   ├── resilience/                  # NEW — resilience test suite
│   │   ├── mod.rs
│   │   ├── signal_handling.rs       # SIGTERM/SIGINT/double-signal
│   │   ├── crash_recovery.rs        # Fork-kill + state verification
│   │   ├── cache_corruption.rs      # SQLite/sled/tantivy/CAS corruption
│   │   ├── disk_failure.rs          # ENOSPC, permissions, passthrough mode
│   │   ├── resource_limits.rs       # fd exhaustion, memory limits
│   │   └── lock_poisoning.rs        # RwLock poison recovery
│   ├── failpoints/                  # NEW — failpoint-gated tests
│   │   ├── mod.rs
│   │   ├── origin_failures.rs       # Injected origin errors
│   │   ├── sync_interruption.rs     # Delta sync crash/resume
│   │   └── cas_failures.rs          # CAS write failures
│   ├── integration/                 # NEW — network integration (Docker)
│   │   ├── docker-compose.yml
│   │   ├── network_faults.rs        # Toxiproxy: latency, drops, bandwidth
│   │   └── origin_failover.rs       # Multi-origin failover integration
│   └── e2e/
│       └── e2e_players.rs           # Existing (unchanged)
```

**Running**:
```bash
# Layer 1: Fast resilience tests (no Docker, no FUSE)
cargo test --lib --tests resilience

# Layer 1: Failpoint tests (sequential, feature-gated)
cargo test --features failpoints --test failpoints -- --test-threads 1

# Layer 2: Process-level tests (included in resilience/)
cargo test --test resilience

# Layer 3: Network integration (requires docker-compose up)
cargo test --test integration -- --ignored

# All layers
cargo nextest run --features failpoints
```

### 4.6 Integration Test Docker Setup

For Layer 3 network fault testing:

```yaml
# tests/integration/docker-compose.yml
services:
  toxiproxy:
    image: ghcr.io/shopify/toxiproxy:2.9.0
    ports:
      - "8474:8474"          # Toxiproxy API
      - "20000-20010:20000-20010"  # Proxy ports

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

Tests use `noxious-client` crate to configure Toxiproxy faults at runtime (latency injection, connection drops, bandwidth throttling).

---

## 5. Cross-Cutting Concerns

### 5.1 Security & Privacy

- Tests run without root — no kernel modules, no privileged containers for Layer 1/2
- Layer 3 Docker tests use ephemeral containers with test credentials only
- No real music files or user data in tests — synthetic `make_file_meta()` fixtures
- `rlimit` tests restore original limits after test (cleanup in all code paths)

### 5.2 Observability

- Failpoint tests log injected faults via `tracing` — test failures include full trace context
- Layer 2 (fork-kill) tests capture daemon stdout/stderr for failure diagnosis
- Test coverage tracked per resilience issue (coverage matrix in Section 7)

### 5.3 Scalability & Performance

- Layer 1 tests: <10ms each, ~25 tests = <1s total
- Layer 2 tests: ~2-5s each (process spawn + signal + verify), ~5 tests = <30s total
- Layer 3 tests: ~5-10s each (Docker network), ~5 tests = <60s total
- Full suite: <2 minutes including failpoint tests (sequential `--test-threads 1`)
- Failpoint global state requires `--test-threads 1` for failpoint tests; all other tests parallelize normally

### 5.4 Testing the Tests

- Corruption tests self-validate: create known-good state → corrupt → verify detection
- FaultyOrigin has mode assertions: `assert_eq!(origin.call_count(), expected)` to verify injection triggered
- Failpoint tests verify both the error path AND the happy path (remove failpoint, retry, verify success)
- Resource limit tests always restore original limits (even on panic — use scopeguard or Drop impl)

---

## 6. Alternatives Considered

### 6.1 Jepsen / Full Chaos Engineering Framework

**Rejected.** Jepsen tests distributed consensus under network partitions. MusicFS is a single-daemon filesystem — its failure modes are local (disk, signals, panics), not distributed. The 3-layer approach covers our actual failure surface with 10x less complexity.

### 6.2 proptest / Property-Based Testing

**Deferred.** Property-based testing (random input generation) is valuable for finding edge cases in path resolution, CDC chunking, and search queries. But it's orthogonal to resilience testing — it tests correctness under random input, not correctness under infrastructure failure. Can be added later without affecting this design.

### 6.3 loom (Concurrency Model Checker)

**Deferred.** loom exhaustively checks all possible thread interleavings for data races and deadlocks. It would be useful for the FUSE↔tokio deadlock issue (5.1) and RwLock poison issue (2.9). However, loom requires rewriting code to use `loom::sync` primitives and is very slow. Not practical for initial resilience coverage. Consider for Phase E hardening.

### 6.4 In-Process Failure Injection Without Failpoints

**Rejected.** Alternative: inject failures via trait-method overrides or runtime flags instead of the `fail` crate. This avoids a new dependency but requires modifying every function signature to accept an error injection parameter. Failpoints are cleaner — they're invisible in production (compiled out) and don't pollute the API surface.

### 6.5 Mock Framework (mockall)

**Rejected for now.** The codebase uses real implementations with TempDir isolation — this pattern is well-established across 43 test files. Introducing `mockall` would split the test codebase into two incompatible patterns. `FaultyOrigin` wrapper achieves the same result while staying consistent with existing patterns.

---

## 7. Implementation Plan

### Phase 1: Test Infrastructure (Days 1-2.5)

| Task | Effort | Deliverable |
|------|--------|-------------|
| Create `musicfs-test-utils` crate | 1 day | `FaultyOrigin`, `FaultyCasStore`, centralized fixtures |
| Add `fail` crate, instrument 10 failpoints | 1 day | Failpoint macros in store.rs, reader.rs, delta.rs, health.rs, db.rs |
| Setup test directory structure | 0.5 day | `tests/resilience/`, `tests/failpoints/`, `tests/integration/` |

### Phase 2: Layer 1 Tests (Days 3-5.5)

| Test Group | Tests | Effort | Can Write Now? |
|------------|-------|--------|----------------|
| Cache corruption (SQLite, sled, tantivy, CAS) | 4 | 0.5 day | ✅ Yes |
| RwLock poison recovery | 2 | 0.25 day | ✅ Yes |
| Health check timeout + parallel checks | 2 | 0.25 day | ✅ Yes |
| tantivy crash recovery | 2 | 0.25 day | ✅ Yes |
| fd exhaustion | 1 | 0.25 day | ✅ Yes |
| Disk space / ENOSPC | 2 | 0.25 day | ✅ Yes |
| Origin failover (FaultyOrigin) | 3 | 0.5 day | ✅ Yes |
| Panic hook + task supervisor | 3 | 0.5 day | ❌ Needs implementation |
| Shutdown orchestration | 3 | 0.5 day | ❌ Needs implementation |
| sd_notify mock socket | 1 | 0.25 day | ❌ Needs implementation |
| Passthrough mode | 1 | 0.25 day | ❌ Needs implementation |
| Systemd service file assertions | 1 | 0.1 day | ✅ Yes |

### Phase 3: Layer 2 Tests (Days 6-7)

| Test | Effort | Requires |
|------|--------|----------|
| SIGTERM triggers clean shutdown | 0.25 day | Signal handler implementation |
| SIGINT triggers clean shutdown | 0.1 day | Signal handler implementation |
| Double-signal forces immediate exit | 0.1 day | Signal handler implementation |
| Kill -9 + stale mount detection | 0.25 day | Stale mount detection implementation |
| 100 concurrent FUSE reads (deadlock) | 0.25 day | FUSE mount in test (Docker or privileged) |

### Phase 4: Layer 3 Tests (Days 8-9)

| Task | Effort | Requires |
|------|--------|----------|
| Docker Compose setup (Toxiproxy + MinIO + SFTP) | 0.5 day | Docker |
| S3 latency spike test | 0.25 day | S3 origin implementation |
| S3 connection drop + failover | 0.25 day | S3 origin implementation |
| SFTP connection drop + failover | 0.25 day | SFTP origin implementation |
| Origin recovery after partition heal | 0.25 day | Docker |

### Rollout

1. **Phase 1 first** — test infrastructure is prerequisite for everything else
2. **Phase 2 "write now" tests** — 11 tests that can be written before resilience implementation; they document expected behavior as executable specs (currently failing)
3. **Phase 2 remaining** — written alongside resilience implementation (test-first development)
4. **Phase 3** — after signal handling and shutdown are implemented
5. **Phase 4** — after S3/SFTP origins are implemented; deferred if origins remain stubs

---

## 8. Coverage Matrix

### 8.1 Issue → Test → Layer Mapping

| Issue | Description | Layer | Test Approach | Write Now? |
|-------|-------------|-------|--------------|------------|
| 2.1 | Signal handling | 2 | Fork daemon + send SIGTERM/SIGINT | ❌ |
| 2.2 | Panic hook | 1 | `catch_unwind` + log capture | ❌ |
| 2.3 | Shutdown orchestration | 1+2 | CancellationToken + ordered teardown | ❌ |
| 2.4 | Cache integrity on startup | 1 | Corrupt file bytes + reopen | ✅ |
| 2.5 | Interrupted sync | 1 | Failpoint `delta-sync-after-batch` | ❌ |
| 2.6 | Task supervisor | 1 | Spawn panicking task + verify restart | ❌ |
| 2.7 | FUSE unmount on crash | 2 | Fork + kill -9 + check /proc/mounts | ❌ |
| 2.8 | Disk space | 1 | Small `max_size` + oversized write | ✅ |
| 2.9 | RwLock poison | 1 | Panic in writer thread + verify read | ✅ |
| 2.10 | sd_notify | 1 | Mock Unix datagram socket | ❌ |
| 3.1 | Watchdog | 1 | Mock sd_notify + verify WATCHDOG=1 | ❌ |
| 3.5 | sled recovery | 1 | Corrupt sled files + reopen | ✅ |
| 3.7 | ExecStop stub | 1 | Assert service file contains fusermount | ✅ |
| 3.8 | FUSE read timeout | 1 | FaultyOrigin with TimeoutMs + verify EIO | ✅ |
| 4.2.1 | Health check timeout | 1 | FaultyOrigin with 30s hang + timer | ✅ |
| 4.2.2 | Parallel health checks | 1 | 3 origins (2 fast, 1 slow) + timer | ✅ |
| 4.2.3 | Offline mode | 1 | All origins fail + verify state machine | ❌ |
| 5.1 | FUSE↔tokio deadlock | 2 | 100 concurrent reads with timeout | ✅ |
| 5.2 | tantivy crash | 1 | Write + `mem::forget` + reopen | ✅ |
| 5.3 | fd exhaustion | 1 | `rlimit` NOFILE=64 + CAS operations | ✅ |
| 5.7 | CAS atomic write | 1 | Failpoint between write and index | ❌ |
| 6.3 | sled dies at runtime | 1 | Corrupt sled + verify EIO not panic | ✅ |
| 6.4 | CAS chunk corruption | 1 | Overwrite chunk file + verify auto-repair | ✅ |
| 6.6 | Passthrough mode | 1 | Read-only cache dir + verify origin read | ❌ |
| Network | Origin failover | 1+3 | FaultyOrigin + Toxiproxy | ✅ (L1) |

### 8.2 Summary

- **Total test cases**: ~35
- **Can write now** (before resilience implementation): 15
- **Need implementation first**: 12
- **Need Docker** (Layer 3 only): 5
- **Need FUSE mount** (Layer 2): 3

---

## 9. Glossary / References

### 9.1 Libraries

| Library | Link | Purpose |
|---------|------|---------|
| `fail` (TiKV failpoints) | [github.com/tikv/fail-rs](https://github.com/tikv/fail-rs) | Conditional fault injection |
| `rlimit` | [docs.rs/rlimit](https://docs.rs/rlimit) | Resource limit manipulation |
| `nix` | [docs.rs/nix](https://docs.rs/nix) | POSIX signal sending |
| `wiremock` | [docs.rs/wiremock](https://docs.rs/wiremock) | HTTP mock server |
| `assert_cmd` | [docs.rs/assert_cmd](https://docs.rs/assert_cmd) | CLI process testing |
| Toxiproxy | [github.com/Shopify/toxiproxy](https://github.com/Shopify/toxiproxy) | Network fault injection proxy |
| `noxious-client` | [docs.rs/noxious-client](https://docs.rs/noxious-client) | Async Toxiproxy Rust client |

### 9.2 References

| Document | Path |
|----------|------|
| Resilience audit | [resilience-fault-tolerance.md](resilience-fault-tolerance.md) |
| Persistent state plan | [persistent-state.md](persistent-state.md) |
| Architecture | [architecture.md](../architecture.md) |
| Requirements | [requirements.md](../requirements.md) |

### 9.3 Glossary

| Term | Definition |
|------|------------|
| **Failpoint** | A conditional injection point in production code, compiled out in release builds |
| **FaultyOrigin** | Test wrapper around `Origin` trait that injects configurable errors |
| **Layer 1** | In-process tests (trait mocks, failpoints) — fastest, no external deps |
| **Layer 2** | Process-level tests (fork, signal, kill) — tests daemon lifecycle |
| **Layer 3** | Network-level tests (Toxiproxy, Docker) — tests real protocol behavior |
| **Passthrough mode** | Operating mode where cache is bypassed; reads go directly to origin |
