use crate::types::{OriginId, VirtualPath};
use tokio::sync::broadcast;

pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn publish(&self, event: Event) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    FileAdded {
        path: VirtualPath,
        origin_id: OriginId,
    },
    FileRemoved {
        path: VirtualPath,
    },
    FileModified {
        path: VirtualPath,
    },
    FileAccessed {
        path: VirtualPath,
        origin_id: OriginId,
        offset: u64,
        size: u32,
    },
    OriginConnected {
        origin_id: OriginId,
    },
    OriginDisconnected {
        origin_id: OriginId,
    },
    SyncStarted {
        origin_id: OriginId,
    },
    SyncCompleted {
        origin_id: OriginId,
        files_changed: u64,
    },
    CacheEviction {
        bytes_freed: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        bus.publish(Event::SyncStarted {
            origin_id: OriginId::from("test"),
        });

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, Event::SyncStarted { .. }));
    }

    #[tokio::test]
    async fn test_event_bus_multiple_subscribers() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.publish(Event::CacheEviction { bytes_freed: 1024 });

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();

        assert!(matches!(e1, Event::CacheEviction { bytes_freed: 1024 }));
        assert!(matches!(e2, Event::CacheEviction { bytes_freed: 1024 }));
    }
}
