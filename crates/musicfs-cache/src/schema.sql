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
    track_total     INTEGER,
    disc_total      INTEGER,
    date            TEXT,
    composer        TEXT,
    comment         TEXT,
    lyrics          TEXT,
    copyright       TEXT,
    compilation     INTEGER,
    artist_sort     TEXT,
    album_artist_sort TEXT,
    album_sort      TEXT,
    title_sort      TEXT,
    mb_recording_id TEXT,
    mb_album_id     TEXT,
    mb_artist_id    TEXT,
    mb_album_artist_id TEXT,
    mb_release_group_id TEXT,
    replaygain_track_gain REAL,
    replaygain_track_peak REAL,
    replaygain_album_gain REAL,
    replaygain_album_peak REAL,
    channels        INTEGER,
    bits_per_sample INTEGER,
    encoder         TEXT,
    custom_tags     TEXT,
    format_layout   BLOB,
    
    origin_mtime    INTEGER NOT NULL,
    origin_size     INTEGER NOT NULL,
    content_hash    TEXT,
    chunk_manifest  BLOB,
    last_sync       INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    
    trashed         INTEGER NOT NULL DEFAULT 0,
    original_path   TEXT,
    trashed_at      INTEGER,
    
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
CREATE INDEX IF NOT EXISTS idx_files_mb_album ON files(mb_album_id);
CREATE INDEX IF NOT EXISTS idx_files_mb_artist ON files(mb_artist_id);
CREATE INDEX IF NOT EXISTS idx_files_genre ON files(genre);
CREATE INDEX IF NOT EXISTS idx_files_year ON files(year);
CREATE INDEX IF NOT EXISTS idx_files_composer ON files(composer);
CREATE INDEX IF NOT EXISTS idx_artwork_file ON artwork(file_id);

CREATE TABLE IF NOT EXISTS directories (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_directories_path ON directories(path);
CREATE INDEX IF NOT EXISTS idx_files_trashed ON files(trashed) WHERE trashed = 1;
