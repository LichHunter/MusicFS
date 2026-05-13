use crate::health::HealthSnapshot;
use dashmap::DashMap;
use musicfs_core::{Event, EventBus, OriginId};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, trace, warn};

pub struct Router {
    priorities: DashMap<OriginId, u8>,
    latency_stats: DashMap<OriginId, LatencyStats>,
    event_bus: Option<Arc<EventBus>>,
}

#[derive(Debug, Clone, Default)]
pub struct LatencyStats {
    pub samples: Vec<u64>,
    pub p50_ms: u64,
    pub p99_ms: u64,
    pub last_update: Option<Instant>,
}

impl LatencyStats {
    pub fn record(&mut self, latency_ms: u64) {
        self.samples.push(latency_ms);

        if self.samples.len() > 100 {
            self.samples.remove(0);
        }

        if !self.samples.is_empty() {
            let mut sorted = self.samples.clone();
            sorted.sort_unstable();

            let p50_idx = sorted.len() / 2;
            let p99_idx = (sorted.len() * 99) / 100;

            self.p50_ms = sorted[p50_idx];
            self.p99_ms = sorted.get(p99_idx).copied().unwrap_or(self.p50_ms);
        }

        self.last_update = Some(Instant::now());
    }
}

impl Router {
    pub fn new() -> Self {
        Self {
            priorities: DashMap::new(),
            latency_stats: DashMap::new(),
            event_bus: None,
        }
    }

    pub fn with_event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.event_bus = Some(bus);
        self
    }

    pub fn set_priority(&self, id: OriginId, priority: u8) {
        self.priorities.insert(id, priority);
    }

    pub fn remove_priority(&self, id: &OriginId) {
        self.priorities.remove(id);
        self.latency_stats.remove(id);
    }

    pub fn get_priority(&self, id: &OriginId) -> u8 {
        self.priorities.get(id).map(|p| *p).unwrap_or(100)
    }

    pub fn record_latency(&self, id: &OriginId, latency_ms: u64) {
        self.latency_stats
            .entry(id.clone())
            .or_default()
            .record(latency_ms);
    }

    pub fn select(&self, candidates: &[OriginId], health: &HealthSnapshot) -> Option<OriginId> {
        let selected = candidates
            .iter()
            .filter(|id| health.is_healthy(id))
            .min_by_key(|id| {
                let priority = self.get_priority(id);
                let latency = self.latency_stats.get(*id).map(|s| s.p50_ms).unwrap_or(0);
                (priority, latency)
            })
            .cloned();

        if let Some(ref id) = selected {
            let priority = self.get_priority(id);
            let latency = self.latency_stats.get(id).map(|s| s.p50_ms).unwrap_or(0);
            trace!(
                origin_id = %id,
                priority = priority,
                latency_ms = latency,
                "Selected healthy origin"
            );
        }

        selected
    }

    pub fn select_with_fallback(
        &self,
        candidates: &[OriginId],
        health: &HealthSnapshot,
    ) -> Option<OriginId> {
        if let Some(id) = self.select(candidates, health) {
            return Some(id);
        }

        debug!("No healthy origins, trying degraded");
        if let Some(id) = candidates
            .iter()
            .filter(|id| health.is_degraded(id))
            .min_by_key(|id| self.get_priority(id))
            .cloned()
        {
            trace!(
                origin_id = %id,
                priority = self.get_priority(&id),
                "Selected degraded origin as fallback"
            );
            return Some(id);
        }

        warn!("All origins unhealthy, selecting least-bad by failure count");

        if let Some(bus) = &self.event_bus {
            bus.publish(Event::AllOriginsUnhealthy {
                candidate_count: candidates.len(),
            });
        }

        let selected = candidates
            .iter()
            .min_by_key(|id| {
                let failures = health.failure_count(id).unwrap_or(u32::MAX);
                let priority = self.get_priority(id);
                (failures, priority)
            })
            .cloned();

        if let Some(ref id) = selected {
            let failures = health.failure_count(id).unwrap_or(u32::MAX);
            trace!(
                origin_id = %id,
                failure_count = failures,
                priority = self.get_priority(id),
                "Selected least-bad unhealthy origin"
            );
        }

        selected
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mock_health(healthy: &[&str], degraded: &[&str]) -> HealthSnapshot {
        HealthSnapshot {
            healthy: healthy.iter().map(|s| OriginId::from(*s)).collect(),
            degraded: degraded.iter().map(|s| OriginId::from(*s)).collect(),
            unhealthy: Vec::new(),
            failure_counts: HashMap::new(),
        }
    }

    #[test]
    fn test_select_by_priority() {
        let router = Router::new();
        router.set_priority(OriginId::from("high"), 1);
        router.set_priority(OriginId::from("low"), 2);

        let candidates = vec![OriginId::from("low"), OriginId::from("high")];
        let health = mock_health(&["high", "low"], &[]);

        let selected = router.select(&candidates, &health);
        assert_eq!(selected, Some(OriginId::from("high")));
    }

    #[test]
    fn test_select_skips_unhealthy() {
        let router = Router::new();
        router.set_priority(OriginId::from("high"), 1);
        router.set_priority(OriginId::from("low"), 2);

        let candidates = vec![OriginId::from("high"), OriginId::from("low")];
        let health = mock_health(&["low"], &[]);

        let selected = router.select(&candidates, &health);
        assert_eq!(selected, Some(OriginId::from("low")));
    }

    #[test]
    fn test_latency_affects_tiebreak() {
        let router = Router::new();
        router.set_priority(OriginId::from("a"), 1);
        router.set_priority(OriginId::from("b"), 1);

        router.record_latency(&OriginId::from("a"), 100);
        router.record_latency(&OriginId::from("b"), 10);

        let candidates = vec![OriginId::from("a"), OriginId::from("b")];
        let health = mock_health(&["a", "b"], &[]);

        let selected = router.select(&candidates, &health);
        assert_eq!(selected, Some(OriginId::from("b")));
    }

    #[test]
    fn test_fallback_to_degraded() {
        let router = Router::new();
        router.set_priority(OriginId::from("a"), 1);
        router.set_priority(OriginId::from("b"), 2);

        let candidates = vec![OriginId::from("a"), OriginId::from("b")];
        let health = mock_health(&[], &["b"]);

        let selected = router.select_with_fallback(&candidates, &health);
        assert_eq!(selected, Some(OriginId::from("b")));
    }

    #[test]
    fn test_fallback_least_bad() {
        let router = Router::new();
        router.set_priority(OriginId::from("a"), 1);
        router.set_priority(OriginId::from("b"), 2);

        let candidates = vec![OriginId::from("a"), OriginId::from("b")];
        let mut failure_counts = HashMap::new();
        failure_counts.insert(OriginId::from("a"), 5);
        failure_counts.insert(OriginId::from("b"), 2);

        let health = HealthSnapshot {
            healthy: Vec::new(),
            degraded: Vec::new(),
            unhealthy: vec![OriginId::from("a"), OriginId::from("b")],
            failure_counts,
        };

        let selected = router.select_with_fallback(&candidates, &health);
        assert_eq!(selected, Some(OriginId::from("b")));
    }
}
