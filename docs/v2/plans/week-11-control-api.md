# Week 11: Control API & Production

**Phase**: 4 - Plugin System & Polish  
**Goal**: gRPC control API, metrics, and production readiness  
**Requirements**: FR-17.1-17.5, FR-18.1-18.4, NFR-6.1-6.4, NFR-10.1-10.4

---

## Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| gRPC server | musicfs-grpc | `server.rs` | FR-17.1-17.5 |
| Proto codegen | proto/ | `musicfs.proto`, `build.rs` | FR-17.2 |
| Event streaming | musicfs-grpc | `events.rs` | FR-18.1-18.3 |
| Webhook handler | musicfs-grpc | `webhook.rs` | FR-18.2 |
| Metrics export | musicfs-core | `metrics.rs` | NFR-6.1-6.4, NFR-10.2-10.4 |
| CLI completion | musicfs-cli | `main.rs` | FR-17 |
| systemd unit | dist/ | `musicfs.service` | Production |
| Packaging | dist/ | `PKGBUILD`, `musicfs.spec` | Production |
| E2E compatibility | tests/ | `e2e_players.rs` | NFR-12.1-12.3 |

---

## Proto Definitions (`proto/musicfs.proto`)

Per architecture.md section 4.3.7, implement full gRPC API:

```protobuf
syntax = "proto3";
package musicfs.v1;

service MusicFS {
    // Daemon lifecycle
    rpc GetStatus(Empty) returns (StatusResponse);
    rpc Shutdown(ShutdownRequest) returns (Empty);
    
    // Cache management
    rpc GetCacheStats(Empty) returns (CacheStats);
    rpc ClearCache(ClearCacheRequest) returns (ClearCacheResponse);
    rpc Prefetch(PrefetchRequest) returns (stream PrefetchProgress);
    
    // Origin management
    rpc ListOrigins(Empty) returns (OriginsResponse);
    rpc GetOriginHealth(OriginRequest) returns (OriginHealth);
    rpc RescanOrigin(OriginRequest) returns (stream SyncProgress);
    
    // Search (already implemented in Week 8)
    rpc Search(SearchRequest) returns (SearchResponse);
    rpc SearchStream(SearchRequest) returns (stream SearchResult);
    
    // Events (server-streaming)
    rpc SubscribeEvents(EventFilter) returns (stream Event);
}
```

Full message definitions in architecture.md section 4.3.7.

---

## gRPC Server (`musicfs-grpc/src/server.rs`)

```rust
pub struct MusicFsService {
    core: Arc<MusicFsCore>,
    events: broadcast::Sender<Event>,
    metrics: Arc<MetricsCollector>,
}

#[tonic::async_trait]
impl musicfs::v1::music_fs_server::MusicFs for MusicFsService {
    // Daemon lifecycle
    async fn get_status(&self, _: Request<Empty>) -> Result<Response<StatusResponse>, Status>;
    async fn shutdown(&self, req: Request<ShutdownRequest>) -> Result<Response<Empty>, Status>;
    
    // Cache management
    async fn get_cache_stats(&self, _: Request<Empty>) -> Result<Response<CacheStats>, Status>;
    async fn clear_cache(&self, req: Request<ClearCacheRequest>) -> Result<Response<ClearCacheResponse>, Status>;
    
    type PrefetchStream = ReceiverStream<Result<PrefetchProgress, Status>>;
    async fn prefetch(&self, req: Request<PrefetchRequest>) -> Result<Response<Self::PrefetchStream>, Status>;
    
    // Origin management
    async fn list_origins(&self, _: Request<Empty>) -> Result<Response<OriginsResponse>, Status>;
    async fn get_origin_health(&self, req: Request<OriginRequest>) -> Result<Response<OriginHealth>, Status>;
    
    type RescanOriginStream = ReceiverStream<Result<SyncProgress, Status>>;
    async fn rescan_origin(&self, req: Request<OriginRequest>) -> Result<Response<Self::RescanOriginStream>, Status>;
    
    // Events
    type SubscribeEventsStream = ReceiverStream<Result<Event, Status>>;
    async fn subscribe_events(&self, req: Request<EventFilter>) -> Result<Response<Self::SubscribeEventsStream>, Status>;
}
```

