# Smart Features API Documentation

## Overview

MusicFS Week 9 introduces three intelligent features:
1. **Smart Collections** - Dynamic playlists based on queries, time ranges, and listening patterns
2. **Artwork Extraction & Caching** - Extract and serve album art in multiple sizes
3. **Predictive Prefetching** - Learn listening patterns to preload likely-next tracks

---

## Smart Collections

### CollectionStore

Manages persistent smart collections using SQLite.

```rust
pub struct CollectionStore {
    db: rusqlite::Connection,
}

pub struct Collection {
    pub id: i64,
    pub name: String,
    pub query: CollectionQuery,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}
```

### CollectionQuery Types

| Query Type | Description | Example |
|------------|-------------|---------|
| `Match(String)` | tantivy search query | `"artist:Metallica"` |
| `DateRange { start, end }` | Files added within range | Last 30 days |
| `RecentlyAdded(days)` | Files added in last N days | `RecentlyAdded(7)` |
| `RecentlyPlayed(days)` | Files played in last N days | `RecentlyPlayed(30)` |
| `MostPlayed(limit)` | Top N most played tracks | `MostPlayed(100)` |
| `Genre(String)` | All tracks matching genre | `"Progressive Rock"` |
| `Compound(Vec)` | AND combination of queries | Multiple conditions |

### API

```rust
impl CollectionStore {
    fn create(&self, name: &str, query: CollectionQuery) -> Result<i64, CollectionError>;
    fn get(&self, id: i64) -> Result<Option<Collection>, CollectionError>;
    fn list(&self) -> Result<Vec<Collection>, CollectionError>;
    fn update(&self, id: i64, name: &str, query: CollectionQuery) -> Result<(), CollectionError>;
    fn delete(&self, id: i64) -> Result<(), CollectionError>;
    fn evaluate(&self, id: i64, index: &SearchIndex, patterns: &PatternStore) -> Result<Vec<FileId>, CollectionError>;
}
```

### FUSE Integration (Planned)

Collections will appear as virtual directories under `/.collections/`:

```bash
$ ls /mnt/musicfs/.collections/
Recent Additions/
Most Played/
80s Metal/

$ ls /mnt/musicfs/.collections/Most\ Played/
001. Track1.flac -> /mnt/musicfs/Artist/Album/Track1.flac
002. Track2.flac -> /mnt/musicfs/Artist/Album/Track2.flac
```

---

## Artwork Extraction & Caching

### ArtworkExtractor

Extracts embedded artwork from audio files.

```rust
pub struct Artwork {
    pub data: Vec<u8>,
    pub mime_type: String,
    pub art_type: ArtType,
    pub width: u32,
    pub height: u32,
}

pub enum ArtType {
    Front,
    Back,
    Other,
}

pub enum ArtSize {
    Thumbnail,  // 150x150 max
    Medium,     // 300x300 max
    Full,       // Original size
}
```

### API

```rust
impl ArtworkExtractor {
    fn extract(&self, path: &Path) -> Result<Vec<Artwork>, ArtworkError>;
    fn extract_first(&self, path: &Path) -> Result<Option<Artwork>, ArtworkError>;
    fn resize(data: &[u8], size: ArtSize) -> Result<Vec<u8>, ArtworkError>;
}
```

### ArtworkCache

Caches artwork in CAS (Content-Addressable Storage).

```rust
impl ArtworkCache {
    async fn store(&self, file_id: i64, artwork: &Artwork) -> Result<ChunkHash, ArtworkError>;
    async fn get(&self, file_id: i64, art_type: &str, size: ArtSize) -> Result<Option<Vec<u8>>, ArtworkError>;
    async fn has(&self, file_id: i64, art_type: &str) -> Result<bool, ArtworkError>;
}
```

### Size Specifications

| Size | Max Dimension | Use Case |
|------|---------------|----------|
| Thumbnail | 150px | List views, grids |
| Medium | 300px | Detail panels |
| Full | Original | High-res display |

### Caching Strategy

1. Original artwork stored in CAS with content hash
2. SQLite maps `(file_id, art_type)` → `chunk_hash`
3. Resizing performed on-demand, not cached (saves storage)
4. Max input size: 10MB (reject larger images)

---

## Predictive Prefetching

### Access Patterns (PatternStore)

Tracks file access history to predict next tracks.

```rust
pub struct AccessPattern {
    pub file_id: FileId,
    pub timestamp: SystemTime,
    pub context: AccessContext,
    pub hour_of_day: u8,
}

pub struct AccessContext {
    pub album_id: Option<i64>,
    pub track_number: Option<u32>,
    pub artist: Option<String>,
}
```

