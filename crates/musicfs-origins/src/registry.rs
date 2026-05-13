use crate::health::{HealthMonitor, HealthSnapshot};
use crate::router::Router;
use crate::traits::{Origin, WatchHandle};
use musicfs_core::{OriginId, RealPath};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

pub struct OriginRegistry {
    origins: RwLock<HashMap<OriginId, Arc<dyn Origin>>>,
    router: Router,
    health_monitor: Arc<HealthMonitor>,
    watch_handles: RwLock<HashMap<OriginId, Vec<WatchHandle>>>,
}

impl OriginRegistry {
    pub fn new(health_monitor: Arc<HealthMonitor>) -> Self {
        Self {
            origins: RwLock::new(HashMap::new()),
            router: Router::new(),
            health_monitor,
            watch_handles: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, origin: Arc<dyn Origin>, priority: u8) {
        let id = origin.id().clone();
        info!("Registering origin {} with priority {}", id, priority);

        self.router.set_priority(id.clone(), priority);
        self.health_monitor.add_origin(origin.clone());
        self.origins.write().insert(id, origin);
    }

    pub fn unregister(&self, id: &OriginId) {
        info!("Unregistering origin {}", id);

        if let Some(handles) = self.watch_handles.write().remove(id) {
            info!("Dropping {} watch handles for origin {}", handles.len(), id);
        }

        self.origins.write().remove(id);
        self.router.remove_priority(id);
        self.health_monitor.remove_origin(id);
    }

    pub fn register_watch(&self, origin_id: &OriginId, handle: WatchHandle) {
        self.watch_handles
            .write()
            .entry(origin_id.clone())
            .or_default()
            .push(handle);
    }

    pub fn get(&self, id: &OriginId) -> Option<Arc<dyn Origin>> {
        self.origins.read().get(id).cloned()
    }

    pub fn list(&self) -> Vec<Arc<dyn Origin>> {
        self.origins.read().values().cloned().collect()
    }

    pub fn route(&self, path: &RealPath) -> Option<Arc<dyn Origin>> {
        let origins = self.origins.read();
        let health = self.health_monitor.snapshot();

        let candidates: Vec<_> = origins
            .iter()
            .filter(|(id, _)| self.can_serve(id, path))
            .map(|(id, origin)| (id.clone(), origin.clone()))
            .collect();

        if candidates.is_empty() {
            warn!("No origin can serve path: {:?}", path);
            return None;
        }

        let candidate_ids: Vec<_> = candidates.iter().map(|(id, _)| id.clone()).collect();
        let selected = self.router.select(&candidate_ids, &health)?;

        candidates
            .into_iter()
            .find(|(id, _)| id == &selected)
            .map(|(_, origin)| origin)
    }

    pub fn route_with_fallback(&self, path: &RealPath) -> Option<Arc<dyn Origin>> {
        let origins = self.origins.read();
        let health = self.health_monitor.snapshot();

        let candidates: Vec<_> = origins
            .iter()
            .filter(|(id, _)| self.can_serve(id, path))
            .map(|(id, origin)| (id.clone(), origin.clone()))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        let candidate_ids: Vec<_> = candidates.iter().map(|(id, _)| id.clone()).collect();
        let selected = self.router.select_with_fallback(&candidate_ids, &health)?;

        candidates
            .into_iter()
            .find(|(id, _)| id == &selected)
            .map(|(_, origin)| origin)
    }

    pub fn route_all(&self, path: &RealPath) -> Vec<Arc<dyn Origin>> {
        let origins = self.origins.read();
        let health = self.health_monitor.snapshot();

        let mut result: Vec<_> = origins
            .iter()
            .filter(|(id, _)| self.can_serve(id, path) && health.is_healthy(id))
            .map(|(_, origin)| origin.clone())
            .collect();

        result.sort_by_key(|o| self.router.get_priority(o.id()));
        result
    }

    fn can_serve(&self, origin_id: &OriginId, path: &RealPath) -> bool {
        path.origin_id == *origin_id
    }

    pub fn health(&self) -> HealthSnapshot {
        self.health_monitor.snapshot()
    }

    pub fn record_latency(&self, id: &OriginId, latency_ms: u64) {
        self.router.record_latency(id, latency_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalOrigin;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_register_and_get() {
        let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)));
        let registry = OriginRegistry::new(monitor);

        let dir = TempDir::new().unwrap();
        let origin = Arc::new(LocalOrigin::new("test", dir.path()));

        registry.register(origin.clone(), 1);

        let retrieved = registry.get(&OriginId::from("test"));
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_unregister() {
        let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)));
        let registry = OriginRegistry::new(monitor);

        let dir = TempDir::new().unwrap();
        let origin = Arc::new(LocalOrigin::new("test", dir.path()));

        registry.register(origin, 1);
        registry.unregister(&OriginId::from("test"));

        assert!(registry.get(&OriginId::from("test")).is_none());
    }

    #[tokio::test]
    async fn test_route_by_priority() {
        let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)));
        let registry = OriginRegistry::new(monitor.clone());

        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();

        let origin1 = Arc::new(LocalOrigin::new("primary", dir1.path()));
        let origin2 = Arc::new(LocalOrigin::new("backup", dir2.path()));

        registry.register(origin1, 1);
        registry.register(origin2, 2);

        monitor.check_now(&OriginId::from("primary")).await;
        monitor.check_now(&OriginId::from("backup")).await;

        let path = RealPath {
            origin_id: OriginId::from("primary"),
            path: PathBuf::from("/test.flac"),
        };

        let routed = registry.route(&path);
        assert!(routed.is_some());
        assert_eq!(routed.unwrap().id(), &OriginId::from("primary"));
    }

    #[test]
    fn test_list_origins() {
        let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)));
        let registry = OriginRegistry::new(monitor);

        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();

        registry.register(Arc::new(LocalOrigin::new("a", dir1.path())), 1);
        registry.register(Arc::new(LocalOrigin::new("b", dir2.path())), 2);

        let list = registry.list();
        assert_eq!(list.len(), 2);
    }
}
