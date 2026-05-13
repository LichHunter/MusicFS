use async_trait::async_trait;
use musicfs_core::{DirEntry, Error, FileStat, HealthStatus, OriginId, OriginType, Result};
use musicfs_origins::{Origin, WatchCallback, WatchHandle};
use parking_lot::RwLock;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncRead;

#[derive(Debug, Clone)]
pub enum FailMode {
    Healthy,
    FailEveryNth(usize),
    FailAfterN(usize),
    TimeoutMs(u64),
    PartialRead { max_bytes: usize },
    ReturnError(ErrorKind),
}

impl Default for FailMode {
    fn default() -> Self {
        FailMode::Healthy
    }
}

pub struct FaultyOrigin {
    inner: Arc<dyn Origin>,
    fail_mode: Arc<RwLock<FailMode>>,
    call_count: AtomicUsize,
}

impl FaultyOrigin {
    pub fn new(inner: Arc<dyn Origin>, mode: FailMode) -> Self {
        Self {
            inner,
            fail_mode: Arc::new(RwLock::new(mode)),
            call_count: AtomicUsize::new(0),
        }
    }

    pub fn wrap(inner: impl Origin + 'static) -> Self {
        Self::new(Arc::new(inner), FailMode::Healthy)
    }

    pub fn set_mode(&self, mode: FailMode) {
        *self.fail_mode.write() = mode;
    }

    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    pub fn reset_count(&self) {
        self.call_count.store(0, Ordering::SeqCst);
    }

    fn increment_and_check(&self) -> Option<Error> {
        let count = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
        let mode = self.fail_mode.read();

        match *mode {
            FailMode::Healthy => None,
            FailMode::FailEveryNth(n) if n > 0 && count % n == 0 => {
                Some(Error::Origin("Injected failure (every Nth)".into()))
            }
            FailMode::FailEveryNth(_) => None,
            FailMode::FailAfterN(n) if count > n => {
                Some(Error::Origin("Injected failure (after N)".into()))
            }
            FailMode::FailAfterN(_) => None,
            FailMode::TimeoutMs(_) => None,
            FailMode::PartialRead { .. } => None,
            FailMode::ReturnError(kind) => {
                Some(Error::Io(io::Error::new(kind, "Injected I/O error")))
            }
        }
    }

    async fn maybe_timeout(&self) -> Option<Error> {
        let mode = self.fail_mode.read().clone();
        if let FailMode::TimeoutMs(ms) = mode {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Some(Error::Timeout("Injected timeout".into()))
        } else {
            None
        }
    }

    fn truncate_if_partial(&self, mut data: Vec<u8>) -> Vec<u8> {
        let mode = self.fail_mode.read();
        if let FailMode::PartialRead { max_bytes } = *mode {
            data.truncate(max_bytes);
        }
        data
    }
}

#[async_trait]
impl Origin for FaultyOrigin {
    fn id(&self) -> &OriginId {
        self.inner.id()
    }

    fn origin_type(&self) -> OriginType {
        self.inner.origin_type()
    }

    fn display_name(&self) -> &str {
        self.inner.display_name()
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        if let Some(err) = self.increment_and_check() {
            return Err(err);
        }
        if let Some(err) = self.maybe_timeout().await {
            return Err(err);
        }
        self.inner.readdir(path).await
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        if let Some(err) = self.increment_and_check() {
            return Err(err);
        }
        if let Some(err) = self.maybe_timeout().await {
            return Err(err);
        }
        self.inner.stat(path).await
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        if let Some(err) = self.increment_and_check() {
            return Err(err);
        }
        if let Some(err) = self.maybe_timeout().await {
            return Err(err);
        }
        let data = self.inner.read(path, offset, size).await?;
        Ok(self.truncate_if_partial(data))
    }

    async fn read_full(&self, path: &Path) -> Result<Vec<u8>> {
        if let Some(err) = self.increment_and_check() {
            return Err(err);
        }
        if let Some(err) = self.maybe_timeout().await {
            return Err(err);
        }
        let data = self.inner.read_full(path).await?;
        Ok(self.truncate_if_partial(data))
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        if let Some(err) = self.increment_and_check() {
            return Err(err);
        }
        if let Some(err) = self.maybe_timeout().await {
            return Err(err);
        }
        self.inner.exists(path).await
    }