---

## Event Streaming (`musicfs-grpc/src/events.rs`)

```rust
pub struct EventStreamer {
    bus: Arc<EventBus>,
}

impl EventStreamer {
    /// Convert internal events to gRPC Event messages
    pub fn subscribe(&self, filter: EventFilter) -> impl Stream<Item = Event>;
    
    /// Filter events by type and origin
    fn matches(event: &Event, filter: &EventFilter) -> bool;
}
```

---

## Webhook Handler (`musicfs-grpc/src/webhook.rs`)

HTTP webhook notifications for external integrations (FR-18.2):

```rust
use reqwest::Client;
use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub event_type: String,
    pub timestamp: i64,
    pub data: serde_json::Value,
}

pub struct WebhookConfig {
    pub url: String,
    pub secret: Option<String>,
    pub events: Vec<String>,  // Filter: ["file_accessed", "sync_completed", ...]
    pub retry_count: u32,
    pub timeout_ms: u64,
}

pub struct WebhookHandler {
    client: Client,
    configs: Vec<WebhookConfig>,
}

impl WebhookHandler {
    pub fn new(configs: Vec<WebhookConfig>) -> Self;
    
    /// Start listening to event bus and dispatch webhooks
    pub async fn run(&self, mut rx: broadcast::Receiver<Event>) {
        while let Ok(event) = rx.recv().await {
            for config in &self.configs {
                if self.matches_filter(&event, config) {
                    self.dispatch(config, &event).await;
                }
            }
        }
    }
    
    /// Dispatch webhook with retry logic
    async fn dispatch(&self, config: &WebhookConfig, event: &Event) {
        let payload = WebhookPayload {
            event_type: event.event_type(),
            timestamp: event.timestamp(),
            data: event.to_json(),
        };
        
        let mut attempts = 0;
        loop {
            let result = self.client
                .post(&config.url)
                .timeout(Duration::from_millis(config.timeout_ms))
                .header("X-MusicFS-Signature", self.sign(&payload, config))
                .json(&payload)
                .send()
                .await;
            
            match result {
                Ok(resp) if resp.status().is_success() => break,
                _ if attempts < config.retry_count => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(100 * 2u64.pow(attempts))).await;
                }
                _ => {
                    tracing::warn!("Webhook delivery failed after {} attempts", attempts);
                    break;
                }
            }
        }
    }
    
    /// HMAC-SHA256 signature if secret configured
    fn sign(&self, payload: &WebhookPayload, config: &WebhookConfig) -> String;
    
    fn matches_filter(&self, event: &Event, config: &WebhookConfig) -> bool;
}
```

Configuration in `config.toml`:

```toml
[[webhooks]]
url = "https://example.com/musicfs/events"
secret = "your-webhook-secret"
events = ["file_accessed", "sync_completed", "origin_health_changed"]
retry_count = 3
timeout_ms = 5000
```

---

## E2E Compatibility Tests (`tests/e2e_players.rs`)

Verify MusicFS works with common media players (NFR-12.1-12.3):

