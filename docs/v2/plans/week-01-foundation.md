# Week 1: Foundation

**Phase**: 1 (MVP)  
**Prerequisites**: None  
**Estimated effort**: 5 days

---

## Objective

Set up Rust workspace, define core types, create FUSE skeleton, implement local origin plugin.

---

## Deliverables

| Task | Crate | Files | Done |
|------|-------|-------|------|
| Workspace setup | root | `Cargo.toml`, `.cargo/config.toml` | [ ] |
| Core types | musicfs-core | `lib.rs`, `error.rs`, `types.rs` | [ ] |
| Event Bus | musicfs-core | `events.rs` | [ ] |
| FUSE skeleton | musicfs-fuse | `lib.rs`, `filesystem.rs` | [ ] |
| Local origin | musicfs-origins | `lib.rs`, `local.rs`, `traits.rs` | [ ] |
| Nix flake | root | `flake.nix` | [ ] |

---

## Task 1: Workspace Setup

### 1.1 Create directory structure

```bash
mkdir -p musicfs
cd musicfs
mkdir -p crates/{musicfs-core,musicfs-fuse,musicfs-cache,musicfs-cas,musicfs-sync,musicfs-origins,musicfs-metadata,musicfs-search,musicfs-plugins,musicfs-grpc,musicfs-cli}
mkdir -p proto tests/{integration,e2e} benches
```

### 1.2 Create root `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
rust-version = "1.75"
authors = ["MusicFS Contributors"]
repository = "https://github.com/user/musicfs"

[workspace.dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"

# Error handling
thiserror = "1"
anyhow = "1"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rmp-serde = "1"  # msgpack

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# FUSE
fuser = "0.14"

# Database
rusqlite = { version = "0.31", features = ["bundled"] }
sled = "0.34"

# Hashing (per architecture 8.3)
xxhash-rust = { version = "0.8", features = ["xxh64"] }

# Testing
tempfile = "3"
```

### 1.3 Create `.cargo/config.toml`

```toml
[build]
rustflags = ["-C", "link-arg=-fuse-ld=lld"]

[target.x86_64-unknown-linux-gnu]
linker = "clang"

[alias]
t = "test"
c = "check"
b = "build"
```

---

## Task 2: Core Types (`musicfs-core`)

### 2.1 Initialize crate

```bash
cd crates/musicfs-core
cargo init --lib
```

### 2.2 Create `Cargo.toml`

```toml
[package]
name = "musicfs-core"
version.workspace = true
edition.workspace = true

[dependencies]
thiserror.workspace = true
serde.workspace = true
tokio = { workspace = true, features = ["sync"] }
xxhash-rust.workspace = true
```

### 2.3 Create `src/lib.rs`

```rust
pub mod error;
pub mod types;
pub mod events;

pub use error::{Error, Result};
pub use types::*;
pub use events::{Event, EventBus};
```

### 2.4 Create `src/error.rs`

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Origin not found: {0}")]
    OriginNotFound(String),
    
    #[error("File not found: {0}")]
    FileNotFound(String),
    
    #[error("Path resolution failed: {0}")]
    PathResolution(String),
    
    #[error("Cache error: {0}")]
    Cache(String),
    
    #[error("Database error: {0}")]
    Database(String),
    
    #[error("NFS stale file handle")]
    NfsStaleHandle,
    
    #[error("Operation not permitted (read-only filesystem)")]
    ReadOnly,
}

pub type Result<T> = std::result::Result<T, Error>;
```

### 2.5 Create `src/types.rs`

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

/// Unique identifier for an origin
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OriginId(pub String);

impl From<&str> for OriginId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Unique identifier for a file in the database
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId(pub i64);

/// Virtual path in metadata-organized tree
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VirtualPath(pub PathBuf);

impl VirtualPath {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
    
    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }
    
    pub fn as_str(&self) -> &str {
        self.0.to_str().unwrap_or("")
    }
}

/// Real path on origin storage
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealPath {
    pub origin_id: OriginId,
    pub path: PathBuf,
}

/// Content-addressable hash (xxHash64 per architecture 8.3)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 8]);

impl ContentHash {
    pub fn from_bytes(data: &[u8]) -> Self {
        use xxhash_rust::xxh64::xxh64;
        Self(xxh64(data, 0).to_le_bytes())
    }
}

