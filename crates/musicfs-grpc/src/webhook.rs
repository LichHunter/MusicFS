use musicfs_core::Event;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, warn};

#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub event_type: String,
    pub timestamp: i64,
    pub data: serde_json::Value,
}

#[derive(Clone, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    #[serde(skip_serializing)]
    pub secret: Option<String>,
    pub events: Vec<String>,
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl std::fmt::Debug for WebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookConfig")
            .field("url", &self.url)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .field("events", &self.events)
            .field("retry_count", &self.retry_count)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

fn default_retry_count() -> u32 {
    3
}

fn default_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error("Failed to initialize HTTP client: {0}")]
    ClientInit(String),
}

pub struct WebhookHandler {
    client: reqwest::Client,
    configs: Vec<WebhookConfig>,
}

impl WebhookHandler {
    pub fn new(configs: Vec<WebhookConfig>) -> Result<Self, WebhookError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                error!(error = %e, "Failed to create webhook HTTP client");
                WebhookError::ClientInit(e.to_string())
            })?;

        Ok(Self { client, configs })
    }

    pub async fn run(&self, mut rx: broadcast::Receiver<Event>) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    for config in &self.configs {
                        if self.matches_filter(&event, config) {
                            self.dispatch(config, &event).await;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "Webhook handler lagged, skipped events");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("Event channel closed, webhook handler stopping");
                    break;
                }
            }
        }
    }

    async fn dispatch(&self, config: &WebhookConfig, event: &Event) {
        let payload = WebhookPayload {
            event_type: self.event_type_name(event),
            timestamp: chrono::Utc::now().timestamp_millis(),
            data: self.event_to_json(event),
        };

        let signature = self.sign(&payload, config);

        let mut attempts = 0u32;
        loop {
            let result = self
                .client
                .post(&config.url)
                .timeout(Duration::from_millis(config.timeout_ms))
                .header("Content-Type", "application/json")
                .header("X-MusicFS-Signature", &signature)
                .header("X-MusicFS-Event", &payload.event_type)
                .json(&payload)
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    debug!(
                        "Webhook delivered to {} for {}",
                        config.url, payload.event_type
                    );
                    break;
                }
                Ok(resp) => {
                    warn!(
                        "Webhook to {} returned status {}, attempt {}/{}",
                        config.url,
                        resp.status(),
                        attempts + 1,
                        config.retry_count + 1
                    );
                }
                Err(e) => {
                    warn!(
                        "Webhook to {} failed: {}, attempt {}/{}",
                        config.url,
                        e,
                        attempts + 1,
                        config.retry_count + 1
                    );
                }
            }

            if attempts >= config.retry_count {
                warn!(
                    "Webhook delivery to {} failed after {} attempts",
                    config.url,
                    attempts + 1
                );
                break;
            }

            attempts += 1;
            let delay = Duration::from_millis(100 * 2u64.pow(attempts));
            tokio::time::sleep(delay).await;
        }
    }

    fn sign(&self, payload: &WebhookPayload, config: &WebhookConfig) -> String {
        match &config.secret {
            Some(secret) => {
                use hmac::{Hmac, Mac};
                use sha2::Sha256;

                type HmacSha256 = Hmac<Sha256>;

                let body = serde_json::to_string(payload).unwrap_or_default();
                let mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
                    Ok(m) => m,
                    Err(e) => {
                        error!(error = %e, "Invalid HMAC key for webhook signature");
                        return String::new();
                    }
                };
                let mut mac = mac;
                mac.update(body.as_bytes());
                let result = mac.finalize();

                format!("sha256={}", hex::encode(result.into_bytes()))
            }
            None => String::new(),
        }
    }

    fn matches_filter(&self, event: &Event, config: &WebhookConfig) -> bool {
        if config.events.is_empty() {
            return true;
        }

        let event_type = self.event_type_name(event);
        config.events.iter().any(|e| e == &event_type)
    }

    fn event_type_name(&self, event: &Event) -> String {
        match event {
            Event::FileAccessed { .. } => "file_accessed",
            Event::FileAdded { .. } => "file_added",
            Event::FileRemoved { .. } => "file_removed",
            Event::FileModified { .. } => "file_modified",
            Event::SyncStarted { .. } => "sync_started",
            Event::SyncCompleted { .. } => "sync_completed",
            Event::OriginHealthChanged { .. } => "origin_health_changed",
            Event::CacheEviction { .. } => "cache_eviction",
            Event::OriginConnected { .. } => "origin_connected",
            Event::OriginDisconnected { .. } => "origin_disconnected",
            Event::AllOriginsUnhealthy { .. } => "all_origins_unhealthy",
        }
        .to_string()
    }

    fn event_to_json(&self, event: &Event) -> serde_json::Value {
        match event {
            Event::FileAccessed {
                file_id,
                origin_id,
                path,
                offset,
                size,
            } => serde_json::json!({
                "file_id": file_id.0,
                "origin_id": origin_id.to_string(),
                "path": path.as_str(),
                "offset": offset,
                "size": size,
            }),
            Event::FileAdded { path, origin_id } => serde_json::json!({
                "path": path.as_str(),
                "origin_id": origin_id.to_string(),
            }),
            Event::FileRemoved { path, file_id } => serde_json::json!({
                "path": path.as_str(),
                "file_id": file_id.map(|id| id.0),
            }),
            Event::FileModified { path } => serde_json::json!({
                "path": path.as_str(),
            }),
            Event::SyncStarted { origin_id } => serde_json::json!({
                "origin_id": origin_id.to_string(),
            }),
            Event::SyncCompleted {
                origin_id,
                files_changed,
            } => serde_json::json!({
                "origin_id": origin_id.to_string(),
                "files_changed": files_changed,
            }),
            Event::OriginHealthChanged { origin_id, healthy } => serde_json::json!({
                "origin_id": origin_id.to_string(),
                "healthy": healthy,
            }),
            Event::CacheEviction { bytes_freed } => serde_json::json!({
                "bytes_freed": bytes_freed,
            }),
            Event::OriginConnected { origin_id } => serde_json::json!({
                "origin_id": origin_id.to_string(),
            }),
            Event::OriginDisconnected { origin_id } => serde_json::json!({
                "origin_id": origin_id.to_string(),
            }),
            Event::AllOriginsUnhealthy { candidate_count } => serde_json::json!({
                "candidate_count": candidate_count,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::OriginId;

    #[test]
    fn test_webhook_config_defaults() {
        let json = r#"{"url": "http://example.com", "events": []}"#;
        let config: WebhookConfig = serde_json::from_str(json).unwrap();

        assert_eq!(config.retry_count, 3);
        assert_eq!(config.timeout_ms, 5000);
    }

    #[test]
    fn test_event_type_name() {
        let handler = WebhookHandler::new(vec![]).unwrap();

        let event = Event::SyncStarted {
            origin_id: OriginId::from("test"),
        };
        assert_eq!(handler.event_type_name(&event), "sync_started");
    }

    #[test]
    fn test_matches_filter_empty() {
        let handler = WebhookHandler::new(vec![]).unwrap();
        let config = WebhookConfig {
            url: "http://example.com".to_string(),
            secret: None,
            events: vec![],
            retry_count: 3,
            timeout_ms: 5000,
        };

        let event = Event::SyncStarted {
            origin_id: OriginId::from("test"),
        };
        assert!(handler.matches_filter(&event, &config));
    }

    #[test]
    fn test_matches_filter_specific() {
        let handler = WebhookHandler::new(vec![]).unwrap();
        let config = WebhookConfig {
            url: "http://example.com".to_string(),
            secret: None,
            events: vec!["sync_started".to_string()],
            retry_count: 3,
            timeout_ms: 5000,
        };

        let event = Event::SyncStarted {
            origin_id: OriginId::from("test"),
        };
        assert!(handler.matches_filter(&event, &config));

        let event2 = Event::SyncCompleted {
            origin_id: OriginId::from("test"),
            files_changed: 0,
        };
        assert!(!handler.matches_filter(&event2, &config));
    }
}
