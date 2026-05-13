# Week 2: Metadata Extraction

**Phase**: 1 (MVP)  
**Prerequisites**: Week 1 (Foundation)  
**Estimated effort**: 5 days

---

## Objective

Implement audio metadata extraction using symphonia and create SQLite schema for metadata cache.

---

## Deliverables

| Task | Crate | Files | Done |
|------|-------|-------|------|
| Audio parsing | musicfs-metadata | `lib.rs`, `parser.rs` | [ ] |
| Format handlers | musicfs-metadata | `formats/*.rs` | [ ] |
| SQLite schema | musicfs-cache | `schema.sql`, `db.rs` | [ ] |
| Metadata cache | musicfs-cache | `metadata.rs` | [ ] |

---

## Task 0: Extend AudioMeta in `musicfs-core`

Add `lyrics` and `composer` fields to `AudioMeta` struct (FR-6.4):

```rust
// In musicfs-core/src/types.rs, add to AudioMeta:
pub struct AudioMeta {
    // ... existing fields ...
    pub lyrics: Option<String>,
    pub composer: Option<String>,
}
```

---

## Task 1: Metadata Parser (`musicfs-metadata`)

### 1.1 Create `Cargo.toml`

```toml
[package]
name = "musicfs-metadata"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
symphonia = { version = "0.5", features = ["all"] }
thiserror.workspace = true
tracing.workspace = true
```

### 1.2 Create `src/lib.rs`

```rust
mod parser;

pub use parser::MetadataParser;
```

### 1.3 Create `src/parser.rs`

```rust
use musicfs_core::{AudioFormat, AudioMeta, Result, Error};
use std::io::{Read, Seek};
use std::path::Path;
use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::debug;

/// Metadata extraction using symphonia (FR-6.1-6.5)
pub struct MetadataParser;

impl MetadataParser {
    pub fn new() -> Self {
        Self
    }
    
    /// Extract metadata from audio file
    pub fn parse_file(&self, path: &Path) -> Result<AudioMeta> {
        let file = std::fs::File::open(path)?;
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        self.parse_reader(file, ext)
    }
    
    /// Extract metadata from reader
    pub fn parse_reader<R: Read + Seek + Send + Sync + 'static>(
        &self,
        reader: R,
        extension: &str,
    ) -> Result<AudioMeta> {
        let mss = MediaSourceStream::new(Box::new(reader), Default::default());
        
        let mut hint = Hint::new();
        if !extension.is_empty() {
            hint.with_extension(extension);
        }
        
        let format_opts = FormatOptions {
            enable_gapless: false,
            ..Default::default()
        };
        
        let metadata_opts = MetadataOptions::default();
        
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| Error::Cache(format!("Failed to probe format: {}", e)))?;
        
        let mut format = probed.format;
        let mut audio_meta = AudioMeta {
            format: AudioFormat::from_extension(extension),
            ..Default::default()
        };
        
        // Extract metadata from container
        if let Some(metadata) = format.metadata().current() {
            self.extract_tags(&mut audio_meta, metadata);
        }
        
        // Also check probed metadata
        if let Some(metadata) = probed.metadata.current() {
            self.extract_tags(&mut audio_meta, metadata);
        }
        
        // Get duration and codec info from track
        if let Some(track) = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL) {
            let params = &track.codec_params;
            
            if let Some(n_frames) = params.n_frames {
                if let Some(sample_rate) = params.sample_rate {
                    audio_meta.duration_ms = Some((n_frames as u64 * 1000) / sample_rate as u64);
                    audio_meta.sample_rate = Some(sample_rate);
                }
            }
            
            if let Some(bits_per_sample) = params.bits_per_sample {
                if let Some(sample_rate) = params.sample_rate {
                    if let Some(channels) = params.channels {
                        audio_meta.bitrate = Some(
                            bits_per_sample * sample_rate * channels.count() as u32 / 1000
                        );
                    }
                }
            }
        }
        
        debug!("Parsed metadata: {:?}", audio_meta);
        Ok(audio_meta)
    }
    
    fn extract_tags(&self, meta: &mut AudioMeta, metadata: &symphonia::core::meta::MetadataRevision) {
        use symphonia::core::meta::StandardTagKey;
        
        for tag in metadata.tags() {
            if let Some(std_key) = tag.std_key {
                let value = tag.value.to_string();
                match std_key {
                    StandardTagKey::TrackTitle => meta.title = Some(value),
                    StandardTagKey::Artist => meta.artist = Some(value),
                    StandardTagKey::Album => meta.album = Some(value),
                    StandardTagKey::AlbumArtist => meta.album_artist = Some(value),
                    StandardTagKey::Genre => meta.genre = Some(value),
                    StandardTagKey::TrackNumber => {
                        meta.track = value.split('/').next()
                            .and_then(|s| s.parse().ok());
                    }
                    StandardTagKey::DiscNumber => {
                        meta.disc = value.split('/').next()
                            .and_then(|s| s.parse().ok());
                    }
                    StandardTagKey::Date | StandardTagKey::ReleaseDate => {
                        meta.year = value.chars().take(4).collect::<String>()
                            .parse().ok();
                    }
                    StandardTagKey::Lyrics => {
                        meta.lyrics = Some(value);
                    }
                    StandardTagKey::Composer => {
                        meta.composer = Some(value);
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Default for MetadataParser {
    fn default() -> Self {
        Self::new()
    }
}
```