/// Chunk-level hash (xxHash64)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkHash(pub [u8; 8]);

impl ChunkHash {
    pub fn from_bytes(data: &[u8]) -> Self {
        use xxhash_rust::xxh64::xxh64;
        Self(xxh64(data, 0).to_le_bytes())
    }
}

/// Audio format enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    Flac,
    Mp3,
    Opus,
    Vorbis,
    Aac,
    Alac,
    Wav,
    Unknown,
}

impl AudioFormat {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "flac" => Self::Flac,
            "mp3" => Self::Mp3,
            "opus" => Self::Opus,
            "ogg" => Self::Vorbis,
            "m4a" | "aac" => Self::Aac,
            "wav" => Self::Wav,
            _ => Self::Unknown,
        }
    }
}

/// Audio metadata extracted from files
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,        // "track" per architecture 4.3.6
    pub disc: Option<u32>,         // "disc" per architecture 4.3.6
    pub duration_ms: Option<u64>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub format: AudioFormat,
}

/// Complete file metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    pub id: FileId,
    pub virtual_path: VirtualPath,
    pub real_path: RealPath,
    pub size: u64,
    pub mtime: SystemTime,
    pub content_hash: Option<ContentHash>,
    pub audio: Option<AudioMeta>,
}

/// Directory entry for readdir
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: SystemTime,
}

/// File stat information
#[derive(Debug, Clone)]
pub struct FileStat {
    pub size: u64,
    pub mtime: SystemTime,
    pub is_dir: bool,
}

/// Origin health status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}
```

### 2.6 Create `src/events.rs`

```rust
use crate::types::{OriginId, VirtualPath};
use tokio::sync::broadcast;

/// Central event bus for system-wide notifications (per architecture 4.2)
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }
    
    pub fn publish(&self, event: Event) {
        // Ignore error if no receivers
        let _ = self.sender.send(event);
    }
    
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

/// System events
#[derive(Clone, Debug)]
pub enum Event {
    FileAdded {
        path: VirtualPath,
        origin_id: OriginId,
    },
    FileRemoved {
        path: VirtualPath,
    },
    FileModified {
        path: VirtualPath,
    },
    FileAccessed {
        path: VirtualPath,
        origin_id: OriginId,
        offset: u64,
        size: u32,
    },
    OriginConnected {
        origin_id: OriginId,
    },
    OriginDisconnected {
        origin_id: OriginId,
    },
    SyncStarted {
        origin_id: OriginId,
    },
    SyncCompleted {
        origin_id: OriginId,
        files_changed: u64,
    },
    CacheEviction {
        bytes_freed: u64,
    },
}
```

---

## Task 3: FUSE Skeleton (`musicfs-fuse`)

### 3.1 Create `Cargo.toml`

```toml
[package]
name = "musicfs-fuse"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
fuser.workspace = true
tokio.workspace = true
tracing.workspace = true
```

### 3.2 Create `src/lib.rs`

```rust
mod filesystem;

pub use filesystem::MusicFs;
```

### 3.3 Create `src/filesystem.rs`

```rust
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyEntry, ReplyOpen, Request, FUSE_ROOT_ID,
};
use musicfs_core::{Error, Result};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info};

const TTL: Duration = Duration::from_secs(1);

/// Main FUSE filesystem implementation
pub struct MusicFs {
    // Will be populated in later weeks:
    // origins: Arc<OriginRegistry>,
    // cache: Arc<CacheManager>,
    // tree: Arc<RwLock<VirtualTree>>,
}

impl MusicFs {
    pub fn new() -> Self {
        Self {}
    }
    
    /// Mount the filesystem
    pub fn mount(self, mountpoint: &std::path::Path) -> Result<()> {
        info!("Mounting MusicFS at {:?}", mountpoint);
        
        let options = vec![
            fuser::MountOption::RO,           // Read-only
            fuser::MountOption::FSName("musicfs".to_string()),
            fuser::MountOption::AutoUnmount,
            fuser::MountOption::AllowOther,
        ];
        
        fuser::mount2(self, mountpoint, &options)
            .map_err(|e| Error::Io(e))?;
        
        Ok(())
    }
    
