# Persistent State: Implementation Plan

**Authors:** AI-assisted  
**Status:** Draft  
**Last Updated:** 2026-05-13  
**Reviewers:** TBD  
**Approvers:** TBD  
**Prerequisites:** [persistent-state.md](persistent-state.md) (research), [phase-a-stop-dying.md](phase-a-stop-dying.md) (signal handling + shutdown)  
**Estimated Effort:** ~8 days

---

[TOC]

---

## 1. Abstract

Wire up the existing SQLite persistence layer into the mount path so that subsequent mounts load from database instead of rescanning origins. This transforms mount time from O(N × origin_latency) to O(N × SQLite_read) — roughly 1000x faster for remote origins.

**Storage decision: SQLite (Option A).** Rationale:
- `Database` struct with full CRUD already exists in `musicfs-cache/src/db.rs`
- Schema with `chunk_manifest BLOB` column already exists in `schema.sql`
- `ChunkManifest::from_db()` and `chunks_to_bytes()` already exist but are never called
- Row-to-`FileMeta` mapping already exists in `get_file_by_virtual_path()`
- WAL mode crash safety already configured
- 2-4 second bulk load for 1M rows is acceptable (target is <5s, not <500ms — the <500ms target is for the mount syscall itself, which returns immediately with lazy tree loading)

No new storage engine. No new dependencies. Wire existing code.

---

## 2. Background

### 2.1 Current State

`run_mount()` in `main.rs`:
1. Opens CAS store ✅
2. Creates origin connection ✅
3. `scan_music_files()` — walks entire origin, parses every file with symphonia ❌ **BOTTLENECK**
4. Builds VirtualTree from scan results (in-memory only) ❌ **LOST ON RESTART**
5. Registers every file in ContentFetcher (in-memory only) ❌ **LOST ON RESTART**
6. Mounts FUSE ✅

### 2.2 What Exists But Is Not Wired

| Component | Exists | Wired Into Mount? |
|-----------|--------|--------------------|
| `Database::open()` + schema + WAL | ✅ | ❌ |
| `Database::upsert_file()` | ✅ | ❌ |
| `Database::get_file_by_virtual_path()` (returns `FileMeta`) | ✅ | ❌ |
| `schema.sql` with `chunk_manifest BLOB` column | ✅ | ❌ |
| `ChunkManifest::chunks_to_bytes()` (serialize) | ✅ | ❌ |
| `ChunkManifest::from_db()` (deserialize) | ✅ | ❌ |
| `TreeBuilder::add_file(&FileMeta)` | ✅ | ✅ (from scan, not from DB) |
| `ContentFetcher::register_file(FileMeta)` | ✅ | ✅ (from scan, not from DB) |
| `PatternStore::new(db_path)` (loads from SQLite on open) | ✅ | ❌ |
| `CollectionStore::new(db_path)` | ✅ | ❌ |
| `SearchIndex::open(path)` (opens tantivy from disk) | ✅ | ❌ |

### 2.3 What's Missing

| Component | Needs Building |
|-----------|----------------|
| `Database::list_all_files()` → `Vec<FileMeta>` | New method (SQL exists, just needs `SELECT *`) |
| `Database::update_manifest(FileId, &[u8])` | New method (column exists) |
| `Database::get_manifest(FileId)` → `Option<Vec<u8>>` | New method |
| `Database::list_all_manifests()` → `Vec<(FileId, ChunkManifest)>` | New method |
| Background delta sync task | New (compare DB state vs origin) |
| First-mount detection | New (check `file_count() > 0`) |

---

## 3. Goals & Non-Goals

### 3.1 Goals

- Subsequent mount loads tree from SQLite, not origin scan
- Chunk manifests persist to SQLite, loaded on mount (no re-download)
- tantivy index, PatternStore, CollectionStore opened on mount
- Background delta sync reconciles DB vs origin after mount
- First mount (empty DB) falls back to current full-scan behavior
- Mount time for 10K files: <1 second (subsequent mount)
- All existing tests pass, no regressions