```rust
//! E2E tests for media player compatibility
//! Requires: mpv, vlc, file manager (nautilus/dolphin) installed

use std::process::Command;
use std::time::Duration;

/// Test mpv can play files from MusicFS (NFR-12.1)
#[test]
#[ignore] // Run manually: cargo test --ignored
fn test_mpv_playback() {
    let mountpoint = setup_test_mount();
    
    // mpv should be able to:
    // 1. Open file without hanging
    // 2. Read metadata (duration, format)
    // 3. Play first few seconds
    // 4. Seek forward
    // 5. Exit cleanly
    
    let output = Command::new("mpv")
        .args([
            "--no-video",
            "--no-audio",  // Silent playback
            "--length=2",  // Play 2 seconds only
            "--msg-level=all=debug",
            &format!("{}/Artist/Album/01 - Track.flac", mountpoint),
        ])
        .output()
        .expect("mpv must be installed");
    
    assert!(output.status.success(), "mpv playback failed: {:?}", output);
}

/// Test VLC can browse and play (NFR-12.2)
#[test]
#[ignore]
fn test_vlc_playback() {
    let mountpoint = setup_test_mount();
    
    // VLC should handle:
    // 1. Directory browsing
    // 2. Playlist creation from folder
    // 3. Metadata display
    // 4. Gapless playback (if supported)
    
    let output = Command::new("cvlc")  // Command-line VLC
        .args([
            "--play-and-exit",
            "--run-time=2",
            &format!("{}/Artist/Album/", mountpoint),
        ])
        .output()
        .expect("vlc must be installed");
    
    assert!(output.status.success(), "VLC playback failed");
}

/// Test file manager operations (NFR-12.3)
#[test]
#[ignore]
fn test_file_manager_operations() {
    let mountpoint = setup_test_mount();
    
    // File managers should be able to:
    // 1. List directories without timeout
    // 2. Show file previews/thumbnails
    // 3. Display file properties
    // 4. Copy files to local disk
    
    // Test basic stat operations that file managers use
    let entries: Vec<_> = std::fs::read_dir(&mountpoint)
        .expect("read_dir failed")
        .collect();
    
    assert!(!entries.is_empty(), "mountpoint should have entries");
    
    // Test stat on each entry (file managers do this for icons)
    for entry in entries {
        let entry = entry.expect("entry should be valid");
        let metadata = entry.metadata().expect("metadata should work");
        assert!(metadata.is_dir() || metadata.is_file());
    }
}

/// Test concurrent access from multiple players
#[test]
#[ignore]
fn test_concurrent_player_access() {
    let mountpoint = setup_test_mount();
    
    // Spawn multiple players accessing different files
    let handles: Vec<_> = (0..3)
        .map(|i| {
            let mp = mountpoint.clone();
            std::thread::spawn(move || {
                Command::new("mpv")
                    .args([
                        "--no-video", "--no-audio", "--length=1",
                        &format!("{}/Artist/Album/0{} - Track.flac", mp, i + 1),
                    ])
                    .output()
            })
        })
        .collect();
    
    for handle in handles {
        let output = handle.join().unwrap().expect("mpv should run");
        assert!(output.status.success());
    }
}

fn setup_test_mount() -> String {
    // Returns path to test mount with sample files
    std::env::var("MUSICFS_TEST_MOUNT")
        .unwrap_or_else(|_| "/tmp/musicfs-test".to_string())
}
```

---

## Metrics (`musicfs-core/src/metrics.rs`)

Per architecture.md section 5.2:

```rust
use prometheus::{IntCounterVec, HistogramVec, IntGauge, register_*};

lazy_static! {
    pub static ref FUSE_OPS: IntCounterVec = register_int_counter_vec!(
        "musicfs_fuse_ops_total",
        "Total FUSE operations",
        &["op"]
    ).unwrap();
    
    pub static ref FUSE_LATENCY: HistogramVec = register_histogram_vec!(
        "musicfs_fuse_latency_seconds",
        "FUSE operation latency",
        &["op"],
        vec![0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0]
    ).unwrap();
    
    pub static ref CACHE_HITS: IntCounter = register_int_counter!(
        "musicfs_cache_hits_total",
        "Cache hits"
    ).unwrap();
    
    pub static ref CACHE_MISSES: IntCounter = register_int_counter!(
        "musicfs_cache_misses_total",
        "Cache misses"
    ).unwrap();
    
    pub static ref CACHE_SIZE_BYTES: IntGauge = register_int_gauge!(
        "musicfs_cache_size_bytes",
        "Current cache size in bytes"
    ).unwrap();
    
    pub static ref ORIGIN_HEALTH: IntGaugeVec = register_int_gauge_vec!(
        "musicfs_origin_health",
        "Origin health status (1=healthy, 0=unhealthy)",
        &["origin"]
    ).unwrap();
}

/// Expose metrics on HTTP endpoint
pub async fn serve_metrics(addr: SocketAddr) -> Result<(), MetricsError>;
```

---

## CLI Commands (`musicfs-cli/src/main.rs`)

