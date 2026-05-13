use crate::index::{SearchError, SearchIndex};
use musicfs_core::{Event, EventBus, FileMeta};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, info_span, warn, Instrument};

pub trait MetadataLookup: Send + Sync {
    fn lookup(&self, path: &musicfs_core::VirtualPath) -> Option<FileMeta>;
}

pub struct Indexer<M: MetadataLookup> {
    index: Arc<SearchIndex>,
    event_bus: Arc<EventBus>,
    metadata_lookup: Arc<M>,
}

impl<M: MetadataLookup + 'static> Indexer<M> {
    pub fn new(
        index: Arc<SearchIndex>,
        event_bus: Arc<EventBus>,
        metadata_lookup: Arc<M>,
    ) -> Self {
        Self {
            index,
            event_bus,
            metadata_lookup,
        }
    }

    pub fn start(self) -> IndexerHandle {
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let mut event_rx = self.event_bus.subscribe();

        info!("Search indexer starting");

        tokio::spawn(
            async move {
                let mut pending_commit = false;
                let mut commit_timer = tokio::time::interval(std::time::Duration::from_secs(5));

                loop {
                    tokio::select! {
                        result = event_rx.recv() => {
                            match result {
                                Ok(event) => {
                                    if let Err(e) = self.handle_event(&event) {
                                        error!("Indexer error: {}", e);
                                    }
                                    pending_commit = true;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    warn!(skipped = n, "Indexer lagged, skipped events");
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    debug!("Event channel closed");
                                    break;
                                }
                            }
                        }
                        _ = commit_timer.tick() => {
                            if pending_commit {
                                if let Err(e) = self.index.commit() {
                                    error!("Index commit error: {}", e);
                                }
                                pending_commit = false;
                            }
                        }
                        _ = stop_rx.recv() => {
                            info!("Indexer stopping");
                            if pending_commit {
                                let _ = self.index.commit();
                            }
                            break;
                        }
                    }
                }
            }
            .instrument(info_span!("search_indexer")),
        );

        IndexerHandle { stop_tx }
    }

    fn handle_event(&self, event: &Event) -> Result<(), SearchError> {
        match event {
            Event::FileAdded { path, .. } => {
                debug!("Indexing added file: {:?}", path);
                if let Some(meta) = self.metadata_lookup.lookup(path) {
                    self.index.index_file(&meta)?;
                } else {
                    warn!("No metadata found for added file: {:?}", path);
                }
            }
            Event::FileRemoved { path, file_id } => {
                debug!("Removing from index: {:?}", path);
                if let Some(id) = file_id {
                    self.index.remove_file(*id)?;
                } else if let Some(meta) = self.metadata_lookup.lookup(path) {
                    self.index.remove_file(meta.id)?;
                } else {
                    self.index.remove_by_path(path)?;
                }
            }
            Event::FileModified { path } => {
                debug!("Re-indexing modified file: {:?}", path);
                if let Some(meta) = self.metadata_lookup.lookup(path) {
                    self.index.remove_file(meta.id)?;
                    self.index.index_file(&meta)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn index_batch(&self, files: &[FileMeta]) -> Result<usize, SearchError> {
        let mut count = 0;
        for file in files {
            self.index.index_file(file)?;
            count += 1;
        }
        self.index.commit()?;
        info!("Indexed {} files", count);
        Ok(count)
    }
}

pub struct IndexerHandle {
    stop_tx: mpsc::Sender<()>,
}

impl IndexerHandle {
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::{AudioFormat, AudioMeta, FileId, OriginId, RealPath, VirtualPath};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::RwLock;
    use tempfile::TempDir;

    struct MockMetadataLookup {
        files: RwLock<HashMap<String, FileMeta>>,
    }

    impl MockMetadataLookup {
        fn new() -> Self {
            Self {
                files: RwLock::new(HashMap::new()),
            }
        }

        fn insert(&self, meta: FileMeta) {
            self.files
                .write()
                .unwrap()
                .insert(meta.virtual_path.as_str().to_string(), meta);
        }
    }

    impl MetadataLookup for MockMetadataLookup {
        fn lookup(&self, path: &VirtualPath) -> Option<FileMeta> {
            self.files.read().unwrap().get(path.as_str()).cloned()
        }
    }

    fn make_file(id: i64, path: &str, artist: &str, title: &str) -> FileMeta {
        FileMeta {
            id: FileId(id),
            virtual_path: VirtualPath::new(path),
            real_path: RealPath {
                origin_id: OriginId::from("test"),
                path: PathBuf::from("test.flac"),
            },
            size: 1000,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            content_hash: None,
            audio: Some(AudioMeta {
                artist: Some(artist.to_string()),
                title: Some(title.to_string()),
                format: AudioFormat::Flac,
                ..Default::default()
            }),
        }
    }

    #[tokio::test]
    async fn test_indexer_handles_file_added() {
        let dir = TempDir::new().unwrap();
        let index = Arc::new(SearchIndex::open(dir.path()).unwrap());
        let event_bus = Arc::new(EventBus::default());
        let metadata = Arc::new(MockMetadataLookup::new());

        let file = make_file(1, "/Artist/Album/Track.flac", "Artist", "Track");
        metadata.insert(file.clone());

        let indexer = Indexer::new(index.clone(), event_bus.clone(), metadata);
        let handle = indexer.start();

        event_bus.publish(Event::FileAdded {
            path: VirtualPath::new("/Artist/Album/Track.flac"),
            origin_id: OriginId::from("test"),
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        index.commit().unwrap();

        let results = index.search("artist", 10).unwrap();
        assert_eq!(results.len(), 1);

        handle.stop().await;
    }

    #[test]
    fn test_index_batch() {
        let dir = TempDir::new().unwrap();
        let index = Arc::new(SearchIndex::open(dir.path()).unwrap());
        let event_bus = Arc::new(EventBus::default());
        let metadata = Arc::new(MockMetadataLookup::new());

        let indexer = Indexer::new(index.clone(), event_bus, metadata);

        let files = vec![
            make_file(1, "/a.flac", "Artist1", "Song1"),
            make_file(2, "/b.flac", "Artist2", "Song2"),
            make_file(3, "/c.flac", "Artist3", "Song3"),
        ];

        let count = indexer.index_batch(&files).unwrap();
        assert_eq!(count, 3);

        let results = index.search("artist1", 10).unwrap();
        assert_eq!(results.len(), 1);
    }
}