### Pattern Learning

| Pattern Type | Description | Use Case |
|--------------|-------------|----------|
| Sequential | A → B → C transitions | Album playback |
| Time-based | Hour-of-day preferences | Morning playlist |
| Frequency | Most played tracks | Popular content |

### API

```rust
impl PatternStore {
    fn record(&self, file_id: FileId, context: AccessContext) -> Result<(), PatternError>;
    fn predict_next(&self, current: FileId, limit: usize) -> Vec<FileId>;
    fn predict_for_time(&self, hour: u8, limit: usize) -> Vec<FileId>;
    fn recently_played(&self, days: u32) -> Result<Vec<FileId>, PatternError>;
    fn most_played(&self, limit: u32) -> Result<Vec<FileId>, PatternError>;
}
```

### PrefetchEngine

Background engine that listens for file access events and prefetches predicted content.

```rust
pub struct PrefetchConfig {
    pub lookahead: usize,      // How many tracks to prefetch (default: 3)
    pub max_concurrent: usize, // Concurrent prefetch limit (default: 2)
    pub cooldown: Duration,    // Delay between prefetch bursts (default: 100ms)
    pub enabled: bool,         // Master switch
}
```

### Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   EventBus      │────▶│ PrefetchEngine  │────▶│ ContentFetcher  │
│ (FileAccessed)  │     │  (predictions)  │     │  (CAS storage)  │
└─────────────────┘     └─────────────────┘     └─────────────────┘
                               │
                               ▼
                        ┌─────────────────┐
                        │  PatternStore   │
                        │  (SQLite DB)    │
                        └─────────────────┘
```

### FUSE Interface

Virtual directory `/.prefetch/` exposes prefetch status and hints:

```bash
$ cat /mnt/musicfs/.prefetch/status
MusicFS Prefetch Status
=======================
running: true
in_flight: 2
most_played: [42, 57, 103, 89, 12]

$ ls /mnt/musicfs/.prefetch/
status
hint_0042
hint_0057
hint_0103

$ cat /mnt/musicfs/.prefetch/hint_0042
57
103
89
```

---

## Performance Targets

| Feature | Metric | Target |
|---------|--------|--------|
| Collection evaluation | Latency | <100ms for 100k files |
| Artwork extraction | Throughput | >10 files/sec |
| Artwork resize | Latency | <50ms per image |
| Pattern prediction | Latency | <10ms |
| Prefetch hit rate | Accuracy | >70% for sequential play |

---

## Error Handling

### CollectionError

| Error | Description |
|-------|-------------|
| `Database(rusqlite::Error)` | SQLite operation failed |
| `NotFound` | Collection ID doesn't exist |
| `InvalidQuery` | Query failed to serialize |
| `Search(SearchError)` | tantivy query failed |
| `Pattern(PatternError)` | Pattern lookup failed |

### ArtworkError

| Error | Description |
|-------|-------------|
| `Database(rusqlite::Error)` | Cache DB operation failed |
| `Cas(CasError)` | CAS storage operation failed |
| `InvalidHash` | Stored hash is malformed |
| `NotFound` | Artwork not in cache |
| `ImageTooLarge(usize)` | Input exceeds 10MB limit |
| `InvalidImage` | Cannot decode image data |
| `ResizeFailed` | Image resize operation failed |

### PatternError

| Error | Description |
|-------|-------------|
| `Database(rusqlite::Error)` | SQLite operation failed |

---

## Configuration

### Default Settings

```toml
[prefetch]
enabled = true
lookahead = 3
max_concurrent = 2
cooldown_ms = 100

[artwork]
max_input_size_mb = 10
thumbnail_size = 150
medium_size = 300

[patterns]
max_history_days = 30
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_collection_crud` | Unit | Create, read, update, delete |
| `test_collection_evaluate_match` | Unit | Match query evaluation |
| `test_collection_persistence` | Unit | Collections survive restart |
| `test_artwork_extract_flac` | Unit | FLAC artwork extraction |
| `test_artwork_cache_store_get` | Unit | Cache round-trip |
| `test_artwork_resize` | Unit | Resize produces valid output |
| `test_pattern_prediction` | Unit | Sequential pattern learning |
| `test_pattern_persistence` | Unit | Patterns survive restart |
| `test_prefetch_config_defaults` | Unit | Default config values |
| `test_prefetch_ops_*` | Unit | FUSE PrefetchOps integration |
