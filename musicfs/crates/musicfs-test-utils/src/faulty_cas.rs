use bytes::Bytes;
use musicfs_cas::{CasConfig, CasError, CasStore, DedupStats};
use musicfs_core::ChunkHash;
use std::io::{self, ErrorKind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

pub struct FaultyCasStore {
    inner: Arc<CasStore>,
    inject_enospc: AtomicBool,
    inject_eio_on_read: AtomicBool,
    inject_eio_on_write: AtomicBool,
    inject_corruption: AtomicBool,
    fail_after_n_puts: AtomicUsize,
    put_count: AtomicUsize,
}

impl FaultyCasStore {
    pub fn new(inner: Arc<CasStore>) -> Self {
        Self {
            inner,
            inject_enospc: AtomicBool::new(false),
            inject_eio_on_read: AtomicBool::new(false),
            inject_eio_on_write: AtomicBool::new(false),
            inject_corruption: AtomicBool::new(false),
            fail_after_n_puts: AtomicUsize::new(usize::MAX),
            put_count: AtomicUsize::new(0),
        }
    }

    pub async fn open(config: CasConfig) -> Result<Self, CasError> {
        let store = CasStore::open(config).await?;
        Ok(Self::new(Arc::new(store)))
    }

    pub fn set_inject_enospc(&self, enabled: bool) {
        self.inject_enospc.store(enabled, Ordering::SeqCst);
    }

    pub fn set_inject_eio_on_read(&self, enabled: bool) {
        self.inject_eio_on_read.store(enabled, Ordering::SeqCst);
    }

    pub fn set_inject_eio_on_write(&self, enabled: bool) {
        self.inject_eio_on_write.store(enabled, Ordering::SeqCst);
    }

    pub fn set_inject_corruption(&self, enabled: bool) {
        self.inject_corruption.store(enabled, Ordering::SeqCst);
    }

    pub fn set_fail_after_n_puts(&self, n: usize) {
        self.fail_after_n_puts.store(n, Ordering::SeqCst);
        self.put_count.store(0, Ordering::SeqCst);
    }

    pub fn reset_faults(&self) {
        self.inject_enospc.store(false, Ordering::SeqCst);
        self.inject_eio_on_read.store(false, Ordering::SeqCst);
        self.inject_eio_on_write.store(false, Ordering::SeqCst);
        self.inject_corruption.store(false, Ordering::SeqCst);
        self.fail_after_n_puts.store(usize::MAX, Ordering::SeqCst);
        self.put_count.store(0, Ordering::SeqCst);
    }

    pub fn put_count(&self) -> usize {
        self.put_count.load(Ordering::SeqCst)
    }

    pub async fn put(&self, data: &[u8]) -> Result<ChunkHash, CasError> {
        let count = self.put_count.fetch_add(1, Ordering::SeqCst);

        if self.inject_enospc.load(Ordering::SeqCst) {
            return Err(CasError::Io(io::Error::new(
                ErrorKind::Other,
                "No space left on device (ENOSPC injected)",
            )));
        }

        if self.inject_eio_on_write.load(Ordering::SeqCst) {
            return Err(CasError::Io(io::Error::new(
                ErrorKind::Other,
                "Input/output error (EIO injected)",
            )));
        }

        let threshold = self.fail_after_n_puts.load(Ordering::SeqCst);
        if count >= threshold {
            return Err(CasError::Io(io::Error::new(
                ErrorKind::Other,
                "Injected failure after N puts",
            )));
        }

        self.inner.put(data).await
    }

    pub async fn get(&self, hash: &ChunkHash) -> Result<Bytes, CasError> {
        if self.inject_eio_on_read.load(Ordering::SeqCst) {
            return Err(CasError::Io(io::Error::new(
                ErrorKind::Other,
                "Input/output error (EIO injected)",
            )));
        }

        let data = self.inner.get(hash).await?;

        if self.inject_corruption.load(Ordering::SeqCst) {
            let mut corrupted = data.to_vec();
            if !corrupted.is_empty() {
                corrupted[0] = corrupted[0].wrapping_add(1);
            }
            return Err(CasError::IntegrityError {
                expected: hash.as_hex(),
                actual: ChunkHash::from_bytes(&corrupted).as_hex(),
            });
        }

        Ok(data)
    }

    pub fn exists(&self, hash: &ChunkHash) -> bool {
        self.inner.exists(hash)
    }

    pub async fn delete(&self, hash: &ChunkHash) -> Result<(), CasError> {
        if self.inject_eio_on_write.load(Ordering::SeqCst) {
            return Err(CasError::Io(io::Error::new(
                ErrorKind::Other,
                "Input/output error (EIO injected)",
            )));
        }
        self.inner.delete(hash).await
    }

    pub fn current_size(&self) -> u64 {
        self.inner.current_size()
    }

    pub fn max_size(&self) -> u64 {
        self.inner.max_size()
    }

    pub fn list_chunks(&self) -> impl Iterator<Item = ChunkHash> + '_ {
        self.inner.list_chunks()
    }

    pub fn dedup_stats(&self) -> DedupStats {
        self.inner.dedup_stats()
    }

    pub fn inner(&self) -> &Arc<CasStore> {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_store() -> (FaultyCasStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            max_size: 1024 * 1024,
            shard_levels: 2,
        };
        let store = FaultyCasStore::open(config).await.unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn test_healthy_passthrough() {
        let (store, _dir) = test_store().await;

        let data = b"test data";
        let hash = store.put(data).await.unwrap();
        let retrieved = store.get(&hash).await.unwrap();
        assert_eq!(&retrieved[..], data);
    }

    #[tokio::test]
    async fn test_inject_enospc() {
        let (store, _dir) = test_store().await;

        store.set_inject_enospc(true);
        let result = store.put(b"test").await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, CasError::Io(_)));

        store.set_inject_enospc(false);
        assert!(store.put(b"test").await.is_ok());
    }

    #[tokio::test]
    async fn test_inject_eio_on_read() {
        let (store, _dir) = test_store().await;

        let hash = store.put(b"test").await.unwrap();

        store.set_inject_eio_on_read(true);
        let result = store.get(&hash).await;
        assert!(result.is_err());

        store.set_inject_eio_on_read(false);
        assert!(store.get(&hash).await.is_ok());
    }

    #[tokio::test]
    async fn test_inject_corruption() {
        let (store, _dir) = test_store().await;

        let hash = store.put(b"test data").await.unwrap();

        store.set_inject_corruption(true);
        let result = store.get(&hash).await;
        assert!(matches!(result, Err(CasError::IntegrityError { .. })));
    }

    #[tokio::test]
    async fn test_fail_after_n_puts() {
        let (store, _dir) = test_store().await;

        store.set_fail_after_n_puts(2);

        assert!(store.put(b"data1").await.is_ok());
        assert!(store.put(b"data2").await.is_ok());
        assert!(store.put(b"data3").await.is_err());
        assert!(store.put(b"data4").await.is_err());
        assert_eq!(store.put_count(), 4);
    }

    #[tokio::test]
    async fn test_reset_faults() {
        let (store, _dir) = test_store().await;

        store.set_inject_enospc(true);
        store.set_inject_eio_on_read(true);
        store.set_fail_after_n_puts(1);

        store.reset_faults();

        assert!(store.put(b"test").await.is_ok());
        let hash = store.put(b"test2").await.unwrap();
        assert!(store.get(&hash).await.is_ok());
    }
}
