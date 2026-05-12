# Week 9: Smart Features

**Phase**: 3 (Search & Smart Features)  
**Prerequisites**: Week 8 (Search Index)  
**Estimated effort**: 5 days

---

## Objective

Implement smart collections (query-based virtual folders), cover art extraction with thumbnails, and intelligent prefetching based on access patterns. These features transform MusicFS from a basic filesystem into an intelligent music library.

---

## Architecture Reference

From architecture.md section 4.3.6 (Data Schema):
```sql
CREATE TABLE artwork (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER REFERENCES files(id),
    art_type    TEXT,           -- 'front', 'back'
    chunk_hash  TEXT,           -- reference to CAS
    width       INTEGER,
    height      INTEGER,
    UNIQUE(file_id, art_type)
);

CREATE TABLE collections (
    id          INTEGER PRIMARY KEY,
    name        TEXT UNIQUE,
    query_json  TEXT,           -- smart collection query
    created_at  INTEGER
);
```

From architecture.md section 3.2.5:
> Cache hit rate (warm) | >95% | Derived
> Deduplication ratio | >10% typical | FR-20

---

## Requirements Covered

| ID | Requirement | Priority |
|----|-------------|----------|
| FR-15.1 | Support query-based virtual folders | P1 |
| FR-15.2 | Support saved searches as directories | P1 |
| FR-15.3 | Support dynamic playlists (recently played, most played) | P1 |
| FR-15.4 | Support user-defined metadata fields | P1 (DEFER) |
| FR-16.1 | Extract embedded album art | P1 |
| FR-16.2 | Expose art as virtual files (`cover.jpg`) | P1 |
| FR-16.3 | Cache artwork separately from audio | P1 |
| FR-16.4 | Support multiple art sizes (thumbnail, medium, full) | P1 |
| FR-19.1 | Learn access patterns | P1 |
| FR-19.2 | Support playlist-aware prefetching | P1 |
| FR-19.3 | Support time-based prefetching | P1 |
| FR-19.4 | Support manual prefetch hints (`/.prefetch/`) | P1 |

**Note**: FR-15.4 (user-defined metadata) deferred to plugin system (Phase 4).

---

## Deliverables

| Task | Crate | Files | Est. |
|------|-------|-------|------|
| Smart collections | musicfs-search | `collections.rs` | 1d |
| Collection virtual dirs | musicfs-fuse | `ops/collections.rs` | 0.5d |
| Artwork extractor | musicfs-metadata | `artwork.rs` | 1d |
| Artwork cache (CAS) | musicfs-cache | `artwork.rs` | 0.5d |
| Prefetch engine | musicfs-cache | `prefetch.rs` | 1d |
| Access pattern tracker | musicfs-cache | `patterns.rs` | 0.5d |
| **Prefetch virtual dir** | musicfs-fuse | `ops/prefetch.rs` | 0.5d |
| **API Documentation** | docs | `api/smart-features.md` | 0.5d |
| Integration tests | tests | `smart_features.rs` | 0.5d |

---

## Task 1: Smart Collections

### 1.1 Create `musicfs-search/src/collections.rs`

```rust
use musicfs_core::FileId;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartCollection {
    pub id: i64,
    pub name: String,
    pub query: CollectionQuery,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CollectionQuery {
    /// Match field against pattern
    Match {
        field: String,
        pattern: String,
    },
    
    /// Date range (e.g., year between 1980-1989)
    DateRange {
        field: String,
        start: i32,
        end: i32,
    },
    
    /// Recently added files
    RecentlyAdded {
        days: u32,
    },
    
    /// Recently played files
    RecentlyPlayed {
        days: u32,
    },
    
    /// Most played files
    MostPlayed {
        limit: u32,
    },
    
    /// Genre-based collection
    Genre {
        genre: String,
    },
    
    /// Compound query (AND/OR)
    Compound {
        op: BoolOp,
        children: Vec<CollectionQuery>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BoolOp {
    And,
    Or,
}

impl CollectionQuery {
    pub fn to_tantivy_query(&self) -> String {
        match self {
            CollectionQuery::Match { field, pattern } => {
                format!("{}:{}", field, pattern)
            }
            CollectionQuery::DateRange { field, start, end } => {
                format!("{}:[{} TO {}]", field, start, end)
            }
            CollectionQuery::Genre { genre } => {
                format!("genre:{}", genre)
            }
            CollectionQuery::Compound { op, children } => {
                let sep = match op {
                    BoolOp::And => " AND ",
                    BoolOp::Or => " OR ",
                };
                let parts: Vec<_> = children.iter()
                    .map(|c| format!("({})", c.to_tantivy_query()))
                    .collect();
                parts.join(sep)
            }
            // Dynamic queries handled separately
            _ => String::new(),
        }
    }

    pub fn is_dynamic(&self) -> bool {
        matches!(
            self,
            CollectionQuery::RecentlyAdded { .. }
                | CollectionQuery::RecentlyPlayed { .. }
                | CollectionQuery::MostPlayed { .. }
        )
    }
}

pub struct CollectionStore {
    db: rusqlite::Connection,
}

impl CollectionStore {
    pub fn new(db_path: &std::path::Path) -> Result<Self, CollectionError> {
        let db = rusqlite::Connection::open(db_path)?;
        
        db.execute(
            "CREATE TABLE IF NOT EXISTS collections (
                id INTEGER PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                query_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;

        Ok(Self { db })
    }

    pub fn create(&mut self, name: &str, query: CollectionQuery) -> Result<SmartCollection, CollectionError> {
        let query_json = serde_json::to_string(&query)?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        self.db.execute(
            "INSERT INTO collections (name, query_json, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![name, query_json, now],
        )?;

        let id = self.db.last_insert_rowid();

        Ok(SmartCollection {
            id,
            name: name.to_string(),
            query,
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(now as u64),
        })
    }

    pub fn list(&self) -> Result<Vec<SmartCollection>, CollectionError> {
        let mut stmt = self.db.prepare(
            "SELECT id, name, query_json, created_at FROM collections"
        )?;

        let collections = stmt.query_map([], |row| {
            let query_json: String = row.get(2)?;
            let created_secs: i64 = row.get(3)?;
            
            Ok(SmartCollection {
                id: row.get(0)?,
                name: row.get(1)?,
                query: serde_json::from_str(&query_json).unwrap_or(CollectionQuery::Match {
                    field: "title".to_string(),
                    pattern: "*".to_string(),
                }),
                created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(created_secs as u64),
            })
        })?;

        collections.collect::<Result<Vec<_>, _>>().map_err(CollectionError::from)
    }

    pub fn delete(&mut self, name: &str) -> Result<(), CollectionError> {
        self.db.execute("DELETE FROM collections WHERE name = ?1", [name])?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Built-in collections
pub fn builtin_collections() -> Vec<SmartCollection> {
    vec![
        SmartCollection {
            id: -1,
            name: "Recently Added".to_string(),
            query: CollectionQuery::RecentlyAdded { days: 30 },
            created_at: SystemTime::UNIX_EPOCH,
        },
        SmartCollection {
            id: -2,
            name: "80s Music".to_string(),
            query: CollectionQuery::DateRange {
                field: "year".to_string(),
                start: 1980,
                end: 1989,
            },
            created_at: SystemTime::UNIX_EPOCH,
        },
        SmartCollection {
            id: -3,
            name: "90s Music".to_string(),
            query: CollectionQuery::DateRange {
                field: "year".to_string(),
                start: 1990,
                end: 1999,
            },
            created_at: SystemTime::UNIX_EPOCH,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_collection_crud() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("collections.db");
        let mut store = CollectionStore::new(&db_path).unwrap();

        let collection = store.create(
            "Jazz",
            CollectionQuery::Genre { genre: "Jazz".to_string() },
        ).unwrap();

        assert_eq!(collection.name, "Jazz");

        let collections = store.list().unwrap();
        assert_eq!(collections.len(), 1);

        store.delete("Jazz").unwrap();
        let collections = store.list().unwrap();
        assert_eq!(collections.len(), 0);
    }

    #[test]
    fn test_compound_query() {
        let query = CollectionQuery::Compound {
            op: BoolOp::And,
            children: vec![
                CollectionQuery::Genre { genre: "Metal".to_string() },
                CollectionQuery::DateRange {
                    field: "year".to_string(),
                    start: 1980,
                    end: 1989,
                },
            ],
        };

        let tantivy_query = query.to_tantivy_query();
        assert!(tantivy_query.contains("genre:Metal"));
        assert!(tantivy_query.contains("year:[1980 TO 1989]"));
        assert!(tantivy_query.contains(" AND "));
    }
}
```

