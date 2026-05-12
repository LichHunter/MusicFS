use crate::local::LocalOrigin;
use crate::traits::{Origin, WatchCallback, WatchHandle};
use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, OriginType, Result};
use std::future::Future;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

pub struct SmbOrigin {
    inner: LocalOrigin,
    share_path: String,
}

impl SmbOrigin {
    pub fn from_mount(
        id: impl Into<OriginId>,
        mount_point: impl Into<PathBuf>,
        share_path: impl Into<String>,
    ) -> Self {
        let mount_point = mount_point.into();
        let share_path = share_path.into();

        Self {
            inner: LocalOrigin::new(id, &mount_point),
            share_path,
        }
    }

    pub async fn is_mounted(&self) -> bool {
        self.inner.exists(Path::new("/")).await.unwrap_or(false)
    }

    async fn retry_on_disconnect<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        const MAX_RETRIES: u32 = 3;

        for attempt in 0..MAX_RETRIES {
            match op().await {
                Ok(val) => return Ok(val),
                Err(e) => {
                    if Self::is_enotconn(&e) && attempt < MAX_RETRIES - 1 {
                        debug!(attempt, "SMB ENOTCONN, retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        unreachable!()
    }

    #[cfg(unix)]
    fn is_enotconn(err: &musicfs_core::Error) -> bool {
        if let musicfs_core::Error::Io(io_err) = err {
            io_err.raw_os_error() == Some(libc::ENOTCONN)
        } else {
            false
        }
    }

    #[cfg(not(unix))]
    fn is_enotconn(_err: &musicfs_core::Error) -> bool {
        false
    }
}

#[async_trait]
impl Origin for SmbOrigin {
    fn id(&self) -> &OriginId {
        self.inner.id()
    }

    fn origin_type(&self) -> OriginType {
        OriginType::Smb
    }

    fn display_name(&self) -> &str {
        &self.share_path
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.retry_on_disconnect(|| self.inner.readdir(path)).await
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        self.retry_on_disconnect(|| self.inner.stat(path)).await
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        self.retry_on_disconnect(|| self.inner.read(path, offset, size)).await
    }

    async fn read_full(&self, path: &Path) -> Result<Vec<u8>> {
        self.retry_on_disconnect(|| self.inner.read_full(path)).await
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        self.retry_on_disconnect(|| self.inner.exists(path)).await
    }

    async fn health(&self) -> HealthStatus {
        let health_timeout = std::time::Duration::from_secs(5);
        match tokio::time::timeout(health_timeout, self.is_mounted()).await {
            Ok(true) => HealthStatus::Healthy,
            Ok(false) | Err(_) => HealthStatus::Unhealthy,
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        self.inner.open_read(path).await
    }

    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle> {
        warn!("SMB watch using inotify - may be unreliable. Consider polling for remote mounts.");
        self.inner.watch(path, callback).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_smb_origin_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.flac"), b"audio").unwrap();

        let origin = SmbOrigin::from_mount("smb-test", dir.path(), "//server/share");

        let entries = origin.readdir(Path::new("/")).await.unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn test_smb_origin_type() {
        let dir = TempDir::new().unwrap();
        let origin = SmbOrigin::from_mount("smb-test", dir.path(), "//server/share");

        assert_eq!(origin.origin_type(), OriginType::Smb);
    }

    #[tokio::test]
    async fn test_smb_display_name() {
        let dir = TempDir::new().unwrap();
        let origin = SmbOrigin::from_mount("smb-test", dir.path(), "//server/music");

        assert_eq!(origin.display_name(), "//server/music");
    }
}
