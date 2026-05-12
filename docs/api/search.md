# Search API Documentation

## Overview

MusicFS provides two search interfaces:
1. **FUSE Virtual Directory** - `/.search/query/` for file manager integration
2. **gRPC API** - `Search` and `SearchStream` RPCs for programmatic access (planned)

---

## FUSE Search Interface

### Endpoint: `/.search/{query}/`

Browse search results as symlinks in a virtual directory.

### Happy Path

1. User navigates to `/.search/metallica/`
2. FUSE returns directory listing of symlinks
3. Each symlink points to absolute path: `/mnt/music/Metallica/Album/Track.flac`
4. User can open symlink directly in media player

**Example:**
```bash
$ ls -la /mnt/musicfs/.search/metallica/
001. Metallica - Enter Sandman.flac -> /mnt/musicfs/Metallica/Black Album/Enter Sandman.flac
002. Metallica - Battery.flac -> /mnt/musicfs/Metallica/Master of Puppets/Battery.flac
```

### Error Cases

| Scenario | Behavior | FUSE Error |
|----------|----------|------------|
| Empty query | Empty directory | (none) |
| No results | Empty directory | (none) |
| Query too long (>256 chars) | Truncated | (none) |
| Invalid UTF-8 in query | EINVAL | `libc::EINVAL` |
| Index corrupted | ENOENT | `libc::ENOENT` |
| Index writer shutdown | EIO | `libc::EIO` |

### Cache Behavior

- Results cached for 5 minutes (TTL)
- Maximum 1000 cached queries (LRU eviction)
- Cache miss triggers tantivy query

---

## gRPC Search API

> **Note:** gRPC API is planned for implementation. See architecture docs for design.

### `Search(SearchRequest) -> SearchResponse`

Single request/response search.

#### Request Schema

```protobuf
message SearchRequest {
  string query = 1;           // Required: tantivy query string
  optional uint32 limit = 2;  // Default: 100, max: 10000
  optional uint32 offset = 3; // Default: 0, for pagination
  optional string origin_id = 4; // Filter by origin (optional)
}
```

#### Response Schema

```protobuf
message SearchResponse {
  repeated SearchResult results = 1;
  uint64 total_matches = 2;      // Approximate total
  uint32 query_time_ms = 3;      // Query execution time
}

message SearchResult {
  int64 file_id = 1;
  string virtual_path = 2;
  optional string artist = 3;
  optional string album = 4;
  optional string title = 5;
  float score = 6;               // Relevance score
  map<string, string> highlights = 7; // Matched fragments
}
```

### Error Cases

| Scenario | gRPC Status | Details |
|----------|-------------|---------|
| Empty query | `INVALID_ARGUMENT` | "Query cannot be empty" |
| Malformed query syntax | `INVALID_ARGUMENT` | tantivy parse error message |
| limit > 10000 | `INVALID_ARGUMENT` | "Limit exceeds maximum (10000)" |
| Index unavailable | `UNAVAILABLE` | "Search index not ready" |
| Index corrupted | `INTERNAL` | "Search index corrupted" |
| Timeout (>5s) | `DEADLINE_EXCEEDED` | Client-specified deadline |

---

## Query Syntax

MusicFS uses tantivy query syntax with custom fuzzy support.

### Supported Operators

| Operator | Example | Description |
|----------|---------|-------------|
| Term | `metallica` | Match in any default field |
| Field | `artist:metallica` | Match specific field |
| Phrase | `"enter sandman"` | Exact phrase match |
| Fuzzy | `metalica~1` | 1-character edit distance |
| Boolean | `metallica AND 1991` | Combine conditions |
| Range | `year:[1980 TO 1989]` | Numeric range |

### Searchable Fields

| Field | Type | Notes |
|-------|------|-------|
| `artist` | TEXT | Full-text searchable, default field |
| `album` | TEXT | Full-text searchable, default field |
| `album_artist` | TEXT | Full-text searchable, default field |
| `title` | TEXT | Full-text searchable, default field |
| `genre` | TEXT | Full-text searchable, default field |
| `composer` | TEXT | Full-text searchable, default field |
| `year` | u64 | Range queries only |

### Fuzzy Query Implementation

Fuzzy queries use the `term~N` syntax where N is the maximum edit distance (0-2).

When a fuzzy query is detected:
1. Query is parsed to extract term and distance
2. `FuzzyTermQuery` is created for each default field
3. Results are combined with `BooleanQuery` (OR semantics)

Example: `metalica~1` matches "Metallica" (edit distance 1).

---

## Performance

| Metric | Target | Notes |
|--------|--------|-------|
| Query latency (1M tracks) | <500ms | tantivy optimized |
| Index throughput | >1000 files/sec | Batch commits recommended |
| Memory per 1M tracks | <500MB | mmap-based index |

---

## Architecture

### Index Schema

```rust
pub struct SearchSchema {
    file_id: Field,      // INDEXED | STORED - for deletion
    virtual_path: Field, // STORED - symlink target
    artist: Field,       // TEXT | STORED
    album: Field,        // TEXT | STORED
    album_artist: Field, // TEXT | STORED
    title: Field,        // TEXT | STORED
    genre: Field,        // TEXT | STORED
    composer: Field,     // TEXT | STORED
    year: Field,         // INDEXED | STORED
    duration_ms: Field,  // STORED
    bitrate: Field,      // STORED
    sample_rate: Field,  // STORED
}
```

### Writer Pattern

Uses `Arc<RwLock<IndexWriter>>` per tantivy best practices:
- `add_document()` and `delete_term()` require READ lock
- `commit()` requires WRITE lock
- Single writer, multiple concurrent indexers

### Event Integration

The `Indexer` subscribes to `EventBus` for:
- `FileAdded` - Index new file via `MetadataLookup`
- `FileRemoved` - Remove from index by file_id
- `FileModified` - Update index entry

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_search_basic` | Unit | Basic search returns results |
| `test_search_fuzzy` | Unit | Typo tolerance (FR-14.3) |
| `test_search_genre` | Unit | Field-specific search |
| `test_index_persistence` | Unit | Index survives restart |
| `test_remove_file` | Unit | Deletion works correctly |
| `test_index_batch` | Unit | Batch indexing via Indexer |
| `test_search_ops_*` | Unit | FUSE SearchOps integration |
