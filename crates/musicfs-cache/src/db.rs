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
        let file_id = if id == 0 {
            conn.query_row(
                "SELECT id FROM files WHERE origin_id = ?1 AND real_path = ?2",
                params![&origin_id.0, real_path.to_string_lossy()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| Error::Database(format!("failed to get file id after upsert: {}", e)))?
        } else {
            id
        };
        debug!(id = file_id, vpath = virtual_path.as_str(), "Upserted file");

        Ok(FileId(file_id))
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
                        ..Default::default()
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

    pub fn path_exists(&self, path: &VirtualPath) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE virtual_path = ?1",
                params![path.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(format!("path_exists query failed: {}", e)))?;
        Ok(count > 0)
    }

    pub fn update_virtual_path(&self, id: FileId, new_path: &VirtualPath) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute(
                "UPDATE files SET virtual_path = ?1 WHERE id = ?2",
                params![new_path.as_str(), id.0],
            )
            .map_err(|e| Error::Database(format!("update_virtual_path failed: {}", e)))?;

        if rows == 0 {
            return Err(Error::FileNotFound(format!("file id {} not found", id.0)));
        }
        debug!(
            id = id.0,
            new_path = new_path.as_str(),
            "updated virtual path"
        );
        Ok(())
    }

    pub fn rename_directory(&self, old_prefix: &str, new_prefix: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();

        let pattern = format!("{}%", old_prefix);
        let old_len = old_prefix.len();

        let rows = conn
            .execute(
                "UPDATE files SET virtual_path = ?1 || substr(virtual_path, ?2) WHERE virtual_path LIKE ?3",
                params![new_prefix, old_len as i64 + 1, pattern],
            )
            .map_err(|e| Error::Database(format!("rename_directory failed: {}", e)))?;

        debug!(old_prefix, new_prefix, rows, "renamed directory paths");
        Ok(rows as u64)
    }

    pub fn get_files_by_prefix(&self, prefix: &str) -> Result<Vec<(FileId, VirtualPath)>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("{}%", prefix);

        let mut stmt = conn
            .prepare("SELECT id, virtual_path FROM files WHERE virtual_path LIKE ?1")
            .map_err(|e| Error::Database(format!("prepare failed: {}", e)))?;

        let files: Vec<(FileId, VirtualPath)> = stmt
            .query_map(params![pattern], |row| {
                Ok((
                    FileId(row.get(0)?),
                    VirtualPath::new(row.get::<_, String>(1)?),
                ))
            })
            .map_err(|e| Error::Database(format!("query failed: {}", e)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(files)
    }

    pub fn insert_directory(&self, path: &VirtualPath) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO directories (path) VALUES (?1)",
            params![path.as_str()],
        )
        .map_err(|e| Error::Database(format!("insert_directory failed: {}", e)))?;
        debug!(path = path.as_str(), "inserted directory");
        Ok(())
    }

    pub fn delete_directory(&self, path: &VirtualPath) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM directories WHERE path = ?1",
            params![path.as_str()],
        )
        .map_err(|e| Error::Database(format!("delete_directory failed: {}", e)))?;
        Ok(())
    }

    pub fn rename_directories(&self, old_prefix: &str, new_prefix: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("{}%", old_prefix);
        let old_len = old_prefix.len();

        let rows = conn
            .execute(
                "UPDATE directories SET path = ?1 || substr(path, ?2) WHERE path LIKE ?3",
                params![new_prefix, old_len as i64 + 1, pattern],
            )
            .map_err(|e| Error::Database(format!("rename_directories failed: {}", e)))?;

        debug!(old_prefix, new_prefix, rows, "renamed directory paths");
        Ok(rows as u64)
    }

    pub fn list_directories(&self) -> Result<Vec<VirtualPath>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn
            .prepare("SELECT path FROM directories ORDER BY path")
            .map_err(|e| Error::Database(format!("prepare failed: {}", e)))?;

        let dirs: Vec<VirtualPath> = stmt
            .query_map([], |row| Ok(VirtualPath::new(row.get::<_, String>(0)?)))
            .map_err(|e| Error::Database(format!("query failed: {}", e)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(dirs)
    }

    pub fn get_file_by_real_path(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
    ) -> Result<Option<VirtualPath>> {
        let conn = self.conn.lock().unwrap();

        conn.query_row(
            "SELECT virtual_path FROM files WHERE origin_id = ?1 AND real_path = ?2",
            params![&origin_id.0, real_path.to_string_lossy()],
            |row| Ok(VirtualPath::new(row.get::<_, String>(0)?)),
        )
        .optional()
        .map_err(|e| Error::Database(format!("query failed: {}", e)))
    }

    pub fn mark_trashed(&self, id: FileId, original_path: &VirtualPath) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute(
                "UPDATE files SET trashed = 1, original_path = ?1, trashed_at = strftime('%s', 'now') WHERE id = ?2",
                params![original_path.as_str(), id.0],
            )
            .map_err(|e| Error::Database(format!("mark_trashed failed: {}", e)))?;

        if rows == 0 {
            return Err(Error::FileNotFound(format!("file id {} not found", id.0)));
        }
        debug!(
            id = id.0,
            original_path = original_path.as_str(),
            "marked file as trashed"
        );
        Ok(())
    }

    pub fn unmark_trashed(&self, id: FileId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE files SET trashed = 0, original_path = NULL, trashed_at = NULL WHERE id = ?1",
            params![id.0],
        )
        .map_err(|e| Error::Database(format!("unmark_trashed failed: {}", e)))?;
        debug!(id = id.0, "unmarked file as trashed");
        Ok(())
    }

    pub fn list_trashed(&self, filter: &TrashedFilter) -> Result<Vec<TrashedFile>> {
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from(
            "SELECT id, virtual_path, original_path, trashed_at, origin_id FROM files WHERE trashed = 1",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref origin) = filter.origin_id {
            sql.push_str(" AND origin_id = ?");
            params_vec.push(Box::new(origin.0.clone()));
        }

        if let Some(ref prefix) = filter.path_prefix {
            sql.push_str(" AND original_path LIKE ?");
            params_vec.push(Box::new(format!("{}%", prefix)));
        }

        if let Some(since) = filter.since {
            let cutoff = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
                - since.as_secs() as i64;
            sql.push_str(" AND trashed_at >= ?");
            params_vec.push(Box::new(cutoff));
        }

        sql.push_str(" ORDER BY trashed_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| Error::Database(format!("prepare failed: {}", e)))?;

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let files: Vec<TrashedFile> = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(TrashedFile {
                    file_id: FileId(row.get(0)?),
                    current_path: VirtualPath::new(row.get::<_, String>(1)?),
                    original_path: VirtualPath::new(row.get::<_, String>(2)?),
                    trashed_at: row.get(3)?,
                    origin_id: OriginId(row.get(4)?),
                })
            })
            .map_err(|e| Error::Database(format!("query failed: {}", e)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(files)
    }

    pub fn get_trashed_by_prefix(&self, prefix: &str) -> Result<Vec<TrashedFile>> {
        self.list_trashed(&TrashedFilter {
            path_prefix: Some(prefix.to_string()),
            ..Default::default()
        })
    }

    pub fn is_trashed(&self, path: &VirtualPath) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE virtual_path = ?1 AND trashed = 1",
                params![path.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(format!("is_trashed query failed: {}", e)))?;
        Ok(count > 0)
    }

    pub fn purge_trashed(&self, filter: &TrashedFilter) -> Result<u64> {
        let trashed = self.list_trashed(filter)?;
        let count = trashed.len() as u64;

        let conn = self.conn.lock().unwrap();
        for file in trashed {
            conn.execute("DELETE FROM files WHERE id = ?1", params![file.file_id.0])
                .map_err(|e| Error::Database(format!("purge delete failed: {}", e)))?;
        }

        debug!(count, "purged trashed files");
        Ok(count)
    }
}