### 3.2 Non-Goals

- Achieving <500ms mount for 1M+ files (requires lazy tree loading — future work)
- LRU eviction persistence (separate task, low urgency)
- Changing the storage engine (SQLite is the decision)
- Config file parsing changes (origin config stays in TOML, not DB)
- Schema migrations for existing data (fresh DB on first mount)

---

## 4. Proposed Design

### 4.1 Implementation Order

```
4.2  Database: list_all_files() + manifest CRUD     (foundation)
 ↓
4.3  Mount path: load tree + fetcher from DB         (core change)
 ↓
4.4  Persist manifests after fetch                   (write path)
 ↓
4.5  Open tantivy + PatternStore + CollectionStore   (quick wiring)
 ↓
4.6  Background delta sync                           (post-mount reconciliation)
 ↓
4.7  First-mount detection + fallback                (edge case)
 ↓
4.8  Shutdown: WAL checkpoint + flush                (cleanup)
```

### 4.2 Database: New Methods

**File**: `musicfs-cache/src/db.rs`

#### list_all_files()

Bulk load all files from DB. Reuses the existing row-to-FileMeta mapping from `get_file_by_virtual_path()`.

```rust
pub fn list_all_files(&self) -> Result<Vec<FileMeta>> {
    let conn = self.conn.lock().unwrap();

    let mut stmt = conn.prepare(
        r#"SELECT id, origin_id, real_path, virtual_path,
                  title, artist, album, album_artist, genre,
                  year, track, disc,
                  duration_ms, bitrate, sample_rate, format,
                  origin_mtime, origin_size, content_hash
           FROM files
           ORDER BY virtual_path"#
    ).map_err(|e| Error::Database(format!("prepare failed: {}", e)))?;

    let files = stmt.query_map([], |row| {
        // Same mapping as get_file_by_virtual_path
        Ok(Self::row_to_file_meta(row))
    })
    .map_err(|e| Error::Database(format!("query failed: {}", e)))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(files)
}
```

Extract the row mapping into a shared `row_to_file_meta(row)` helper to avoid duplication with `get_file_by_virtual_path()`.

#### Manifest CRUD

```rust
pub fn update_manifest(&self, file_id: FileId, manifest_blob: &[u8]) -> Result<()> {
    let conn = self.conn.lock().unwrap();
    conn.execute(
        "UPDATE files SET chunk_manifest = ?1 WHERE id = ?2",
        params![manifest_blob, file_id.0],
    ).map_err(|e| Error::Database(format!("update manifest failed: {}", e)))?;
    Ok(())
}

pub fn get_manifest(&self, file_id: FileId) -> Result<Option<Vec<u8>>> {
    let conn = self.conn.lock().unwrap();
    conn.query_row(
        "SELECT chunk_manifest FROM files WHERE id = ?1",
        params![file_id.0],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| Error::Database(format!("get manifest failed: {}", e)))
}

pub fn list_all_manifests(&self) -> Result<Vec<(FileId, u64, i64, Vec<u8>)>> {
    let conn = self.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, origin_size, origin_mtime, chunk_manifest FROM files WHERE chunk_manifest IS NOT NULL"
    ).map_err(|e| Error::Database(format!("prepare failed: {}", e)))?;

    let manifests = stmt.query_map([], |row| {
        Ok((
            FileId(row.get(0)?),
            row.get::<_, i64>(1)? as u64,
            row.get::<_, i64>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })
    .map_err(|e| Error::Database(format!("query failed: {}", e)))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(manifests)
}
```

#### WAL Checkpoint

```rust
pub fn checkpoint(&self) -> Result<()> {
    let conn = self.conn.lock().unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .map_err(|e| Error::Database(format!("WAL checkpoint failed: {}", e)))?;
    info!("SQLite WAL checkpoint completed");
    Ok(())
}
```

