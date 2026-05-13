use image::ImageFormat;
use musicfs_cas::CasStore;
use musicfs_core::ChunkHash;
use musicfs_metadata::artwork::{ArtSize, ArtType, Artwork};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use tracing::debug;

const MAX_ARTWORK_INPUT_SIZE: usize = 10 * 1024 * 1024;

pub struct ArtworkCache {
    store: Arc<CasStore>,
    db_path: std::path::PathBuf,
}

#[derive(Debug)]
pub struct CachedArtwork {
    pub file_id: i64,
    pub art_type: String,
    pub chunk_hash: ChunkHash,
    pub width: u32,
    pub height: u32,
}

impl ArtworkCache {
    pub fn new(store: Arc<CasStore>, db_path: &Path) -> Result<Self, ArtworkError> {
        let db = rusqlite::Connection::open(db_path)?;

        db.execute(
            "CREATE TABLE IF NOT EXISTS artwork (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL,
                art_type TEXT NOT NULL,
                chunk_hash TEXT NOT NULL,
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                UNIQUE(file_id, art_type)
            )",
            [],
        )?;

        Ok(Self {
            store,
            db_path: db_path.to_path_buf(),
        })
    }

    pub async fn store(&self, file_id: i64, artwork: &Artwork) -> Result<ChunkHash, ArtworkError> {
        if artwork.data.len() > MAX_ARTWORK_INPUT_SIZE {
            return Err(ArtworkError::ImageTooLarge(artwork.data.len()));
        }

        let hash = self.store.put(&artwork.data).await?;

        let art_type_str = match artwork.art_type {
            ArtType::Front => "front",
            ArtType::Back => "back",
            ArtType::Other => "other",
        };

        let db_path = self.db_path.clone();
        let art_type_clone = art_type_str.to_string();
        let hash_hex = hash.to_hex();
        let width = artwork.width;
        let height = artwork.height;

        tokio::task::spawn_blocking(move || {
            let db = rusqlite::Connection::open(&db_path)?;
            db.execute(
                "INSERT OR REPLACE INTO artwork 
                 (file_id, art_type, chunk_hash, width, height) 
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![file_id, art_type_clone, hash_hex, width, height],
            )?;
            Ok::<_, ArtworkError>(())
        })
        .await
        .map_err(|e| ArtworkError::SpawnBlocking(e.to_string()))??;

        debug!("Cached artwork for file {}", file_id);
        Ok(hash)
    }

    pub async fn get(
        &self,
        file_id: i64,
        art_type: &str,
        size: ArtSize,
    ) -> Result<Option<Vec<u8>>, ArtworkError> {
        let db_path = self.db_path.clone();
        let art_type_clone = art_type.to_string();

        let hash_hex: Option<String> = tokio::task::spawn_blocking(move || {
            let db = rusqlite::Connection::open(&db_path)?;
            db.query_row(
                "SELECT chunk_hash FROM artwork WHERE file_id = ?1 AND art_type = ?2",
                rusqlite::params![file_id, art_type_clone],
                |row| row.get(0),
            )
            .ok()
            .ok_or(ArtworkError::NotFound)
        })
        .await
        .map_err(|e| ArtworkError::SpawnBlocking(e.to_string()))?
        .ok();

        match hash_hex {
            Some(hex) => {
                let hash = ChunkHash::from_hex(&hex).ok_or(ArtworkError::InvalidHash)?;
                let data = self.store.get(&hash).await?;

                match size {
                    ArtSize::Full => Ok(Some(data.to_vec())),
                    ArtSize::Thumbnail | ArtSize::Medium => {
                        let resized = self.resize_on_demand(&data, size)?;
                        Ok(Some(resized))
                    }
                }
            }
            None => Ok(None),
        }
    }

    pub async fn has(&self, file_id: i64, art_type: &str) -> Result<bool, ArtworkError> {
        let db_path = self.db_path.clone();
        let art_type_clone = art_type.to_string();

        tokio::task::spawn_blocking(move || {
            let db = rusqlite::Connection::open(&db_path)?;
            let count: i64 = db.query_row(
                "SELECT COUNT(*) FROM artwork WHERE file_id = ?1 AND art_type = ?2",
                rusqlite::params![file_id, art_type_clone],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .await
        .map_err(|e| ArtworkError::SpawnBlocking(e.to_string()))?
    }

    fn resize_on_demand(&self, data: &[u8], size: ArtSize) -> Result<Vec<u8>, ArtworkError> {
        let max_dim = size.max_dimension().unwrap_or(300);
        let img = image::load_from_memory(data).map_err(|_| ArtworkError::InvalidImage)?;

        if img.width() <= max_dim && img.height() <= max_dim {
            return Ok(data.to_vec());
        }

        let resized = img.thumbnail(max_dim, max_dim);
        let mut output = Vec::new();
        let mut cursor = Cursor::new(&mut output);
        resized
            .write_to(&mut cursor, ImageFormat::Jpeg)
            .map_err(|_| ArtworkError::ResizeFailed)?;

        Ok(output)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArtworkError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("CAS error: {0}")]
    Cas(#[from] musicfs_cas::CasError),

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_artwork_size() {
        assert_eq!(MAX_ARTWORK_INPUT_SIZE, 10 * 1024 * 1024);
    }
}
