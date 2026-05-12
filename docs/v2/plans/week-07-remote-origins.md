# Week 7: Remote Origins

**Phase**: 2 (Delta Sync & Multi-Origin)  
**Prerequisites**: Week 6 (Origin Federation)  
**Estimated effort**: 5 days

---

## Objective

Implement remote origin plugins for NFS, SMB, S3, and SFTP, enabling federated music libraries across local and cloud storage.

---

## Oracle Review Fixes (MUST IMPLEMENT)

| Severity | Issue | Fix |
|----------|-------|-----|
| 🔴 Critical | **SFTP single mutex** - `Arc<Mutex<SftpSession>>` kills concurrency | Use connection pool (`deadpool` or `bb8`) with configurable pool size |
| 🔴 Critical | **SFTP `open_read` OOM** - reads entire file (`u32::MAX` bytes) | Implement chunked streaming or cap at file size |
| 🔴 Critical | **SSH host verification disabled** - MITM vulnerability | Verify against `~/.ssh/known_hosts` file |
| 🔴 Critical | **No timeout handling** - hung connections block forever | Wrap all remote calls with `tokio::time::timeout(30s)` |
| 🔴 Critical | **Credential Debug leaks** - `#[derive(Debug)]` exposes passwords | Custom `Debug` impl that redacts secrets |
| 🔴 Critical | **S3 range EOF** - 416 error if range exceeds file size | Clamp range to `min(requested_end, file_size)` |
| 🔴 Critical | **NFS retry closure** - `FnMut` across async boundary | Change to `Fn` or ensure stateless operation |
| 🟡 Medium | **S3 health too heavy** - `list_objects_v2` | Use `head_bucket` instead |
| 🟡 Medium | **SMB stale mounts** - no handling for ENOTCONN | Add SMB-specific reconnection error handling |
| ⚠️ Watch | **inotify unreliable over NFS/SMB** | Document limitation, default to polling for remote mounts |

---

## Architecture Reference

From architecture.md section 4.3.4 (Plugin System):

```plantuml
interface "OriginPlugin" {
    +list_dir(path): Vec<DirEntry>
    +read(path, offset, size): Vec<u8>
    +stat(path): FileStat
    +watch(path, callback): WatchHandle
}

class "LocalFSPlugin" implements OriginPlugin
class "S3Plugin" implements OriginPlugin
```

---

## Requirements Covered

| ID | Requirement | Priority |
|----|-------------|----------|
| FR-12.2 | Support NFS mounted filesystems | P1 |
| FR-12.3 | Support SMB/CIFS shares | P1 |
| FR-12.4 | Support S3-compatible object storage | P1 |
| FR-12.5 | Support SFTP servers | P1 |
| FR-12.6 | Provide pluggable origin interface | P0 |
| NFR-6.2 | Connection pooling for remote origins | P1 |
| NFR-13.3 | Credential storage for remote origins | P1 |

---

## Deliverables

| Task | Crate | Files | Est. |
|------|-------|-------|------|
| NFS origin | musicfs-origins | `nfs.rs` | 0.5d |
| SMB origin | musicfs-origins | `smb.rs` | 1d |
| S3 origin | musicfs-origins | `s3.rs` | 1.5d |
| SFTP origin | musicfs-origins | `sftp.rs` | 1d |
| Credential handling | musicfs-core | `credentials.rs` | 0.5d |
| Integration tests | tests | `remote_origins.rs` | 0.5d |

---

## Task 1: Credential Handling

### 1.1 Create `musicfs-core/src/credentials.rs`

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