#### Tests

```rust
#[test]
fn test_list_all_files() {
    let db = Database::open_memory().unwrap();
    // Insert 3 files
    // list_all_files() returns 3
    // Verify FileMeta fields match what was inserted
}

#[test]
fn test_manifest_roundtrip() {
    let db = Database::open_memory().unwrap();
    // Insert file, update_manifest with blob, get_manifest returns same blob
}

#[test]
fn test_list_all_manifests_skips_null() {
    let db = Database::open_memory().unwrap();
    // Insert 3 files, only 1 with manifest
    // list_all_manifests() returns 1
}
```

---

### 4.3 Mount Path: Load From DB

**File**: `musicfs-cli/src/main.rs` — rewrite `run_mount()`

The key change: replace `scan_music_files()` with DB load when data exists.

```rust
fn run_mount(mountpoint: PathBuf, origin_path: Option<PathBuf>, cache_dir: Option<PathBuf>) -> Result<()> {
    let origin_path = origin_path.context("--origin is required")?;
    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.handle().clone();

    let (tree, reader, db) = runtime.block_on(async {
        let cache_dir = resolve_cache_dir(cache_dir);
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&mountpoint)?;

        // Open CAS store
        let store = Arc::new(CasStore::open(CasConfig {
            chunks_dir: cache_dir.join("chunks"),
            ..Default::default()
        }).await?);

        // Open database
        let db_path = cache_dir.join("metadata.db");
        let db = Arc::new(Database::open_with_integrity_check(&db_path)
            .or_else(|_| Database::open(&db_path))?);  // Fallback to normal open if integrity check fails

        let fetcher = Arc::new(ContentFetcher::new(store.clone()));
        let origin_id = OriginId::from("local");
        let origin = Arc::new(LocalOrigin::new(origin_id.clone(), origin_path.clone()));
        fetcher.register_origin(origin);

        // Decide: load from DB or full scan
        let file_count = db.file_count().unwrap_or(0);

        let files = if file_count > 0 {
            // SUBSEQUENT MOUNT — load from DB
            info!(file_count, "Loading metadata from database");
            let start = Instant::now();
            let files = db.list_all_files()?;
            info!(elapsed_ms = start.elapsed().as_millis() as u64, "Database load complete");
            files
        } else {
            // FIRST MOUNT — full origin scan
            info!("First mount: scanning origin");
            let files = scan_music_files(&origin_path, &origin_id).await?;
            info!(file_count = files.len(), "Scan complete, persisting to database");

            // Persist to DB for next mount
            for file in &files {
                if let Some(ref audio) = file.audio {
                    db.upsert_file(
                        &file.real_path.origin_id,
                        &file.real_path.path,
                        &file.virtual_path,
                        audio,
                        file.mtime,
                        file.size,
                    )?;
                }
            }
            info!("Metadata persisted to database");
            files
        };

        // Build tree + register files (same as before, but from DB or scan)
        let mut builder = TreeBuilder::new();
        for file in &files {
            builder.add_file(file);
            fetcher.register_file(file.clone());
        }
        let tree = Arc::new(RwLock::new(builder.build()));

        // Load manifests from DB
        let reader = Arc::new(FileReader::with_fetcher(store, fetcher));
        let manifest_count = load_manifests_from_db(&db, &reader)?;
        if manifest_count > 0 {
            info!(manifest_count, "Loaded chunk manifests from database");
        }

        Ok::<_, anyhow::Error>((tree, reader, db))
    })?;

    // Open search index
    let search_dir = cache_dir.join("search.idx");
    let _search_index = SearchIndex::open_with_recovery(&search_dir)
        .context("Failed to open search index")?;

    // Open pattern store
    let patterns_path = cache_dir.join("patterns.db");
    let _pattern_store = PatternStore::new(&patterns_path, 30)
        .context("Failed to open pattern store")?;

    // ... mount, signal handler, shutdown (same as current) ...

    // On shutdown: checkpoint WAL
    db.checkpoint().unwrap_or_else(|e| warn!("WAL checkpoint failed: {}", e));
}
```

