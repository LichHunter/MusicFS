#![allow(dead_code)]
//! SFTP origin - feature-gated to avoid russh/deadpool dependencies

#[cfg(feature = "sftp")]
mod implementation {
    // Full SFTP implementation with connection pooling
    // Oracle fixes to implement:
    // 1. Use deadpool connection pool, not Arc<Mutex<SftpSession>>
    // 2. Verify SSH host keys against ~/.ssh/known_hosts
    // 3. Wrap all operations with tokio::time::timeout(30s)
    // 4. Cap open_read to actual file size, not u32::MAX
}
