//! S3-compatible object storage origin
//!
//! This module is feature-gated behind the `s3` feature to avoid heavy AWS SDK dependencies.
//!
//! # Oracle Security Fixes (MUST IMPLEMENT)
//!
//! 1. **Range EOF** - Clamp range to `min(requested_end, file_size)` to avoid 416 errors
//! 2. **Health check** - Use `head_bucket` not `list_objects_v2` (lighter operation)
//! 3. **Timeout handling** - Wrap all remote calls with `tokio::time::timeout(30s)`
//!
//! # Example Implementation (when feature enabled)
//!
//! ```ignore
//! async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
//!     // Oracle fix: Clamp range to file size to avoid 416 error
//!     let file_size = self.stat(path).await?.size;
//!     let end = std::cmp::min(offset + size as u64, file_size).saturating_sub(1);
//!
//!     if offset >= file_size {
//!         return Ok(Vec::new()); // EOF
//!     }
//!
//!     let range = format!("bytes={}-{}", offset, end);
//!
//!     // Oracle fix: Add timeout to prevent hung connections
//!     let resp = tokio::time::timeout(
//!         Duration::from_secs(30),
//!         self.client.get_object().bucket(&self.bucket).key(&key).range(range).send()
//!     )
//!     .await
//!     .map_err(|_| Error::Timeout("S3 read timed out".into()))?
//!     .map_err(|e| Error::S3(e.to_string()))?;
//!
//!     // ...
//! }
//!
//! async fn health(&self) -> HealthStatus {
//!     // Oracle fix: Use head_bucket instead of list_objects_v2 (lighter)
//!     match self.client.head_bucket().bucket(&self.bucket).send().await {
//!         Ok(_) => HealthStatus::Healthy,
//!         Err(_) => HealthStatus::Unhealthy,
//!     }
//! }
//! ```

#[cfg(feature = "s3")]
mod implementation {
    // Full S3 implementation would go here when aws-sdk-s3 is enabled
}
