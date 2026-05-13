use musicfs_core::{Event, EventBus, OriginId, VirtualPath};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info, info_span, trace, Instrument};

const DEBOUNCE_MS: u64 = 200;

pub struct OriginWatcher {
    origin_id: OriginId,
    root: PathBuf,
    event_bus: Arc<EventBus>,
}

impl OriginWatcher {
    pub fn new(origin_id: OriginId, root: PathBuf, event_bus: Arc<EventBus>) -> Self {
        Self {
            origin_id,
            root,
            event_bus,
        }
    }

    pub fn start(self) -> WatchHandle {
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

        let origin_id = self.origin_id.clone();
        let root = self.root.clone();
        let event_bus = self.event_bus.clone();

        let origin_id_str = origin_id.to_string();
        tokio::spawn(
            async move {
                if let Err(e) = Self::watch_loop(&origin_id, &root, &event_bus, &mut stop_rx).await
                {
                    error!("Watcher error: {}", e);
                }
            }
            .instrument(info_span!("file_watcher", origin_id = %origin_id_str)),
        );

        WatchHandle { stop_tx }
    }

    async fn watch_loop(
        origin_id: &OriginId,
        root: &Path,
        event_bus: &EventBus,
        stop_rx: &mut mpsc::Receiver<()>,
    ) -> Result<(), WatchError> {
        let (tx, mut rx) = mpsc::channel(100);

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            Config::default(),
        )
        .map_err(|e| WatchError::Init(e.to_string()))?;

        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| WatchError::Watch(e.to_string()))?;

        info!(origin_id = %origin_id, path = ?root, "Watcher started");

        let mut debouncer: HashMap<PathBuf, Instant> = HashMap::new();

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    Self::handle_notify_event(origin_id, root, event_bus, event, &mut debouncer);
                }
                _ = stop_rx.recv() => {
                    info!(origin_id = %origin_id, "Watcher stopped");
                    break;
                }
            }
        }

        Ok(())
    }

    fn handle_notify_event(
        origin_id: &OriginId,
        root: &Path,
        event_bus: &EventBus,
        event: notify::Event,
        debouncer: &mut HashMap<PathBuf, Instant>,
    ) {
        use notify::EventKind;

        let now = Instant::now();

        for path in event.paths {
            let relative = match path.strip_prefix(root) {
                Ok(p) => p.to_path_buf(),
                Err(_) => continue,
            };

            if !Self::is_audio_file(&path) {
                continue;
            }

            if let Some(last_seen) = debouncer.get(&relative) {
                if now.duration_since(*last_seen).as_millis() < DEBOUNCE_MS as u128 {
                    trace!(origin_id = %origin_id, path = ?relative, "Debouncing event");
                    continue;
                }
            }
            debouncer.insert(relative.clone(), now);

            let vpath = VirtualPath::new(format!("/{}", relative.display()));

            match event.kind {
                EventKind::Create(_) => {
                    trace!(origin_id = %origin_id, path = ?relative, "File created");
                    event_bus.publish(Event::FileAdded {
                        path: vpath,
                        origin_id: origin_id.clone(),
                    });
                }
                EventKind::Remove(_) => {
                    trace!(origin_id = %origin_id, path = ?relative, "File removed");
                    event_bus.publish(Event::FileRemoved {
                        path: vpath,
                        file_id: None,
                    });
                }
                EventKind::Modify(_) => {
                    trace!(origin_id = %origin_id, path = ?relative, "File modified");
                    event_bus.publish(Event::FileModified { path: vpath });
                }
                _ => {}
            }
        }
    }

    fn is_audio_file(path: &Path) -> bool {
        matches!(
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .as_deref(),
            Some("flac" | "mp3" | "ogg" | "wav" | "m4a" | "aac" | "opus")
        )
    }
}

pub struct WatchHandle {
    stop_tx: mpsc::Sender<()>,
}

impl WatchHandle {
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        trace!("WatchHandle dropped");
        let _ = self.stop_tx.try_send(());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("Failed to initialize watcher: {0}")]
    Init(String),

    #[error("Failed to watch path: {0}")]
    Watch(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_watcher_detects_create() {
        let dir = TempDir::new().unwrap();
        let event_bus = Arc::new(EventBus::default());
        let mut rx = event_bus.subscribe();

        let watcher =
            OriginWatcher::new(OriginId::from("test"), dir.path().to_path_buf(), event_bus);
        let handle = watcher.start();

        tokio::time::sleep(Duration::from_millis(100)).await;

        std::fs::write(dir.path().join("test.flac"), b"audio").unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;

        let event = rx.try_recv();
        assert!(matches!(event, Ok(Event::FileAdded { .. })));

        handle.stop().await;
    }

    #[test]
    fn test_is_audio_file() {
        assert!(OriginWatcher::is_audio_file(Path::new("/music/song.flac")));
        assert!(OriginWatcher::is_audio_file(Path::new("/music/song.MP3")));
        assert!(!OriginWatcher::is_audio_file(Path::new("/music/cover.jpg")));
        assert!(!OriginWatcher::is_audio_file(Path::new(
            "/music/readme.txt"
        )));
    }
}
