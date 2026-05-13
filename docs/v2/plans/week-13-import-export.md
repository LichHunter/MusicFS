# Week 13: Import & Export

**Phase**: 5 - P1 Feature Completion  
**Goal**: Import metadata from existing library managers, export library data  
**Requirements**: FR-22.1-22.3

---

## Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Beets database import | musicfs-import | `beets.rs` | FR-22.1 |
| iTunes/Apple Music import | musicfs-import | `itunes.rs` | FR-22.2 |
| Library export | musicfs-import | `export.rs` | FR-22.3 |
| Import CLI | musicfs-cli | `import.rs` | All |

---

## Task 1: Create `musicfs-import` Crate

### 1.1 `Cargo.toml`

```toml
[package]
name = "musicfs-import"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
musicfs-cache = { path = "../musicfs-cache" }
rusqlite = { workspace = true, features = ["bundled"] }
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
plist = "1.5"  # For iTunes XML parsing
tokio.workspace = true
tracing.workspace = true
thiserror.workspace = true
csv = "1.3"
url = "2.4"
percent-encoding = "2.3"
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
tempfile.workspace = true
```

### 1.2 `src/lib.rs`

```rust
pub mod beets;
pub mod itunes;
pub mod export;

use musicfs_core::Result;

/// Common import result
#[derive(Debug, Default)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<ImportError>,
}

#[derive(Debug)]
pub struct ImportError {
    pub path: String,
    pub reason: String,
}

/// Import progress callback
pub type ProgressCallback = Box<dyn Fn(ImportProgress) + Send>;

#[derive(Debug, Clone)]
pub struct ImportProgress {
    pub current: usize,
    pub total: usize,
    pub current_file: String,
}
```

---

## Task 2: Beets Database Import (`musicfs-import/src/beets.rs`)

