use musicfs_cache::TreeBuilder;
use musicfs_cas::{CasConfig, CasStore};
use musicfs_core::{
    AudioFormat, AudioMeta, FileId, FileMeta, OriginId, RealPath, VirtualPath,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;
use tempfile::TempDir;

pub fn make_file_meta(id: i64, vpath: &str, size: u64) -> FileMeta {
    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(vpath),
        real_path: RealPath {
            origin_id: OriginId::from("test"),
            path: PathBuf::from(vpath),
        },
        size,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: None,
    }
}

pub fn make_file_meta_with_origin(id: i64, vpath: &str, size: u64, origin_id: &str) -> FileMeta {
    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(vpath),
        real_path: RealPath {
            origin_id: OriginId::from(origin_id),
            path: PathBuf::from(vpath),
        },
        size,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: None,
    }
}

pub fn make_audio_meta(artist: &str, album: &str, title: &str) -> AudioMeta {
    AudioMeta {
        title: Some(title.to_string()),
        artist: Some(artist.to_string()),
        album: Some(album.to_string()),
        album_artist: None,
        genre: None,
        year: None,
        track: None,
        disc: None,
        duration_ms: Some(180_000),
        bitrate: Some(320),
        sample_rate: Some(44100),
        format: AudioFormat::Flac,
    }
}

pub fn make_audio_file(
    id: i64,
    vpath: &str,
    size: u64,
    artist: &str,
    album: &str,
    title: &str,
) -> FileMeta {
    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(vpath),
        real_path: RealPath {
            origin_id: OriginId::from("test"),
            path: PathBuf::from(vpath),
        },
        size,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: Some(make_audio_meta(artist, album, title)),
    }
}

pub fn make_audio_file_full(
    id: i64,
    vpath: &str,
    size: u64,
    artist: &str,
    album: &str,
    title: &str,
    track: u32,
    year: u32,
) -> FileMeta {
    let mut audio = make_audio_meta(artist, album, title);
    audio.track = Some(track);
    audio.year = Some(year);

    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(vpath),
        real_path: RealPath {
            origin_id: OriginId::from("test"),
            path: PathBuf::from(vpath),
        },
        size,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: Some(audio),
    }
}

pub struct TestCasStore {
    pub store: Arc<CasStore>,
    pub dir: TempDir,
}

pub async fn setup_test_cas() -> TestCasStore {
    let dir = TempDir::new().expect("Failed to create temp dir for CAS");
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        max_size: 100 * 1024 * 1024,
        shard_levels: 2,
    };
    let store = CasStore::open(config)
        .await
        .expect("Failed to open CAS store");
    TestCasStore {
        store: Arc::new(store),
        dir,
    }
}

pub async fn setup_test_cas_with_size(max_size: u64) -> TestCasStore {
    let dir = TempDir::new().expect("Failed to create temp dir for CAS");
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        max_size,
        shard_levels: 2,
    };
    let store = CasStore::open(config)
        .await
        .expect("Failed to open CAS store");
    TestCasStore {
        store: Arc::new(store),
        dir,
    }
}

pub fn setup_test_tree(files: &[FileMeta]) -> Arc<RwLock<musicfs_cache::VirtualTree>> {
    let mut builder = TreeBuilder::new();
    for file in files {
        builder.add_file(file);
    }
    Arc::new(RwLock::new(builder.build()))
}

pub fn create_test_file(dir: &Path, relative_path: &str, content: &[u8]) -> PathBuf {
    let full_path = dir.join(relative_path);
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent).expect("Failed to create parent directories");
    }
    std::fs::write(&full_path, content).expect("Failed to write test file");
    full_path
}

pub fn create_test_dir_structure(base: &Path, structure: &[&str]) {
    for path in structure {
        let full_path = base.join(path);
        if path.ends_with('/') {
            std::fs::create_dir_all(&full_path).expect("Failed to create directory");
        } else {
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).expect("Failed to create parent");
            }
            std::fs::write(&full_path, format!("content of {}", path))
                .expect("Failed to write file");
        }
    }
}

pub struct TestOriginDir {
    pub dir: TempDir,
}

impl TestOriginDir {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().expect("Failed to create origin temp dir"),
        }
    }

    pub fn add_file(&self, path: &str, content: &[u8]) -> PathBuf {
        create_test_file(self.dir.path(), path, content)
    }

    pub fn add_audio_file(&self, path: &str) -> PathBuf {
        let fake_audio = b"FAKE_FLAC_HEADER_FOR_TESTING_ONLY";
        self.add_file(path, fake_audio)
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Default for TestOriginDir {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_file_meta() {
        let meta = make_file_meta(1, "/Artist/Album/Track.flac", 1000);
        assert_eq!(meta.id.0, 1);
        assert_eq!(meta.virtual_path.as_str(), "/Artist/Album/Track.flac");
        assert_eq!(meta.size, 1000);
        assert!(meta.audio.is_none());
    }

    #[test]
    fn test_make_audio_file() {
        let meta = make_audio_file(1, "/path.flac", 5000, "Artist", "Album", "Title");
        assert!(meta.audio.is_some());
        let audio = meta.audio.unwrap();
        assert_eq!(audio.artist, Some("Artist".to_string()));
        assert_eq!(audio.album, Some("Album".to_string()));
        assert_eq!(audio.title, Some("Title".to_string()));
    }

    #[tokio::test]
    async fn test_setup_test_cas() {
        let test_cas = setup_test_cas().await;
        let hash = test_cas.store.put(b"test data").await.unwrap();
        assert!(test_cas.store.exists(&hash));
    }

    #[test]
    fn test_setup_test_tree() {
        let files = vec![
            make_file_meta(1, "/A/B/1.flac", 100),
            make_file_meta(2, "/A/B/2.flac", 200),
        ];
        let tree = setup_test_tree(&files);
        let guard = tree.read().unwrap();
        assert!(guard.file_count() > 0);
    }

    #[test]
    fn test_origin_dir() {
        let origin = TestOriginDir::new();
        let path = origin.add_file("artist/album/track.flac", b"content");
        assert!(path.exists());
    }
}
