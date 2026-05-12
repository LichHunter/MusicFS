use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Origin not found: {0}")]
    OriginNotFound(String),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Path resolution failed: {0}")]
    PathResolution(String),

    #[error("Cache error: {0}")]
    Cache(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("NFS stale file handle")]
    NfsStaleHandle,

    #[error("Operation not permitted (read-only filesystem)")]
    ReadOnly,
}

pub type Result<T> = std::result::Result<T, Error>;