---

## Task 2: Cache Database (`musicfs-cache`)

### 2.1 Create `Cargo.toml`

```toml
[package]
name = "musicfs-cache"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
rusqlite = { workspace = true, features = ["bundled"] }
sled.workspace = true
tokio.workspace = true
tracing.workspace = true
thiserror.workspace = true
serde.workspace = true
rmp-serde.workspace = true
```

### 2.2 Create `src/lib.rs`

```rust
mod db;
mod metadata;

pub use db::Database;
pub use metadata::MetadataCache;
```

### 2.3 Create `src/schema.sql`

```sql
-- MusicFS Metadata Cache Schema
-- Per architecture.md section 4.3.6
-- NOTE: Chunk index stored in sled (chunks.sled/), NOT SQLite

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS files (
    id              INTEGER PRIMARY KEY,
    origin_id       TEXT NOT NULL,
    real_path       TEXT NOT NULL,
    virtual_path    TEXT NOT NULL,
    
    -- Audio metadata (FR-6.1-6.5)
    title           TEXT,
    artist          TEXT,
    album           TEXT,
    album_artist    TEXT,
    genre           TEXT,
    year            INTEGER,
    track           INTEGER,
    disc            INTEGER,
    duration_ms     INTEGER,
    bitrate         INTEGER,
    sample_rate     INTEGER,
    format          TEXT,
    
    -- Sync state
    origin_mtime    INTEGER NOT NULL,
    origin_size     INTEGER NOT NULL,
    content_hash    TEXT,       -- hex-encoded xxHash64
    chunk_manifest  BLOB,       -- msgpack: [(chunk_hash, offset, size)]
    last_sync       INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    
    UNIQUE(origin_id, real_path)
);

CREATE TABLE IF NOT EXISTS artwork (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    art_type    TEXT NOT NULL,      -- 'front', 'back', 'disc'
    chunk_hash  TEXT NOT NULL,       -- hex-encoded reference to CAS
    width       INTEGER,
    height      INTEGER,
    mime_type   TEXT,
    UNIQUE(file_id, art_type)
);

CREATE TABLE IF NOT EXISTS collections (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    query_json  TEXT NOT NULL,      -- smart collection query
    created_at  INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    updated_at  INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Indexes for performance (NFR-1.1, NFR-1.2)
CREATE INDEX IF NOT EXISTS idx_files_virtual ON files(virtual_path);
CREATE INDEX IF NOT EXISTS idx_files_artist_album ON files(artist, album);
CREATE INDEX IF NOT EXISTS idx_files_content_hash ON files(content_hash);
CREATE INDEX IF NOT EXISTS idx_files_real ON files(origin_id, real_path);  -- FR-7.3
CREATE INDEX IF NOT EXISTS idx_files_origin ON files(origin_id);
CREATE INDEX IF NOT EXISTS idx_files_last_sync ON files(last_sync);
CREATE INDEX IF NOT EXISTS idx_artwork_file ON artwork(file_id);
```