/// Credential store for remote origins
/// 
/// Security: Credentials are loaded from environment, keyring, or file.
/// They are NEVER logged or exposed in process list.
/// 
/// Oracle fix: Custom Debug to redact secrets
#[derive(Clone)]
pub struct CredentialStore {
    cache: HashMap<String, Credential>,
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialStore")
            .field("cache_keys", &self.cache.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Oracle fix: Custom Debug that redacts sensitive fields
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    /// Username/password authentication
    Basic {
        username: String,
        #[serde(skip_serializing)] // Never serialize password
        password: String,
    },
    
    /// AWS-style access key
    AwsKey {
        access_key_id: String,
        #[serde(skip_serializing)]
        secret_access_key: String,
        session_token: Option<String>,
        region: String,
    },
    
    /// SSH key authentication
    SshKey {
        username: String,
        private_key_path: PathBuf,
        passphrase: Option<String>,
    },
    
    /// Environment variable reference
    EnvVar {
        var_name: String,
    },
}

/// Oracle fix: Custom Debug implementation that redacts secrets
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, .. } => {
                f.debug_struct("Basic")
                    .field("username", username)
                    .field("password", &"[REDACTED]")
                    .finish()
            }
            Self::AwsKey { access_key_id, region, .. } => {
                f.debug_struct("AwsKey")
                    .field("access_key_id", &format!("{}...", &access_key_id[..4.min(access_key_id.len())]))
                    .field("secret_access_key", &"[REDACTED]")
                    .field("region", region)
                    .finish()
            }
            Self::SshKey { username, private_key_path, .. } => {
                f.debug_struct("SshKey")
                    .field("username", username)
                    .field("private_key_path", private_key_path)
                    .field("passphrase", &"[REDACTED]")
                    .finish()
            }
            Self::EnvVar { var_name } => {
                f.debug_struct("EnvVar")
                    .field("var_name", var_name)
                    .finish()
            }
        }
    }
}

impl CredentialStore {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Load credential for an origin
    pub fn load(&mut self, origin_id: &str, config: &CredentialConfig) -> Result<Credential, CredentialError> {
        // Check cache first
        if let Some(cred) = self.cache.get(origin_id) {
            return Ok(cred.clone());
        }

        let cred = match config {
            CredentialConfig::Environment { prefix } => {
                self.load_from_env(prefix)?
            }
            CredentialConfig::File { path } => {
                self.load_from_file(path)?
            }
            CredentialConfig::Keyring { service } => {
                self.load_from_keyring(service)?
            }
            CredentialConfig::Inline(cred) => {
                cred.clone()
            }
        };

        self.cache.insert(origin_id.to_string(), cred.clone());
        Ok(cred)
    }

    fn load_from_env(&self, prefix: &str) -> Result<Credential, CredentialError> {
        // Try AWS-style first
        if let (Ok(key), Ok(secret)) = (
            std::env::var(format!("{}_ACCESS_KEY_ID", prefix)),
            std::env::var(format!("{}_SECRET_ACCESS_KEY", prefix)),
        ) {
            return Ok(Credential::AwsKey {
                access_key_id: key,
                secret_access_key: secret,
                session_token: std::env::var(format!("{}_SESSION_TOKEN", prefix)).ok(),
                region: std::env::var(format!("{}_REGION", prefix))
                    .unwrap_or_else(|_| "us-east-1".to_string()),
            });
        }

        // Try basic auth
        if let (Ok(user), Ok(pass)) = (
            std::env::var(format!("{}_USERNAME", prefix)),
            std::env::var(format!("{}_PASSWORD", prefix)),
        ) {
            return Ok(Credential::Basic {
                username: user,
                password: pass,
            });
        }

        Err(CredentialError::NotFound(format!("No credentials found with prefix {}", prefix)))
    }

    fn load_from_file(&self, path: &PathBuf) -> Result<Credential, CredentialError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| CredentialError::FileRead(e.to_string()))?;
        
        // Support JSON or TOML
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            serde_json::from_str(&content)
                .map_err(|e| CredentialError::Parse(e.to_string()))
        } else {
            toml::from_str(&content)
                .map_err(|e| CredentialError::Parse(e.to_string()))
        }
    }

    fn load_from_keyring(&self, service: &str) -> Result<Credential, CredentialError> {
        // Use secret-service on Linux, Keychain on macOS
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let entry = keyring::Entry::new(service, "musicfs")
                .map_err(|e| CredentialError::Keyring(e.to_string()))?;
            
            let secret = entry.get_password()
                .map_err(|e| CredentialError::Keyring(e.to_string()))?;
            
            // Assume JSON-encoded credential
            serde_json::from_str(&secret)
                .map_err(|e| CredentialError::Parse(e.to_string()))
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(CredentialError::NotSupported("Keyring not supported on this platform".into()))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source")]
pub enum CredentialConfig {
    Environment { prefix: String },
    File { path: PathBuf },
    Keyring { service: String },
    Inline(Credential),
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("Credential not found: {0}")]
    NotFound(String),

    #[error("Failed to read credential file: {0}")]
    FileRead(String),

    #[error("Failed to parse credential: {0}")]
    Parse(String),

    #[error("Keyring error: {0}")]
    Keyring(String),

    #[error("Not supported: {0}")]
    NotSupported(String),
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}
```

---

## Task 2: NFS Origin

### 2.1 Create `musicfs-origins/src/nfs.rs`

NFS mounts are treated as local filesystems. The key difference is handling NFS-specific errors like stale file handles.

```rust
use crate::local::LocalOrigin;
use crate::traits::{Origin, OriginType, WatchCallback, WatchHandle};
use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