Helper function:

```rust
fn load_manifests_from_db(db: &Database, reader: &FileReader) -> Result<usize> {
    let manifests = db.list_all_manifests()?;
    let mut count = 0;
    for (file_id, total_size, mtime, blob) in manifests {
        if let Some(manifest) = ChunkManifest::from_db(file_id, total_size, mtime, &blob) {
            reader.register_manifest(manifest);
            count += 1;
        }
    }
    Ok(count)
}
```

---

### 4.4 Persist Manifests After Fetch

**File**: `musicfs-cas/src/fetcher.rs`

After `fetch_file()` downloads and chunks a file, persist the manifest to SQLite.

The fetcher currently doesn't have access to the Database. Two options:
1. Pass `Arc<Database>` to ContentFetcher (adds dependency musicfs-cas → musicfs-cache)
2. Emit an event with the manifest, have the caller persist it

**Approach**: Option 2 — use the existing EventBus. Add a new event variant:

**File**: `musicfs-core/src/events.rs`

```rust
pub enum Event {
    // ... existing variants
    ManifestCached {
        file_id: FileId,
        manifest_blob: Vec<u8>,
    },
}
```

**File**: `musicfs-cas/src/fetcher.rs` — emit event after fetch:

```rust
pub async fn fetch_file(&self, file_id: FileId) -> Result<ChunkManifest, FetchError> {
    // ... existing fetch + chunk logic ...

    // Emit manifest for persistence
    if let Some(bus) = &self.event_bus {
        bus.publish(Event::ManifestCached {
            file_id,
            manifest_blob: manifest.chunks_to_bytes(),
        });
    }

    Ok(manifest)
}
```

**File**: `musicfs-cli/src/main.rs` — subscribe to ManifestCached events:

```rust
// Spawn manifest persistence listener
let db_for_manifests = db.clone();
let mut manifest_rx = event_bus.subscribe();
tokio::spawn(async move {
    while let Ok(event) = manifest_rx.recv().await {
        if let Event::ManifestCached { file_id, manifest_blob } = event {
            if let Err(e) = db_for_manifests.update_manifest(file_id, &manifest_blob) {
                warn!(file_id = ?file_id, error = %e, "Failed to persist manifest");
            }
        }
    }
});
```

---

### 4.5 Open tantivy + PatternStore + CollectionStore

These already have `open()` methods that load from disk. Just call them in the mount path.

**File**: `musicfs-cli/src/main.rs`

```rust
// After tree is built, before FUSE mount

// Search index
let search_dir = cache_dir.join("search.idx");
let search_index = Arc::new(
    SearchIndex::open_with_recovery(&search_dir)
        .unwrap_or_else(|e| {
            warn!("Search index failed, creating fresh: {}", e);
            SearchIndex::open(&search_dir).expect("Failed to create search index")
        })
);

// Pattern store (already persists to SQLite, loads sequence_counts on open)
let patterns_path = cache_dir.join("patterns.db");
let pattern_store = Arc::new(
    PatternStore::new(&patterns_path, 30)
        .unwrap_or_else(|e| {
            warn!("Pattern store failed: {}", e);
            PatternStore::new(&patterns_path, 30).expect("Failed to create pattern store")
        })
);

// Collection store
let collections_path = cache_dir.join("collections.db");
let collection_store = Arc::new(
    CollectionStore::new(&collections_path)
        .unwrap_or_else(|e| {
            warn!("Collection store failed: {}", e);
            CollectionStore::new(&collections_path).expect("Failed to create collection store")
        })
);
```

For tantivy: if this is a first mount, index all files after scan:

```rust
if file_count == 0 {
    // First mount — index all files
    info!("First mount: building search index");
    let indexer = Indexer::new(search_index.clone(), event_bus.clone(), /* metadata_lookup */);
    indexer.index_batch(&files)?;
}
```