```rust
use rusqlite::{Connection, params};
use std::path::Path;

/// Beets database schema (simplified)
/// Full schema: https://beets.readthedocs.io/en/stable/dev/db.html
#[derive(Debug)]
pub struct BeetsItem {
    pub id: i64,
    pub path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track: Option<i32>,
    pub disc: Option<i32>,
    pub length: Option<f64>,
    pub bitrate: Option<i32>,
    pub sample_rate: Option<i32>,
    pub format: Option<String>,
    pub mb_trackid: Option<String>,
    pub mb_albumid: Option<String>,
    pub mb_artistid: Option<String>,
    pub mtime: f64,
}

pub struct BeetsImporter {
    beets_db: Connection,
    target_db: Arc<Database>,
}

impl BeetsImporter {
    /// Open beets database for import (FR-22.1)
    pub fn new(beets_db_path: &Path, target_db: Arc<Database>) -> Result<Self, ImportError> {
        let conn = Connection::open_with_flags(
            beets_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        
        // Verify this is a beets database
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        
        if !tables.contains(&"items".to_string()) {
            return Err(ImportError::InvalidDatabase("Not a beets database"));
        }
        
        Ok(Self {
            beets_db: conn,
            target_db,
        })
    }
    
    /// Count items to import
    pub fn count_items(&self) -> Result<usize, ImportError> {
        self.beets_db
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .map_err(Into::into)
    }
    
    /// Import all items with progress callback
    pub fn import_all(&self, progress: Option<ProgressCallback>) -> Result<ImportResult, ImportError> {
        let total = self.count_items()?;
        let mut result = ImportResult::default();
        
        let mut stmt = self.beets_db.prepare(r#"
            SELECT id, path, title, artist, album, albumartist, genre,
                   year, track, disc, length, bitrate, samplerate, format,
                   mb_trackid, mb_albumid, mb_artistid, mtime
            FROM items
        "#)?;
        
        let items = stmt.query_map([], |row| {
            Ok(BeetsItem {
                id: row.get(0)?,
                path: row.get(1)?,
                title: row.get(2)?,
                artist: row.get(3)?,
                album: row.get(4)?,
                album_artist: row.get(5)?,
                genre: row.get(6)?,
                year: row.get(7)?,
                track: row.get(8)?,
                disc: row.get(9)?,
                length: row.get(10)?,
                bitrate: row.get(11)?,
                sample_rate: row.get(12)?,
                format: row.get(13)?,
                mb_trackid: row.get(14)?,
                mb_albumid: row.get(15)?,
                mb_artistid: row.get(16)?,
                mtime: row.get(17)?,
            })
        })?;
        
        for (idx, item) in items.enumerate() {
            match item {
                Ok(item) => {
                    if let Some(ref cb) = progress {
                        cb(ImportProgress {
                            current: idx + 1,
                            total,
                            current_file: item.path.clone(),
                        });
                    }
                    
                    match self.import_item(&item) {
                        Ok(_) => result.imported += 1,
                        Err(e) => {
                            result.errors.push(ImportError {
                                path: item.path,
                                reason: e.to_string(),
                            });
                        }
                    }
                }
                Err(e) => {
                    result.skipped += 1;
                    result.errors.push(ImportError {
                        path: format!("item_{}", idx),
                        reason: e.to_string(),
                    });
                }
            }
        }
        
        Ok(result)
    }
    
    fn import_item(&self, item: &BeetsItem) -> Result<(), ImportError> {
        let path = Path::new(&item.path);
        
        // Convert to our AudioMeta
        let audio_meta = AudioMeta {
            title: item.title.clone(),
            artist: item.artist.clone(),
            album: item.album.clone(),
            album_artist: item.album_artist.clone(),
            genre: item.genre.clone(),
            year: item.year.map(|y| y as u32),
            track: item.track.map(|t| t as u32),
            disc: item.disc.map(|d| d as u32),
            duration_ms: item.length.map(|l| (l * 1000.0) as u64),
            bitrate: item.bitrate.map(|b| b as u32),
            sample_rate: item.sample_rate.map(|s| s as u32),
            format: AudioFormat::from_extension(
                path.extension().and_then(|e| e.to_str()).unwrap_or("")
            ),
            ..Default::default()
        };
        
        // Generate virtual path using our resolver
        let virtual_path = VirtualPath::from_metadata(&audio_meta, path);
        
        // Import to our database
        self.target_db.upsert_file(
            &OriginId::from("beets-import"),
            path,
            &virtual_path,
            &audio_meta,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(item.mtime),
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        )?;
        
        Ok(())
    }
}
```

---

## Task 3: iTunes/Apple Music Import (`musicfs-import/src/itunes.rs`)