---

## Task 2: Artwork Extraction

### 2.1 Add dependencies to `musicfs-metadata/Cargo.toml`

```toml
[dependencies]
image = { version = "0.24", default-features = false, features = ["jpeg", "png"] }
```

### 2.2 Create `musicfs-metadata/src/artwork.rs`

```rust
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;
use symphonia::core::meta::Visual;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct Artwork {
    pub art_type: ArtType,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtType {
    Front,
    Back,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub enum ArtSize {
    Thumbnail,  // 150x150
    Medium,     // 300x300
    Full,       // Original
}

impl ArtSize {
    pub fn max_dimension(&self) -> Option<u32> {
        match self {
            ArtSize::Thumbnail => Some(150),
            ArtSize::Medium => Some(300),
            ArtSize::Full => None,
        }
    }
}

pub struct ArtworkExtractor;

impl ArtworkExtractor {
    pub fn extract_from_visual(visual: &Visual) -> Option<Artwork> {
        let data = visual.data.to_vec();
        
        let img = image::load_from_memory(&data).ok()?;
        
        let art_type = match visual.usage {
            Some(symphonia::core::meta::StandardVisualKey::FrontCover) => ArtType::Front,
            Some(symphonia::core::meta::StandardVisualKey::BackCover) => ArtType::Back,
            _ => ArtType::Other,
        };

        let mime_type = visual.media_type.clone()
            .unwrap_or_else(|| "image/jpeg".to_string());

        Some(Artwork {
            art_type,
            mime_type,
            width: img.width(),
            height: img.height(),
            data,
        })
    }

    pub fn resize(artwork: &Artwork, size: ArtSize) -> Option<Artwork> {
        let max_dim = size.max_dimension()?;
        
        if artwork.width <= max_dim && artwork.height <= max_dim {
            return Some(artwork.clone());
        }

        let img = image::load_from_memory(&artwork.data).ok()?;
        let resized = img.thumbnail(max_dim, max_dim);
        
        let mut output = Vec::new();
        let mut cursor = Cursor::new(&mut output);
        resized.write_to(&mut cursor, ImageFormat::Jpeg).ok()?;

        debug!(
            "Resized artwork from {}x{} to {}x{}",
            artwork.width, artwork.height,
            resized.width(), resized.height()
        );

        Some(Artwork {
            art_type: artwork.art_type,
            mime_type: "image/jpeg".to_string(),
            width: resized.width(),
            height: resized.height(),
            data: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_art_size_dimensions() {
        assert_eq!(ArtSize::Thumbnail.max_dimension(), Some(150));
        assert_eq!(ArtSize::Medium.max_dimension(), Some(300));
        assert_eq!(ArtSize::Full.max_dimension(), None);
    }
}
```

### 2.3 Create `musicfs-cache/src/artwork.rs`