---

### 4.6 Background Delta Sync

After mount completes, spawn a background task that compares DB state against origin and reconciles differences.

**File**: `musicfs-sync/src/delta.rs` or new `musicfs-cli/src/sync.rs`

```rust
pub async fn background_delta_sync(
    origin: Arc<dyn Origin>,
    origin_id: OriginId,
    db: Arc<Database>,
    tree: Arc<RwLock<VirtualTree>>,
    fetcher: Arc<ContentFetcher>,
    event_bus: Arc<EventBus>,
) -> Result<SyncSummary> {
    info!("Starting background delta sync");
    let start = Instant::now();

    let mut added = 0u64;
    let mut modified = 0u64;
    let mut removed = 0u64;
    let mut unchanged = 0u64;

    // Get all files currently in DB
    let db_files: HashMap<PathBuf, FileMeta> = db.list_all_files()?
        .into_iter()
        .map(|f| (f.real_path.path.clone(), f))
        .collect();

    // Walk origin
    let origin_files = scan_origin_recursive(&origin, Path::new("/")).await?;

    // Compare
    for (path, origin_stat) in &origin_files {
        match db_files.get(path) {
            Some(db_file) if db_file.mtime == origin_stat.mtime && db_file.size == origin_stat.size => {
                unchanged += 1;
            }
            Some(db_file) => {
                // Modified — re-parse metadata, update DB, update tree
                modified += 1;
                // ... update logic ...
            }
            None => {
                // New file — parse metadata, add to DB + tree
                added += 1;
                // ... add logic ...
            }
        }
    }

    // Find removed files (in DB but not on origin)
    let origin_paths: HashSet<_> = origin_files.keys().collect();
    for (path, db_file) in &db_files {
        if !origin_paths.contains(path) {
            removed += 1;
            db.delete_file(db_file.id)?;
            tree.write().remove_file(&db_file.virtual_path);
        }
    }

    let elapsed = start.elapsed();
    info!(
        added, modified, removed, unchanged,
        elapsed_ms = elapsed.as_millis() as u64,
        "Delta sync complete"
    );

    Ok(SyncSummary { added, modified, removed, unchanged })
}
```

Spawn in `run_mount()` after FUSE mount:

```rust
// Background delta sync (non-blocking)
let sync_db = db.clone();
let sync_tree = tree.clone();
let sync_fetcher = fetcher.clone();
let sync_origin = origin.clone();
let sync_origin_id = origin_id.clone();
let sync_bus = event_bus.clone();
tokio::spawn(async move {
    if let Err(e) = background_delta_sync(
        sync_origin, sync_origin_id, sync_db, sync_tree, sync_fetcher, sync_bus,
    ).await {
        warn!("Delta sync failed: {}", e);
    }
});
```

---

### 4.7 First-Mount Detection

Simple: check `db.file_count()`:

```rust
let file_count = db.file_count().unwrap_or(0);

if file_count > 0 {
    // Load from DB
} else {
    // Full scan + persist
}
```

This is already shown in Section 4.3. No separate implementation step.

---

### 4.8 Shutdown: WAL Checkpoint + Flush

**File**: `musicfs-cli/src/main.rs` — in the shutdown sequence (after signal, before dropping session):

```rust
info!("Beginning ordered shutdown");
shutdown_token.cancel();
tokio::time::sleep(Duration::from_millis(500)).await;

// Flush persistence
if let Err(e) = db.checkpoint() {
    warn!("SQLite WAL checkpoint failed: {}", e);
}
info!("Background tasks stopped, state flushed");
```

---

## 5. Cross-Cutting Concerns

### 5.1 Security & Privacy

- No new attack surface — SQLite file has same permissions as cache directory
- Metadata in DB is the same as what's already in the FUSE virtual tree (not new data)
- `chunk_manifest` BLOB is binary chunk hashes — not sensitive