```rust
use plist::Value;
use std::collections::HashMap;
use url::Url;

/// iTunes Library XML format
#[derive(Debug)]
pub struct ItunesTrack {
    pub track_id: u64,
    pub name: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub total_time: Option<u64>,  // milliseconds
    pub bit_rate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub location: Option<String>,  // file:// URL
    pub date_added: Option<String>,
}

pub struct ItunesImporter {
    tracks: Vec<ItunesTrack>,
    target_db: Arc<Database>,
}

impl ItunesImporter {
    /// Parse iTunes Library.xml (FR-22.2)
    pub fn from_xml(xml_path: &Path, target_db: Arc<Database>) -> Result<Self, ImportError> {
        let file = std::fs::File::open(xml_path)?;
        let plist: Value = plist::from_reader(file)?;
        
        let dict = plist.as_dictionary()
            .ok_or(ImportError::InvalidFormat("Expected dictionary at root"))?;
        
        let tracks_dict = dict.get("Tracks")
            .and_then(|v| v.as_dictionary())
            .ok_or(ImportError::InvalidFormat("Missing Tracks dictionary"))?;
        
        let mut tracks = Vec::new();
        
        for (_, track_value) in tracks_dict {
            if let Some(track_dict) = track_value.as_dictionary() {
                tracks.push(Self::parse_track(track_dict)?);
            }
        }
        
        Ok(Self { tracks, target_db })
    }
    
    fn parse_track(dict: &plist::Dictionary) -> Result<ItunesTrack, ImportError> {
        Ok(ItunesTrack {
            track_id: dict.get("Track ID")
                .and_then(|v| v.as_unsigned_integer())
                .unwrap_or(0),
            name: dict.get("Name").and_then(|v| v.as_string()).map(String::from),
            artist: dict.get("Artist").and_then(|v| v.as_string()).map(String::from),
            album: dict.get("Album").and_then(|v| v.as_string()).map(String::from),
            album_artist: dict.get("Album Artist").and_then(|v| v.as_string()).map(String::from),
            genre: dict.get("Genre").and_then(|v| v.as_string()).map(String::from),
            year: dict.get("Year").and_then(|v| v.as_unsigned_integer()).map(|v| v as u32),
            track_number: dict.get("Track Number").and_then(|v| v.as_unsigned_integer()).map(|v| v as u32),
            disc_number: dict.get("Disc Number").and_then(|v| v.as_unsigned_integer()).map(|v| v as u32),
            total_time: dict.get("Total Time").and_then(|v| v.as_unsigned_integer()),
            bit_rate: dict.get("Bit Rate").and_then(|v| v.as_unsigned_integer()).map(|v| v as u32),
            sample_rate: dict.get("Sample Rate").and_then(|v| v.as_unsigned_integer()).map(|v| v as u32),
            location: dict.get("Location").and_then(|v| v.as_string()).map(String::from),
            date_added: dict.get("Date Added").and_then(|v| v.as_string()).map(String::from),
        })
    }
    
    /// Convert file:// URL to path
    fn url_to_path(url_str: &str) -> Option<PathBuf> {
        Url::parse(url_str).ok()
            .filter(|u| u.scheme() == "file")
            .and_then(|u| u.to_file_path().ok())
    }
    
    pub fn count_tracks(&self) -> usize {
        self.tracks.len()
    }
    
    /// Import all tracks
    pub fn import_all(&self, progress: Option<ProgressCallback>) -> Result<ImportResult, ImportError> {
        let total = self.tracks.len();
        let mut result = ImportResult::default();
        
        for (idx, track) in self.tracks.iter().enumerate() {
            if let Some(ref cb) = progress {
                cb(ImportProgress {
                    current: idx + 1,
                    total,
                    current_file: track.name.clone().unwrap_or_default(),
                });
            }
            
            // Skip tracks without location
            let Some(ref location) = track.location else {
                result.skipped += 1;
                continue;
            };
            
            let Some(path) = Self::url_to_path(location) else {
                result.skipped += 1;
                result.errors.push(ImportError {
                    path: location.clone(),
                    reason: "Invalid file URL".to_string(),
                });
                continue;
            };
            
            match self.import_track(track, &path) {
                Ok(_) => result.imported += 1,
                Err(e) => {
                    result.errors.push(ImportError {
                        path: path.display().to_string(),
                        reason: e.to_string(),
                    });
                }
            }
        }
        
        Ok(result)
    }
    
    fn import_track(&self, track: &ItunesTrack, path: &Path) -> Result<(), ImportError> {
        let audio_meta = AudioMeta {
            title: track.name.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            album_artist: track.album_artist.clone(),
            genre: track.genre.clone(),
            year: track.year,
            track: track.track_number,
            disc: track.disc_number,
            duration_ms: track.total_time,
            bitrate: track.bit_rate,
            sample_rate: track.sample_rate,
            format: AudioFormat::from_extension(
                path.extension().and_then(|e| e.to_str()).unwrap_or("")
            ),
            ..Default::default()
        };
        
        let virtual_path = VirtualPath::from_metadata(&audio_meta, path);
        
        let mtime = std::fs::metadata(path)
            .map(|m| m.modified().unwrap_or(std::time::UNIX_EPOCH))
            .unwrap_or(std::time::UNIX_EPOCH);
        
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        
        self.target_db.upsert_file(
            &OriginId::from("itunes-import"),
            path,
            &virtual_path,
            &audio_meta,
            mtime,
            size,
        )?;
        
        Ok(())
    }
}
```

---

## Task 4: Library Export (`musicfs-import/src/export.rs`)