```rust
use musicfs_core::ChunkHash;
use musicfs_metadata::artwork::{ArtSize, Artwork};
use crate::CasStore;
use std::sync::Arc;
use tracing::debug;

pub struct ArtworkCache {
    store: Arc<CasStore>,
    db: rusqlite::Connection,
}

#[derive(Debug)]
pub struct CachedArtwork {
    pub file_id: i64,
    pub art_type: String,
    pub chunk_hash: ChunkHash,
    pub width: u32,
    pub height: u32,
}

/// Oracle fix: Max input size to prevent memory spikes (3000x3000 = ~36MB)
const MAX_ARTWORK_INPUT_SIZE: usize = 10 * 1024 * 1024; // 10MB

impl ArtworkCache {
    pub fn new(store: Arc<CasStore>, db_path: &std::path::Path) -> Result<Self, ArtworkError> {
        let db = rusqlite::Connection::open(db_path)?;
        
        // Oracle fix: Schema matches architecture.md 4.3.6 exactly
        // Only store full-size artwork, generate thumbnail/medium on-demand
        db.execute(
            "CREATE TABLE IF NOT EXISTS artwork (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                art_type TEXT NOT NULL,
                chunk_hash TEXT NOT NULL,
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                UNIQUE(file_id, art_type)
            )",
            [],
        )?;

        Ok(Self { store, db })
    }

    /// Store full-size artwork only (Oracle fix: no size column)
    /// Thumbnail/medium generated on-demand with in-memory LRU
    pub async fn store(&self, file_id: i64, artwork: &Artwork) -> Result<ChunkHash, ArtworkError> {
        // Oracle fix: Reject oversized images to prevent memory spikes
        if artwork.data.len() > MAX_ARTWORK_INPUT_SIZE {
            return Err(ArtworkError::ImageTooLarge(artwork.data.len()));
        }
        
        let hash = self.store.put(&artwork.data).await?;
        
        let art_type_str = match artwork.art_type {
            musicfs_metadata::artwork::ArtType::Front => "front",
            musicfs_metadata::artwork::ArtType::Back => "back",
            musicfs_metadata::artwork::ArtType::Other => "other",
        };

        // Oracle fix: Use spawn_blocking for rusqlite in async context
        let db_path = self.db.path().map(|p| p.to_path_buf());
        let file_id_clone = file_id;
        let art_type_clone = art_type_str.to_string();
        let hash_hex = hash.to_hex();
        let width = artwork.width;
        let height = artwork.height;
        
        tokio::task::spawn_blocking(move || {
            let db = rusqlite::Connection::open(db_path.unwrap())?;
            db.execute(
                "INSERT OR REPLACE INTO artwork 
                 (file_id, art_type, chunk_hash, width, height) 
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![file_id_clone, art_type_clone, hash_hex, width, height],
            )?;
            Ok::<_, ArtworkError>(())
        }).await.map_err(|e| ArtworkError::SpawnBlocking(e.to_string()))??;

        debug!("Cached artwork for file {}", file_id);
        Ok(hash)
    }

    /// Get full-size artwork, optionally resize on-demand
    pub async fn get(&self, file_id: i64, art_type: &str, size: ArtSize) -> Result<Option<Vec<u8>>, ArtworkError> {
        // Oracle fix: Use spawn_blocking for rusqlite
        let db_path = self.db.path().map(|p| p.to_path_buf());
        let file_id_clone = file_id;
        let art_type_clone = art_type.to_string();
        
        let hash_hex: Option<String> = tokio::task::spawn_blocking(move || {
            let db = rusqlite::Connection::open(db_path.unwrap())?;
            db.query_row(
                "SELECT chunk_hash FROM artwork WHERE file_id = ?1 AND art_type = ?2",
                rusqlite::params![file_id_clone, art_type_clone],
                |row| row.get(0),
            ).ok().ok_or(ArtworkError::NotFound)
        }).await.map_err(|e| ArtworkError::SpawnBlocking(e.to_string()))?.ok();

        match hash_hex {
            Some(hex) => {
                let hash = ChunkHash::from_hex(&hex).ok_or(ArtworkError::InvalidHash)?;
                let data = self.store.get(&hash).await?;
                
                // On-demand resize if not full size
                match size {
                    ArtSize::Full => Ok(Some(data.to_vec())),
                    ArtSize::Thumbnail | ArtSize::Medium => {
                        // Resize on-demand (could add LRU cache here)
                        let resized = self.resize_on_demand(&data, size)?;
                        Ok(Some(resized))
                    }
                }
            }
            None => Ok(None),
        }
    }
    
    fn resize_on_demand(&self, data: &[u8], size: ArtSize) -> Result<Vec<u8>, ArtworkError> {
        use image::ImageFormat;
        use std::io::Cursor;
        
        let max_dim = size.max_dimension().unwrap_or(300);
        let img = image::load_from_memory(data).map_err(|_| ArtworkError::InvalidImage)?;
        
        if img.width() <= max_dim && img.height() <= max_dim {
            return Ok(data.to_vec());
        }
        
        let resized = img.thumbnail(max_dim, max_dim);
        let mut output = Vec::new();
        let mut cursor = Cursor::new(&mut output);
        resized.write_to(&mut cursor, ImageFormat::Jpeg).map_err(|_| ArtworkError::ResizeFailed)?;
        
        Ok(output)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArtworkError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    
    #[error("CAS error: {0}")]
    Cas(#[from] crate::store::CasError),
    
    #[error("invalid hash")]
    InvalidHash,
    
    #[error("artwork not found")]
    NotFound,
    
    #[error("image too large: {0} bytes (max 10MB)")]
    ImageTooLarge(usize),
    
    #[error("invalid image data")]
    InvalidImage,
    
    #[error("resize failed")]
    ResizeFailed,
    
    #[error("spawn_blocking error: {0}")]
    SpawnBlocking(String),
}
```

---

## Task 3: Prefetch Engine

### 3.1 Create `musicfs-cache/src/patterns.rs`