### 5.2 Observability

- Mount time logged: "Loading metadata from database" with elapsed_ms
- First-mount detected and logged: "First mount: scanning origin"
- Delta sync summary logged: added/modified/removed/unchanged counts + elapsed
- WAL checkpoint logged on shutdown
- Manifest persistence failures logged at WARN (non-fatal)

### 5.3 Scalability

| Library Size | First Mount (scan) | Subsequent Mount (DB load) |
|---|---|---|
| 1K files | ~1-2s | <100ms |
| 10K files | ~10-20s | ~200ms |
| 100K files | ~2-5 min | ~1-2s |
| 1M files | ~20-60 min | ~2-4s |

Delta sync runs in background — mount returns immediately, user sees stale-but-functional data while sync catches up.

### 5.4 Testing

```rust
// Test: subsequent mount loads from DB
#[tokio::test]
async fn test_mount_loads_from_db() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();

    // Insert files
    for i in 0..100 {
        db.upsert_file(/* ... */).unwrap();
    }

    // Load all
    let files = db.list_all_files().unwrap();
    assert_eq!(files.len(), 100);

    // Build tree from DB files (same as mount path)
    let mut builder = TreeBuilder::new();
    for f in &files { builder.add_file(f); }
    let tree = builder.build();
    assert_eq!(tree.file_count(), 100);
}

// Test: manifest roundtrip through DB
#[tokio::test]
async fn test_manifest_persists_and_loads() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();

    let id = db.upsert_file(/* ... */).unwrap();

    let manifest = ChunkManifest { /* ... */ };
    let blob = manifest.chunks_to_bytes();
    db.update_manifest(id, &blob).unwrap();

    let loaded = db.get_manifest(id).unwrap().unwrap();
    let restored = ChunkManifest::from_db(id, 1000, 0, &loaded).unwrap();
    assert_eq!(restored.chunks.len(), manifest.chunks.len());
}

// Test: first mount detects empty DB
#[tokio::test]
async fn test_first_mount_detection() {
    let dir = TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    assert_eq!(db.file_count().unwrap(), 0); // First mount
}

// Test: delta sync detects changes
#[tokio::test]
async fn test_delta_sync_detects_added_file() {
    // DB has files A, B
    // Origin has files A, B, C
    // Delta sync should detect C as added
}

// Test: delta sync detects removed file
#[tokio::test]
async fn test_delta_sync_detects_removed_file() {
    // DB has files A, B, C
    // Origin has files A, B
    // Delta sync should detect C as removed
}

// Test: shutdown checkpoints WAL
#[tokio::test]
async fn test_shutdown_checkpoints_wal() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path).unwrap();
    db.upsert_file(/* ... */).unwrap();

    // WAL file should exist
    let wal_path = db_path.with_extension("db-wal");
    // After checkpoint, WAL should be truncated
    db.checkpoint().unwrap();
}
```

---

## 6. Alternatives Considered

### 6.1 sled for Tree Storage (Option B)

