use crate::{CasStore, ChunkManifest, ChunkRef};
use musicfs_core::{Event, EventBus, FileId, FileMeta, OriginId};
use musicfs_origins::Origin;
use musicfs_sync::CdcChunker;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{debug, info};

pub struct ContentFetcher {
    store: Arc<CasStore>,
    origins: RwLock<HashMap<OriginId, Arc<dyn Origin>>>,
    file_meta: RwLock<HashMap<FileId, FileMeta>>,
    event_bus: Option<Arc<EventBus>>,
    chunker: CdcChunker,
}

impl ContentFetcher {
    pub fn new(store: Arc<CasStore>) -> Self {
        Self {
            store,
            origins: RwLock::new(HashMap::new()),
            file_meta: RwLock::new(HashMap::new()),
            event_bus: None,
            chunker: CdcChunker::default(),
        }
    }

    pub fn with_event_bus(store: Arc<CasStore>, event_bus: Arc<EventBus>) -> Self {
        Self {
            store,
            origins: RwLock::new(HashMap::new()),
            file_meta: RwLock::new(HashMap::new()),
            event_bus: Some(event_bus),
            chunker: CdcChunker::default(),
        }
    }

    pub fn register_origin(&self, origin: Arc<dyn Origin>) {
        let id = origin.id().clone();
        self.origins.write().unwrap().insert(id, origin);
    }

    pub fn register_file(&self, meta: FileMeta) {
        self.file_meta.write().unwrap().insert(meta.id, meta);
    }

    pub fn register_files(&self, files: impl IntoIterator<Item = FileMeta>) {
        let mut map = self.file_meta.write().unwrap();
        for meta in files {
            map.insert(meta.id, meta);
        }
    }

    pub async fn fetch_file(&self, file_id: FileId) -> Result<ChunkManifest, FetchError> {
        let meta = {
            let files = self.file_meta.read().unwrap();
            files
                .get(&file_id)
                .cloned()
                .ok_or(FetchError::FileNotFound(file_id))?
        };

        let origin = {
            let origins = self.origins.read().unwrap();
            origins
                .get(&meta.real_path.origin_id)
                .cloned()
                .ok_or_else(|| FetchError::OriginNotFound(meta.real_path.origin_id.clone()))?
        };

        info!(
            "Fetching file {:?} from origin {}",
            file_id,
            origin.id()
        );

        let data = origin
            .read_full(&meta.real_path.path)
            .await
            .map_err(|e| FetchError::OriginRead(e.to_string()))?;

        let mtime = meta
            .mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let chunks = self.chunker.chunk_refs(&data);
        info!("Chunked {:?} into {} chunks", file_id, chunks.len());

        let mut chunk_refs = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            if !self.store.exists(&chunk.hash) {
                self.store.put(chunk.data).await.map_err(FetchError::Store)?;
            }

            chunk_refs.push(ChunkRef {
                hash: chunk.hash,
                offset: chunk.offset,
                size: chunk.length,
            });
        }

        let manifest = ChunkManifest {
            file_id,
            total_size: meta.size,
            mtime,
            chunks: chunk_refs,
        };

        debug!(
            "Created manifest for {:?}: {} bytes, {} chunks",
            file_id,
            meta.size,
            manifest.chunks.len()
        );

