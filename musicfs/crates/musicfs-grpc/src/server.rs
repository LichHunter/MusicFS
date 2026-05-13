use crate::proto::musicfs::v1::{
    music_fs_server::MusicFs, CacheStats, ClearCacheRequest, ClearCacheResponse, Empty, Event,
    EventFilter, HealthStatus, MountState, OriginHealthResponse, OriginRequest, OriginsResponse,
    PrefetchProgress, PrefetchRequest, SearchRequest, SearchResponse, SearchResult,
    ShutdownRequest, StatusResponse, SyncProgress, TierStats,
};
use musicfs_core::{Event as CoreEvent, EventBus};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::{debug, info, instrument};

pub struct MusicFsServer {
    start_time: Instant,
    event_bus: Arc<EventBus>,
    version: String,
}

impl MusicFsServer {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            start_time: Instant::now(),
            event_bus,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn event_to_proto(event: &CoreEvent) -> Event {
        let (event_type, origin_id, path, file_id) = match event {
            CoreEvent::FileAccessed {
                file_id,
                origin_id,
                path,
                ..
            } => (
                "file_accessed".to_string(),
                Some(origin_id.to_string()),
                Some(path.as_str().to_string()),
                Some(file_id.0),
            ),
            CoreEvent::FileAdded { path, origin_id } => (
                "file_added".to_string(),
                Some(origin_id.to_string()),
                Some(path.as_str().to_string()),
                None,
            ),
            CoreEvent::FileRemoved { path, file_id } => (
                "file_removed".to_string(),
                None,
                Some(path.as_str().to_string()),
                file_id.map(|id| id.0),
            ),
            CoreEvent::FileModified { path } => (
                "file_modified".to_string(),
                None,
                Some(path.as_str().to_string()),
                None,
            ),
            CoreEvent::SyncStarted { origin_id } => (
                "sync_started".to_string(),
                Some(origin_id.to_string()),
                None,
                None,
            ),
            CoreEvent::SyncCompleted {
                origin_id,
                files_changed,
            } => {
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("files_changed".to_string(), files_changed.to_string());
                return Event {
                    event_type: "sync_completed".to_string(),
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    origin_id: Some(origin_id.to_string()),
                    path: None,
                    file_id: None,
                    metadata,
                };
            }
            CoreEvent::OriginHealthChanged { origin_id, healthy } => {
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("healthy".to_string(), healthy.to_string());
                return Event {
                    event_type: "origin_health_changed".to_string(),
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    origin_id: Some(origin_id.to_string()),
                    path: None,
                    file_id: None,
                    metadata,
                };
            }
            CoreEvent::CacheEviction { bytes_freed } => {
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("bytes_freed".to_string(), bytes_freed.to_string());
                return Event {
                    event_type: "cache_eviction".to_string(),
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    origin_id: None,
                    path: None,
                    file_id: None,
                    metadata,
                };
            }
            CoreEvent::OriginConnected { origin_id } => (
                "origin_connected".to_string(),
                Some(origin_id.to_string()),
                None,
                None,
            ),
            CoreEvent::OriginDisconnected { origin_id } => (
                "origin_disconnected".to_string(),
                Some(origin_id.to_string()),
                None,
                None,
            ),
            CoreEvent::AllOriginsUnhealthy { candidate_count } => {
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("candidate_count".to_string(), candidate_count.to_string());
                return Event {
                    event_type: "all_origins_unhealthy".to_string(),
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    origin_id: None,
                    path: None,
                    file_id: None,
                    metadata,
                };
            }
        };

        Event {
            event_type,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            origin_id,
            path,
            file_id,
            metadata: Default::default(),
        }
    }