sled is faster for bulk key-value reads (~1-2s for 1M entries vs SQLite's ~2-4s). Rejected because:
- SQLite code already exists (schema, CRUD, row mapping)
- sled would require new serialization layer (bincode/msgpack for FileMeta)
- Two persistence engines is more complex
- SQLite's 2-4s is acceptable for the target

### 6.2 Flat File Snapshot (Option C)

Fastest possible bulk load (<1s via mmap). Rejected because:
- No incremental updates — every change rewrites the entire file
- At 1M files (~500MB), delta sync triggers a 500MB write for each changed file
- No concurrent access safety
- No crash recovery for partial writes

### 6.3 Lazy Tree Loading

Instead of loading all files into memory on mount, load only the root directories and fetch deeper levels on demand from SQLite. This would achieve true O(1) mount. Deferred because:
- Requires significant refactoring of VirtualTree (currently all-in-memory)
- SQLite 2-4s load is good enough for production
- Can be added later as optimization without changing the persistence layer

### 6.4 Separate Manifest Store

Instead of storing manifests in the `files.chunk_manifest` column, use a separate sled tree or SQLite table. Rejected because the column already exists and the schema already supports it.

---

## 7. Implementation Plan

### 7.1 Task Sequence

| Day | Task | Deliverable |
|-----|------|-------------|
| 1 | Database methods: `list_all_files()`, `update_manifest()`, `get_manifest()`, `list_all_manifests()`, `checkpoint()`. Extract `row_to_file_meta()` helper. | New DB methods + tests |
| 2 | Rewrite `run_mount()`: DB load path vs scan path. First-mount detection. | Core mount change |
| 3 | Persist manifests: `ManifestCached` event + listener in main.rs. Load manifests on mount via `load_manifests_from_db()`. | Manifest persistence |
| 4 | Wire tantivy + PatternStore + CollectionStore into mount path. First-mount indexing. | Search/patterns on mount |
| 5 | Background delta sync: compare DB vs origin, update differences. | Delta sync task |
| 6 | Shutdown: WAL checkpoint. Upsert files to DB during first-mount scan. | Clean shutdown |
| 7 | Integration testing: full mount→read→restart→mount cycle. Verify tree + manifests survive restart. | E2E validation |
| 8 | Buffer for issues found during integration. | — |

### 7.2 Verification Checklist

- [ ] `cargo check` — zero errors
- [ ] `cargo test --workspace --exclude musicfs-grpc` — all pass
- [ ] Manual test: first mount (empty cache dir) — scans origin, creates DB
- [ ] Manual test: second mount (DB exists) — loads from DB, no origin scan
- [ ] Manual test: add file to origin, restart — delta sync discovers it
- [ ] Manual test: `kill -9` daemon, restart — DB loads, manifests intact
- [ ] Mount time for 10K test files: <1 second on subsequent mount
- [ ] `ls -la ~/.cache/musicfs/metadata.db` exists after first mount

---

## 8. Files Changed

| File | Change |
|------|--------|
| `musicfs-cache/src/db.rs` | `list_all_files()`, `update_manifest()`, `get_manifest()`, `list_all_manifests()`, `checkpoint()`, `row_to_file_meta()` refactor |
| `musicfs-core/src/events.rs` | Add `ManifestCached` event variant |
| `musicfs-cli/src/main.rs` | Rewrite `run_mount()`: DB load vs scan, open tantivy/patterns/collections, manifest listener, delta sync spawn, shutdown checkpoint |
| `musicfs-cli/Cargo.toml` | Add `musicfs-search`, `musicfs-cache` dependencies (for PatternStore, CollectionStore, SearchIndex) |
| `musicfs-cas/src/fetcher.rs` | Emit `ManifestCached` event after `fetch_file()` |
| `musicfs-sync/src/delta.rs` | New `background_delta_sync()` function (or new file) |
| `musicfs-test-utils/tests/resilience.rs` | New tests: mount-from-DB, manifest roundtrip, delta sync, first-mount detection |

---

## 9. Glossary / References

| Term | Definition |
|------|------------|
| **First mount** | Initial mount with empty database — triggers full origin scan |
| **Subsequent mount** | Mount with existing database — loads from SQLite |
| **Delta sync** | Background task that compares DB state against origin after mount |
| **Stale data window** | Time between mount and delta sync completion when data may be outdated |
| **WAL checkpoint** | SQLite operation that flushes write-ahead log to main database file |

| Document | Path |
|----------|------|
| Persistent state research | [persistent-state.md](persistent-state.md) |
| Phase A (signals, shutdown) | [phase-a-stop-dying.md](phase-a-stop-dying.md) |
| Phase B (crash recovery) | [phase-b-crash-recovery.md](phase-b-crash-recovery.md) |
| Architecture | [architecture.md](../architecture.md) |