```rust
use csv::Writer;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ExportedTrack {
    pub virtual_path: String,
    pub real_path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration_ms: Option<u64>,
    pub format: String,
    pub musicbrainz_id: Option<String>,
}

pub struct LibraryExporter {
    db: Arc<Database>,
}

impl LibraryExporter {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
    
    /// Export library to CSV (FR-22.3)
    pub fn export_csv(&self, output: &Path) -> Result<usize, ExportError> {
        let files = self.db.list_all_files()?;
        let mut writer = Writer::from_path(output)?;
        
        let mut count = 0;
        for file in files {
            let audio = file.audio.as_ref();
            
            writer.serialize(ExportedTrack {
                virtual_path: file.virtual_path.as_str().to_string(),
                real_path: file.real_path.path.display().to_string(),
                title: audio.and_then(|a| a.title.clone()).unwrap_or_default(),
                artist: audio.and_then(|a| a.artist.clone()).unwrap_or_default(),
                album: audio.and_then(|a| a.album.clone()).unwrap_or_default(),
                album_artist: audio.and_then(|a| a.album_artist.clone()).unwrap_or_default(),
                genre: audio.and_then(|a| a.genre.clone()).unwrap_or_default(),
                year: audio.and_then(|a| a.year),
                track: audio.and_then(|a| a.track),
                disc: audio.and_then(|a| a.disc),
                duration_ms: audio.and_then(|a| a.duration_ms),
                format: audio.map(|a| format!("{:?}", a.format)).unwrap_or_default(),
                musicbrainz_id: None,  // TODO: Include if enriched
            })?;
            
            count += 1;
        }
        
        writer.flush()?;
        Ok(count)
    }
    
    /// Export library to JSON
    pub fn export_json(&self, output: &Path) -> Result<usize, ExportError> {
        let files = self.db.list_all_files()?;
        
        let tracks: Vec<ExportedTrack> = files.iter()
            .map(|file| {
                let audio = file.audio.as_ref();
                ExportedTrack {
                    virtual_path: file.virtual_path.as_str().to_string(),
                    real_path: file.real_path.path.display().to_string(),
                    title: audio.and_then(|a| a.title.clone()).unwrap_or_default(),
                    artist: audio.and_then(|a| a.artist.clone()).unwrap_or_default(),
                    album: audio.and_then(|a| a.album.clone()).unwrap_or_default(),
                    album_artist: audio.and_then(|a| a.album_artist.clone()).unwrap_or_default(),
                    genre: audio.and_then(|a| a.genre.clone()).unwrap_or_default(),
                    year: audio.and_then(|a| a.year),
                    track: audio.and_then(|a| a.track),
                    disc: audio.and_then(|a| a.disc),
                    duration_ms: audio.and_then(|a| a.duration_ms),
                    format: audio.map(|a| format!("{:?}", a.format)).unwrap_or_default(),
                    musicbrainz_id: None,
                }
            })
            .collect();
        
        let json = serde_json::to_string_pretty(&tracks)?;
        std::fs::write(output, json)?;
        
        Ok(tracks.len())
    }
    
    /// Export to M3U playlist format
    pub fn export_m3u(&self, output: &Path, base_path: Option<&Path>) -> Result<usize, ExportError> {
        let files = self.db.list_all_files()?;
        
        let mut content = String::from("#EXTM3U\n");
        
        for file in &files {
            let duration = file.audio.as_ref()
                .and_then(|a| a.duration_ms)
                .map(|d| d / 1000)
                .unwrap_or(0);
            
            let title = file.audio.as_ref()
                .and_then(|a| a.title.clone())
                .unwrap_or_else(|| file.virtual_path.as_str().to_string());
            
            let artist = file.audio.as_ref()
                .and_then(|a| a.artist.clone())
                .unwrap_or_default();
            
            content.push_str(&format!(
                "#EXTINF:{},{} - {}\n",
                duration, artist, title
            ));
            
            // Use virtual path relative to base, or absolute real path
            let path = if let Some(base) = base_path {
                base.join(file.virtual_path.as_str().trim_start_matches('/'))
                    .display().to_string()
            } else {
                file.real_path.path.display().to_string()
            };
            
            content.push_str(&path);
            content.push('\n');
        }
        
        std::fs::write(output, content)?;
        Ok(files.len())
    }
}
```