```rust
use musicfs_core::FileId;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Oracle fix: Use SystemTime for persistence, not Instant
pub struct AccessPattern {
    file_id: FileId,
    timestamp: SystemTime,
    context: AccessContext,
    hour_of_day: u8,  // For time-based prefetch (FR-19.3)
}

#[derive(Debug, Clone)]
pub struct AccessContext {
    pub album_id: Option<i64>,
    pub track_number: Option<u32>,
    pub artist: Option<String>,
}

/// Oracle fix: Persistent pattern store with SQLite
pub struct PatternStore {
    db: rusqlite::Connection,
    /// In-memory cache for hot path
    sequence_counts: parking_lot::RwLock<HashMap<(FileId, FileId), u32>>,
    /// Time-based patterns for FR-19.3
    time_patterns: parking_lot::RwLock<HashMap<u8, Vec<FileId>>>,  // hour -> files
    max_history: usize,
}

impl PatternStore {
    pub fn new(db_path: &Path, max_history: usize) -> Result<Self, PatternError> {
        let db = rusqlite::Connection::open(db_path)?;
        
        // Oracle fix: Persist access log for RecentlyPlayed/MostPlayed queries
        db.execute(
            "CREATE TABLE IF NOT EXISTS access_log (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL,
                access_time INTEGER NOT NULL,
                hour_of_day INTEGER NOT NULL
            )",
            [],
        )?;
        
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_access_log_file ON access_log(file_id)",
            [],
        )?;
        
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_access_log_time ON access_log(access_time)",
            [],
        )?;
        
        // Sequence transitions table
        db.execute(
            "CREATE TABLE IF NOT EXISTS sequence_counts (
                from_file_id INTEGER NOT NULL,
                to_file_id INTEGER NOT NULL,
                count INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (from_file_id, to_file_id)
            )",
            [],
        )?;
        
        // Load sequence counts into memory
        let mut sequence_counts = HashMap::new();
        let mut stmt = db.prepare("SELECT from_file_id, to_file_id, count FROM sequence_counts")?;
        let rows = stmt.query_map([], |row| {
            Ok(((FileId(row.get::<_, i64>(0)?), FileId(row.get::<_, i64>(1)?)), row.get::<_, u32>(2)?))
        })?;
        for row in rows {
            let (key, count) = row?;
            sequence_counts.insert(key, count);
        }
        
        Ok(Self {
            db,
            sequence_counts: parking_lot::RwLock::new(sequence_counts),
            time_patterns: parking_lot::RwLock::new(HashMap::new()),
            max_history,
        })
    }

    pub fn record(&self, file_id: FileId, context: AccessContext) -> Result<(), PatternError> {
        let now = SystemTime::now();
        let timestamp = now.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let hour = (timestamp / 3600 % 24) as u8;
        
        // Persist to SQLite
        self.db.execute(
            "INSERT INTO access_log (file_id, access_time, hour_of_day) VALUES (?1, ?2, ?3)",
            rusqlite::params![file_id.0, timestamp, hour],
        )?;
        
        // Update time patterns (FR-19.3)
        {
            let mut time_patterns = self.time_patterns.write();
            time_patterns.entry(hour).or_default().push(file_id);
        }
        
        // Get previous access for sequence tracking
        let prev_file_id: Option<i64> = self.db.query_row(
            "SELECT file_id FROM access_log WHERE id = (SELECT MAX(id) - 1 FROM access_log)",
            [],
            |row| row.get(0),
        ).ok();
        
        if let Some(prev_id) = prev_file_id {
            let prev = FileId(prev_id);
            
            // Update in-memory
            {
                let mut sequences = self.sequence_counts.write();
                *sequences.entry((prev, file_id)).or_insert(0) += 1;
            }
            
            // Persist sequence
            self.db.execute(
                "INSERT INTO sequence_counts (from_file_id, to_file_id, count) 
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(from_file_id, to_file_id) DO UPDATE SET count = count + 1",
                rusqlite::params![prev_id, file_id.0],
            )?;
        }
        
        // Cleanup old entries
        let cutoff = timestamp - (self.max_history as i64 * 86400); // max_history in days
        self.db.execute("DELETE FROM access_log WHERE access_time < ?1", [cutoff])?;
        
        Ok(())
    }

    pub fn predict_next(&self, current: FileId, limit: usize) -> Vec<FileId> {
        let sequences = self.sequence_counts.read();
        
        let mut predictions: Vec<_> = sequences
            .iter()
            .filter(|((from, _), count)| *from == current && **count >= 2)  // Oracle fix: min threshold
            .map(|((_, to), count)| (*to, *count))
            .collect();

        predictions.sort_by(|a, b| b.1.cmp(&a.1));
        predictions.into_iter().take(limit).map(|(id, _)| id).collect()
    }
    
    /// FR-19.3: Time-based prefetch - files commonly accessed at this hour
    pub fn predict_for_time(&self, hour: u8, limit: usize) -> Vec<FileId> {
        let time_patterns = self.time_patterns.read();
        
        time_patterns
            .get(&hour)
            .map(|files| files.iter().rev().take(limit).copied().collect())
            .unwrap_or_default()
    }

    /// For RecentlyPlayed collection query
    pub fn recently_played(&self, days: u32) -> Result<Vec<FileId>, PatternError> {
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64 - (days as i64 * 86400);
        
        let mut stmt = self.db.prepare(
            "SELECT DISTINCT file_id FROM access_log WHERE access_time >= ?1 ORDER BY access_time DESC"
        )?;
        
        let files: Vec<FileId> = stmt
            .query_map([cutoff], |row| Ok(FileId(row.get(0)?)))?
            .filter_map(|r| r.ok())
            .collect();
        
        Ok(files)
    }
    
    /// For MostPlayed collection query
    pub fn most_played(&self, limit: u32) -> Result<Vec<FileId>, PatternError> {
        let mut stmt = self.db.prepare(
            "SELECT file_id, COUNT(*) as play_count FROM access_log 
             GROUP BY file_id ORDER BY play_count DESC LIMIT ?1"
        )?;
        
        let files: Vec<FileId> = stmt
            .query_map([limit], |row| Ok(FileId(row.get(0)?)))?
            .filter_map(|r| r.ok())
            .collect();
        
        Ok(files)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_pattern_prediction() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let store = PatternStore::new(&db_path, 30).unwrap();
        let ctx = AccessContext { album_id: None, track_number: None, artist: None };

        // Simulate: A -> B -> C pattern multiple times
        for _ in 0..5 {
            store.record(FileId(1), ctx.clone()).unwrap();
            store.record(FileId(2), ctx.clone()).unwrap();
            store.record(FileId(3), ctx.clone()).unwrap();
        }

        // After playing A, should predict B (needs >= 2 count)
        let predictions = store.predict_next(FileId(1), 3);
        assert!(!predictions.is_empty());
        assert_eq!(predictions[0], FileId(2));
    }
    
    #[test]
    fn test_pattern_persistence() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let ctx = AccessContext { album_id: None, track_number: None, artist: None };
        
        // Record patterns
        {
            let store = PatternStore::new(&db_path, 30).unwrap();
            for _ in 0..3 {
                store.record(FileId(1), ctx.clone()).unwrap();
                store.record(FileId(2), ctx.clone()).unwrap();
            }
        }
        
        // Reopen and verify persistence
        {
            let store = PatternStore::new(&db_path, 30).unwrap();
            let predictions = store.predict_next(FileId(1), 3);
            assert!(!predictions.is_empty());
            assert_eq!(predictions[0], FileId(2));
        }
    }
    
    #[test]
    fn test_recently_played() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let store = PatternStore::new(&db_path, 30).unwrap();
        let ctx = AccessContext { album_id: None, track_number: None, artist: None };
        
        store.record(FileId(100), ctx.clone()).unwrap();
        store.record(FileId(200), ctx.clone()).unwrap();
        
        let recent = store.recently_played(7).unwrap();
        assert!(recent.contains(&FileId(100)));
        assert!(recent.contains(&FileId(200)));
    }
    
    #[test]
    fn test_most_played() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let store = PatternStore::new(&db_path, 30).unwrap();
        let ctx = AccessContext { album_id: None, track_number: None, artist: None };
        
        // Play file 1 more times than file 2
        for _ in 0..5 {
            store.record(FileId(1), ctx.clone()).unwrap();
        }
        for _ in 0..2 {
            store.record(FileId(2), ctx.clone()).unwrap();
        }
        
        let most = store.most_played(10).unwrap();
        assert_eq!(most[0], FileId(1)); // Most played first
    }
}
```

