use crate::FormatLayout;
use musicfs_core::{
    AudioFormat, AudioMeta, ContentHash, Error, FileId, FileMeta, OriginId, RealPath, Result,
    VirtualPath,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
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
        self.upsert_file_with_layout(
            origin_id,
            real_path,
            virtual_path,
            audio_meta,
            origin_mtime,
            origin_size,
            None,
            None,
        )
    }

    pub fn upsert_file_with_layout(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
        virtual_path: &VirtualPath,
        audio_meta: &AudioMeta,
        origin_mtime: SystemTime,
        origin_size: u64,
        format_layout: Option<&FormatLayout>,
        custom_tags: Option<&HashMap<String, String>>,
    ) -> Result<FileId> {
        let conn = self.conn.lock().unwrap();

        let mtime_secs = origin_mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Serialize format_layout as msgpack BLOB
        let format_layout_blob: Option<Vec<u8>> = format_layout
            .map(|fl| rmp_serde::to_vec(fl))
            .transpose()
            .map_err(|e| Error::Database(format!("format_layout serialization failed: {}", e)))?;

        // Serialize custom_tags as JSON TEXT
        let custom_tags_json: Option<String> = custom_tags
            .map(|ct| serde_json::to_string(ct))
            .transpose()
            .map_err(|e| Error::Database(format!("custom_tags serialization failed: {}", e)))?;

        conn.execute(
            r#"
            INSERT INTO files (
                origin_id, real_path, virtual_path,
                title, artist, album, album_artist, genre,
                year, track, disc,
                duration_ms, bitrate, sample_rate, format,
                track_total, disc_total, date, composer, comment,
                lyrics, copyright, compilation,
                artist_sort, album_artist_sort, album_sort, title_sort,
                mb_recording_id, mb_album_id, mb_artist_id, mb_album_artist_id, mb_release_group_id,
                replaygain_track_gain, replaygain_track_peak, replaygain_album_gain, replaygain_album_peak,
                channels, bits_per_sample, encoder,
                custom_tags, format_layout,
                origin_mtime, origin_size
            ) VALUES (
                ?1, ?2, ?3,
                ?4, ?5, ?6, ?7, ?8,
                ?9, ?10, ?11,
                ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20,
                ?21, ?22, ?23,
                ?24, ?25, ?26, ?27,
                ?28, ?29, ?30, ?31, ?32,
                ?33, ?34, ?35, ?36,
                ?37, ?38, ?39,
                ?40, ?41,
                ?42, ?43
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
                track_total = excluded.track_total,
                disc_total = excluded.disc_total,
                date = excluded.date,
                composer = excluded.composer,
                comment = excluded.comment,
                lyrics = excluded.lyrics,
                copyright = excluded.copyright,
                compilation = excluded.compilation,
                artist_sort = excluded.artist_sort,
                album_artist_sort = excluded.album_artist_sort,
                album_sort = excluded.album_sort,
                title_sort = excluded.title_sort,
                mb_recording_id = excluded.mb_recording_id,
                mb_album_id = excluded.mb_album_id,
                mb_artist_id = excluded.mb_artist_id,
                mb_album_artist_id = excluded.mb_album_artist_id,
                mb_release_group_id = excluded.mb_release_group_id,
                replaygain_track_gain = excluded.replaygain_track_gain,
                replaygain_track_peak = excluded.replaygain_track_peak,
                replaygain_album_gain = excluded.replaygain_album_gain,
                replaygain_album_peak = excluded.replaygain_album_peak,
                channels = excluded.channels,
                bits_per_sample = excluded.bits_per_sample,
                encoder = excluded.encoder,
                custom_tags = excluded.custom_tags,
                format_layout = excluded.format_layout,
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
                &audio_meta.track_total,
                &audio_meta.disc_total,
                &audio_meta.date,
                &audio_meta.composer,
                &audio_meta.comment,
                &audio_meta.lyrics,
                &audio_meta.copyright,
                &audio_meta.compilation.map(|b| if b { 1i32 } else { 0i32 }),
                &audio_meta.artist_sort,
                &audio_meta.album_artist_sort,
                &audio_meta.album_sort,
                &audio_meta.title_sort,
                &audio_meta.mb_recording_id,
                &audio_meta.mb_album_id,
                &audio_meta.mb_artist_id,
                &audio_meta.mb_album_artist_id,
                &audio_meta.mb_release_group_id,
                &audio_meta.replaygain_track_gain,
                &audio_meta.replaygain_track_peak,
                &audio_meta.replaygain_album_gain,
                &audio_meta.replaygain_album_peak,
                &audio_meta.channels,
                &audio_meta.bits_per_sample,
                &audio_meta.encoder,
                &custom_tags_json,
                &format_layout_blob,
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
                   track_total, disc_total, date, composer, comment,
                   lyrics, copyright, compilation,
                   artist_sort, album_artist_sort, album_sort, title_sort,
                   mb_recording_id, mb_album_id, mb_artist_id, mb_album_artist_id, mb_release_group_id,
                   replaygain_track_gain, replaygain_track_peak, replaygain_album_gain, replaygain_album_peak,
                   channels, bits_per_sample, encoder,
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

                let compilation_int: Option<i32> = row.get(23)?;
                let content_hash: Option<String> = row.get(42)?;

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
                        track_total: row.get(16)?,
                        disc_total: row.get(17)?,
                        date: row.get(18)?,
                        composer: row.get(19)?,
                        comment: row.get(20)?,
                        lyrics: row.get(21)?,
                        copyright: row.get(22)?,
                        compilation: compilation_int.map(|i| i != 0),
                        artist_sort: row.get(24)?,
                        album_artist_sort: row.get(25)?,
                        album_sort: row.get(26)?,
                        title_sort: row.get(27)?,
                        mb_recording_id: row.get(28)?,
                        mb_album_id: row.get(29)?,
                        mb_artist_id: row.get(30)?,
                        mb_album_artist_id: row.get(31)?,
                        mb_release_group_id: row.get(32)?,
                        replaygain_track_gain: row.get(33)?,
                        replaygain_track_peak: row.get(34)?,
                        replaygain_album_gain: row.get(35)?,
                        replaygain_album_peak: row.get(36)?,
                        channels: row.get(37)?,
                        bits_per_sample: row.get(38)?,
                        encoder: row.get(39)?,
                    }),
                    size: row.get::<_, i64>(41)? as u64,
                    mtime: UNIX_EPOCH + Duration::from_secs(row.get::<_, i64>(40)? as u64),
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

    pub fn get_file_metadata_row(&self, file_id: FileId) -> Result<AudioMeta> {
        let conn = self.conn.lock().unwrap();

        conn.query_row(
            r#"
            SELECT title, artist, album, album_artist, genre,
                   year, track, disc,
                   duration_ms, bitrate, sample_rate, format,
                   track_total, disc_total, date, composer, comment,
                   lyrics, copyright, compilation,
                   artist_sort, album_artist_sort, album_sort, title_sort,
                   mb_recording_id, mb_album_id, mb_artist_id, mb_album_artist_id, mb_release_group_id,
                   replaygain_track_gain, replaygain_track_peak, replaygain_album_gain, replaygain_album_peak,
                   channels, bits_per_sample, encoder
            FROM files
            WHERE id = ?1
            "#,
            params![file_id.0],
            |row| {
                let format_str: Option<String> = row.get(11)?;
                let format = format_str
                    .as_deref()
                    .map(parse_audio_format)
                    .unwrap_or(AudioFormat::Unknown);

                let compilation_int: Option<i32> = row.get(19)?;

                Ok(AudioMeta {
                    title: row.get(0)?,
                    artist: row.get(1)?,
                    album: row.get(2)?,
                    album_artist: row.get(3)?,
                    genre: row.get(4)?,
                    year: row.get(5)?,
                    track: row.get(6)?,
                    disc: row.get(7)?,
                    duration_ms: row.get::<_, Option<i64>>(8)?.map(|d| d as u64),
                    bitrate: row.get(9)?,
                    sample_rate: row.get(10)?,
                    format,
                    track_total: row.get(12)?,
                    disc_total: row.get(13)?,
                    date: row.get(14)?,
                    composer: row.get(15)?,
                    comment: row.get(16)?,
                    lyrics: row.get(17)?,
                    copyright: row.get(18)?,
                    compilation: compilation_int.map(|i| i != 0),
                    artist_sort: row.get(20)?,
                    album_artist_sort: row.get(21)?,
                    album_sort: row.get(22)?,
                    title_sort: row.get(23)?,
                    mb_recording_id: row.get(24)?,
                    mb_album_id: row.get(25)?,
                    mb_artist_id: row.get(26)?,
                    mb_album_artist_id: row.get(27)?,
                    mb_release_group_id: row.get(28)?,
                    replaygain_track_gain: row.get(29)?,
                    replaygain_track_peak: row.get(30)?,
                    replaygain_album_gain: row.get(31)?,
                    replaygain_album_peak: row.get(32)?,
                    channels: row.get(33)?,
                    bits_per_sample: row.get(34)?,
                    encoder: row.get(35)?,
                })
            },
        )
        .map_err(|e| Error::Database(format!("get_file_metadata_row failed: {}", e)))
    }

    pub fn get_format_layout(&self, file_id: FileId) -> Result<Option<FormatLayout>> {
        let conn = self.conn.lock().unwrap();

        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT format_layout FROM files WHERE id = ?1",
                params![file_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Database(format!("get_format_layout query failed: {}", e)))?
            .flatten();

        match blob {
            Some(data) => {
                let layout: FormatLayout = rmp_serde::from_slice(&data).map_err(|e| {
                    Error::Database(format!("format_layout deserialization failed: {}", e))
                })?;
                Ok(Some(layout))
            }
            None => Ok(None),
        }
    }

    pub fn get_custom_tags(&self, file_id: FileId) -> Result<Option<HashMap<String, String>>> {
        let conn = self.conn.lock().unwrap();

        let json: Option<String> = conn
            .query_row(
                "SELECT custom_tags FROM files WHERE id = ?1",
                params![file_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Database(format!("get_custom_tags query failed: {}", e)))?
            .flatten();

        match json {
            Some(data) => {
                let tags: HashMap<String, String> = serde_json::from_str(&data).map_err(|e| {
                    Error::Database(format!("custom_tags deserialization failed: {}", e))
                })?;
                Ok(Some(tags))
            }
            None => Ok(None),
        }
    }

    pub fn update_metadata(&self, file_id: FileId, metadata: &AudioMeta) -> Result<()> {
        let mut updates = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        macro_rules! add_field {
            ($field:ident, $col:literal) => {
                if let Some(ref val) = metadata.$field {
                    updates.push(concat!($col, " = ?"));
                    params_vec.push(Box::new(val.clone()));
                }
            };
            ($field:ident, $col:literal, u32) => {
                if let Some(val) = metadata.$field {
                    updates.push(concat!($col, " = ?"));
                    params_vec.push(Box::new(val as i64));
                }
            };
            ($field:ident, $col:literal, u64) => {
                if let Some(val) = metadata.$field {
                    updates.push(concat!($col, " = ?"));
                    params_vec.push(Box::new(val as i64));
                }
            };
            ($field:ident, $col:literal, f32) => {
                if let Some(val) = metadata.$field {
                    updates.push(concat!($col, " = ?"));
                    params_vec.push(Box::new(val as f64));
                }
            };
            ($field:ident, $col:literal, bool) => {
                if let Some(val) = metadata.$field {
                    updates.push(concat!($col, " = ?"));
                    params_vec.push(Box::new(if val { 1i32 } else { 0i32 }));
                }
            };
        }

        add_field!(title, "title");
        add_field!(artist, "artist");
        add_field!(album, "album");
        add_field!(album_artist, "album_artist");
        add_field!(genre, "genre");
        add_field!(year, "year", u32);
        add_field!(track, "track", u32);
        add_field!(disc, "disc", u32);
        add_field!(duration_ms, "duration_ms", u64);
        add_field!(bitrate, "bitrate", u32);
        add_field!(sample_rate, "sample_rate", u32);
        add_field!(track_total, "track_total", u32);
        add_field!(disc_total, "disc_total", u32);
        add_field!(date, "date");
        add_field!(composer, "composer");
        add_field!(comment, "comment");
        add_field!(lyrics, "lyrics");
        add_field!(copyright, "copyright");
        add_field!(compilation, "compilation", bool);
        add_field!(artist_sort, "artist_sort");
        add_field!(album_artist_sort, "album_artist_sort");
        add_field!(album_sort, "album_sort");
        add_field!(title_sort, "title_sort");
        add_field!(mb_recording_id, "mb_recording_id");
        add_field!(mb_album_id, "mb_album_id");
        add_field!(mb_artist_id, "mb_artist_id");
        add_field!(mb_album_artist_id, "mb_album_artist_id");
        add_field!(mb_release_group_id, "mb_release_group_id");
        add_field!(replaygain_track_gain, "replaygain_track_gain", f32);
        add_field!(replaygain_track_peak, "replaygain_track_peak", f32);
        add_field!(replaygain_album_gain, "replaygain_album_gain", f32);
        add_field!(replaygain_album_peak, "replaygain_album_peak", f32);
        add_field!(channels, "channels", u32);
        add_field!(bits_per_sample, "bits_per_sample", u32);
        add_field!(encoder, "encoder");

        if updates.is_empty() {
            return Ok(());
        }

        let sql = format!("UPDATE files SET {} WHERE id = ?", updates.join(", "));
        params_vec.push(Box::new(file_id.0));

        let conn = self.conn.lock().unwrap();
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = conn
            .execute(&sql, params_refs.as_slice())
            .map_err(|e| Error::Database(format!("update_metadata failed: {}", e)))?;

        if rows == 0 {
            return Err(Error::FileNotFound(format!(
                "file id {} not found",
                file_id.0
            )));
        }

        debug!(id = file_id.0, fields = updates.len(), "updated metadata");
        Ok(())
    }

    pub fn clear_overlay(&self, file_id: FileId) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        let rows = conn
            .execute(
                r#"
                UPDATE files SET
                    title = NULL, artist = NULL, album = NULL, album_artist = NULL, genre = NULL,
                    year = NULL, track = NULL, disc = NULL,
                    duration_ms = NULL, bitrate = NULL, sample_rate = NULL, format = NULL,
                    track_total = NULL, disc_total = NULL, date = NULL, composer = NULL, comment = NULL,
                    lyrics = NULL, copyright = NULL, compilation = NULL,
                    artist_sort = NULL, album_artist_sort = NULL, album_sort = NULL, title_sort = NULL,
                    mb_recording_id = NULL, mb_album_id = NULL, mb_artist_id = NULL, mb_album_artist_id = NULL, mb_release_group_id = NULL,
                    replaygain_track_gain = NULL, replaygain_track_peak = NULL, replaygain_album_gain = NULL, replaygain_album_peak = NULL,
                    channels = NULL, bits_per_sample = NULL, encoder = NULL,
                    custom_tags = NULL, format_layout = NULL
                WHERE id = ?1
                "#,
                params![file_id.0],
            )
            .map_err(|e| Error::Database(format!("clear_overlay failed: {}", e)))?;

        if rows == 0 {
            return Err(Error::FileNotFound(format!(
                "file id {} not found",
                file_id.0
            )));
        }

        debug!(id = file_id.0, "cleared overlay metadata");
        Ok(())
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

    #[test]
    fn test_new_metadata_fields_roundtrip() {
        let db = Database::open_memory().unwrap();

        let audio_meta = AudioMeta {
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            album_artist: Some("Test Album Artist".to_string()),
            genre: Some("Rock".to_string()),
            year: Some(2024),
            track: Some(5),
            disc: Some(1),
            duration_ms: Some(180000),
            bitrate: Some(320),
            sample_rate: Some(44100),
            format: AudioFormat::Flac,
            track_total: Some(12),
            disc_total: Some(2),
            date: Some("2024-03-15".to_string()),
            composer: Some("Test Composer".to_string()),
            comment: Some("Test comment".to_string()),
            lyrics: Some("La la la".to_string()),
            copyright: Some("2024 Test Records".to_string()),
            compilation: Some(true),
            artist_sort: Some("Artist, Test".to_string()),
            album_artist_sort: Some("Album Artist, Test".to_string()),
            album_sort: Some("Album, Test".to_string()),
            title_sort: Some("Track, Test".to_string()),
            mb_recording_id: Some("rec-123".to_string()),
            mb_album_id: Some("alb-456".to_string()),
            mb_artist_id: Some("art-789".to_string()),
            mb_album_artist_id: Some("aa-012".to_string()),
            mb_release_group_id: Some("rg-345".to_string()),
            replaygain_track_gain: Some(-6.5),
            replaygain_track_peak: Some(0.95),
            replaygain_album_gain: Some(-7.2),
            replaygain_album_peak: Some(0.98),
            channels: Some(2),
            bits_per_sample: Some(24),
            encoder: Some("FLAC 1.4.0".to_string()),
        };

        let vpath = VirtualPath::new("/Artist/Album/05 - Track.flac");
        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/music/test.flac"),
                &vpath,
                &audio_meta,
                UNIX_EPOCH,
                5000000,
            )
            .unwrap();

        let retrieved = db.get_file_by_virtual_path(&vpath).unwrap().unwrap();
        let audio = retrieved.audio.unwrap();

        assert_eq!(audio.title, Some("Test Track".to_string()));
        assert_eq!(audio.track_total, Some(12));
        assert_eq!(audio.disc_total, Some(2));
        assert_eq!(audio.date, Some("2024-03-15".to_string()));
        assert_eq!(audio.composer, Some("Test Composer".to_string()));
        assert_eq!(audio.comment, Some("Test comment".to_string()));
        assert_eq!(audio.lyrics, Some("La la la".to_string()));
        assert_eq!(audio.copyright, Some("2024 Test Records".to_string()));
        assert_eq!(audio.compilation, Some(true));
        assert_eq!(audio.artist_sort, Some("Artist, Test".to_string()));
        assert_eq!(audio.mb_recording_id, Some("rec-123".to_string()));
        assert_eq!(audio.mb_album_id, Some("alb-456".to_string()));
        assert!(audio.replaygain_track_gain.is_some());
        assert!((audio.replaygain_track_gain.unwrap() - (-6.5)).abs() < 0.01);
        assert_eq!(audio.channels, Some(2));
        assert_eq!(audio.bits_per_sample, Some(24));
        assert_eq!(audio.encoder, Some("FLAC 1.4.0".to_string()));

        let meta_row = db.get_file_metadata_row(id).unwrap();
        assert_eq!(meta_row.title, Some("Test Track".to_string()));
        assert_eq!(meta_row.track_total, Some(12));
        assert_eq!(meta_row.mb_album_id, Some("alb-456".to_string()));
    }

    #[test]
    fn test_update_metadata_partial() {
        let db = Database::open_memory().unwrap();

        let audio_meta = AudioMeta {
            title: Some("Original Title".to_string()),
            artist: Some("Original Artist".to_string()),
            album: Some("Original Album".to_string()),
            track: Some(1),
            ..Default::default()
        };

        let vpath = VirtualPath::new("/Artist/Album/Track.flac");
        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &vpath,
                &audio_meta,
                UNIX_EPOCH,
                1000,
            )
            .unwrap();

        let update = AudioMeta {
            title: Some("Updated Title".to_string()),
            composer: Some("New Composer".to_string()),
            ..Default::default()
        };
        db.update_metadata(id, &update).unwrap();

        let retrieved = db.get_file_metadata_row(id).unwrap();
        assert_eq!(retrieved.title, Some("Updated Title".to_string()));
        assert_eq!(retrieved.artist, Some("Original Artist".to_string()));
        assert_eq!(retrieved.album, Some("Original Album".to_string()));
        assert_eq!(retrieved.composer, Some("New Composer".to_string()));
        assert_eq!(retrieved.track, Some(1));
    }

    #[test]
    fn test_update_metadata_empty_noop() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Track.flac"),
                &AudioMeta {
                    title: Some("Title".to_string()),
                    ..Default::default()
                },
                UNIX_EPOCH,
                1000,
            )
            .unwrap();

        let empty_update = AudioMeta::default();
        db.update_metadata(id, &empty_update).unwrap();

        let retrieved = db.get_file_metadata_row(id).unwrap();
        assert_eq!(retrieved.title, Some("Title".to_string()));
    }

    #[test]
    fn test_clear_overlay() {
        let db = Database::open_memory().unwrap();

        let audio_meta = AudioMeta {
            title: Some("Title".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            composer: Some("Composer".to_string()),
            mb_album_id: Some("mb-123".to_string()),
            replaygain_track_gain: Some(-5.0),
            ..Default::default()
        };

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Track.flac"),
                &audio_meta,
                UNIX_EPOCH,
                1000,
            )
            .unwrap();

        db.clear_overlay(id).unwrap();

        let retrieved = db.get_file_metadata_row(id).unwrap();
        assert!(retrieved.title.is_none());
        assert!(retrieved.artist.is_none());
        assert!(retrieved.album.is_none());
        assert!(retrieved.composer.is_none());
        assert!(retrieved.mb_album_id.is_none());
        assert!(retrieved.replaygain_track_gain.is_none());
    }

    #[test]
    fn test_format_layout_roundtrip() {
        use crate::FormatLayout;

        let db = Database::open_memory().unwrap();

        let layout = FormatLayout {
            audio_start: 1024,
            audio_end: 5000000,
            format: AudioFormat::Flac,
            format_data: Some(vec![0x66, 0x4c, 0x61, 0x43]),
        };

        let id = db
            .upsert_file_with_layout(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                5000000,
                Some(&layout),
                None,
            )
            .unwrap();

        let retrieved = db.get_format_layout(id).unwrap().unwrap();
        assert_eq!(retrieved.audio_start, 1024);
        assert_eq!(retrieved.audio_end, 5000000);
        assert_eq!(retrieved.format, AudioFormat::Flac);
        assert_eq!(retrieved.format_data, Some(vec![0x66, 0x4c, 0x61, 0x43]));
    }

    #[test]
    fn test_custom_tags_roundtrip() {
        let db = Database::open_memory().unwrap();

        let mut custom_tags = HashMap::new();
        custom_tags.insert("CUSTOM_FIELD".to_string(), "custom value".to_string());
        custom_tags.insert("ANOTHER_TAG".to_string(), "another value".to_string());

        let id = db
            .upsert_file_with_layout(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                1000,
                None,
                Some(&custom_tags),
            )
            .unwrap();

        let retrieved = db.get_custom_tags(id).unwrap().unwrap();
        assert_eq!(
            retrieved.get("CUSTOM_FIELD"),
            Some(&"custom value".to_string())
        );
        assert_eq!(
            retrieved.get("ANOTHER_TAG"),
            Some(&"another value".to_string())
        );
    }

    #[test]
    fn test_format_layout_none() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                1000,
            )
            .unwrap();

        let layout = db.get_format_layout(id).unwrap();
        assert!(layout.is_none());
    }

    #[test]
    fn test_custom_tags_none() {
        let db = Database::open_memory().unwrap();

        let id = db
            .upsert_file(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Track.flac"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                1000,
            )
            .unwrap();

        let tags = db.get_custom_tags(id).unwrap();
        assert!(tags.is_none());
    }
}
