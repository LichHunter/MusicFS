use musicfs_cache::TreeBuilder;
use musicfs_cas::{CasConfig, CasStore, ChunkManifest, ChunkRef, FileReader};
use musicfs_core::{FileId, FileMeta, OriginId, RealPath, VirtualPath};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;
use tempfile::TempDir;

fn make_file_meta(id: i64, vpath: &str, size: u64) -> FileMeta {
    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(vpath),
        real_path: RealPath {
            origin_id: OriginId::from("test"),
            path: PathBuf::from("/test"),
        },
        size,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: None,
    }
}

#[tokio::test]
async fn test_cas_and_tree_integration() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        ..Default::default()
    };
    let store = Arc::new(CasStore::open(config).await.unwrap());

    let file_data = b"This is test audio file content for testing.";
    let chunk_hash = store.put(file_data).await.unwrap();

    let mut builder = TreeBuilder::new();
    builder.add_file(&make_file_meta(
        1,
        "/Artist/Album/Track.flac",
        file_data.len() as u64,
    ));
    let _tree = Arc::new(RwLock::new(builder.build()));

    let reader = Arc::new(FileReader::new(store.clone()));
    reader.register_manifest(ChunkManifest {
        file_id: FileId(1),
        total_size: file_data.len() as u64,
        chunks: vec![ChunkRef {
            hash: chunk_hash,
            offset: 0,
            size: file_data.len() as u32,
        }],
    });

    let result = reader
        .read(FileId(1), 0, file_data.len() as u32)
        .await
        .unwrap();
    assert_eq!(&result[..], file_data);
}

#[tokio::test]
async fn test_cache_persistence() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        ..Default::default()
    };

    let data = b"persistent data";
    let hash = {
        let store = CasStore::open(config.clone()).await.unwrap();
        store.put(data).await.unwrap()
    };

    let store = CasStore::open(config).await.unwrap();
    let retrieved = store.get(&hash).await.unwrap();
    assert_eq!(&retrieved[..], data);
}

#[tokio::test]
async fn test_deduplication() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        ..Default::default()
    };
    let store = CasStore::open(config).await.unwrap();

    let data = b"duplicate this content";

    let hash1 = store.put(data).await.unwrap();
    let size_after_first = store.current_size();

    let hash2 = store.put(data).await.unwrap();
    let size_after_second = store.current_size();

    assert_eq!(hash1, hash2);
    assert_eq!(size_after_first, size_after_second);
}