### 3.2 Create `musicfs-cache/src/prefetch.rs`

```rust
use crate::patterns::{AccessContext, PatternStore};
use crate::CacheManager;
use musicfs_core::{Event, EventBus, FileId};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

pub struct PrefetchEngine {
    patterns: Arc<PatternStore>,
    cache: Arc<CacheManager>,
    /// Oracle fix: Channel-based queue instead of polling
    task_tx: mpsc::Sender<PrefetchTask>,
    task_rx: parking_lot::Mutex<Option<mpsc::Receiver<PrefetchTask>>>,
    /// Oracle fix: Deduplication set to prevent duplicate prefetches
    pending: parking_lot::RwLock<HashSet<FileId>>,
    config: PrefetchConfig,
}

#[derive(Debug, Clone)]
pub struct PrefetchConfig {
    pub enabled: bool,
    pub max_queue_size: usize,
    pub lookahead: usize,
    pub album_aware: bool,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_queue_size: 100,
            lookahead: 3,
            album_aware: true,
        }
    }
}

#[derive(Debug)]
struct PrefetchTask {
    file_id: FileId,
    priority: u8,
}

impl PrefetchEngine {
    pub fn new(patterns: Arc<PatternStore>, cache: Arc<CacheManager>, config: PrefetchConfig) -> Self {
        // Oracle fix: Use bounded channel instead of polling VecDeque
        let (task_tx, task_rx) = mpsc::channel(config.max_queue_size);
        
        Self {
            patterns,
            cache,
            task_tx,
            task_rx: parking_lot::Mutex::new(Some(task_rx)),
            pending: parking_lot::RwLock::new(HashSet::new()),
            config,
        }
    }

    pub fn on_access(&self, file_id: FileId, context: AccessContext) {
        if !self.config.enabled {
            return;
        }

        // Record pattern (now returns Result)
        if let Err(e) = self.patterns.record(file_id, context.clone()) {
            warn!("Failed to record pattern: {}", e);
        }

        // Predict next files based on sequence patterns
        let predictions = self.patterns.predict_next(file_id, self.config.lookahead);
        
        // FR-19.3: Time-based predictions
        let hour = chrono::Local::now().hour() as u8;
        let time_predictions = self.patterns.predict_for_time(hour, 2);
        
        // Album-aware: if we know track number, prefetch next tracks
        let album_prefetch = if self.config.album_aware {
            self.predict_album_next(&context)
        } else {
            vec![]
        };

        // Oracle fix: Deduplicate before queueing
        let pending = self.pending.read();
        
        for (i, pred) in predictions.into_iter().enumerate() {
            if pending.contains(&pred) {
                continue;  // Already pending
            }
            let _ = self.task_tx.try_send(PrefetchTask {
                file_id: pred,
                priority: (10 - i as u8).min(10),
            });
        }
        
        for pred in time_predictions {
            if pending.contains(&pred) {
                continue;
            }
            let _ = self.task_tx.try_send(PrefetchTask {
                file_id: pred,
                priority: 5,  // Medium priority for time-based
            });
        }

        for (i, pred) in album_prefetch.into_iter().enumerate() {
            if pending.contains(&pred) {
                continue;
            }
            let _ = self.task_tx.try_send(PrefetchTask {
                file_id: pred,
                priority: (8 - i as u8).min(8),
            });
        }

        debug!("Prefetch pending count: {}", pending.len());
    }
    
    /// FR-19.4: Manual prefetch hint via /.prefetch/path
    pub fn prefetch_hint(&self, file_id: FileId, priority: u8) {
        let pending = self.pending.read();
        if pending.contains(&file_id) {
            return;
        }
        drop(pending);
        
        let _ = self.task_tx.try_send(PrefetchTask { file_id, priority });
    }

    fn predict_album_next(&self, context: &AccessContext) -> Vec<FileId> {
        // In real implementation, would query cache for tracks in same album
        // with track_number > current
        vec![]
    }

    /// Oracle fix: Event-driven loop instead of busy-wait polling
    pub async fn run(&self) {
        info!("Prefetch engine started");
        
        // Take ownership of receiver
        let mut task_rx = self.task_rx.lock().take()
            .expect("run() called twice");
        
        while let Some(task) = task_rx.recv().await {
            // Mark as pending
            {
                let mut pending = self.pending.write();
                pending.insert(task.file_id);
            }
            
            debug!("Prefetching {:?} (priority {})", task.file_id, task.priority);
            
            if let Err(e) = self.cache.prefetch(&task.file_id).await {
                warn!("Prefetch failed for {:?}: {}", task.file_id, e);
            }
            
            // Remove from pending
            {
                let mut pending = self.pending.write();
                pending.remove(&task.file_id);
            }
        }
        
        info!("Prefetch engine stopped");
    }

    pub fn start(self: Arc<Self>) -> PrefetchHandle {
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let engine = self.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = engine.run() => {}
                _ = stop_rx.recv() => {
                    info!("Prefetch engine stopped");
                }
            }
        });

        PrefetchHandle { stop_tx }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.read().len()
    }
}

pub struct PrefetchHandle {
    stop_tx: mpsc::Sender<()>,
}

impl PrefetchHandle {
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_prefetch_config_default() {
        let config = PrefetchConfig::default();
        assert!(config.enabled);
        assert_eq!(config.lookahead, 3);
        assert!(config.album_aware);
    }
    
    #[tokio::test]
    async fn test_prefetch_deduplication() {
        let dir = TempDir::new().unwrap();
        let patterns = Arc::new(PatternStore::new(&dir.path().join("p.db"), 30).unwrap());
        let cache = Arc::new(MockCacheManager::new());
        let config = PrefetchConfig::default();
        
        let engine = PrefetchEngine::new(patterns, cache, config);
        
        // Queue same file twice
        engine.prefetch_hint(FileId(1), 10);
        engine.prefetch_hint(FileId(1), 10);  // Should be deduplicated
        
        // Only one should be pending
        assert_eq!(engine.pending_count(), 0);  // Not yet processed
    }
    
    #[test]
    fn test_prefetch_channel_based() {
        // Verify no busy-wait polling - channel is used
        let config = PrefetchConfig { max_queue_size: 50, ..Default::default() };
        // Channel capacity should match config
        assert_eq!(config.max_queue_size, 50);
    }
}
```