#[derive(Debug, Clone)]
pub struct TrashedFile {
    pub file_id: FileId,
    pub current_path: VirtualPath,
    pub original_path: VirtualPath,
    pub trashed_at: i64,
    pub origin_id: OriginId,
}

#[derive(Debug, Clone, Default)]
pub struct TrashedFilter {
    pub origin_id: Option<OriginId>,
    pub path_prefix: Option<String>,
    pub since: Option<Duration>,
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

    #[test]
    fn test_path_exists() {
        let db = Database::open_memory().unwrap();

        let path = VirtualPath::new("/Artist/Album/Track.flac");
        assert!(!db.path_exists(&path).unwrap());

        db.upsert_file(
            &OriginId::from("local"),
            Path::new("/test.flac"),
            &path,
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        assert!(db.path_exists(&path).unwrap());
        assert!(!db
            .path_exists(&VirtualPath::new("/Other/Path.flac"))
            .unwrap());
    }

    #[test]
    fn test_update_virtual_path() {
        let db = Database::open_memory().unwrap();

        let old_path = VirtualPath::new("/Old/Path/Track.flac");
        let new_path = VirtualPath::new("/New/Path/Track.flac");

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &old_path,
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        db.update_virtual_path(id, &new_path).unwrap();

        assert!(db.get_file_by_virtual_path(&old_path).unwrap().is_none());
        assert!(db.get_file_by_virtual_path(&new_path).unwrap().is_some());
    }

    #[test]
    fn test_rename_directory() {
        let db = Database::open_memory().unwrap();
        let origin = OriginId::from("local");

        db.upsert_file(
            &origin,
            Path::new("/a.flac"),
            &VirtualPath::new("/Artist/Album/Track1.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        db.upsert_file(
            &origin,
            Path::new("/b.flac"),
            &VirtualPath::new("/Artist/Album/Track2.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        db.upsert_file(
            &origin,
            Path::new("/c.flac"),
            &VirtualPath::new("/Other/Track.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        let count = db.rename_directory("/Artist/", "/Renamed Artist/").unwrap();
        assert_eq!(count, 2);

        assert!(db
            .path_exists(&VirtualPath::new("/Renamed Artist/Album/Track1.flac"))
            .unwrap());
        assert!(db
            .path_exists(&VirtualPath::new("/Renamed Artist/Album/Track2.flac"))
            .unwrap());
        assert!(db
            .path_exists(&VirtualPath::new("/Other/Track.flac"))
            .unwrap());
        assert!(!db
            .path_exists(&VirtualPath::new("/Artist/Album/Track1.flac"))
            .unwrap());
    }

    #[test]
    fn test_get_files_by_prefix() {
        let db = Database::open_memory().unwrap();
        let origin = OriginId::from("local");

        db.upsert_file(
            &origin,
            Path::new("/a.flac"),
            &VirtualPath::new("/Artist/Album/Track1.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        db.upsert_file(
            &origin,
            Path::new("/b.flac"),
            &VirtualPath::new("/Artist/Album/Track2.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        db.upsert_file(
            &origin,
            Path::new("/c.flac"),
            &VirtualPath::new("/Other/Track.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            100,
        )
        .unwrap();

        let files = db.get_files_by_prefix("/Artist/").unwrap();
        assert_eq!(files.len(), 2);

        let files = db.get_files_by_prefix("/Other/").unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_mark_trashed() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Artist/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        db.mark_trashed(id, &VirtualPath::new("/Artist/Track.flac"))
            .unwrap();

        let trashed = db.list_trashed(&TrashedFilter::default()).unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].original_path.as_str(), "/Artist/Track.flac");
    }

    #[test]
    fn test_unmark_trashed() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Artist/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        db.mark_trashed(id, &VirtualPath::new("/Artist/Track.flac"))
            .unwrap();
        assert_eq!(db.list_trashed(&TrashedFilter::default()).unwrap().len(), 1);

        db.unmark_trashed(id).unwrap();
        assert_eq!(db.list_trashed(&TrashedFilter::default()).unwrap().len(), 0);
    }

    #[test]
    fn test_list_trashed_with_filter() {
        let db = Database::open_memory().unwrap();
        let origin1 = OriginId::from("local1");
        let origin2 = OriginId::from("local2");

        let id1 = db
            .upsert_file(
                &origin1,
                Path::new("/a.flac"),
                &VirtualPath::new("/Artist1/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        let id2 = db
            .upsert_file(
                &origin2,
                Path::new("/b.flac"),
                &VirtualPath::new("/Artist2/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        db.mark_trashed(id1, &VirtualPath::new("/Artist1/Track.flac"))
            .unwrap();
        db.mark_trashed(id2, &VirtualPath::new("/Artist2/Track.flac"))
            .unwrap();

        let all = db.list_trashed(&TrashedFilter::default()).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = db
            .list_trashed(&TrashedFilter {
                origin_id: Some(origin1.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].origin_id, origin1);

        let by_path = db.get_trashed_by_prefix("/Artist1").unwrap();
        assert_eq!(by_path.len(), 1);
    }

    #[test]
    fn test_purge_trashed() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        db.mark_trashed(id, &VirtualPath::new("/Track.flac"))
            .unwrap();
        assert_eq!(db.list_trashed(&TrashedFilter::default()).unwrap().len(), 1);

        let count = db.purge_trashed(&TrashedFilter::default()).unwrap();
        assert_eq!(count, 1);
        assert_eq!(db.list_trashed(&TrashedFilter::default()).unwrap().len(), 0);
        assert_eq!(db.file_count().unwrap(), 0);
    }
}