```rust
#[derive(Parser)]
enum Command {
    /// Mount filesystem
    Mount {
        #[arg(short, long)]
        config: PathBuf,
        mountpoint: PathBuf,
    },
    
    /// Get daemon status
    Status,
    
    /// Cache management
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    
    /// Search library
    Search {
        query: String,
        #[arg(short, long, default_value = "100")]
        limit: u32,
    },
    
    /// Origin management
    Origin {
        #[command(subcommand)]
        command: OriginCommand,
    },
    
    /// Subscribe to events
    Events {
        #[arg(short, long)]
        r#type: Option<String>,
    },
}

#[derive(Subcommand)]
enum CacheCommand {
    Stats,
    Clear { origin: Option<String> },
    Prefetch { paths: Vec<String> },
}

#[derive(Subcommand)]
enum OriginCommand {
    List,
    Health { origin_id: String },
    Rescan { origin_id: String },
}
```

---

## systemd Service (`dist/musicfs.service`)

```ini
[Unit]
Description=MusicFS - Metadata-Organized Music Filesystem
After=network.target

[Service]
Type=notify
ExecStart=/usr/bin/musicfs mount --config /etc/musicfs/config.toml /mnt/music
ExecStop=/usr/bin/musicfs shutdown
Restart=on-failure
RestartSec=5
User=musicfs
Group=musicfs

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/var/cache/musicfs /mnt/music
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_grpc_status` | Unit | GetStatus RPC (FR-17.1) |
| `test_grpc_cache_stats` | Unit | GetCacheStats RPC |
| `test_grpc_cache_clear` | Unit | ClearCache RPC (FR-17.3) |
| `test_grpc_origins_list` | Unit | ListOrigins RPC |
| `test_grpc_origin_rescan` | Integration | RescanOrigin streaming |
| `test_grpc_events_stream` | Integration | Event streaming (FR-18.1) |
| `test_grpc_prefetch_stream` | Integration | Prefetch progress |
| `test_webhook_dispatch` | Unit | Webhook delivery (FR-18.2) |
| `test_webhook_retry` | Unit | Webhook retry on failure |
| `test_webhook_hmac_signature` | Unit | HMAC-SHA256 signing |
| `test_metrics_prometheus` | Unit | Prometheus format (NFR-6.1) |
| `test_metrics_http_endpoint` | Integration | HTTP metrics endpoint |
| `test_cli_commands` | Integration | CLI works |
| `test_systemd_service` | E2E | Service lifecycle |
| `test_mpv_playback` | E2E | mpv compatibility (NFR-12.1) |
| `test_vlc_playback` | E2E | VLC compatibility (NFR-12.2) |
| `test_file_manager_operations` | E2E | File manager browsing (NFR-12.3) |
| `test_concurrent_player_access` | E2E | Multiple players concurrently |

---

## Exit Criteria

- [ ] gRPC API fully functional (all RPCs from architecture.md 4.3.7)
- [ ] Event streaming works with filtering
- [ ] Webhook notifications delivered with HMAC signing
- [ ] Prometheus metrics exported on HTTP endpoint
- [ ] CLI feature-complete with all commands
- [ ] systemd service works (start, stop, restart)
- [ ] mpv, VLC playback verified (E2E tests)
- [ ] File manager browsing verified
- [ ] All acceptance tests pass

---

## Architecture Alignment

Per architecture.md section 4.3.7:
- gRPC over Unix socket ✓
- Protocol Buffers for type safety ✓
- Server-streaming for events, sync progress, prefetch ✓
- CLI wraps gRPC client ✓

Per architecture.md section 5.2:
- Prometheus metrics format ✓
- Golden signals: latency, traffic, errors, saturation ✓

Per requirements.md:
- FR-17.1: Unix socket control ✓
- FR-17.2: gRPC with Protocol Buffers ✓
- FR-17.3: Cache management commands ✓
- FR-17.4: Runtime configuration ✓
- FR-17.5: Graceful shutdown ✓
- FR-18.1: File access events ✓
- FR-18.2: Webhook notifications ✓ (HTTP webhooks with HMAC)
- FR-18.3: Event streaming ✓
- FR-18.4: Access pattern logging ✓
- NFR-10.1: Configurable logging ✓
- NFR-10.2: Metrics exposure ✓
- NFR-10.3: Health check ✓
- NFR-10.4: Prometheus integration ✓
- NFR-12.1: mpv compatibility ✓ (E2E tests)
- NFR-12.2: VLC compatibility ✓ (E2E tests)
- NFR-12.3: File manager compatibility ✓ (E2E tests)