    async fn health(&self) -> HealthStatus {
        let mode = self.fail_mode.read().clone();
        match mode {
            FailMode::Healthy => self.inner.health().await,
            FailMode::ReturnError(_) => HealthStatus::Unhealthy,
            FailMode::TimeoutMs(ms) => {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                HealthStatus::Unhealthy
            }
            FailMode::FailAfterN(n) if self.call_count.load(Ordering::SeqCst) >= n => {
                HealthStatus::Unhealthy
            }
            _ => self.inner.health().await,
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        if let Some(err) = self.increment_and_check() {
            return Err(err);
        }
        if let Some(err) = self.maybe_timeout().await {
            return Err(err);
        }
        self.inner.open_read(path).await
    }

    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle> {
        self.inner.watch(path, callback).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    struct MockOrigin {
        id: OriginId,
    }

    impl MockOrigin {
        fn new(id: &str) -> Self {
            Self {
                id: OriginId::from(id),
            }
        }
    }

    #[async_trait]
    impl Origin for MockOrigin {
        fn id(&self) -> &OriginId {
            &self.id
        }

        fn origin_type(&self) -> OriginType {
            OriginType::Local
        }

        fn display_name(&self) -> &str {
            "mock"
        }

        async fn readdir(&self, _path: &Path) -> Result<Vec<DirEntry>> {
            Ok(vec![])
        }

        async fn stat(&self, _path: &Path) -> Result<FileStat> {
            Ok(FileStat {
                size: 1000,
                mtime: SystemTime::now(),
                is_dir: false,
            })
        }

        async fn read(&self, _path: &Path, _offset: u64, size: u32) -> Result<Vec<u8>> {
            Ok(vec![0u8; size as usize])
        }

        async fn read_full(&self, _path: &Path) -> Result<Vec<u8>> {
            Ok(vec![0u8; 100])
        }

        async fn exists(&self, _path: &Path) -> Result<bool> {
            Ok(true)
        }

        async fn health(&self) -> HealthStatus {
            HealthStatus::Healthy
        }

        async fn open_read(&self, _path: &Path) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
            Err(Error::Origin("Not implemented".into()))
        }

        async fn watch(&self, _path: &Path, _callback: WatchCallback) -> Result<WatchHandle> {
            Err(Error::Origin("Not implemented".into()))
        }
    }

    #[tokio::test]
    async fn test_healthy_passthrough() {
        let inner = Arc::new(MockOrigin::new("test"));
        let faulty = FaultyOrigin::new(inner, FailMode::Healthy);

        let result = faulty.stat(Path::new("/test")).await;
        assert!(result.is_ok());
        assert_eq!(faulty.call_count(), 1);
    }

    #[tokio::test]
    async fn test_fail_every_nth() {
        let inner = Arc::new(MockOrigin::new("test"));
        let faulty = FaultyOrigin::new(inner, FailMode::FailEveryNth(2));

        assert!(faulty.stat(Path::new("/test")).await.is_ok());
        assert!(faulty.stat(Path::new("/test")).await.is_err());
        assert!(faulty.stat(Path::new("/test")).await.is_ok());
        assert!(faulty.stat(Path::new("/test")).await.is_err());
        assert_eq!(faulty.call_count(), 4);
    }

    #[tokio::test]
    async fn test_fail_after_n() {
        let inner = Arc::new(MockOrigin::new("test"));
        let faulty = FaultyOrigin::new(inner, FailMode::FailAfterN(2));

        assert!(faulty.stat(Path::new("/test")).await.is_ok());
        assert!(faulty.stat(Path::new("/test")).await.is_ok());
        assert!(faulty.stat(Path::new("/test")).await.is_err());
        assert!(faulty.stat(Path::new("/test")).await.is_err());
    }

    #[tokio::test]
    async fn test_partial_read() {
        let inner = Arc::new(MockOrigin::new("test"));
        let faulty = FaultyOrigin::new(inner, FailMode::PartialRead { max_bytes: 10 });

        let data = faulty.read(Path::new("/test"), 0, 100).await.unwrap();
        assert_eq!(data.len(), 10);
    }

    #[tokio::test]
    async fn test_mode_change_mid_test() {
        let inner = Arc::new(MockOrigin::new("test"));
        let faulty = FaultyOrigin::new(inner, FailMode::ReturnError(ErrorKind::ConnectionRefused));

        assert!(faulty.stat(Path::new("/test")).await.is_err());

        faulty.set_mode(FailMode::Healthy);
        assert!(faulty.stat(Path::new("/test")).await.is_ok());
    }

    #[tokio::test]
    async fn test_health_reflects_mode() {
        let inner = Arc::new(MockOrigin::new("test"));
        let faulty = FaultyOrigin::new(inner, FailMode::Healthy);

        assert_eq!(faulty.health().await, HealthStatus::Healthy);

        faulty.set_mode(FailMode::ReturnError(ErrorKind::ConnectionRefused));
        assert_eq!(faulty.health().await, HealthStatus::Unhealthy);
    }
}
