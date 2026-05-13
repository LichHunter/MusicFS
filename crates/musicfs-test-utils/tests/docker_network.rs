#![cfg(feature = "docker-tests")]

use musicfs_core::{OriginId, OriginType};
use musicfs_origins::{HealthMonitor, LocalOrigin, OriginRegistry};
use noxious_client::{Client, StreamDirection, Toxic, ToxicKind};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

const TOXIPROXY_API: &str = "http://localhost:8474";
const TOXIPROXY_LISTEN: &str = "localhost:18080";
const UPSTREAM_ADDR: &str = "minio:9000";

async fn require_toxiproxy() {
    let available = match reqwest::get(format!("{}/version", TOXIPROXY_API)).await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    };
    assert!(
        available,
        "Toxiproxy not available at {}. Run: cd tests/integration && docker-compose up -d",
        TOXIPROXY_API
    );
}

#[tokio::test]
#[ignore = "Requires docker-compose up -d (tests/integration/docker-compose.yml)"]
async fn test_toxiproxy_latency_injection() {
    require_toxiproxy().await;

    let client = Client::new(TOXIPROXY_API);
    let proxy = client
        .create_proxy("minio_latency", TOXIPROXY_LISTEN, UPSTREAM_ADDR)
        .await
        .expect("Failed to create proxy");

    let toxic = Toxic {
        name: "latency_downstream".to_string(),
        kind: ToxicKind::Latency {
            latency: 500,
            jitter: 100,
        },
        direction: StreamDirection::Downstream,
        toxicity: 1.0,
    };

    proxy.add_toxic(&toxic).await.expect("Failed to add toxic");

    let start = std::time::Instant::now();
    let _ = reqwest::get(format!("http://{}/minio/health/live", TOXIPROXY_LISTEN)).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(400),
        "Latency should be injected, got {:?}",
        elapsed
    );

    proxy.delete().await.expect("Failed to cleanup proxy");
}

#[tokio::test]
#[ignore = "Requires docker-compose up -d (tests/integration/docker-compose.yml)"]
async fn test_toxiproxy_timeout_simulates_network_partition() {
    require_toxiproxy().await;

    let client = Client::new(TOXIPROXY_API);
    let proxy = client
        .create_proxy("minio_partition", TOXIPROXY_LISTEN, UPSTREAM_ADDR)
        .await
        .expect("Failed to create proxy");

    let result = reqwest::get(format!("http://{}/minio/health/live", TOXIPROXY_LISTEN)).await;
    assert!(result.is_ok(), "Should reach MinIO through proxy initially");

    let toxic = Toxic {
        name: "timeout".to_string(),
        kind: ToxicKind::Timeout { timeout: 0 },
        direction: StreamDirection::Downstream,
        toxicity: 1.0,
    };

    proxy.add_toxic(&toxic).await.expect("Failed to add toxic");

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        reqwest::get(format!("http://{}/minio/health/live", TOXIPROXY_LISTEN)),
    )
    .await;

    assert!(
        result.is_err() || result.unwrap().is_err(),
        "Should timeout during partition"
    );

    proxy
        .remove_toxic("timeout")
        .await
        .expect("Failed to remove toxic");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = reqwest::get(format!("http://{}/minio/health/live", TOXIPROXY_LISTEN)).await;
    assert!(result.is_ok(), "Should reach MinIO after partition heals");

    proxy.delete().await.expect("Failed to cleanup proxy");
}

#[tokio::test]
#[ignore = "Requires docker-compose up -d (tests/integration/docker-compose.yml)"]
async fn test_toxiproxy_slow_close_throttles_responses() {
    require_toxiproxy().await;

    let client = Client::new(TOXIPROXY_API);
    let proxy = client
        .create_proxy("minio_slow", TOXIPROXY_LISTEN, UPSTREAM_ADDR)
        .await
        .expect("Failed to create proxy");

    let toxic = Toxic {
        name: "slow_close".to_string(),
        kind: ToxicKind::SlowClose { delay: 1000 },
        direction: StreamDirection::Downstream,
        toxicity: 1.0,
    };

    proxy.add_toxic(&toxic).await.expect("Failed to add toxic");

    let start = std::time::Instant::now();
    let _ = reqwest::get(format!("http://{}/minio/health/live", TOXIPROXY_LISTEN)).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(800),
        "Slow close should delay response, got {:?}",
        elapsed
    );

    proxy.delete().await.expect("Failed to cleanup proxy");
}
