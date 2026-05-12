use crate::chunks::ChunkRef;
use crate::store::CasStore;
use bytes::{Bytes, BytesMut};
use musicfs_core::FileId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub file_id: FileId,
    pub total_size: u64,
    pub chunks: Vec<ChunkRef>,
}

impl ChunkManifest {
    pub fn chunks_to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(&self.chunks).unwrap_or_default()
    }

    pub fn chunks_from_bytes(data: &[u8]) -> Option<Vec<ChunkRef>> {
        rmp_serde::from_slice(data).ok()
    }

    pub fn from_db(file_id: FileId, total_size: u64, chunk_blob: &[u8]) -> Option<Self> {
        let chunks = Self::chunks_from_bytes(chunk_blob)?;
        Some(Self {
            file_id,
            total_size,
            chunks,
        })
    }
}

pub struct FileReader {
    store: std::sync::Arc<CasStore>,
    manifests: RwLock<HashMap<FileId, ChunkManifest>>,
}

impl FileReader {
    pub fn new(store: std::sync::Arc<CasStore>) -> Self {
        Self {
            store,
            manifests: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_manifest(&self, manifest: ChunkManifest) {
        let mut manifests = self.manifests.write().unwrap();
        manifests.insert(manifest.file_id, manifest);
    }

    pub async fn read(
        &self,
        file_id: FileId,
        offset: u64,
        size: u32,
    ) -> Result<Bytes, ReaderError> {
        let manifest = {
            let manifests = self.manifests.read().unwrap();
            manifests
                .get(&file_id)
                .cloned()
                .ok_or(ReaderError::ManifestNotFound(file_id))?
        };

        if offset >= manifest.total_size {
            return Ok(Bytes::new());
        }

        let end = std::cmp::min(offset + size as u64, manifest.total_size);
        let mut result = BytesMut::with_capacity((end - offset) as usize);

        for chunk_ref in &manifest.chunks {
            let chunk_start = chunk_ref.offset;
            let chunk_end = chunk_ref.offset + chunk_ref.size as u64;

            if chunk_end <= offset || chunk_start >= end {
                continue;
            }

            let chunk_data = self.store.get(&chunk_ref.hash).await?;

            let read_start = if offset > chunk_start {
                (offset - chunk_start) as usize
            } else {
                0
            };

            let read_end = if end < chunk_end {
                (end - chunk_start) as usize
            } else {
                chunk_ref.size as usize
            };

            result.extend_from_slice(&chunk_data[read_start..read_end]);
        }

        Ok(result.freeze())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error("Manifest not found for file {0:?}")]
    ManifestNotFound(FileId),

    #[error("CAS error: {0}")]
    Cas(#[from] crate::store::CasError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CasConfig;
    use musicfs_core::ChunkHash;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_file_reader_simple() {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = std::sync::Arc::new(CasStore::open(config).await.unwrap());

        let data = b"Hello, World!";
        let hash = store.put(data).await.unwrap();

        let reader = FileReader::new(store);
        reader.register_manifest(ChunkManifest {
            file_id: FileId(1),
            total_size: data.len() as u64,
            chunks: vec![ChunkRef {
                hash,
                offset: 0,
                size: data.len() as u32,
            }],
        });

        let result = reader.read(FileId(1), 0, data.len() as u32).await.unwrap();
        assert_eq!(&result[..], data);
    }

    #[tokio::test]
    async fn test_file_reader_partial() {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = std::sync::Arc::new(CasStore::open(config).await.unwrap());

        let data = b"ABCDEFGHIJ";
        let hash = store.put(data).await.unwrap();

        let reader = FileReader::new(store);
        reader.register_manifest(ChunkManifest {
            file_id: FileId(1),
            total_size: data.len() as u64,
            chunks: vec![ChunkRef {
                hash,
                offset: 0,
                size: data.len() as u32,
            }],
        });

        let result = reader.read(FileId(1), 3, 4).await.unwrap();
        assert_eq!(&result[..], b"DEFG");
    }

    #[tokio::test]
    async fn test_file_reader_multi_chunk() {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = std::sync::Arc::new(CasStore::open(config).await.unwrap());

        let chunk1 = b"AAAA";
        let chunk2 = b"BBBB";
        let hash1 = store.put(chunk1).await.unwrap();
        let hash2 = store.put(chunk2).await.unwrap();

        let reader = FileReader::new(store);
        reader.register_manifest(ChunkManifest {
            file_id: FileId(1),
            total_size: 8,
            chunks: vec![
                ChunkRef {
                    hash: hash1,
                    offset: 0,
                    size: 4,
                },
                ChunkRef {
                    hash: hash2,
                    offset: 4,
                    size: 4,
                },
            ],
        });

        let result = reader.read(FileId(1), 2, 4).await.unwrap();
        assert_eq!(&result[..], b"AABB");
    }

    #[tokio::test]
    async fn test_file_reader_eof() {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = std::sync::Arc::new(CasStore::open(config).await.unwrap());

        let data = b"short";
        let hash = store.put(data).await.unwrap();

        let reader = FileReader::new(store);
        reader.register_manifest(ChunkManifest {
            file_id: FileId(1),
            total_size: data.len() as u64,
            chunks: vec![ChunkRef {
                hash,
                offset: 0,
                size: data.len() as u32,
            }],
        });

        let result = reader.read(FileId(1), 100, 10).await.unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_chunk_manifest_serialization() {
        let manifest = ChunkManifest {
            file_id: FileId(42),
            total_size: 1024,
            chunks: vec![ChunkRef {
                hash: ChunkHash::from_bytes(b"test"),
                offset: 0,
                size: 1024,
            }],
        };

        let bytes = manifest.chunks_to_bytes();
        let restored = ChunkManifest::chunks_from_bytes(&bytes).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].size, 1024);
    }
}
