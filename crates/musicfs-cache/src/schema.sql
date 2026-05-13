PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS files (
    id              INTEGER PRIMARY KEY,
    origin_id       TEXT NOT NULL,
    real_path       TEXT NOT NULL,
    virtual_path    TEXT NOT NULL,
    
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
    
    origin_mtime    INTEGER NOT NULL,
    origin_size     INTEGER NOT NULL,
    content_hash    TEXT,
    chunk_manifest  BLOB,
    last_sync       INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    
    UNIQUE(origin_id, real_path)
);

CREATE TABLE IF NOT EXISTS artwork (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    art_type    TEXT NOT NULL,
    chunk_hash  TEXT NOT NULL,
    width       INTEGER,
    height      INTEGER,
    mime_type   TEXT,
    UNIQUE(file_id, art_type)
);

CREATE TABLE IF NOT EXISTS collections (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    query_json  TEXT NOT NULL,
    created_at  INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    updated_at  INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_files_virtual ON files(virtual_path);
CREATE INDEX IF NOT EXISTS idx_files_artist_album ON files(artist, album);
CREATE INDEX IF NOT EXISTS idx_files_content_hash ON files(content_hash);
CREATE INDEX IF NOT EXISTS idx_files_real ON files(origin_id, real_path);
CREATE INDEX IF NOT EXISTS idx_files_origin ON files(origin_id);
CREATE INDEX IF NOT EXISTS idx_files_last_sync ON files(last_sync);
CREATE INDEX IF NOT EXISTS idx_artwork_file ON artwork(file_id);
