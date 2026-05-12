use musicfs_cache::TreeBuilder;
use musicfs_cas::{CasConfig, CasStore, ChunkManifest, ChunkRef, ContentFetcher, FileReader};
use musicfs_core::{FileId, FileMeta, OriginId, RealPath, VirtualPath};
use musicfs_origins::LocalOrigin;
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
        mtime: 0,
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

#[tokio::test]
async fn test_fetcher_cache_miss_flow() {
    let origin_dir = TempDir::new().unwrap();
    let cas_dir = TempDir::new().unwrap();

    let test_content = b"This is audio content that will be fetched on cache miss";
    let test_file_path = origin_dir.path().join("test.flac");
    std::fs::write(&test_file_path, test_content).unwrap();

    let config = CasConfig {
        chunks_dir: cas_dir.path().join("chunks"),
        ..Default::default()
    };
    let store = Arc::new(CasStore::open(config).await.unwrap());

    let origin_id = OriginId::from("test-origin");
    let origin = Arc::new(LocalOrigin::new(origin_id.clone(), origin_dir.path().to_path_buf()));

    let fetcher = ContentFetcher::new(store.clone());
    fetcher.register_origin(origin);

    let file_id = FileId(42);
    let file_meta = FileMeta {
        id: file_id,
        virtual_path: VirtualPath::new("/Artist/Album/test.flac"),
        real_path: RealPath {
            origin_id,
            path: PathBuf::from("/test.flac"),
        },
        size: test_content.len() as u64,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: None,
    };
    fetcher.register_file(file_meta);

    let manifest = fetcher.fetch_file(file_id).await.unwrap();

    assert_eq!(manifest.file_id, file_id);
    assert_eq!(manifest.total_size, test_content.len() as u64);
    assert_eq!(manifest.chunks.len(), 1);

    let chunk_data = store.get(&manifest.chunks[0].hash).await.unwrap();
    assert_eq!(&chunk_data[..], test_content);
}

#[tokio::test]
async fn test_reader_with_fetcher_integration() {
    let origin_dir = TempDir::new().unwrap();
    let cas_dir = TempDir::new().unwrap();

    let test_content = b"Audio file content for reader integration test";
    let test_file_path = origin_dir.path().join("song.flac");
    std::fs::write(&test_file_path, test_content).unwrap();

    let config = CasConfig {
        chunks_dir: cas_dir.path().join("chunks"),
        ..Default::default()
    };
    let store = Arc::new(CasStore::open(config).await.unwrap());

    let origin_id = OriginId::from("local");
    let origin = Arc::new(LocalOrigin::new(origin_id.clone(), origin_dir.path().to_path_buf()));

    let fetcher = ContentFetcher::new(store.clone());
    fetcher.register_origin(origin);

    let file_id = FileId(100);
    let file_meta = FileMeta {
        id: file_id,
        virtual_path: VirtualPath::new("/Test/song.flac"),
        real_path: RealPath {
            origin_id,
            path: PathBuf::from("/song.flac"),
        },
        size: test_content.len() as u64,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: None,
    };
    fetcher.register_file(file_meta);

    let reader = FileReader::with_fetcher(store, Arc::new(fetcher));

    let result = reader
        .read(file_id, 0, test_content.len() as u32)
        .await
        .unwrap();

    assert_eq!(&result[..], test_content);

    let result2 = reader.read(file_id, 0, 10).await.unwrap();
    assert_eq!(&result2[..], &test_content[..10]);
}
