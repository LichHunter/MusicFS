# MusicFS Persistent State Plan

**Date**: 2026-05-13
**Status**: Research Complete — Design Decision Needed
**Prerequisites**: [architecture.md](../architecture.md), [resilience-fault-tolerance.md](resilience-fault-tolerance.md)
**Related Requirements**: G1 (O(1) mount time), NFR-1.7 (<500ms mount), FR-7.1 (cache persists across restarts)

---

## 1. Problem Statement

Every mount is a full cold start. The `run_mount()` function in `main.rs` does not use any persistent storage — it walks the entire origin filesystem, parses metadata from every audio file, and builds all runtime state from scratch.

The architecture designed persistence infrastructure (SQLite schema, `chunk_manifest` column, `ChunkManifest::from_db()`, `chunks_to_bytes()`) but **none of it is wired into the mount path**. The mount flow doesn't even open the database.

### Mount Time by Library Size (Current)

| Library Size | Estimated Mount Time | Target (NFR-1.7) |
|---|---|---|
| 1K files | ~1-2s | <500ms |
| 10K files | ~10-20s | <500ms |
| 100K files | ~2-5 minutes | <500ms |
| 1M files | ~20-60 minutes | <500ms |
| 10M files (stretch) | hours | <500ms |

---

## 2. In-Memory State Inventory

### 2.1 State That Must Survive Restart

These are the large, expensive-to-rebuild data structures. Losing them forces a full origin rescan.

#### VirtualTree (~300-400MB at 1M files)

**Location**: `musicfs-cache/src/tree.rs`

**Contents**:
- `nodes: HashMap<Inode, VirtualNode>` — every directory and file node
- `path_to_inode: HashMap<VirtualPath, Inode>` — reverse path lookup
- `next_inode: AtomicU64` — inode counter

**Currently rebuilt from**: Full recursive origin scan + metadata parse of every audio file. This is the single most expensive operation on mount — it touches every file on origin, runs symphonia metadata extraction, and builds the entire tree structure.

**What's needed**: Load from persistent storage on mount. Rebuild only on first-ever mount or if storage is corrupt.

---

#### ContentFetcher.file_meta (~200MB at 1M files)

**Location**: `musicfs-cas/src/fetcher.rs`

**Contents**:
- `file_meta: RwLock<HashMap<FileId, FileMeta>>` — full metadata for every file
- Each `FileMeta` contains: id, virtual_path, real_path (origin_id + path), size, mtime, content_hash, audio metadata

**Currently rebuilt from**: Same origin scan that builds the tree. Every file is registered via `fetcher.register_file(meta)`.

**What's needed**: This is essentially a duplicate of the tree data in a different shape. If the tree is loaded from storage, this map should be populated from the same source.

---

#### FileReader.manifests (~100MB at 1M files)

**Location**: `musicfs-cas/src/reader.rs`

**Contents**:
- `manifests: RwLock<HashMap<FileId, ChunkManifest>>` — maps FileId to list of chunk hashes + offsets
- Each `ChunkManifest` contains: file_id, total_size, mtime, chunks (Vec<ChunkRef> with hash + offset + size)

**Currently rebuilt from**: Re-fetched from origin on first `read()` after restart. The fetcher downloads the entire file, chunks it via CDC, stores chunks in CAS (dedup catches existing ones), and builds the manifest. This means every file is re-downloaded once after restart even though the chunks are already on disk.

**What's needed**: Persist manifests to storage after fetch. Load on mount. This is the difference between "restart = re-download everything" and "restart = instant reads from cache."

**Existing dead code**: SQLite `files` table has `chunk_manifest BLOB` column. `ChunkManifest::chunks_to_bytes()` and `ChunkManifest::from_db()` exist but are never called.

---

#### LruEviction access times (~50MB at 100K chunks)

**Location**: `musicfs-cache/src/eviction.rs`