    fn root_attr(&self) -> FileAttr {
        FileAttr {
            ino: FUSE_ROOT_ID,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

impl Default for MusicFs {
    fn default() -> Self {
        Self::new()
    }
}

impl Filesystem for MusicFs {
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut fuser::KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        info!("MusicFS initialized");
        Ok(())
    }
    
    fn destroy(&mut self) {
        info!("MusicFS destroyed");
    }
    
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup(parent={}, name={:?})", parent, name);
        
        // TODO: Implement in Week 3
        reply.error(libc::ENOENT);
    }
    
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        debug!("getattr(ino={})", ino);
        
        if ino == FUSE_ROOT_ID {
            reply.attr(&TTL, &self.root_attr());
        } else {
            // TODO: Implement in Week 3
            reply.error(libc::ENOENT);
        }
    }
    
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir(ino={}, offset={})", ino, offset);
        
        if ino == FUSE_ROOT_ID {
            // Root directory with . and ..
            if offset == 0 {
                let _ = reply.add(FUSE_ROOT_ID, 1, FileType::Directory, ".");
            }
            if offset <= 1 {
                let _ = reply.add(FUSE_ROOT_ID, 2, FileType::Directory, "..");
            }
            // TODO: Add actual entries in Week 3
            reply.ok();
        } else {
            // TODO: Implement in Week 3
            reply.error(libc::ENOENT);
        }
    }
    
    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        debug!("open(ino={}, flags={})", ino, flags);
        
        // Check for write flags - we're read-only (FR-4.1)
        let write_flags = libc::O_WRONLY | libc::O_RDWR | libc::O_APPEND | libc::O_TRUNC;
        if flags & write_flags != 0 {
            reply.error(libc::EROFS);
            return;
        }
        
        // TODO: Implement in Week 3
        reply.error(libc::ENOENT);
    }
    
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        debug!("read(ino={}, offset={}, size={})", ino, offset, size);
        
        // TODO: Implement in Week 3
        reply.error(libc::ENOENT);
    }
    
    fn release(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        debug!("release(ino={})", ino);
        reply.ok();
    }
    
    // Write operations - always return EROFS (FR-4.1-4.4)
    
    fn write(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        reply.error(libc::EROFS);
    }
    
    fn mkdir(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }
    
    fn unlink(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }
    
    fn rmdir(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }
    
    fn rename(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _newparent: u64,
        _newname: &OsStr,
        _flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        reply.error(libc::EROFS);
    }
    
    fn create(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        reply.error(libc::EROFS);
    }
    
    // Additional EROFS handlers (FR-4.5)
    
    fn setattr(
        &mut self,
        _req: &Request,
        _ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        reply.error(libc::EROFS);
    }
    
    fn symlink(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _link: &std::path::Path,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }
    
    fn link(
        &mut self,
        _req: &Request,
        _ino: u64,
        _newparent: u64,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }
    
    fn mknod(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }
}
```

---

## Task 4: Local Origin (`musicfs-origins`)

### 4.1 Create `Cargo.toml`

```toml
[package]
name = "musicfs-origins"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
async-trait.workspace = true
tokio = { workspace = true, features = ["fs"] }
tracing.workspace = true
```

### 4.2 Create `src/lib.rs`

```rust
mod traits;
mod local;

pub use traits::Origin;
pub use local::LocalOrigin;
```

### 4.3 Create `src/traits.rs`

```rust
use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, Result};
use std::path::Path;
use tokio::io::AsyncRead;

/// Origin type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginType {
    Local,
    Nfs,
    Smb,
    S3,
    Sftp,
}

/// Origin plugin interface (per architecture 4.3.4)
#[async_trait]
pub trait Origin: Send + Sync {
    /// Unique identifier for this origin
    fn id(&self) -> &OriginId;
    
    /// Origin type
    fn origin_type(&self) -> OriginType;
    
    /// Human-readable display name
    fn display_name(&self) -> &str;
    
    /// List entries in directory
    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>>;
    
    /// Get file/directory metadata
    async fn stat(&self, path: &Path) -> Result<FileStat>;
    
    /// Read file content at offset
    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>>;
    
    /// Check if path exists
    async fn exists(&self, path: &Path) -> Result<bool>;
    
    /// Health check
    async fn health(&self) -> HealthStatus;
    
    /// Get a reader for streaming large files
    async fn open_read(&self, path: &Path) -> Result<Box<dyn AsyncRead + Send + Unpin>>;
    
