use crate::traits::{Origin, WatchCallback, WatchHandle};
use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, OriginType, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncRead;
use tracing::debug;

pub struct LocalOrigin {
    id: OriginId,
    root: PathBuf,
    display_name: String,
}

impl LocalOrigin {
    pub fn new(id: impl Into<OriginId>, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let display_name = format!("Local: {}", root.display());
        Self {
            id: id.into(),
            root,
            display_name,
        }
    }

    fn full_path(&self, path: &Path) -> PathBuf {
        if path.as_os_str().is_empty() || path == Path::new("/") {
            self.root.clone()
        } else {
            self.root.join(path.strip_prefix("/").unwrap_or(path))
        }
    }
}

#[async_trait]
impl Origin for LocalOrigin {
    fn id(&self) -> &OriginId {
        &self.id
    }

    fn origin_type(&self) -> OriginType {
        OriginType::Local
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let full_path = self.full_path(path);
        debug!("LocalOrigin::readdir({:?})", full_path);

        let mut entries = Vec::new();
        let mut dir = fs::read_dir(&full_path).await?;

        while let Some(entry) = dir.next_entry().await? {
            let metadata = entry.metadata().await?;
            let name = entry.file_name().to_string_lossy().into_owned();

            entries.push(DirEntry {
                name,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                mtime: metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
            });
        }

        Ok(entries)
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        let full_path = self.full_path(path);
        debug!("LocalOrigin::stat({:?})", full_path);

        let metadata = fs::metadata(&full_path).await?;

        Ok(FileStat {
            size: metadata.len(),
            mtime: metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
            is_dir: metadata.is_dir(),
        })
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let full_path = self.full_path(path);
        debug!(
            "LocalOrigin::read({:?}, offset={}, size={})",
            full_path, offset, size
        );

        let mut file = fs::File::open(&full_path).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;

        // FIX: Loop until all requested bytes are read or EOF
        // Single read() only returns kernel buffer (~2MB), not full request
        let mut buffer = Vec::with_capacity(size as usize);
        let mut temp_buf = vec![0u8; 64 * 1024]; // 64KB chunks
        let mut total_read = 0usize;

        while total_read < size as usize {
            let to_read = std::cmp::min(temp_buf.len(), size as usize - total_read);
            let n = file.read(&mut temp_buf[..to_read]).await?;
            if n == 0 {
                break; // EOF
            }
            buffer.extend_from_slice(&temp_buf[..n]);
            total_read += n;
        }

        Ok(buffer)
    }

    async fn read_full(&self, path: &Path) -> Result<Vec<u8>> {
        let full_path = self.full_path(path);
        debug!("LocalOrigin::read_full({:?})", full_path);
        Ok(fs::read(&full_path).await?)
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        let full_path = self.full_path(path);
        Ok(fs::try_exists(&full_path).await?)
    }

    async fn health(&self) -> HealthStatus {
        match fs::try_exists(&self.root).await {
            Ok(true) => HealthStatus::Healthy,
            Ok(false) => HealthStatus::Unhealthy,
            Err(_) => HealthStatus::Unhealthy,
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let full_path = self.full_path(path);
        let file = fs::File::open(&full_path).await?;
        Ok(Box::new(file))
    }

    async fn watch(&self, path: &Path, _callback: WatchCallback) -> Result<WatchHandle> {
        debug!("LocalOrigin::watch({:?}) - stub implementation", path);
        let (tx, _rx) = tokio::sync::oneshot::channel();
        Ok(WatchHandle::new(tx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_local_origin_readdir() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let origin = LocalOrigin::new("test", dir.path());
        let entries = origin.readdir(Path::new("/")).await.unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.name == "test.txt" && !e.is_dir));
        assert!(entries.iter().any(|e| e.name == "subdir" && e.is_dir));
    }

    #[tokio::test]
    async fn test_local_origin_stat() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world").unwrap();

        let origin = LocalOrigin::new("test", dir.path());
        let stat = origin.stat(Path::new("/test.txt")).await.unwrap();

        assert_eq!(stat.size, 11);
        assert!(!stat.is_dir);
    }

    #[tokio::test]
    async fn test_local_origin_read() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world").unwrap();

        let origin = LocalOrigin::new("test", dir.path());
        let data = origin.read(Path::new("/test.txt"), 0, 5).await.unwrap();

        assert_eq!(data, b"hello");
    }

    #[tokio::test]
    async fn test_local_origin_read_offset() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world").unwrap();

        let origin = LocalOrigin::new("test", dir.path());
        let data = origin.read(Path::new("/test.txt"), 6, 5).await.unwrap();

        assert_eq!(data, b"world");
    }

    #[tokio::test]
    async fn test_local_origin_exists() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

        let origin = LocalOrigin::new("test", dir.path());

        assert!(origin.exists(Path::new("/test.txt")).await.unwrap());
        assert!(!origin.exists(Path::new("/nonexistent.txt")).await.unwrap());
    }

    #[tokio::test]
    async fn test_local_origin_health() {
        let dir = TempDir::new().unwrap();
        let origin = LocalOrigin::new("test", dir.path());

        assert_eq!(origin.health().await, HealthStatus::Healthy);
    }
}