**Contents**:
- `access_times: RwLock<BTreeMap<Instant, ChunkHash>>` — ordered by access time
- `hash_to_time: RwLock<HashMap<ChunkHash, Instant>>` — reverse lookup

**Currently rebuilt from**: Nothing. After restart, all chunks have equal eviction priority. The album you're currently listening to is just as likely to be evicted as something you played 6 months ago.

**What's needed**: Persist last-access timestamps. On mount, load and reconstruct the LRU order so hot data stays cached.

---

### 2.2 State That Survives But Is Ignored on Mount

These persist on disk but `run_mount()` never opens them.

| Component | Persisted To | Loaded on Mount? | Effect |
|---|---|---|---|
| SQLite metadata (files table) | `metadata.db` | ❌ | All metadata re-scanned from origin |
| tantivy search index | `search.idx/` | ❌ | Index rebuilt from scratch (or not at all) |
| PatternStore (access patterns) | SQLite (separate DB) | ❌ | Predictions reset to zero |
| CollectionStore (smart collections) | SQLite (same as patterns) | ❌ | Collections unavailable until opened |

### 2.3 State That Correctly Does Not Need Persistence

| Component | Why Transient Is Fine |
|---|---|
| OriginRegistry (origin connections) | Reconstructed from config on startup |
| Router (priorities, latency stats) | Priorities from config; latency stats warm up within seconds |
| HealthMonitor (health state) | All origins start as Unknown, converge within one check cycle (~30s) |
| EventBus (in-flight events) | Transient by nature |
| PrefetchEngine.in_flight | Transient work queue |
| PluginManager | Re-loaded from config + plugin directories |
| MusicFs.query_inodes | Transient search session state |
| CasStore.current_size | Recalculated on open (though currently broken — see resilience doc 3.10) |

---

## 3. Storage Decision

### 3.1 Requirements for Persistent State

1. **Bulk sequential read on mount** — load ~1M records into in-memory structures as fast as possible
2. **Incremental updates at runtime** — delta sync adds/removes/modifies individual files
3. **Crash safety** — no corruption on unclean shutdown (SIGKILL, power loss)
4. **Manifest storage** — binary blobs (msgpack-encoded chunk lists), variable size (100 bytes to 10KB per file)
5. **LRU timestamps** — simple key-value (ChunkHash → last_access_timestamp)
6. **Already in project** — minimize new dependencies

### 3.2 Options

#### Option A: SQLite (Current Architecture Choice)

**Already in project**: `rusqlite` dependency, `schema.sql` with `files` table, `Database` struct with full CRUD, `chunk_manifest BLOB` column ready.

| Metric | Performance |
|---|---|
| Bulk load 1M rows | ~2-4 seconds (WAL mode, indexed) |
| Single row upsert | ~50μs |
| Crash safety | WAL mode — excellent |
| Manifest blobs | Native BLOB support, no size limit |

**Pros**: Already built (schema, code, tests exist). Well-understood crash semantics. Single file backup. SQL queries for debugging. The `chunk_manifest` column and `from_db()`/`to_bytes()` methods are already written.

**Cons**: Not the fastest for pure key-value workloads. WAL checkpoint can cause brief write pauses. Single-writer limitation (Mutex around connection).

**Effort to wire up**: ~5-7 days (mostly connecting existing code, not writing new code)

---

#### Option B: sled (Already in Project for CAS Index)

**Already in project**: Used for CAS chunk hash → location mapping.

| Metric | Performance |
|---|---|
| Bulk load 1M entries | ~1-2 seconds (LSM, sequential reads) |
| Single entry upsert | ~10-20μs |
| Crash safety | Built-in WAL — good |
| Manifest blobs | Native byte value support |

**Pros**: Faster than SQLite for pure key-value. Already a dependency. Good for LRU timestamps (simple k/v).

**Cons**: No SQL — querying for debugging is harder. No schema migration story. Limited tooling. Has known issues with large datasets (memory usage during compaction). Two persistence engines = two things to maintain.

**Effort**: ~7-9 days (new serialization layer, no existing code to reuse)