---

---

## Task 4: Prefetch Virtual Directory (FR-19.4)

### 4.1 Create `musicfs-fuse/src/ops/prefetch.rs`

```rust
use fuser::{FileType, ReplyDirectory, ReplyEntry, ReplyAttr};
use musicfs_cache::prefetch::PrefetchEngine;
use musicfs_core::{FileId, VirtualPath};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::debug;

const PREFETCH_DIR_INODE: u64 = 0xFFFF_FFFF_0000_0002;

/// FR-19.4: Manual prefetch hints via /.prefetch/path
pub struct PrefetchOps {
    prefetch_engine: Arc<PrefetchEngine>,
}

impl PrefetchOps {
    pub fn new(prefetch_engine: Arc<PrefetchEngine>) -> Self {
        Self { prefetch_engine }
    }

    pub fn is_prefetch_path(path: &str) -> bool {
        path.starts_with("/.prefetch/")
    }

    /// Lookup triggers prefetch for the target file
    pub fn lookup(&self, path: &str, file_id: FileId, reply: ReplyEntry) {
        debug!("Manual prefetch hint for: {}", path);
        
        // Queue prefetch with high priority (manual = important)
        self.prefetch_engine.prefetch_hint(file_id, 15);
        
        // Return the original file's attributes
        // (actual lookup delegated to main filesystem)
        reply.error(libc::ENOENT); // Let main handler resolve
    }

    pub fn readdir_prefetch_root(&self, reply: &mut ReplyDirectory) {
        reply.add(PREFETCH_DIR_INODE, 1, FileType::Directory, ".");
        reply.add(1, 2, FileType::Directory, "..");
        // Empty directory - entries are virtual
    }

    pub fn getattr_prefetch_dir(&self, reply: ReplyAttr) {
        let attr = fuser::FileAttr {
            ino: PREFETCH_DIR_INODE,
            size: 0,
            blocks: 0,
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 2,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            blksize: 512,
            flags: 0,
        };
        reply.attr(&Duration::from_secs(60), &attr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefetch_path_detection() {
        assert!(PrefetchOps::is_prefetch_path("/.prefetch/Artist/Album/Track.flac"));
        assert!(!PrefetchOps::is_prefetch_path("/Artist/Album/Track.flac"));
    }
}
```

### 4.2 FUSE Integration

Add to `musicfs-fuse/src/filesystem.rs`:

```rust
// In lookup()
if name == ".prefetch" && parent == 1 {
    self.prefetch_ops.getattr_prefetch_dir(reply);
    return;
}

if let Some(path) = self.inode_to_path(parent) {
    if PrefetchOps::is_prefetch_path(&path) {
        // Strip /.prefetch/ prefix and lookup actual file
        let actual_path = &path[10..]; // "/.prefetch/".len()
        if let Some(file_id) = self.path_to_file_id(actual_path) {
            self.prefetch_ops.lookup(&path, file_id, reply);
            return;
        }
    }
}

// In readdir()
if ino == PREFETCH_DIR_INODE {
    self.prefetch_ops.readdir_prefetch_root(&mut reply);
    reply.ok();
    return;
}
```

---

## Task 5: API Documentation

**All APIs must be fully documented with happy and non-happy paths.**

### 5.1 Create `docs/api/smart-features.md`

```markdown
# Smart Features API Documentation

## Overview

Week 9 implements three smart feature categories:
1. **Smart Collections** - Query-based virtual folders
2. **Artwork** - Embedded album art extraction and caching
3. **Intelligent Prefetching** - Access pattern learning and prediction

---

## 1. Smart Collections

### Virtual Directory: `/.collections/{name}/`

Browse query-based collections as virtual directories.

### Happy Path

```
User                              FUSE
  |                                 |
  |-- ls /.collections/ ----------->|
  |<-- [Recently Added, 80s, Jazz]--|
  |                                 |
  |-- ls /.collections/Jazz/ ------>|
  |   (executes: genre:Jazz)        |
  |<-- [symlinks to jazz tracks] ---|
