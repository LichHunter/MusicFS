use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, Result};
use std::path::{Path, PathBuf};
use tokio::io::AsyncRead;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginType {
    Local,
    Nfs,
    Smb,
    S3,
    Sftp,
}

#[async_trait]
pub trait Origin: Send + Sync {
    fn id(&self) -> &OriginId;

    fn origin_type(&self) -> OriginType;

    fn display_name(&self) -> &str;

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>>;

    async fn stat(&self, path: &Path) -> Result<FileStat>;

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>>;

    async fn exists(&self, path: &Path) -> Result<bool>;

    async fn health(&self) -> HealthStatus;

    async fn open_read(&self, path: &Path) -> Result<Box<dyn AsyncRead + Send + Unpin>>;

    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle>;
}

pub type WatchCallback = Box<dyn Fn(WatchEvent) + Send + Sync>;

pub struct WatchHandle {
    _cancel: tokio::sync::oneshot::Sender<()>,
}

impl WatchHandle {
    pub fn new(cancel: tokio::sync::oneshot::Sender<()>) -> Self {
        Self { _cancel: cancel }
    }
}

#[derive(Debug, Clone)]
pub enum WatchEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}