---

#### Option C: Flat File (bincode/msgpack dump)

| Metric | Performance |
|---|---|
| Bulk load 1M entries | <1 second (mmap, zero-parse with bincode) |
| Single entry upsert | N/A — full rewrite required |
| Crash safety | Must write atomically (tmp + rename) |
| Manifest blobs | Part of serialized struct |

**Pros**: Fastest possible bulk load. Simplest implementation.

**Cons**: No incremental updates — every change requires serializing and rewriting the entire file. At 1M files (~500MB serialized), a single file modification triggers a 500MB write. No concurrent access. No recovery from partial corruption.

**Effort**: ~3-4 days but creates ongoing maintenance burden for delta updates

---

#### Option D: Hybrid (SQLite for metadata + sled for hot-path data)

Use SQLite for structured metadata (files, collections, patterns — already built) and sled for hot-path key-value data (manifests, LRU timestamps — performance-critical).

**Pros**: Each store optimized for its access pattern. SQLite for queryable metadata, sled for fast blob lookup.

**Cons**: Two persistence engines to coordinate. Consistency between them on crash. More complex startup/shutdown.

---

### 3.3 Recommendation

**Pending your decision.** The tradeoffs are:
- **Simplest path**: Option A (SQLite) — most code already exists, just needs wiring
- **Fastest hot-path**: Option D (Hybrid) — but more complexity
- **Fastest bulk load**: Option C (Flat file) — but no incremental updates

The choice depends on what you value most. SQLite at 1M files loads in ~2-4 seconds — is that acceptable vs the <500ms target? If not, a flat file or sled for the tree data with SQLite for everything else might be needed.

---

## 4. What Needs to Change

Regardless of storage choice, these are the code changes needed:

### 4.1 Mount Path (musicfs-cli/src/main.rs)

Current `run_mount()` flow:
```
1. Open CAS store                           → O(1)
2. Create origin connection                 → O(1)
3. scan_music_files() — FULL ORIGIN WALK    → O(N × origin_latency)  ← BOTTLENECK
4. Build tree from scan results             → O(N)
5. Register files in fetcher                → O(N)
6. Mount FUSE                               → O(1)
```

Required flow:
```
1. Open CAS store                           → O(1)
2. Open persistent state store              → O(1)
3. IF store has data:
     Load tree from store                   → O(N × local_read)  ← ~1000x faster
     Load manifests from store              → O(N × local_read)
     Load LRU access times from store       → O(chunks)
   ELSE (first mount):
     Full origin scan (current behavior)    → O(N × origin_latency)
     Persist results to store               → O(N × local_write)
4. Open tantivy search index                → O(1)
5. Open PatternStore                        → O(1)
6. Create origin connections                → O(1)
7. Mount FUSE                               → O(1)
8. Background: delta sync (origin vs store) → incremental, non-blocking
```

### 4.2 Runtime Persistence (Write Path)

These operations must persist state changes as they happen, not just on shutdown:

| Event | What to Persist | When |
|---|---|---|
| File discovered during sync | FileMeta → store | Immediately (in batch if scanning) |
| File removed during sync | Delete from store | Immediately |
| File metadata changed | Update FileMeta in store | Immediately |
| File content fetched (cache miss) | ChunkManifest → store | After fetch completes |
| Chunk accessed | Update LRU timestamp | Batched (every 10s or 100 accesses) |
| Search index updated | tantivy handles its own persistence | On commit (every 5s) |
| Access pattern recorded | PatternStore handles its own persistence | Already persisted per-access |

### 4.3 Files That Need Changes