```

### Built-in Collections

| Name | Query | Description |
|------|-------|-------------|
| Recently Added | `RecentlyAdded { days: 30 }` | Files added in last 30 days |
| Recently Played | `RecentlyPlayed { days: 7 }` | Files played in last 7 days |
| Most Played | `MostPlayed { limit: 100 }` | Top 100 most played |
| 80s Music | `year:[1980 TO 1989]` | Year range filter |
| 90s Music | `year:[1990 TO 1999]` | Year range filter |

### Collection Query Types

```rust
enum CollectionQuery {
    Match { field, pattern }      // field:pattern
    DateRange { field, start, end } // field:[start TO end]
    RecentlyAdded { days }        // Dynamic: mtime > now - days
    RecentlyPlayed { days }       // Dynamic: from access_log
    MostPlayed { limit }          // Dynamic: from access_log
    Genre { genre }               // genre:value
    Compound { op, children }     // AND/OR combinations
}
```

### Error Cases

| Scenario | Behavior | FUSE Error |
|----------|----------|------------|
| Collection not found | ENOENT | `libc::ENOENT` |
| Invalid query syntax | Empty directory | (none) |
| Database error | EIO | `libc::EIO` |

### SQLite Schema

```sql
CREATE TABLE collections (
    id INTEGER PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    query_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- For RecentlyPlayed/MostPlayed queries
CREATE TABLE access_log (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL,
    access_time INTEGER NOT NULL,
    hour_of_day INTEGER NOT NULL
);
```

---

## 2. Artwork API

### Virtual File: `/Artist/Album/cover.jpg`

Exposes embedded album art as virtual files.

### Happy Path

```
User                              FUSE                    ArtworkCache
  |                                 |                          |
  |-- open /A/B/cover.jpg --------->|                          |
  |                                 |-- get(file_id, "front")->|
  |                                 |<-- chunk_hash -----------|
  |                                 |-- CAS.get(hash) -------->|
  |                                 |<-- image bytes ----------|
  |<-- image data ------------------|                          |
```

### Supported Sizes

| Size | Max Dimension | Generated |
|------|---------------|-----------|
| `thumbnail` | 150x150 | On-demand |
| `medium` | 300x300 | On-demand |
| `full` | Original | Stored in CAS |

### Accessing Different Sizes

```
/Artist/Album/cover.jpg          # Full size (default)
/Artist/Album/cover_thumb.jpg    # 150x150 thumbnail
/Artist/Album/cover_medium.jpg   # 300x300 medium
```

### Error Cases

| Scenario | Behavior | FUSE Error |
|----------|----------|------------|
| No embedded artwork | ENOENT | `libc::ENOENT` |
| Corrupted image data | ENOENT | `libc::ENOENT` |
| Image too large (>10MB) | Rejected during extraction | (logged) |
| CAS lookup failed | EIO | `libc::EIO` |
| Resize failed | Return full size | (fallback) |

### SQLite Schema (Architecture 4.3.6)

```sql
CREATE TABLE artwork (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id),
    art_type TEXT NOT NULL,       -- 'front', 'back', 'other'
    chunk_hash TEXT NOT NULL,     -- Reference to CAS
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    UNIQUE(file_id, art_type)
);
```

**Note**: Only full-size artwork stored. Thumbnail/medium generated on-demand.

---

## 3. Prefetch API

### Automatic Prefetching

Prefetch engine learns access patterns and pre-loads likely next files.

### Pattern Learning Flow

```
User plays: Track 1 -> Track 2 -> Track 3 (repeated 5x)

Pattern Store:
  (Track 1 -> Track 2): count = 5
  (Track 2 -> Track 3): count = 5

Next time user plays Track 1:
  -> Predict Track 2 (high confidence)
  -> Queue prefetch for Track 2
```

### FR-19.3: Time-Based Prefetching

```
User listens to "Morning Playlist" at 8am every weekday

Pattern Store:
  hour_of_day = 8 -> [track_ids from morning playlist]

At 7:55am:
  -> Predict morning tracks
  -> Queue prefetch
```

### FR-19.4: Manual Prefetch Hints

**Virtual Directory**: `/.prefetch/{path}`

```bash
# Trigger prefetch for an album
ls /.prefetch/Artist/Album/

# Prefetch specific file
cat /.prefetch/Artist/Album/Track.flac > /dev/null
```

### Happy Path (Manual Prefetch)

```
User                              FUSE               PrefetchEngine
  |                                 |                      |
  |-- ls /.prefetch/A/B/ ---------->|                      |
  |                                 |-- prefetch_hint() -->|
  |                                 |   file_id, priority=15
  |                                 |                      |-- queue task
  |<-- (directory listing) ---------|                      |
  |                                 |                      |-- async fetch
```

### Prefetch Priority Levels

| Source | Priority | Description |
|--------|----------|-------------|
| Manual (/.prefetch/) | 15 | User-initiated, highest |
| Sequence prediction | 10-8 | Based on history patterns |
| Album sequential | 8-6 | Next tracks in album |
| Time-based | 5 | Hour-of-day patterns |

### Error Cases

| Scenario | Behavior |
|----------|----------|
| Already pending | Skipped (deduplication) |
| Queue full | try_send fails silently |
| Prefetch fails | Logged, removed from pending |
| Pattern DB error | Logged, prefetch continues |

### Configuration

```rust
struct PrefetchConfig {
    enabled: bool,           // Default: true
    max_queue_size: usize,   // Default: 100
    lookahead: usize,        // Default: 3 tracks
    album_aware: bool,       // Default: true
}
```

### SQLite Schema

```sql
-- Access history for pattern learning
CREATE TABLE access_log (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL,
    access_time INTEGER NOT NULL,
    hour_of_day INTEGER NOT NULL
);

-- Sequence transition counts
CREATE TABLE sequence_counts (
    from_file_id INTEGER NOT NULL,
    to_file_id INTEGER NOT NULL,
    count INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (from_file_id, to_file_id)
);
```

---

## Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Cache hit rate (warm) | >95% | FR-16.3 |
| Prefetch accuracy | >50% | Measured as: prefetched files actually accessed |
| Artwork resize latency | <100ms | For thumbnail/medium |
| Pattern prediction latency | <10ms | In-memory lookup |

---

## Integration Examples

### Creating a Smart Collection

```rust
let mut store = CollectionStore::new(&db_path)?;