        Ok(manifest)
    }

    pub async fn ensure_cached(&self, file_id: FileId) -> Result<ChunkManifest, FetchError> {
        self.fetch_file(file_id).await
    }

    pub fn get_file_meta(&self, file_id: FileId) -> Option<FileMeta> {
        self.file_meta.read().unwrap().get(&file_id).cloned()
    }

    pub fn emit_access_event(&self, meta: &FileMeta, offset: u64, size: u32) {
        if let Some(bus) = &self.event_bus {
            bus.publish(Event::FileAccessed {
                path: meta.virtual_path.clone(),
                origin_id: meta.real_path.origin_id.clone(),
                offset,
                size,
            });
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("File not found: {0:?}")]
    FileNotFound(FileId),

    #[error("Origin not found: {0}")]
    OriginNotFound(OriginId),

    #[error("Origin read error: {0}")]
    OriginRead(String),

    #[error("Store error: {0}")]
    Store(#[from] crate::CasError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CasConfig;
    use musicfs_core::{RealPath, VirtualPath};
    use musicfs_origins::LocalOrigin;
    use std::path::PathBuf;
    use std::time::SystemTime;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_fetch_file() {
        let cas_dir = TempDir::new().unwrap();
        let origin_dir = TempDir::new().unwrap();

        std::fs::write(origin_dir.path().join("test.flac"), b"fake audio data").unwrap();

        let config = CasConfig {
            chunks_dir: cas_dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(config).await.unwrap());
        let fetcher = ContentFetcher::new(store.clone());

        let origin = Arc::new(LocalOrigin::new("local", origin_dir.path()));
        fetcher.register_origin(origin);

        let meta = FileMeta {
            id: FileId(1),
            virtual_path: VirtualPath::new("/Artist/Album/test.flac"),
            real_path: RealPath {
                origin_id: OriginId::from("local"),
                path: PathBuf::from("/test.flac"),
            },
            size: 15,
            mtime: SystemTime::now(),
            content_hash: None,
            audio: None,
        };
        fetcher.register_file(meta);

        let manifest = fetcher.fetch_file(FileId(1)).await.unwrap();
        assert_eq!(manifest.total_size, 15);
        assert_eq!(manifest.chunks.len(), 1);

        let data = store.get(&manifest.chunks[0].hash).await.unwrap();
        assert_eq!(&data[..], b"fake audio data");
    }

    #[tokio::test]
    async fn test_fetch_file_not_found() {
        let cas_dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: cas_dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(config).await.unwrap());
        let fetcher = ContentFetcher::new(store);

        let result = fetcher.fetch_file(FileId(999)).await;
        assert!(matches!(result, Err(FetchError::FileNotFound(_))));
    }

    #[tokio::test]
    async fn test_fetch_emits_event() {
        let cas_dir = TempDir::new().unwrap();
        let origin_dir = TempDir::new().unwrap();
        std::fs::write(origin_dir.path().join("test.flac"), b"audio").unwrap();

        let config = CasConfig {
            chunks_dir: cas_dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(config).await.unwrap());
        let event_bus = Arc::new(EventBus::default());
        let mut rx = event_bus.subscribe();

        let fetcher = ContentFetcher::with_event_bus(store, event_bus);
        let origin = Arc::new(LocalOrigin::new("local", origin_dir.path()));
        fetcher.register_origin(origin);

        let meta = FileMeta {
            id: FileId(1),
            virtual_path: VirtualPath::new("/Artist/test.flac"),
            real_path: RealPath {
                origin_id: OriginId::from("local"),
                path: PathBuf::from("/test.flac"),
            },
            size: 5,
            mtime: SystemTime::now(),
            content_hash: None,
            audio: None,
        };
        fetcher.register_file(meta.clone());

        fetcher.emit_access_event(&meta, 0, 5);

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, Event::FileAccessed { .. }));
    }

    #[tokio::test]
    async fn test_fetch_origin_not_found() {
        let cas_dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: cas_dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(config).await.unwrap());
        let fetcher = ContentFetcher::new(store);

        let meta = FileMeta {
            id: FileId(1),
            virtual_path: VirtualPath::new("/test.flac"),
            real_path: RealPath {
                origin_id: OriginId::from("nonexistent"),
                path: PathBuf::from("/test.flac"),
            },
            size: 100,
            mtime: SystemTime::now(),
            content_hash: None,
            audio: None,
        };
        fetcher.register_file(meta);

        let result = fetcher.fetch_file(FileId(1)).await;
        assert!(matches!(result, Err(FetchError::OriginNotFound(_))));
    }
}