| File | Change |
|---|---|
| `musicfs-cli/src/main.rs` | Rewrite `run_mount()` to load from store; add background delta sync |
| `musicfs-cache/src/db.rs` | Add `list_all_files()` bulk load; add manifest read/write methods (if SQLite) |
| `musicfs-cache/src/tree.rs` | Add `TreeBuilder::from_file_metas(iter)` — build tree from stored records |
| `musicfs-cas/src/reader.rs` | Load manifests from store on startup; persist after fetch |
| `musicfs-cas/src/fetcher.rs` | After `fetch_file()`, persist manifest to store |
| `musicfs-cache/src/eviction.rs` | Persist access times; load on startup |
| `musicfs-search/src/indexer.rs` | On mount, check what's already indexed vs what's in store — skip known files |
| `musicfs-sync/src/delta.rs` | Background delta sync: compare store state vs origin, sync differences |

### 4.4 Shutdown Persistence

On graceful shutdown (after signal handling from resilience plan Phase A is implemented):

| Step | What |
|---|---|
| 1 | Flush any batched LRU timestamp updates |
| 2 | Commit tantivy index writer |
| 3 | WAL checkpoint SQLite (if SQLite): `PRAGMA wal_checkpoint(TRUNCATE)` |
| 4 | Flush sled (if sled): `sled::Db::flush()` |
| 5 | Close all database connections |

On crash (no graceful shutdown):
- SQLite WAL mode: automatic recovery on next open (no data loss for committed transactions)
- sled: automatic recovery via internal WAL
- tantivy: up to 5 seconds of uncommitted documents lost, but recoverable from store
- LRU timestamps: batched updates may lose last batch (10s window) — acceptable

---

## 5. Background Delta Sync

After mounting from persistent state, the data may be stale (origin changed while daemon was stopped). A background sync reconciles:

```
1. Walk origin (or use watcher for inotify-capable origins)
2. For each file on origin:
   a. Compare mtime + size against stored record
   b. If unchanged → skip
   c. If modified → re-parse metadata, update store, update tree, invalidate manifest
   d. If new → parse metadata, add to store + tree
3. For each file in store not found on origin:
   a. Remove from store + tree
4. Update search index for changed files
5. Log summary: "Delta sync complete: N added, M modified, K removed, T unchanged"
```

This runs in the background AFTER mount completes. Users see the filesystem immediately (from stored state), and it converges to current reality within minutes.

### 5.1 Stale Data Window

Between mount and delta sync completion, users may see:
- Files that were deleted on origin (will get ENOENT or EIO on read — origin returns not found)
- Files with old metadata (wrong track name, etc.)
- Missing files that were added to origin (won't appear until sync discovers them)

This is acceptable — it's the same behavior as any cached filesystem (NFS, CIFS). The key insight: **stale data for 30 seconds is infinitely better than no data for 5 minutes.**

---

## 6. First Mount vs Subsequent Mount

| | First Mount (empty store) | Subsequent Mount (store has data) |
|---|---|---|
| **Tree source** | Origin scan + metadata parse | Load from store |
| **Manifests** | None (populated on first read) | Loaded from store |
| **Search index** | Built during/after scan | Opened from disk |
| **LRU data** | Empty (cold cache) | Loaded from store |
| **Mount time** | O(N × origin_latency) — same as today | O(N × local_read) — target <5s for 1M files |
| **Accuracy** | 100% current | Stale until delta sync completes |
| **Detection** | Store file doesn't exist or is empty | Store file exists with data |

---

## 7. Estimated Effort

| Task | Effort | Depends On |
|---|---|---|
| Rewrite `run_mount()` with store loading + fallback | 2 days | Storage decision |
| Persist chunk manifests after fetch | 1 day | Storage decision |
| Load manifests on mount + register in FileReader | 0.5 day | Above |
| Open tantivy on mount, skip known files | 1 day | — |
| Open PatternStore + CollectionStore on mount | 0.5 day | — |
| Background delta sync | 1.5 days | — |
| Persist LRU access times + load on mount | 1 day | Storage decision |
| First-mount detection + fallback to full scan | 0.5 day | — |
| **Total** | **~8 days** | |

---

## 8. Open Decision

**Which storage engine for the persistent state?**

The answer drives the implementation of every task above. See Section 3 for tradeoffs.
