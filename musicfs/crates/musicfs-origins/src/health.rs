use crate::traits::Origin;
use dashmap::DashMap;
use musicfs_core::{Event, EventBus, HealthStatus, OriginId, OriginType};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, info_span, Instrument};

pub struct HealthMonitor {
    origins: DashMap<OriginId, Arc<dyn Origin>>,
    state: DashMap<OriginId, OriginHealthState>,
    check_interval: Duration,
    default_threshold: u32,
    per_type_thresholds: HashMap<OriginType, u32>,
    event_bus: Option<Arc<EventBus>>,
}

#[derive(Debug, Clone)]
pub struct OriginHealthState {
    pub status: HealthStatus,
    pub last_check: Instant,
    pub consecutive_failures: u32,
    pub last_latency_ms: Option<u64>,
}

impl Default for OriginHealthState {
    fn default() -> Self {
        Self {
            status: HealthStatus::Unknown,
            last_check: Instant::now(),
            consecutive_failures: 0,
            last_latency_ms: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HealthSnapshot {
    pub healthy: Vec<OriginId>,
    pub degraded: Vec<OriginId>,
    pub unhealthy: Vec<OriginId>,
    pub failure_counts: HashMap<OriginId, u32>,
}

impl HealthSnapshot {
    pub fn is_healthy(&self, id: &OriginId) -> bool {
        self.healthy.contains(id)
    }

    pub fn is_degraded(&self, id: &OriginId) -> bool {
        self.degraded.contains(id)
    }

    pub fn is_unhealthy(&self, id: &OriginId) -> bool {
        self.unhealthy.contains(id)
    }

    pub fn failure_count(&self, id: &OriginId) -> Option<u32> {
        self.failure_counts.get(id).copied()
    }

    pub fn all_unhealthy(&self) -> bool {
        self.healthy.is_empty() && self.degraded.is_empty()
    }

    pub fn total_candidates(&self) -> usize {
        self.healthy.len() + self.degraded.len() + self.unhealthy.len()
    }
}

impl HealthMonitor {
    pub fn new(check_interval: Duration) -> Self {
        let mut per_type = HashMap::new();
        per_type.insert(OriginType::Local, 1);
        per_type.insert(OriginType::Nfs, 3);
        per_type.insert(OriginType::Smb, 3);
        per_type.insert(OriginType::S3, 3);
        per_type.insert(OriginType::Sftp, 3);

        Self {
            origins: DashMap::new(),
            state: DashMap::new(),
            check_interval,
            default_threshold: 3,
            per_type_thresholds: per_type,
            event_bus: None,
        }
    }

    pub fn with_threshold(mut self, threshold: u32) -> Self {
        self.default_threshold = threshold;
        self
    }

    pub fn with_per_type_thresholds(mut self, thresholds: HashMap<OriginType, u32>) -> Self {
        self.per_type_thresholds = thresholds;
        self
    }

    pub fn with_event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.event_bus = Some(bus);
        self
    }

    fn threshold_for(&self, origin_type: OriginType) -> u32 {
        self.per_type_thresholds
            .get(&origin_type)
            .copied()
            .unwrap_or(self.default_threshold)
    }

    pub fn add_origin(&self, origin: Arc<dyn Origin>) {
        let id = origin.id().clone();
        self.origins.insert(id.clone(), origin);
        self.state.insert(id, OriginHealthState::default());
    }

    pub fn remove_origin(&self, id: &OriginId) {
        self.origins.remove(id);
        self.state.remove(id);
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        let mut healthy = Vec::new();
        let mut degraded = Vec::new();
        let mut unhealthy = Vec::new();
        let mut failure_counts = HashMap::new();

        for entry in self.state.iter() {
            let id = entry.key().clone();
            failure_counts.insert(id.clone(), entry.value().consecutive_failures);

            match entry.value().status {
                HealthStatus::Healthy => healthy.push(id),
                HealthStatus::Degraded => degraded.push(id),
                HealthStatus::Unhealthy => unhealthy.push(id),
                HealthStatus::Unknown => degraded.push(id),
            }
        }

        HealthSnapshot {
            healthy,
            degraded,
            unhealthy,
            failure_counts,
        }
    }

    pub fn start(self: Arc<Self>) -> HealthCheckHandle {
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let monitor = self.clone();
        let interval_secs = monitor.check_interval.as_secs();

        info!(
            interval_secs = interval_secs,
            origin_count = monitor.origins.len(),
            "Health monitor starting"
        );

        tokio::spawn(
            async move {
                let mut interval = tokio::time::interval(monitor.check_interval);

                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            monitor.check_all().await;
                        }
                        _ = stop_rx.recv() => {
                            info!("Health monitor stopping");
                            break;
                        }
                    }
                }
            }
            .instrument(info_span!("health_monitor")),
        );