    /// Watch path for changes (per architecture 4.3.4)
    /// Returns handle that cancels watch on drop
    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle>;
}

/// Callback for file change notifications
pub type WatchCallback = Box<dyn Fn(WatchEvent) + Send + Sync>;

/// Handle to cancel a watch - cancels on drop
pub struct WatchHandle {
    _cancel: tokio::sync::oneshot::Sender<()>,
}

/// Watch event types
#[derive(Debug, Clone)]
pub enum WatchEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}
```

### 4.4 Create `src/local.rs`

```rust
use crate::traits::{Origin, OriginType};
use async_trait::async_trait;
use musicfs_core::{DirEntry, Error, FileStat, HealthStatus, OriginId, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncRead;
use tracing::debug;

/// Local filesystem origin (FR-12.1)
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
        self.root.join(path)
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
        debug!("LocalOrigin::read({:?}, offset={}, size={})", full_path, offset, size);
        
        let mut file = fs::File::open(&full_path).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        
        let mut buffer = vec![0u8; size as usize];
        let bytes_read = file.read(&mut buffer).await?;
        buffer.truncate(bytes_read);
        
        Ok(buffer)
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
    
    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle> {
        // Stub implementation for Week 1
        // Full inotify/notify implementation deferred to Week 5 (FR-10.2)
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        
        // In Week 5: Use notify crate for real filesystem watching
        // For now, just return a handle that does nothing
        debug!("LocalOrigin::watch({:?}) - stub implementation", path);
        
        Ok(WatchHandle { _cancel: tx })
    }
}
```

---

## Task 5: Nix Flake

### 5.1 Create `flake.nix`

```nix
{
  description = "MusicFS - FUSE filesystem for music libraries";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain
            pkg-config
            fuse3
            sqlite
            openssl
            
            # Development tools
            cargo-watch
            cargo-nextest
            cargo-criterion
            
            # gRPC
            protobuf
            grpcurl
          ];
          
          RUST_BACKTRACE = 1;
          RUST_LOG = "debug";
        };
        
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "musicfs";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.fuse3 pkgs.sqlite pkgs.openssl ];
        };
      }
    );
}
```

---

## Tests

### Unit Tests

Create `crates/musicfs-core/src/lib.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_content_hash() {
        let data = b"hello world";
        let hash1 = ContentHash::from_bytes(data);
        let hash2 = ContentHash::from_bytes(data);
        assert_eq!(hash1, hash2);
        
        let hash3 = ContentHash::from_bytes(b"different");
        assert_ne!(hash1, hash3);
    }
    
    #[test]
    fn test_audio_format_from_extension() {
        assert_eq!(AudioFormat::from_extension("flac"), AudioFormat::Flac);
        assert_eq!(AudioFormat::from_extension("MP3"), AudioFormat::Mp3);
        assert_eq!(AudioFormat::from_extension("unknown"), AudioFormat::Unknown);
    }
    
    #[tokio::test]
    async fn test_event_bus() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        
        bus.publish(Event::SyncStarted {
            origin_id: OriginId::from("test"),
        });
        
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, Event::SyncStarted { .. }));
    }
}
```

### Integration Tests

Create `tests/integration/basic_mount.rs`:

```rust
use musicfs_fuse::MusicFs;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn test_fuse_mount_unmount() {
    let mount_dir = TempDir::new().unwrap();
    let mount_path = mount_dir.path();
    
    // Fork and mount in child process
    let fs = MusicFs::new();
    
    // For now, just verify construction works
    // Full mount test requires FUSE permissions
    drop(fs);
}

#[test]
fn test_read_only_enforcement() {
    // Verify write operations return EROFS
    // This will be expanded in Week 3
}
```

---

## Exit Criteria

- [ ] `cargo build` succeeds for all crates
- [ ] `cargo test` passes
- [ ] `nix develop` enters shell with all dependencies
- [ ] FUSE skeleton compiles with all required trait methods
- [ ] LocalOrigin can list files in a test directory
- [ ] Write operations return EROFS
- [ ] EventBus publishes and receives events

---

## Verification Commands

```bash
# Build all crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Check for warnings
cargo clippy --workspace -- -D warnings

# Enter nix shell
nix develop

# Test local origin (manual)
cargo run --example local_origin_test
```

---

## Next Week

Week 2 will implement metadata extraction using symphonia and create the SQLite schema.