    fn matches_filter(event: &CoreEvent, filter: &EventFilter) -> bool {
        if !filter.event_types.is_empty() {
            let event_type = match event {
                CoreEvent::FileAccessed { .. } => "file_accessed",
                CoreEvent::FileAdded { .. } => "file_added",
                CoreEvent::FileRemoved { .. } => "file_removed",
                CoreEvent::FileModified { .. } => "file_modified",
                CoreEvent::SyncStarted { .. } => "sync_started",
                CoreEvent::SyncCompleted { .. } => "sync_completed",
                CoreEvent::OriginHealthChanged { .. } => "origin_health_changed",
                CoreEvent::CacheEviction { .. } => "cache_eviction",
                CoreEvent::OriginConnected { .. } => "origin_connected",
                CoreEvent::OriginDisconnected { .. } => "origin_disconnected",
                CoreEvent::AllOriginsUnhealthy { .. } => "all_origins_unhealthy",
            };

            if !filter.event_types.iter().any(|t| t == event_type) {
                return false;
            }
        }

        if let Some(ref origin_filter) = filter.origin_id {
            let event_origin = match event {
                CoreEvent::FileAccessed { origin_id, .. }
                | CoreEvent::FileAdded { origin_id, .. }
                | CoreEvent::SyncStarted { origin_id }
                | CoreEvent::SyncCompleted { origin_id, .. }
                | CoreEvent::OriginHealthChanged { origin_id, .. }
                | CoreEvent::OriginConnected { origin_id }
                | CoreEvent::OriginDisconnected { origin_id } => Some(origin_id.to_string()),
                CoreEvent::FileRemoved { .. }
                | CoreEvent::FileModified { .. }
                | CoreEvent::CacheEviction { .. }
                | CoreEvent::AllOriginsUnhealthy { .. } => None,
            };

            if event_origin.as_ref() != Some(origin_filter) {
                return false;
            }
        }

        true
    }
}

#[tonic::async_trait]
impl MusicFs for MusicFsServer {
    async fn search(
        &self,
        _request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        Err(Status::unimplemented(
            "Use SearchService for search operations",
        ))
    }

    type SearchStreamStream = ReceiverStream<Result<SearchResult, Status>>;

    async fn search_stream(
        &self,
        _request: Request<SearchRequest>,
    ) -> Result<Response<Self::SearchStreamStream>, Status> {
        Err(Status::unimplemented(
            "Use SearchService for search operations",
        ))
    }

