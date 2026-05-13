use musicfs_core::{
    AudioFormat, AudioMeta, ContentHash, Error, FileId, FileMeta, OriginId, RealPath, Result,
    VirtualPath,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

const SCHEMA: &str = include_str!("schema.sql");

pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        debug!(?path, "Opening database");

        let conn =
            Connection::open(path).map_err(|e| Error::Database(format!("open failed: {}", e)))?;

        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Database(format!("schema init failed: {}", e)))?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        let count = db.file_count().unwrap_or(0);
        info!(path = ?path, file_count = count, "Database opened");
        Ok(db)
    }

    pub fn open_with_integrity_check(path: &Path) -> Result<Self> {
        debug!(?path, "Opening database with integrity check");

        let conn =
            Connection::open(path).map_err(|e| Error::Database(format!("open failed: {}", e)))?;

        let integrity: String = conn
            .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
            .map_err(|e| Error::Database(format!("integrity check failed: {}", e)))?;

        if integrity != "ok" {
            warn!(path = ?path, result = %integrity, "Database integrity check failed");
            return Err(Error::DatabaseCorrupted(format!(
                "integrity check failed: {}",
                integrity
            )));
        }

        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Database(format!("schema init failed: {}", e)))?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        let count = db.file_count().unwrap_or(0);
        info!(path = ?path, file_count = count, "Database opened (integrity verified)");
        Ok(db)
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("open_in_memory failed: {}", e)))?;

        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Database(format!("schema init failed: {}", e)))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

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
        )
        .map_err(|e| Error::Database(format!("upsert failed: {}", e)))?;

        let id = conn.last_insert_rowid();
        debug!(id, vpath = virtual_path.as_str(), "Upserted file");

        Ok(FileId(id))
    }

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
                let format_str: Option<String> = row.get(15)?;
                let format = format_str
                    .as_deref()
                    .map(parse_audio_format)
                    .unwrap_or(AudioFormat::Unknown);

                let content_hash: Option<String> = row.get(18)?;

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
                        format,
                    }),
                    size: row.get::<_, i64>(17)? as u64,
                    mtime: UNIX_EPOCH + Duration::from_secs(row.get::<_, i64>(16)? as u64),
                    content_hash: content_hash.and_then(|s| parse_content_hash(&s)),
                })
            },
        )
        .optional()
        .map_err(|e| Error::Database(format!("query failed: {}", e)))
    }

    pub fn get_file_by_id(&self, id: FileId) -> Result<Option<FileMeta>> {
        let conn = self.conn.lock().unwrap();

        let vpath: Option<String> = conn
            .query_row(
                "SELECT virtual_path FROM files WHERE id = ?1",
                params![id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Database(format!("query failed: {}", e)))?;

        drop(conn);

        match vpath {
            Some(p) => self.get_file_by_virtual_path(&VirtualPath::new(p)),
            None => Ok(None),
        }
    }

    pub fn list_files_by_origin(&self, origin_id: &OriginId) -> Result<Vec<VirtualPath>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn
            .prepare("SELECT virtual_path FROM files WHERE origin_id = ?1")
            .map_err(|e| Error::Database(format!("prepare failed: {}", e)))?;

        let paths: Vec<VirtualPath> = stmt
            .query_map(params![&origin_id.0], |row| {
                Ok(VirtualPath::new(row.get::<_, String>(0)?))
            })
            .map_err(|e| Error::Database(format!("query failed: {}", e)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(paths)
    }

    pub fn delete_file(&self, id: FileId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM files WHERE id = ?1", params![id.0])
            .map_err(|e| Error::Database(format!("delete failed: {}", e)))?;
        Ok(())
    }

    pub fn file_count(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))
            .map(|c| c as u64)
            .map_err(|e| Error::Database(format!("count failed: {}", e)))
    }

    pub fn update_content_hash(&self, id: FileId, hash: &ContentHash) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE files SET content_hash = ?1 WHERE id = ?2",
            params![hash.to_hex(), id.0],
        )
        .map_err(|e| Error::Database(format!("update hash failed: {}", e)))?;
        Ok(())
    }

    pub fn get_mtime_by_real_path(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
    ) -> Result<Option<SystemTime>> {
        let conn = self.conn.lock().unwrap();

        conn.query_row(
            "SELECT origin_mtime FROM files WHERE origin_id = ?1 AND real_path = ?2",
            params![&origin_id.0, real_path.to_string_lossy()],
            |row| {
                let mtime_secs: i64 = row.get(0)?;
                Ok(UNIX_EPOCH + Duration::from_secs(mtime_secs as u64))
            },
        )
        .optional()
        .map_err(|e| Error::Database(format!("query mtime failed: {}", e)))
    }
}