### 2.4 Create `src/db.rs`

```rust
use musicfs_core::{AudioMeta, ContentHash, Error, FileId, FileMeta, OriginId, RealPath, Result, VirtualPath};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

const SCHEMA: &str = include_str!("schema.sql");

/// SQLite database connection manager
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open or create database at path
    pub fn open(path: &Path) -> Result<Self> {
        info!("Opening database at {:?}", path);
        
        let conn = Connection::open(path)
            .map_err(|e| Error::Database(e.to_string()))?;
        
        // Execute schema
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Database(e.to_string()))?;
        
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
    
    /// Open in-memory database (for testing)
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(e.to_string()))?;
        
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Database(e.to_string()))?;
        
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
    
    /// Insert or update file metadata
    pub fn upsert_file(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
        virtual_path: &VirtualPath,
        audio_meta: &AudioMeta,
        origin_mtime: SystemTime,
        origin_size: u64,
    ) -> Result<FileId> {
        let conn = self.conn.lock().unwrap();
        
        let mtime_secs = origin_mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        
        conn.execute(
            r#"
            INSERT INTO files (
                origin_id, real_path, virtual_path,
                title, artist, album, album_artist, genre,
                   year, track, disc,
                duration_ms, bitrate, sample_rate, format,
                origin_mtime, origin_size
            ) VALUES (
                ?1, ?2, ?3,
                ?4, ?5, ?6, ?7, ?8,
                ?9, ?10, ?11,
                ?12, ?13, ?14, ?15,
                ?16, ?17
            )
            ON CONFLICT(origin_id, real_path) DO UPDATE SET
                virtual_path = excluded.virtual_path,
                title = excluded.title,
                artist = excluded.artist,
                album = excluded.album,
                album_artist = excluded.album_artist,
                genre = excluded.genre,
                year = excluded.year,
                track = excluded.track,
                disc = excluded.disc,
                duration_ms = excluded.duration_ms,
                bitrate = excluded.bitrate,
                sample_rate = excluded.sample_rate,
                format = excluded.format,
                origin_mtime = excluded.origin_mtime,
                origin_size = excluded.origin_size,
                last_sync = strftime('%s', 'now')
            "#,
            params![
                &origin_id.0,
                real_path.to_string_lossy(),
                virtual_path.as_str(),
                &audio_meta.title,
                &audio_meta.artist,
                &audio_meta.album,
                &audio_meta.album_artist,
                &audio_meta.genre,
                &audio_meta.year,
                &audio_meta.track,
                &audio_meta.disc,
                &audio_meta.duration_ms.map(|d| d as i64),
                &audio_meta.bitrate,
                &audio_meta.sample_rate,
                format!("{:?}", audio_meta.format),
                mtime_secs,
                origin_size as i64,
            ],
        ).map_err(|e| Error::Database(e.to_string()))?;
        
        let id = conn.last_insert_rowid();
        debug!("Upserted file {} with id {}", virtual_path.as_str(), id);
        
        Ok(FileId(id))
    }
    
    /// Get file by virtual path
    pub fn get_file_by_virtual_path(&self, path: &VirtualPath) -> Result<Option<FileMeta>> {
        let conn = self.conn.lock().unwrap();
        
        conn.query_row(
            r#"
            SELECT id, origin_id, real_path, virtual_path,
                   title, artist, album, album_artist, genre,
                year, track, disc,
                   duration_ms, bitrate, sample_rate, format,
                   origin_mtime, origin_size, content_hash
            FROM files
            WHERE virtual_path = ?1
            "#,
            params![path.as_str()],
            |row| {
                Ok(FileMeta {
                    id: FileId(row.get(0)?),
                    real_path: RealPath {
                        origin_id: OriginId(row.get(1)?),
                        path: PathBuf::from(row.get::<_, String>(2)?),
                    },
                    virtual_path: VirtualPath::new(row.get::<_, String>(3)?),
                    audio: Some(AudioMeta {
                        title: row.get(4)?,
                        artist: row.get(5)?,
                        album: row.get(6)?,
                        album_artist: row.get(7)?,
                        genre: row.get(8)?,
                        year: row.get(9)?,
                        track: row.get(10)?,
                        disc: row.get(11)?,
                        duration_ms: row.get::<_, Option<i64>>(12)?.map(|d| d as u64),
                        bitrate: row.get(13)?,
                        sample_rate: row.get(14)?,
                        format: musicfs_core::AudioFormat::Unknown, // TODO: parse
                    }),
                    size: row.get::<_, i64>(17)? as u64,
                    mtime: UNIX_EPOCH + std::time::Duration::from_secs(row.get::<_, i64>(16)? as u64),
                    content_hash: row.get::<_, Option<Vec<u8>>>(18)?
                        .map(|b| ContentHash(b.try_into().unwrap_or([0; 8]))),
                })
            },
        )
        .optional()
        .map_err(|e| Error::Database(e.to_string()))
    }
    
    /// Get file by ID
    pub fn get_file_by_id(&self, id: FileId) -> Result<Option<FileMeta>> {
        let conn = self.conn.lock().unwrap();
        
        conn.query_row(
            "SELECT virtual_path FROM files WHERE id = ?1",
            params![id.0],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| Error::Database(e.to_string()))?
        .map(|vp| self.get_file_by_virtual_path(&VirtualPath::new(vp)))
        .transpose()
        .map(|o| o.flatten())
    }
    
    /// List all files for an origin
    pub fn list_files(&self, origin_id: &OriginId) -> Result<Vec<FileMeta>> {
        let conn = self.conn.lock().unwrap();
        
        let mut stmt = conn.prepare(
            "SELECT virtual_path FROM files WHERE origin_id = ?1"
        ).map_err(|e| Error::Database(e.to_string()))?;
        
        let paths: Vec<String> = stmt
            .query_map(params![&origin_id.0], |row| row.get(0))
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        
        drop(stmt);
        drop(conn);
        
        paths
            .into_iter()
            .filter_map(|p| self.get_file_by_virtual_path(&VirtualPath::new(p)).ok().flatten())
            .collect::<Vec<_>>()
            .pipe(Ok)
    }
    
    /// Delete file by ID
    pub fn delete_file(&self, id: FileId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM files WHERE id = ?1", params![id.0])
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }
    
    /// Get file count
    pub fn file_count(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))
            .map(|c| c as u64)
            .map_err(|e| Error::Database(e.to_string()))
    }
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}
```

