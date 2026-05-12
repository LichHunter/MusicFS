use crate::local::LocalOrigin;
use crate::traits::{Origin, WatchCallback, WatchHandle};
use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, OriginType, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

pub struct NfsOrigin {
    inner: LocalOrigin,
    max_retries: u32,
    display_name: String,
}

impl NfsOrigin {
    pub fn new(id: impl Into<OriginId>, mount_point: impl Into<PathBuf>) -> Self {
        let mount_point = mount_point.into();
        let display_name = format!("NFS: {}", mount_point.display());

        Self {
            inner: LocalOrigin::new(id, &mount_point),
            max_retries: 3,
            display_name,
        }
    }

    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    async fn retry_on_stale<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut delay = Duration::from_millis(100);

        for attempt in 0..self.max_retries {
            match op().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if let Some(io_err) = e.downcast_io() {
                        #[cfg(unix)]
                        if io_err.raw_os_error() == Some(libc::ESTALE) {
                            warn!(
                                "NFS stale handle (attempt {}/{}), retrying after {:?}",
                                attempt + 1,
                                self.max_retries,
                                delay
                            );
                            sleep(delay).await;
                            delay *= 2;
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        Err(musicfs_core::Error::NfsStaleHandle)
    }
}

#[async_trait]
impl Origin for NfsOrigin {
    fn id(&self) -> &OriginId {
        self.inner.id()
    }

    fn origin_type(&self) -> OriginType {
        OriginType::Nfs
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.retry_on_stale(|| self.inner.readdir(path)).await
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        self.retry_on_stale(|| self.inner.stat(path)).await
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        self.retry_on_stale(|| self.inner.read(path, offset, size))
            .await
    }

    async fn read_full(&self, path: &Path) -> Result<Vec<u8>> {
        self.retry_on_stale(|| self.inner.read_full(path)).await
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        self.retry_on_stale(|| self.inner.exists(path)).await
    }

    async fn health(&self) -> HealthStatus {
        let health_timeout = Duration::from_secs(5);
        match tokio::time::timeout(health_timeout, self.inner.stat(Path::new("/"))).await {
            Ok(Ok(_)) => HealthStatus::Healthy,
            Ok(Err(_)) | Err(_) => HealthStatus::Unhealthy,
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        self.inner.open_read(path).await
    }

    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle> {
        debug!("NFS watch - inotify may be unreliable over NFS, consider polling");
        self.inner.watch(path, callback).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_nfs_origin_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.flac"), b"audio").unwrap();

        let origin = NfsOrigin::new("nfs-test", dir.path());

        let entries = origin.readdir(Path::new("/")).await.unwrap();
        assert_eq!(entries.len(), 1);

        let data = origin.read(Path::new("/test.flac"), 0, 5).await.unwrap();
        assert_eq!(&data, b"audio");
    }

    #[tokio::test]
    async fn test_nfs_origin_health() {
        let dir = TempDir::new().unwrap();
        let origin = NfsOrigin::new("nfs-test", dir.path());

        assert_eq!(origin.health().await, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn test_nfs_origin_type() {
        let dir = TempDir::new().unwrap();
        let origin = NfsOrigin::new("nfs-test", dir.path());

        assert_eq!(origin.origin_type(), OriginType::Nfs);
    }

    #[test]
    fn test_retry_uses_fn_not_fnmut() {
        fn assert_fn<F: Fn() -> Fut, Fut>(_: F) {}

        let closure = || async { Ok::<_, musicfs_core::Error>(()) };
        assert_fn(closure);
    }
}