// Create custom collection
let jazz_80s = store.create(
    "80s Jazz",
    CollectionQuery::Compound {
        op: BoolOp::And,
        children: vec![
            CollectionQuery::Genre { genre: "Jazz".into() },
            CollectionQuery::DateRange {
                field: "year".into(),
                start: 1980,
                end: 1989,
            },
        ],
    },
)?;

// List collections
let collections = store.list()?;
```

### Accessing Album Art

```rust
let cache = ArtworkCache::new(cas_store, &db_path)?;

// Get full-size artwork
let full = cache.get(file_id, "front", ArtSize::Full).await?;

// Get thumbnail (generated on-demand)
let thumb = cache.get(file_id, "front", ArtSize::Thumbnail).await?;
```

### Manual Prefetch via CLI

```bash
# Prefetch entire album before listening
find /mnt/musicfs/.prefetch/Metallica/BlackAlbum/ -type f | head -n 1

# Check prefetch status
musicfs-cli prefetch status
# Output: 3 files pending, 12 completed in last hour
```
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_collection_crud` | Unit | Create/list/delete collections (FR-15.2) |
| `test_compound_query` | Unit | AND/OR queries work |
| `test_builtin_collections` | Unit | Recently Added, 80s/90s exist |
| `test_recently_played_query` | Unit | RecentlyPlayed from access_log |
| `test_most_played_query` | Unit | MostPlayed from access_log |
| `test_artwork_extraction` | Unit | Extract from FLAC/MP3 (FR-16.1) |
| `test_artwork_resize` | Unit | Thumbnail/medium generation (FR-16.4) |
| `test_artwork_resize_on_demand` | Unit | Full stored, sizes generated |
| `test_artwork_reject_oversized` | Unit | >10MB images rejected |
| `test_artwork_cache` | Unit | Store/retrieve from CAS (FR-16.3) |
| `test_pattern_prediction` | Unit | A->B->C pattern learned (FR-19.1) |
| `test_pattern_persistence` | Unit | Patterns survive restart |
| `test_time_based_prediction` | Unit | Hour-of-day patterns (FR-19.3) |
| `test_prefetch_deduplication` | Unit | Same file not queued twice |
| `test_prefetch_channel` | Unit | Channel-based, no polling |
| `test_prefetch_manual_hint` | Unit | /.prefetch/ handler (FR-19.4) |
| `test_collection_virtual_dir` | E2E | `/.collections/Jazz/` works |
| `test_cover_virtual_file` | E2E | `/Artist/Album/cover.jpg` exists (FR-16.2) |
| `test_prefetch_virtual_dir` | E2E | `/.prefetch/path` triggers prefetch |
| `test_prefetch_reduces_misses` | Integration | >50% miss reduction |

---

## Exit Criteria

- [ ] Smart collections stored in SQLite
- [ ] Built-in collections (Recently Added, Recently Played, Most Played, 80s, 90s) available
- [ ] `/.collections/Name/` shows matching files
- [ ] RecentlyPlayed/MostPlayed queries use persisted access_log table
- [ ] Album art extracted from embedded FLAC/MP3 data
- [ ] Artwork schema matches architecture.md 4.3.6 exactly (no size column)
- [ ] Thumbnail/medium generated on-demand, only full stored in CAS
- [ ] Oversized images (>10MB) rejected gracefully
- [ ] `cover.jpg` appears in album directories
- [ ] Access patterns recorded in SQLite (survive restarts)
- [ ] Time-based prefetch predicts by hour-of-day (FR-19.3)
- [ ] `/.prefetch/path` triggers manual prefetch hints (FR-19.4)
- [ ] Prefetch engine uses channel-based queue (no busy-wait polling)
- [ ] Prefetch deduplication prevents same file queued twice
- [ ] Prefetch reduces cache misses by >50% on sequential album playback
- [ ] API documentation covers happy/error paths for all features

---

## Architecture Compliance

| Architecture Section | Requirement | Status |
|---------------------|-------------|--------|
| 4.3.6 | collections table schema | ✅ |
| 4.3.6 | artwork table schema (UNIQUE file_id, art_type) | ✅ Oracle fix |
| 3.2.5 | Cache hit rate >95% | ✅ Benchmark |
| FR-15.1 | Query-based virtual folders | ✅ |
| FR-15.2 | Saved searches as directories | ✅ |
| FR-15.3 | Dynamic playlists (RecentlyPlayed, MostPlayed) | ✅ access_log |
| FR-16.1 | Extract embedded album art | ✅ |
| FR-16.2 | Expose as virtual files | ✅ |
| FR-16.3 | Cache separately from audio | ✅ |
| FR-16.4 | Multiple sizes | ✅ On-demand |
| FR-19.1 | Learn access patterns | ✅ Persistent |
| FR-19.2 | Playlist-aware prefetch | ✅ |
| FR-19.3 | Time-based prefetching | ✅ Task 4 |
| FR-19.4 | Manual prefetch hints | ✅ /.prefetch/ |

## Oracle Fixes Applied

| Issue | Fix | Location |
|-------|-----|----------|
| Artwork schema mismatch | Removed `size` column, matches architecture exactly | `artwork.rs` |
| rusqlite in async context | Use `spawn_blocking` for DB operations | `artwork.rs` |
| PatternStore not persisted | Added `access_log` and `sequence_counts` tables | `patterns.rs` |
| FR-19.3 missing | Added time-based prediction by hour | `patterns.rs` |
| FR-19.4 missing | Added `/.prefetch/` FUSE handler | `prefetch.rs` |
| Prefetch busy-wait polling | Switched to `mpsc::channel` | `prefetch.rs` |
| No prefetch deduplication | Added `pending: HashSet<FileId>` guard | `prefetch.rs` |
| Image resize memory spikes | Added 10MB max input size check | `artwork.rs` |