---

## Task 5: Import CLI Commands (`musicfs-cli/src/import.rs`)

```rust
#[derive(Subcommand)]
pub enum ImportCommand {
    /// Import from beets database
    Beets {
        /// Path to beets library.db
        #[arg(short, long)]
        db: PathBuf,
    },
    
    /// Import from iTunes Library.xml
    Itunes {
        /// Path to iTunes Library.xml
        #[arg(short, long)]
        xml: PathBuf,
    },
    
    /// Export library
    Export {
        /// Output file path
        #[arg(short, long)]
        output: PathBuf,
        
        /// Format: csv, json, m3u
        #[arg(short, long, default_value = "csv")]
        format: String,
    },
}

pub async fn handle_import(cmd: ImportCommand, db: Arc<Database>) -> Result<()> {
    match cmd {
        ImportCommand::Beets { db: beets_path } => {
            println!("Importing from beets database: {:?}", beets_path);
            
            let importer = BeetsImporter::new(&beets_path, db)?;
            let total = importer.count_items()?;
            println!("Found {} items to import", total);
            
            let pb = ProgressBar::new(total as u64);
            let result = importer.import_all(Some(Box::new(move |p| {
                pb.set_position(p.current as u64);
            })))?;
            
            println!("\nImport complete:");
            println!("  Imported: {}", result.imported);
            println!("  Skipped:  {}", result.skipped);
            println!("  Errors:   {}", result.errors.len());
        }
        
        ImportCommand::Itunes { xml } => {
            println!("Importing from iTunes Library: {:?}", xml);
            
            let importer = ItunesImporter::from_xml(&xml, db)?;
            let total = importer.count_tracks();
            println!("Found {} tracks to import", total);
            
            let pb = ProgressBar::new(total as u64);
            let result = importer.import_all(Some(Box::new(move |p| {
                pb.set_position(p.current as u64);
            })))?;
            
            println!("\nImport complete:");
            println!("  Imported: {}", result.imported);
            println!("  Skipped:  {}", result.skipped);
            println!("  Errors:   {}", result.errors.len());
        }
        
        ImportCommand::Export { output, format } => {
            let exporter = LibraryExporter::new(db);
            
            let count = match format.as_str() {
                "csv" => exporter.export_csv(&output)?,
                "json" => exporter.export_json(&output)?,
                "m3u" => exporter.export_m3u(&output, None)?,
                _ => return Err(anyhow::anyhow!("Unknown format: {}", format)),
            };
            
            println!("Exported {} tracks to {:?}", count, output);
        }
    }
    
    Ok(())
}
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_beets_import_valid` | Integration | Beets database parsing (FR-22.1) |
| `test_beets_import_missing_fields` | Unit | Handle incomplete metadata |
| `test_itunes_xml_parsing` | Unit | iTunes XML parsing (FR-22.2) |
| `test_itunes_url_to_path` | Unit | file:// URL conversion |
| `test_itunes_import_tracks` | Integration | Full iTunes import |
| `test_export_csv` | Unit | CSV export (FR-22.3) |
| `test_export_json` | Unit | JSON export |
| `test_export_m3u` | Unit | M3U playlist export |
| `test_import_preserves_musicbrainz_ids` | Integration | External IDs preserved |
| `test_import_deduplication` | Integration | No duplicates on re-import |

---

## Exit Criteria

- [ ] Beets database import works with real beets.db
- [ ] iTunes Library.xml import parses all tracks
- [ ] CSV/JSON/M3U export generates valid files
- [ ] Progress reporting works during import
- [ ] Errors are reported without crashing
- [ ] Import is idempotent (re-import updates, doesn't duplicate)
- [ ] MusicBrainz IDs from beets are preserved

---

## Architecture Alignment

Per requirements.md:
- FR-22.1: Import from beets database ✓
- FR-22.2: Import from iTunes/Apple Music ✓
- FR-22.3: Export library metadata ✓
