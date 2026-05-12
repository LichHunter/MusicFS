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

    #[error("Metadata extraction error: {0}")]
    Metadata(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("NFS stale file handle")]
    NfsStaleHandle,

    #[error("Operation not permitted (read-only filesystem)")]
    ReadOnly,

    #[error("No origin available to serve request")]
    NoOriginAvailable,

    #[error("Maximum retries exceeded")]
    MaxRetriesExceeded,

    #[error("Origin error: {0}")]
    Origin(String),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("SFTP error: {0}")]
    Sftp(String),

    #[error("Operation timed out: {0}")]
    Timeout(String),

    #[error("Credential error: {0}")]
    Credential(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::FileNotFound(_))
    }

    pub fn downcast_io(&self) -> Option<&std::io::Error> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}