        HealthCheckHandle { stop_tx }
    }

    async fn check_all(&self) {
        let origins: Vec<_> = self
            .origins
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();

        for (id, origin) in origins {
            self.check_one(&id, &origin).await;
        }
    }

    async fn check_one(&self, id: &OriginId, origin: &Arc<dyn Origin>) {
        let start = Instant::now();
        let status = origin.health().await;
        let latency_ms = start.elapsed().as_millis() as u64;

        let threshold = self.threshold_for(origin.origin_type());
        let prev_healthy = self
            .state
            .get(id)
            .map(|s| s.status == HealthStatus::Healthy)
            .unwrap_or(false);

        let mut state = self.state.entry(id.clone()).or_default();

        match status {
            HealthStatus::Healthy => {
                if state.status != HealthStatus::Healthy {
                    info!(
                        origin_id = %id,
                        previous_status = ?state.status,
                        duration_ms = latency_ms,
                        "Origin health state transition to healthy"
                    );
                }
                state.status = HealthStatus::Healthy;
                state.consecutive_failures = 0;
            }
            HealthStatus::Degraded => {
                if state.status != HealthStatus::Degraded {
                    info!(
                        origin_id = %id,
                        previous_status = ?state.status,
                        duration_ms = latency_ms,
                        "Origin health state transition to degraded"
                    );
                }
                state.status = HealthStatus::Degraded;
            }
            HealthStatus::Unhealthy => {
                state.consecutive_failures += 1;
                if state.consecutive_failures >= threshold {
                    if state.status != HealthStatus::Unhealthy {
                        info!(
                            origin_id = %id,
                            previous_status = ?state.status,
                            consecutive_failures = state.consecutive_failures,
                            threshold = threshold,
                            duration_ms = latency_ms,
                            "Origin health state transition to unhealthy"
                        );
                    }
                    state.status = HealthStatus::Unhealthy;
                } else {
                    debug!(
                        origin_id = %id,
                        consecutive_failures = state.consecutive_failures,
                        threshold = threshold,
                        "Origin health check failed"
                    );
                    state.status = HealthStatus::Degraded;
                }
            }
            HealthStatus::Unknown => {
                state.status = HealthStatus::Unknown;
            }
        }

        state.last_check = Instant::now();
        state.last_latency_ms = Some(latency_ms);

        let now_healthy = state.status == HealthStatus::Healthy;
        if prev_healthy != now_healthy {
            if let Some(bus) = &self.event_bus {
                bus.publish(Event::OriginHealthChanged {
                    origin_id: id.clone(),
                    healthy: now_healthy,
                });
            }
        }
    }

    pub async fn check_now(&self, id: &OriginId) {
        if let Some(origin) = self.origins.get(id) {
            self.check_one(id, &origin.clone()).await;
        }
    }

    pub fn get_state(&self, id: &OriginId) -> Option<OriginHealthState> {
        self.state.get(id).map(|e| e.value().clone())
    }
}

pub struct HealthCheckHandle {
    stop_tx: mpsc::Sender<()>,
}

impl HealthCheckHandle {
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalOrigin;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_health_monitor_basic() {
        let monitor = HealthMonitor::new(Duration::from_secs(30));

        let dir = TempDir::new().unwrap();
        let origin = Arc::new(LocalOrigin::new("test", dir.path()));

        monitor.add_origin(origin);

        let snapshot = monitor.snapshot();
        assert!(!snapshot.is_healthy(&OriginId::from("test")));
    }

    #[tokio::test]
    async fn test_health_check() {
        let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)));

        let dir = TempDir::new().unwrap();
        let origin = Arc::new(LocalOrigin::new("test", dir.path()));

        monitor.add_origin(origin);
        monitor.check_now(&OriginId::from("test")).await;

        let snapshot = monitor.snapshot();
        assert!(snapshot.is_healthy(&OriginId::from("test")));
    }

    #[tokio::test]
    async fn test_failure_tracking() {
        let mut thresholds = HashMap::new();
        thresholds.insert(OriginType::Local, 3);

        let monitor = HealthMonitor::new(Duration::from_secs(30))
            .with_per_type_thresholds(thresholds);

        let origin = Arc::new(LocalOrigin::new("missing", std::path::Path::new("/nonexistent")));
        monitor.add_origin(origin);

        monitor.check_now(&OriginId::from("missing")).await;
        let state = monitor.get_state(&OriginId::from("missing")).unwrap();
        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(state.status, HealthStatus::Degraded);

        monitor.check_now(&OriginId::from("missing")).await;
        monitor.check_now(&OriginId::from("missing")).await;

        let state = monitor.get_state(&OriginId::from("missing")).unwrap();
        assert_eq!(state.consecutive_failures, 3);
        assert_eq!(state.status, HealthStatus::Unhealthy);
    }

    #[tokio::test]
    async fn test_local_origin_threshold_is_one() {
        let monitor = HealthMonitor::new(Duration::from_secs(30));

        let origin = Arc::new(LocalOrigin::new("missing", std::path::Path::new("/nonexistent")));
        monitor.add_origin(origin);

        monitor.check_now(&OriginId::from("missing")).await;
        let state = monitor.get_state(&OriginId::from("missing")).unwrap();
        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(state.status, HealthStatus::Unhealthy);
    }

    #[test]
    fn test_snapshot_all_unhealthy() {
        let snapshot = HealthSnapshot {
            healthy: Vec::new(),
            degraded: Vec::new(),
            unhealthy: vec![OriginId::from("a")],
            failure_counts: HashMap::new(),
        };
        assert!(snapshot.all_unhealthy());
    }
}