### 2.5 Create `src/metadata.rs`

```rust
use crate::db::Database;
use musicfs_core::{AudioMeta, FileMeta, OriginId, Result, VirtualPath};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

/// High-level metadata cache interface
pub struct MetadataCache {
    db: Arc<Database>,
}

impl MetadataCache {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
    
    /// Store file metadata
    pub fn store(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
        virtual_path: &VirtualPath,
        audio_meta: &AudioMeta,
        origin_mtime: SystemTime,
        origin_size: u64,
    ) -> Result<()> {
        self.db.upsert_file(
            origin_id,
            real_path,
            virtual_path,
            audio_meta,
            origin_mtime,
            origin_size,
        )?;
        Ok(())
    }
    
    /// Lookup by virtual path
    pub fn lookup(&self, path: &VirtualPath) -> Result<Option<FileMeta>> {
        self.db.get_file_by_virtual_path(path)
    }
    
    /// Check if file exists and is fresh
    pub fn is_fresh(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
        current_mtime: SystemTime,
    ) -> Result<bool> {
        // TODO: Compare mtime with cached value
        Ok(false)
    }
}
```

---

## Tests

### Unit Tests (`musicfs-metadata`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    
    #[test]
    fn test_parse_flac_metadata() {
        // Use a real FLAC file for testing
        // For CI, embed a small test file or use a fixture
        let parser = MetadataParser::new();
        
        // This would need a real file path
        // let meta = parser.parse_file(Path::new("test.flac")).unwrap();
        // assert!(meta.title.is_some());
    }
    
    #[test]
    fn test_audio_format_detection() {
        assert_eq!(AudioFormat::from_extension("flac"), AudioFormat::Flac);
        assert_eq!(AudioFormat::from_extension("mp3"), AudioFormat::Mp3);
        assert_eq!(AudioFormat::from_extension("opus"), AudioFormat::Opus);
    }
}
```

### Unit Tests (`musicfs-cache`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::{AudioFormat, AudioMeta, OriginId, VirtualPath};
    use std::time::UNIX_EPOCH;
    
    #[test]
    fn test_database_creation() {
        let db = Database::open_memory().unwrap();
        assert_eq!(db.file_count().unwrap(), 0);
    }
    
    #[test]
    fn test_upsert_and_retrieve() {
        let db = Database::open_memory().unwrap();
        
        let origin_id = OriginId::from("local");
        let real_path = Path::new("/music/test.flac");
        let virtual_path = VirtualPath::new("/Artist/Album/01 - Track.flac");
        let audio_meta = AudioMeta {
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            track: Some(1),
            format: AudioFormat::Flac,
            ..Default::default()
        };
        
        let id = db.upsert_file(
            &origin_id,
            real_path,
            &virtual_path,
            &audio_meta,
            UNIX_EPOCH,
            1000,
        ).unwrap();
        
        let retrieved = db.get_file_by_virtual_path(&virtual_path).unwrap().unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.audio.as_ref().unwrap().title, Some("Track".to_string()));
    }
    
    #[test]
    fn test_upsert_updates_existing() {
        let db = Database::open_memory().unwrap();
        
        let origin_id = OriginId::from("local");
        let real_path = Path::new("/music/test.flac");
        let virtual_path = VirtualPath::new("/Artist/Album/01 - Track.flac");
        
        // First insert
        let meta1 = AudioMeta {
            title: Some("Original".to_string()),
            ..Default::default()
        };
        db.upsert_file(&origin_id, real_path, &virtual_path, &meta1, UNIX_EPOCH, 1000).unwrap();
        
        // Update
        let meta2 = AudioMeta {
            title: Some("Updated".to_string()),
            ..Default::default()
        };
        db.upsert_file(&origin_id, real_path, &virtual_path, &meta2, UNIX_EPOCH, 1000).unwrap();
        
        // Should still be 1 file
        assert_eq!(db.file_count().unwrap(), 1);
        
        // Title should be updated
        let retrieved = db.get_file_by_virtual_path(&virtual_path).unwrap().unwrap();
        assert_eq!(retrieved.audio.as_ref().unwrap().title, Some("Updated".to_string()));
    }
    
    #[test]
    fn test_metadata_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        
        // Create and populate
        {
            let db = Database::open(&db_path).unwrap();
            db.upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Test.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            ).unwrap();
        }
        
        // Reopen and verify
        {
            let db = Database::open(&db_path).unwrap();
            assert_eq!(db.file_count().unwrap(), 1);
        }
    }
}
```

---

## Exit Criteria

- [ ] Parse FLAC metadata (title, artist, album, track, duration)
- [ ] Parse MP3 metadata (ID3v2 and ID3v1 fallback)
- [ ] Parse Opus/Vorbis comments
- [ ] Parse M4A/AAC metadata
- [ ] Handle missing metadata gracefully (FR-6.5)
- [ ] SQLite schema creates all tables
- [ ] Metadata persists across daemon restarts (FR-7.4)
- [ ] Upsert correctly updates existing records

---

## Verification Commands

```bash
# Run metadata tests
cargo test -p musicfs-metadata

# Run cache tests
cargo test -p musicfs-cache

# Test with real audio file
cargo run --example parse_metadata -- /path/to/test.flac
```

---

## Next Week

Week 3 will implement the virtual path resolver and tree cache, connecting metadata to the FUSE operations.
