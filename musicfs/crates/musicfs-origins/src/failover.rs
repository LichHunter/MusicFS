use crate::registry::OriginRegistry;
use crate::traits::Origin;
use musicfs_core::{Error, RealPath, Result};
use std::sync::Arc;
use std::time::Duration;
use tracing::{trace, warn};

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub delays: Vec<Duration>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self::spec_compliant()
    }
}

impl RetryConfig {
    pub fn spec_compliant() -> Self {
        Self {
            max_attempts: 3,
            delays: vec![
                Duration::from_millis(100),
                Duration::from_millis(500),
                Duration::from_millis(2000),
            ],
        }
    }

    pub fn with_delays(delays: Vec<Duration>) -> Self {
        Self {
            max_attempts: delays.len() as u32,
            delays,
        }
    }

    fn delay_for_attempt(&self, attempt: u32) -> Duration {
        self.delays
            .get(attempt as usize)
            .copied()
            .unwrap_or(*self.delays.last().unwrap_or(&Duration::from_millis(100)))
    }
}

pub struct FailoverExecutor {
    registry: Arc<OriginRegistry>,
    retry_config: RetryConfig,
}

impl FailoverExecutor {
    pub fn new(registry: Arc<OriginRegistry>, retry_config: RetryConfig) -> Self {
        Self {
            registry,
            retry_config,
        }
    }

    pub async fn read_with_failover(
        &self,
        path: &RealPath,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>> {
        let origins = self.registry.route_all(path);

        if origins.is_empty() {
            if let Some(origin) = self.registry.route_with_fallback(path) {
                warn!(
                    "No healthy origins, using fallback origin {}",
                    origin.id()
                );
                return self.read_with_retry(&origin, &path.path, offset, size).await;
            }
            return Err(Error::NoOriginAvailable);
        }

        let mut last_error = None;

        for origin in origins {
            trace!(origin_id = %origin.id(), "Attempting read from origin");
            let start = std::time::Instant::now();
            match self.read_with_retry(&origin, &path.path, offset, size).await {
                Ok(data) => {
                    let latency = start.elapsed().as_millis() as u64;
                    self.registry.record_latency(origin.id(), latency);
                    return Ok(data);
                }
                Err(e) => {
                    warn!(origin_id = %origin.id(), error = %e, "Origin failed, trying next");
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or(Error::NoOriginAvailable))
    }

    async fn read_with_retry(
        &self,
        origin: &Arc<dyn Origin>,
        path: &std::path::Path,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>> {
        for attempt in 0..self.retry_config.max_attempts {
            match origin.read(path, offset, size).await {
                Ok(data) => return Ok(data),
                Err(e) if attempt + 1 < self.retry_config.max_attempts => {
                    let delay = self.retry_config.delay_for_attempt(attempt);
                    warn!(
                        origin_id = %origin.id(),
                        attempt = attempt + 1,
                        max_attempts = self.retry_config.max_attempts,
                        error = %e,
                        delay_ms = delay.as_millis() as u64,
                        "Retrying read operation"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::MaxRetriesExceeded)
    }

    pub async fn read_full_with_failover(&self, path: &RealPath) -> Result<Vec<u8>> {
        let origins = self.registry.route_all(path);

        if origins.is_empty() {
            if let Some(origin) = self.registry.route_with_fallback(path) {
                warn!(
                    "No healthy origins for full read, using fallback {}",
                    origin.id()
                );
                return self.read_full_with_retry(&origin, &path.path).await;
            }
            return Err(Error::NoOriginAvailable);
        }

        let mut last_error = None;

        for origin in origins {
            trace!(origin_id = %origin.id(), "Attempting full read from origin");
            let start = std::time::Instant::now();
            match self.read_full_with_retry(&origin, &path.path).await {
                Ok(data) => {
                    let latency = start.elapsed().as_millis() as u64;
                    self.registry.record_latency(origin.id(), latency);
                    return Ok(data);
                }
                Err(e) => {
                    warn!(origin_id = %origin.id(), error = %e, "Origin failed full read, trying next");
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or(Error::NoOriginAvailable))
    }

    async fn read_full_with_retry(
        &self,
        origin: &Arc<dyn Origin>,
        path: &std::path::Path,
    ) -> Result<Vec<u8>> {
        for attempt in 0..self.retry_config.max_attempts {
            match origin.read_full(path).await {
                Ok(data) => return Ok(data),
                Err(e) if attempt + 1 < self.retry_config.max_attempts => {
                    let delay = self.retry_config.delay_for_attempt(attempt);
                    warn!(
                        origin_id = %origin.id(),
                        attempt = attempt + 1,
                        max_attempts = self.retry_config.max_attempts,
                        error = %e,
                        delay_ms = delay.as_millis() as u64,
                        "Retrying full read operation"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::MaxRetriesExceeded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.delays[0], Duration::from_millis(100));
        assert_eq!(config.delays[1], Duration::from_millis(500));
        assert_eq!(config.delays[2], Duration::from_millis(2000));
    }

    #[test]
    fn test_delay_for_attempt() {
        let config = RetryConfig::spec_compliant();

        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(500));
        assert_eq!(config.delay_for_attempt(2), Duration::from_millis(2000));
        assert_eq!(config.delay_for_attempt(10), Duration::from_millis(2000));
    }

    #[test]
    fn test_custom_delays() {
        let config = RetryConfig::with_delays(vec![
            Duration::from_millis(50),
            Duration::from_millis(100),
        ]);

        assert_eq!(config.max_attempts, 2);
        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(50));
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(100));
    }
}