    #[instrument(level = "debug", skip(self, _request), fields(method = "get_status"))]
    async fn get_status(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<StatusResponse>, Status> {
        debug!("gRPC get_status called");
        let uptime = self.start_time.elapsed().as_secs();

        Ok(Response::new(StatusResponse {
            version: self.version.clone(),
            uptime_secs: uptime,
            mount_point: String::new(),
            state: MountState::MountReady as i32,
            open_file_handles: 0,
            fuse_ops_total: 0,
            files_indexed: 0,
            cache_size_bytes: 0,
            origins: vec![],
        }))
    }

    #[instrument(level = "info", skip(self, request), fields(method = "shutdown"))]
    async fn shutdown(
        &self,
        request: Request<ShutdownRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        info!(
            graceful = req.graceful,
            timeout_secs = req.timeout_secs,
            "gRPC shutdown requested"
        );

        Ok(Response::new(Empty {}))
    }

    #[instrument(level = "debug", skip(self, _request), fields(method = "get_cache_stats"))]
    async fn get_cache_stats(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<CacheStats>, Status> {
        debug!("gRPC get_cache_stats called");
        Ok(Response::new(CacheStats {
            total_size_bytes: 0,
            used_size_bytes: 0,
            size_limit_bytes: 0,
            chunk_count: 0,
            chunks_unique: 0,
            dedup_ratio: 0.0,
            hit_count: 0,
            miss_count: 0,
            hit_ratio: 0.0,
            metadata_entries: 0,
            metadata_bytes: 0,
            l1_metadata: Some(TierStats {
                entries: 0,
                size_bytes: 0,
                hits: 0,
                misses: 0,
            }),
            l2_headers: Some(TierStats {
                entries: 0,
                size_bytes: 0,
                hits: 0,
                misses: 0,
            }),
            l3_chunks: Some(TierStats {
                entries: 0,
                size_bytes: 0,
                hits: 0,
                misses: 0,
            }),
        }))
    }

    #[instrument(level = "info", skip(self, request), fields(method = "clear_cache"))]
    async fn clear_cache(
        &self,
        request: Request<ClearCacheRequest>,
    ) -> Result<Response<ClearCacheResponse>, Status> {
        let req = request.into_inner();
        info!(
            origin_id = ?req.origin_id,
            clear_metadata = req.clear_metadata,
            clear_chunks = req.clear_chunks,
            "gRPC clear_cache"
        );

        Ok(Response::new(ClearCacheResponse {
            bytes_cleared: 0,
            chunks_cleared: 0,
        }))
    }

    type PrefetchStream = ReceiverStream<Result<PrefetchProgress, Status>>;

    #[instrument(level = "debug", skip(self, request), fields(method = "prefetch"))]
    async fn prefetch(
        &self,
        request: Request<PrefetchRequest>,
    ) -> Result<Response<Self::PrefetchStream>, Status> {
        let req = request.into_inner();
        let total = req.paths.len() as u32;
        debug!(file_count = total, "gRPC prefetch started");

        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            for (i, path) in req.paths.into_iter().enumerate() {
                let progress = PrefetchProgress {
                    current_path: path,
                    completed: i as u32 + 1,
                    total,
                    bytes_fetched: 0,
                };
                if tx.send(Ok(progress)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    #[instrument(level = "debug", skip(self, _request), fields(method = "list_origins"))]
    async fn list_origins(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<OriginsResponse>, Status> {
        debug!("gRPC list_origins called");
        Ok(Response::new(OriginsResponse { origins: vec![] }))
    }

    #[instrument(level = "debug", skip(self, request), fields(method = "get_origin_health"))]
    async fn get_origin_health(
        &self,
        request: Request<OriginRequest>,
    ) -> Result<Response<OriginHealthResponse>, Status> {
        let req = request.into_inner();
        debug!(origin_id = %req.origin_id, "gRPC get_origin_health");

        Ok(Response::new(OriginHealthResponse {
            origin_id: req.origin_id,
            status: HealthStatus::HealthUnknown as i32,
            message: None,
            last_check_secs: 0,
        }))
    }

    type RescanOriginStream = ReceiverStream<Result<SyncProgress, Status>>;

    #[instrument(level = "info", skip(self, request), fields(method = "rescan_origin"))]
    async fn rescan_origin(
        &self,
        request: Request<OriginRequest>,
    ) -> Result<Response<Self::RescanOriginStream>, Status> {
        let req = request.into_inner();
        info!(origin_id = %req.origin_id, "gRPC rescan_origin started");

        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            let phases = ["scanning", "indexing", "complete"];
            for (i, phase) in phases.iter().enumerate() {
                let progress = SyncProgress {
                    phase: phase.to_string(),
                    current: i as u32 + 1,
                    total: phases.len() as u32,
                    current_path: String::new(),
                    bytes_synced: 0,
                };
                if tx.send(Ok(progress)).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    type SubscribeEventsStream = ReceiverStream<Result<Event, Status>>;

    #[instrument(level = "info", skip(self, request), fields(method = "subscribe_events"))]
    async fn subscribe_events(
        &self,
        request: Request<EventFilter>,
    ) -> Result<Response<Self::SubscribeEventsStream>, Status> {
        info!("gRPC subscribe_events: client connected");
        let filter = request.into_inner();
        let mut rx = self.event_bus.subscribe();
        let (tx, out_rx) = mpsc::channel(100);

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if Self::matches_filter(&event, &filter) {
                            let proto_event = Self::event_to_proto(&event);
                            if tx.send(Ok(proto_event)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Event subscriber lagged, skipped events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!("Event channel closed");
                        break;
                    }
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(out_rx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_status() {
        let event_bus = Arc::new(EventBus::new(16));
        let server = MusicFsServer::new(event_bus);

        let response = server.get_status(Request::new(Empty {})).await.unwrap();
        let status = response.into_inner();

        assert!(!status.version.is_empty());
        assert!(status.uptime_secs < 5);
    }

    #[tokio::test]
    async fn test_get_cache_stats() {
        let event_bus = Arc::new(EventBus::new(16));
        let server = MusicFsServer::new(event_bus);

        let response = server
            .get_cache_stats(Request::new(Empty {}))
            .await
            .unwrap();
        let stats = response.into_inner();

        assert_eq!(stats.hit_ratio, 0.0);
    }
}