fn parse_audio_format(s: &str) -> AudioFormat {
    match s {
        "Flac" => AudioFormat::Flac,
        "Mp3" => AudioFormat::Mp3,
        "Aac" => AudioFormat::Aac,
        "Opus" => AudioFormat::Opus,
        "Vorbis" => AudioFormat::Vorbis,
        "Wav" => AudioFormat::Wav,
        "Alac" => AudioFormat::Alac,
        _ => AudioFormat::Unknown,
    }
}

fn parse_content_hash(hex: &str) -> Option<ContentHash> {
    if hex.len() != 16 {
        return None;
    }
    let mut bytes = [0u8; 8];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        if i >= 8 {
            break;
        }
        let s = std::str::from_utf8(chunk).ok()?;
        bytes[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(ContentHash(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let id = db
            .upsert_file(
                &origin_id,
                real_path,
                &virtual_path,
                &audio_meta,
                UNIX_EPOCH,
                1000,
            )
            .unwrap();

        let retrieved = db.get_file_by_virtual_path(&virtual_path).unwrap().unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(
            retrieved.audio.as_ref().unwrap().title,
            Some("Track".to_string())
        );
    }

    #[test]
    fn test_upsert_updates_existing() {
        let db = Database::open_memory().unwrap();

        let origin_id = OriginId::from("local");
        let real_path = Path::new("/music/test.flac");
        let virtual_path = VirtualPath::new("/Artist/Album/01 - Track.flac");

        let meta1 = AudioMeta {
            title: Some("Original".to_string()),
            ..Default::default()
        };
        db.upsert_file(
            &origin_id,
            real_path,
            &virtual_path,
            &meta1,
            UNIX_EPOCH,
            1000,
        )
        .unwrap();

        let meta2 = AudioMeta {
            title: Some("Updated".to_string()),
            ..Default::default()
        };
        db.upsert_file(
            &origin_id,
            real_path,
            &virtual_path,
            &meta2,
            UNIX_EPOCH,
            1000,
        )
        .unwrap();

        assert_eq!(db.file_count().unwrap(), 1);

        let retrieved = db.get_file_by_virtual_path(&virtual_path).unwrap().unwrap();
        assert_eq!(
            retrieved.audio.as_ref().unwrap().title,
            Some("Updated".to_string())
        );
    }

    #[test]
    fn test_metadata_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        {
            let db = Database::open(&db_path).unwrap();
            db.upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Test.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();
        }

        {
            let db = Database::open(&db_path).unwrap();
            assert_eq!(db.file_count().unwrap(), 1);
        }
    }

    #[test]
    fn test_delete_file() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Test.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        assert_eq!(db.file_count().unwrap(), 1);
        db.delete_file(id).unwrap();
        assert_eq!(db.file_count().unwrap(), 0);
    }

    #[test]
    fn test_list_files_by_origin() {
        let db = Database::open_memory().unwrap();
        let origin = OriginId::from("local");

        db.upsert_file(
            &origin,
            Path::new("/a.flac"),
            &VirtualPath::new("/A.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        db.upsert_file(
            &origin,
            Path::new("/b.flac"),
            &VirtualPath::new("/B.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        let paths = db.list_files_by_origin(&origin).unwrap();
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_content_hash_update() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Test.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        let hash = ContentHash::from_bytes(b"test data");
        db.update_content_hash(id, &hash).unwrap();

        let retrieved = db
            .get_file_by_virtual_path(&VirtualPath::new("/Test.flac"))
            .unwrap()
            .unwrap();
        assert!(retrieved.content_hash.is_some());
    }
}