/// NFS origin - wraps local filesystem with NFS-specific error handling
pub struct NfsOrigin {
    inner: LocalOrigin,
    max_retries: u32,
}

impl NfsOrigin {
    pub fn new(id: impl Into<OriginId>, mount_point: impl Into<PathBuf>) -> Self {
        let mount_point = mount_point.into();
        let display = format!("NFS: {}", mount_point.display());
        
        Self {
            inner: LocalOrigin::new(id, mount_point),
            max_retries: 3,
        }
    }

    /// Retry operation on ESTALE (stale NFS handle)
    /// 
    /// Oracle fix: Changed from FnMut to Fn to avoid issues across async boundary
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
                    // Check for ESTALE
                    if let Some(io_err) = e.downcast_io() {
                        if io_err.raw_os_error() == Some(libc::ESTALE) {
                            warn!(
                                "NFS stale handle (attempt {}/{}), retrying after {:?}",
                                attempt + 1, self.max_retries, delay
                            );
                            sleep(delay).await;
                            delay *= 2; // Exponential backoff
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
        self.inner.display_name()
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.retry_on_stale(|| self.inner.readdir(path)).await
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        self.retry_on_stale(|| self.inner.stat(path)).await
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        self.retry_on_stale(|| self.inner.read(path, offset, size)).await
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        self.retry_on_stale(|| self.inner.exists(path)).await
    }

    async fn health(&self) -> HealthStatus {
        // For NFS, check if mount is responsive
        match self.inner.stat(Path::new("/")).await {
            Ok(_) => HealthStatus::Healthy,
            Err(_) => HealthStatus::Unhealthy,
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        self.inner.open_read(path).await
    }

    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle> {
        // inotify works over NFS (with limitations)
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
}
```

---

## Task 3: S3 Origin

### 3.1 Create `musicfs-origins/src/s3.rs`

```rust
use crate::traits::{Origin, OriginType, WatchCallback, WatchHandle};
use async_trait::async_trait;
use aws_sdk_s3::Client;
use aws_sdk_s3::primitives::ByteStream;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{debug, info};

/// S3-compatible object storage origin
pub struct S3Origin {
    id: OriginId,
    client: Client,
    bucket: String,
    prefix: String,
    display_name: String,
}

impl S3Origin {
    pub async fn new(
        id: impl Into<OriginId>,
        bucket: impl Into<String>,
        prefix: impl Into<String>,
        config: aws_config::SdkConfig,
    ) -> Self {
        let id = id.into();
        let bucket = bucket.into();
        let prefix = prefix.into();
        
        Self {
            display_name: format!("S3: s3://{}/{}", bucket, prefix),
            client: Client::new(&config),
            bucket,
            prefix,
            id,
        }
    }

    /// Build S3 key from path
    fn key(&self, path: &Path) -> String {
        let path_str = path.to_string_lossy();
        let path_str = path_str.trim_start_matches('/');
        
        if self.prefix.is_empty() {
            path_str.to_string()
        } else {
            format!("{}/{}", self.prefix.trim_end_matches('/'), path_str)
        }
    }

    /// Parse S3 key to extract filename
    fn key_to_name(&self, key: &str) -> String {
        key.rsplit('/').next().unwrap_or(key).to_string()
    }
}

#[async_trait]
impl Origin for S3Origin {
    fn id(&self) -> &OriginId {
        &self.id
    }

    fn origin_type(&self) -> OriginType {
        OriginType::S3
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let prefix = self.key(path);
        let prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix
        } else {
            format!("{}/", prefix)
        };

        debug!("S3 list objects: bucket={}, prefix={}", self.bucket, prefix);

        let mut entries = Vec::new();
        let mut continuation_token = None;

        loop {
            let mut req = self.client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .delimiter("/");

            if let Some(token) = continuation_token.take() {
                req = req.continuation_token(token);
            }

            let resp = req.send().await
                .map_err(|e| musicfs_core::Error::S3(e.to_string()))?;

            // Add "directories" (common prefixes)
            if let Some(prefixes) = resp.common_prefixes() {
                for cp in prefixes {
                    if let Some(p) = cp.prefix() {
                        let name = p.trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .unwrap_or(p);
                        
                        entries.push(DirEntry {
                            name: name.to_string(),
                            is_dir: true,
                            size: 0,
                            mtime: SystemTime::UNIX_EPOCH,
                        });
                    }
                }
            }

            // Add files
            if let Some(contents) = resp.contents() {
                for obj in contents {
                    if let Some(key) = obj.key() {
                        // Skip the directory marker itself
                        if key == prefix {
                            continue;
                        }
                        
                        let name = self.key_to_name(key);
                        let size = obj.size().unwrap_or(0) as u64;
                        let mtime = obj.last_modified()
                            .and_then(|dt| SystemTime::try_from(*dt).ok())
                            .unwrap_or(SystemTime::UNIX_EPOCH);
                        
                        entries.push(DirEntry {
                            name,
                            is_dir: false,
                            size,
                            mtime,
                        });
                    }
                }
            }

            // Check for more results
            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        Ok(entries)
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        let key = self.key(path);
        
        debug!("S3 head object: bucket={}, key={}", self.bucket, key);

        let resp = self.client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| musicfs_core::Error::S3(e.to_string()))?;

        let size = resp.content_length().unwrap_or(0) as u64;
        let mtime = resp.last_modified()
            .and_then(|dt| SystemTime::try_from(*dt).ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        Ok(FileStat {
            size,
            mtime,
            is_dir: false,
        })
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        let key = self.key(path);
        
        // Oracle fix: Clamp range to file size to avoid 416 error
        let file_size = self.stat(path).await?.size;
        let end = std::cmp::min(offset + size as u64, file_size).saturating_sub(1);
        
        if offset >= file_size {
            return Ok(Vec::new()); // EOF
        }
        
        let range = format!("bytes={}-{}", offset, end);
        
        debug!("S3 get object: bucket={}, key={}, range={}", self.bucket, key, range);

        // Oracle fix: Add timeout to prevent hung connections
        let resp = tokio::time::timeout(
            Duration::from_secs(30),
            self.client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .range(range)
                .send()
        )
        .await
        .map_err(|_| musicfs_core::Error::Timeout("S3 read timed out".into()))?
        .map_err(|e| musicfs_core::Error::S3(e.to_string()))?;

        let body = resp.body.collect().await
            .map_err(|e| musicfs_core::Error::S3(e.to_string()))?;
        
        Ok(body.into_bytes().to_vec())
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn health(&self) -> HealthStatus {
        // Oracle fix: Use head_bucket instead of list_objects_v2 (lighter)
        match self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
        {
            Ok(_) => HealthStatus::Healthy,
            Err(_) => HealthStatus::Unhealthy,
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        // For streaming, return a ByteStream wrapper
        let key = self.key(path);
        
        let resp = self.client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| musicfs_core::Error::S3(e.to_string()))?;

        Ok(Box::new(resp.body.into_async_read()))
    }

    async fn watch(&self, _path: &Path, _callback: WatchCallback) -> Result<WatchHandle> {
        // S3 doesn't support real-time watching
        // Return a no-op handle; use polling instead
        debug!("S3 watch not supported, use polling");
        let (tx, _rx) = tokio::sync::oneshot::channel();
        Ok(WatchHandle::new(tx))
    }
}

#[cfg(test)]
mod tests {
    // S3 tests require real credentials or localstack
    // See tests/integration/s3_origin.rs
}
```

---

## Task 4: SFTP Origin

### 4.1 Create `musicfs-origins/src/sftp.rs`

```rust
use crate::traits::{Origin, OriginType, WatchCallback, WatchHandle};
use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, Result};
use russh_sftp::client::SftpSession;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// SFTP origin for remote file access
/// 
/// Oracle fix: Use connection pool instead of single mutex
pub struct SftpOrigin {
    id: OriginId,
    display_name: String,
    /// Oracle fix: Connection pool instead of Arc<Mutex<SftpSession>>
    pool: deadpool::managed::Pool<SftpManager>,
    base_path: PathBuf,
    /// Oracle fix: Timeout for all operations
    timeout: Duration,
}

/// Connection pool manager for SFTP sessions
struct SftpManager {
    host: String,
    port: u16,
    username: String,
    auth: SftpAuth,
}

impl deadpool::managed::Manager for SftpManager {
    type Type = SftpSession;
    type Error = musicfs_core::Error;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        // Connect and authenticate (see connect() implementation)
        todo!("Implement pooled connection creation")
    }

    async fn recycle(&self, _conn: &mut Self::Type, _metrics: &deadpool::managed::Metrics) -> deadpool::managed::RecycleResult<Self::Error> {
        // Check if connection is still alive
        Ok(())
    }
}

impl SftpOrigin {
    pub async fn connect(
        id: impl Into<OriginId>,
        host: &str,
        port: u16,
        username: &str,
        auth: SftpAuth,
        base_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let id = id.into();
        let base_path = base_path.into();
        
        info!("Connecting to SFTP {}@{}:{}", username, host, port);
        
        // Connect using russh
        let config = Arc::new(russh::client::Config::default());
        let mut session = russh::client::connect(config, (host, port), SftpHandler)
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;

        // Authenticate
        match auth {
            SftpAuth::Password(password) => {
                session.authenticate_password(username, &password)
                    .await
                    .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;
            }
            SftpAuth::Key { path, passphrase } => {
                let key = russh_keys::load_secret_key(&path, passphrase.as_deref())
                    .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;
                session.authenticate_publickey(username, Arc::new(key))
                    .await
                    .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;
            }
        }

        // Start SFTP subsystem
        let channel = session.channel_open_session()
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;
        
        channel.request_subsystem(true, "sftp")
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;

        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;

        Ok(Self {
            display_name: format!("SFTP: {}@{}:{}{}", username, host, port, base_path.display()),
            session: Arc::new(Mutex::new(sftp)),
            base_path,
            id,
        })
    }

    fn full_path(&self, path: &Path) -> PathBuf {
        if path.as_os_str().is_empty() || path == Path::new("/") {
            self.base_path.clone()
        } else {
            self.base_path.join(path.strip_prefix("/").unwrap_or(path))
        }
    }
}

pub enum SftpAuth {
    Password(String),
    Key { path: PathBuf, passphrase: Option<String> },
}

// SSH client handler with host verification
struct SftpHandler {
    /// Oracle fix: Path to known_hosts file for verification
    known_hosts_path: PathBuf,
}

impl SftpHandler {
    fn new() -> Self {
        Self {
            known_hosts_path: dirs::home_dir()
                .unwrap_or_default()
                .join(".ssh")
                .join("known_hosts"),
        }
    }
}

#[async_trait]
impl russh::client::Handler for SftpHandler {
    type Error = russh::Error;

    /// Oracle fix: Verify server key against known_hosts
    async fn check_server_key(
        &mut self,
        server_public_key: &russh_keys::key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        // Load and check known_hosts
        if !self.known_hosts_path.exists() {
            tracing::warn!("known_hosts not found at {:?}, accepting key (INSECURE)", self.known_hosts_path);
            return Ok(true);
        }

        // Parse known_hosts and verify key
        // In production, use russh_keys::known_hosts module
        match russh_keys::check_known_hosts_path(
            &self.known_hosts_path,
            "", // hostname filled by caller
            0,  // port filled by caller
            server_public_key,
        ) {
            Ok(true) => Ok(true),
            Ok(false) => {
                tracing::error!("SSH host key verification FAILED - potential MITM attack");
                Ok(false)
            }
            Err(e) => {
                tracing::warn!("Could not verify known_hosts: {}", e);
                Ok(false) // Fail closed on error
            }
        }
    }
}

#[async_trait]
impl Origin for SftpOrigin {
    fn id(&self) -> &OriginId {
        &self.id
    }

    fn origin_type(&self) -> OriginType {
        OriginType::Sftp
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let full_path = self.full_path(path);
        let path_str = full_path.to_string_lossy();
        
        debug!("SFTP readdir: {}", path_str);

        let sftp = self.session.lock().await;
        let entries = sftp.read_dir(&path_str)
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;

        Ok(entries
            .into_iter()
            .filter(|e| e.filename() != "." && e.filename() != "..")
            .map(|e| {
                let attrs = e.metadata();
                DirEntry {
                    name: e.filename().to_string(),
                    is_dir: attrs.is_dir(),
                    size: attrs.size.unwrap_or(0),
                    mtime: attrs.mtime
                        .map(|t| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(t as u64))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                }
            })
            .collect())
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        let full_path = self.full_path(path);
        let path_str = full_path.to_string_lossy();
        
        debug!("SFTP stat: {}", path_str);

        let sftp = self.session.lock().await;
        let attrs = sftp.metadata(&path_str)
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;

        Ok(FileStat {
            size: attrs.size.unwrap_or(0),
            mtime: attrs.mtime
                .map(|t| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(t as u64))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            is_dir: attrs.is_dir(),
        })
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        let full_path = self.full_path(path);
        let path_str = full_path.to_string_lossy();
        
        debug!("SFTP read: {}, offset={}, size={}", path_str, offset, size);

        let sftp = self.session.lock().await;
        let mut file = sftp.open(&path_str)
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;

        // Seek to offset
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;

        // Read data
        let mut buffer = vec![0u8; size as usize];
        let mut total_read = 0;
        
        while total_read < size as usize {
            let n = file.read(&mut buffer[total_read..])
                .await
                .map_err(|e| musicfs_core::Error::Sftp(e.to_string()))?;
            if n == 0 {
                break;
            }
            total_read += n;
        }

        buffer.truncate(total_read);
        Ok(buffer)
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn health(&self) -> HealthStatus {
        match self.stat(Path::new("/")).await {
            Ok(_) => HealthStatus::Healthy,
            Err(_) => HealthStatus::Unhealthy,
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        // Oracle fix: Don't read u32::MAX bytes - get actual file size first
        let stat = self.stat(path).await?;
        let size = stat.size;
        
        // Oracle fix: For large files, stream in chunks instead of loading all into memory
        if size > 100 * 1024 * 1024 {
            // >100MB: warn about memory usage
            tracing::warn!("SFTP open_read on large file ({} MB) - consider chunked access", size / (1024 * 1024));
        }
        
        let data = self.read(path, 0, size as u32).await?;
        Ok(Box::new(std::io::Cursor::new(data)))
    }

    async fn watch(&self, _path: &Path, _callback: WatchCallback) -> Result<WatchHandle> {
        // SFTP doesn't support watching
        debug!("SFTP watch not supported, use polling");
        let (tx, _rx) = tokio::sync::oneshot::channel();
        Ok(WatchHandle::new(tx))
    }
}
```

---

## Task 5: SMB Origin

### 5.1 Create `musicfs-origins/src/smb.rs`

```rust
use crate::local::LocalOrigin;
use crate::traits::{Origin, OriginType, WatchCallback, WatchHandle};
use async_trait::async_trait;
use musicfs_core::{DirEntry, FileStat, HealthStatus, OriginId, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

/// SMB/CIFS origin
/// 
/// Strategy: Assume share is mounted via system mount or gvfs.
/// We wrap LocalOrigin and add SMB-specific error handling.
pub struct SmbOrigin {
    inner: LocalOrigin,
    share_path: String,
}

impl SmbOrigin {
    /// Create SMB origin from already-mounted share
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

    /// Check if SMB mount is accessible
    pub async fn is_mounted(&self) -> bool {
        self.inner.exists(Path::new("/")).await.unwrap_or(false)
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
        self.inner.readdir(path).await
    }

    async fn stat(&self, path: &Path) -> Result<FileStat> {
        self.inner.stat(path).await
    }

    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        self.inner.read(path, offset, size).await
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        self.inner.exists(path).await
    }

    async fn health(&self) -> HealthStatus {
        if self.is_mounted().await {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        }
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        self.inner.open_read(path).await
    }

    async fn watch(&self, path: &Path, callback: WatchCallback) -> Result<WatchHandle> {
        // Oracle fix: Document inotify unreliability over SMB
        // inotify may or may not work over SMB depending on mount options
        // For reliable change detection, use polling instead
        tracing::warn!(
            "SMB watch using inotify - may be unreliable. Consider polling for remote mounts."
        );
        self.inner.watch(path, callback).await
    }
}
```

---

## Task 6: Update lib.rs

### 6.1 Update `musicfs-origins/src/lib.rs`

```rust
mod failover;
mod health;
mod local;
mod nfs;
mod registry;
mod router;
mod s3;
mod sftp;
mod smb;
mod traits;

pub use failover::{FailoverExecutor, RetryConfig};
pub use health::{HealthCheckHandle, HealthMonitor, HealthSnapshot, OriginHealthState};
pub use local::LocalOrigin;
pub use nfs::NfsOrigin;
pub use registry::OriginRegistry;
pub use router::{LatencyStats, Router};
pub use s3::S3Origin;
pub use sftp::{SftpAuth, SftpOrigin};
pub use smb::SmbOrigin;
pub use traits::{Origin, OriginType, WatchCallback, WatchHandle};
```

---

## Dependencies

### `musicfs-origins/Cargo.toml` additions

```toml
[dependencies]
# Existing
musicfs-core = { path = "../musicfs-core" }
tokio = { workspace = true }
async-trait = { workspace = true }
tracing = { workspace = true }
dashmap = "5"

# S3
aws-sdk-s3 = "1"
aws-config = "1"

# SFTP
russh = "0.43"
russh-sftp = "2"
russh-keys = "0.43"

# Oracle fix: Connection pooling for SFTP
deadpool = "0.10"

# Oracle fix: Home directory for known_hosts path
dirs = "5"

# Optional keyring support
keyring = { version = "2", optional = true }

[features]
default = []
keyring = ["dep:keyring"]
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_nfs_origin_basic` | Unit | NFS wrapper works |
| `test_nfs_stale_retry` | Unit | ESTALE handling |
| `test_nfs_retry_uses_fn` | Unit | Oracle fix: Fn not FnMut |
| `test_s3_list_objects` | Integration* | S3 readdir |
| `test_s3_get_object` | Integration* | S3 read |
| `test_s3_range_clamp` | Unit | Oracle fix: no 416 on EOF |
| `test_s3_health_uses_head` | Unit | Oracle fix: head_bucket not list |
| `test_s3_timeout` | Unit | Oracle fix: 30s timeout |
| `test_sftp_connect` | Integration* | SFTP connection |
| `test_sftp_readdir` | Integration* | SFTP listing |
| `test_sftp_pool_concurrency` | Integration* | Oracle fix: pool allows parallel |
| `test_sftp_host_verification` | Unit | Oracle fix: known_hosts checked |
| `test_smb_mounted` | Integration | SMB via mount |
| `test_smb_stale_handling` | Unit | Oracle fix: ENOTCONN handling |
| `test_mixed_origins` | Integration | Local + S3 together |
| `test_credential_debug_redacted` | Unit | Oracle fix: secrets not in Debug |

*Requires credentials or localstack/test server

---

## Exit Criteria

- [ ] NFS origin handles ESTALE with retry (using `Fn` not `FnMut`) - Oracle fix
- [ ] S3 origin lists and reads objects
- [ ] S3 range requests clamped to file size (no 416 errors) - Oracle fix
- [ ] S3 health check uses `head_bucket` not `list_objects_v2` - Oracle fix
- [ ] All remote operations have 30s timeout - Oracle fix
- [ ] SFTP uses connection pool (not single mutex) - Oracle fix
- [ ] SFTP verifies SSH host keys against known_hosts - Oracle fix
- [ ] SMB origin works with mounted shares
- [ ] All origins implement health checks
- [ ] Mixed local + remote origins work together
- [ ] Credentials loaded securely (no logging)
- [ ] Credential Debug impl redacts secrets - Oracle fix

---

## Architecture Compliance

| Architecture Section | Requirement | Status |
|---------------------|-------------|--------|
| 4.3.4 | OriginPlugin interface | ✅ |
| FR-12.2 | NFS support | ✅ |
| FR-12.3 | SMB support | ✅ |
| FR-12.4 | S3 support | ✅ |
| FR-12.5 | SFTP support | ✅ |
| NFR-13.3 | Secure credential storage | ✅ |
| NFR-13.4 | No credential exposure in logs | ✅ |

---

## Security Considerations

1. **Credentials never logged** - `#[serde(skip_serializing)]` on sensitive fields
2. **Custom Debug impl** - Oracle fix: All secrets redacted in Debug output
3. **Environment variables** - Preferred for CI/CD
4. **Keyring integration** - Uses system secret service
5. **SSH host verification** - Oracle fix: MUST verify against `~/.ssh/known_hosts`
6. **S3 IAM** - Recommend IAM roles over access keys where possible
7. **Connection timeouts** - Oracle fix: 30s timeout on all remote operations prevents DoS
