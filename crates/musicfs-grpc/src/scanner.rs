use musicfs_cache::{Database, VirtualTree};
use musicfs_cas::ContentFetcher;
use musicfs_core::{AudioMeta, Error, Event, EventBus, FileId, FileMeta, OriginId, RealPath, Result, VirtualPath};
use musicfs_metadata::MetadataParser;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub struct ScanResult {
    pub new_files: Vec<SyncedFileInfo>,
    pub changed: u32,
    pub deleted: u32,
    pub unchanged: u32,
    pub bytes_synced: u64,
}

pub struct SyncedFileInfo {
    pub path: String,
    pub file_id: FileId,
    pub virtual_path: String,
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub phase: String,
    pub current: u32,
    pub total: u32,
    pub current_path: String,
    pub bytes_synced: u64,
}

pub struct OriginScanner {
    db: Arc<Database>,
    event_bus: Arc<EventBus>,
    tree: Arc<RwLock<VirtualTree>>,
    fetcher: Arc<ContentFetcher>,
    parser: MetadataParser,
}

impl OriginScanner {
    pub fn new(
        db: Arc<Database>,
        event_bus: Arc<EventBus>,
        tree: Arc<RwLock<VirtualTree>>,
        fetcher: Arc<ContentFetcher>,
    ) -> Self {
        Self {
            db,
            event_bus,
            tree,
            fetcher,
            parser: MetadataParser,
        }
    }

    pub async fn scan(
        &self,
        origin_id: &OriginId,
        origin_root: &Path,
        subdir: Option<&str>,
        progress_tx: mpsc::Sender<ScanProgress>,
    ) -> Result<ScanResult> {
        let scan_root = match subdir {
            Some(sub) if !sub.is_empty() => origin_root.join(sub),
            _ => origin_root.to_path_buf(),
        };

        if !scan_root.exists() {
            return Err(Error::Origin(format!(
                "scan path does not exist: {}",
                scan_root.display()
            )));
        }

        // Phase 1: Scanning
        let audio_files = self.collect_audio_files(&scan_root, &progress_tx)?;
        let total_files = audio_files.len() as u32;
        info!(files = total_files, "scan phase complete");

        // Phase 2: Hashing + categorization
        let mut new_files = Vec::new();
        let mut unchanged = 0u32;

        for (i, abs_path) in audio_files.iter().enumerate() {
            let _ = progress_tx.try_send(ScanProgress {
                phase: "hashing".to_string(),
                current: i as u32 + 1,
                total: total_files,
                current_path: abs_path.display().to_string(),
                bytes_synced: 0,
            });

            let rel_path = abs_path.strip_prefix(origin_root).unwrap_or(abs_path);

            let existing = self.db.get_file_by_real_path(origin_id, rel_path)?;
            if existing.is_some() {
                unchanged += 1;
                continue;
            }

            let size = std::fs::metadata(abs_path)
                .map(|m| m.len())
                .unwrap_or(0);

            new_files.push(DiscoveredFile {
                abs_path: abs_path.clone(),
                rel_path: rel_path.to_path_buf(),
                size,
            });
        }

        info!(
            new = new_files.len(),
            unchanged = unchanged,
            "hash phase complete"
        );

        // Phase 3: Indexing
        let mut synced = Vec::new();
        let mut bytes_synced = 0u64;
        let ingest_total = new_files.len() as u32;

        for (i, file) in new_files.iter().enumerate() {
            let _ = progress_tx.try_send(ScanProgress {
                phase: "indexing".to_string(),
                current: i as u32 + 1,
                total: ingest_total,
                current_path: file.abs_path.display().to_string(),
                bytes_synced,
            });

            let audio_meta = match self.parser.parse_file(&file.abs_path) {
                Ok(meta) => meta,
                Err(e) => {
                    warn!(path = %file.abs_path.display(), error = %e, "parse failed, using defaults");
                    AudioMeta::default()
                }
            };

            let virtual_path = derive_virtual_path(&audio_meta, &file.rel_path);

            let file_id = self.db.upsert_file(
                origin_id,
                &file.rel_path,
                &virtual_path,
                &audio_meta,
                UNIX_EPOCH,
                file.size,
            )?;

            let file_meta = FileMeta {
                id: file_id,
                virtual_path: virtual_path.clone(),
                real_path: RealPath {
                    origin_id: origin_id.clone(),
                    path: file.rel_path.clone(),
                },
                size: file.size,
                mtime: UNIX_EPOCH,
                content_hash: None,
                audio: Some(audio_meta),
            };

            {
                let mut tree = self.tree.write();
                tree.insert_file(&file_meta);
            }

            self.fetcher.register_file(file_meta.clone());

            self.event_bus.publish(Event::FileAdded {
                path: virtual_path.clone(),
                origin_id: origin_id.clone(),
            });

            bytes_synced += file.size;

            synced.push(SyncedFileInfo {
                path: file.abs_path.display().to_string(),
                file_id,
                virtual_path: virtual_path.as_str().to_string(),
            });
        }

        Ok(ScanResult {
            new_files: synced,
            changed: 0,
            deleted: 0,
            unchanged,
            bytes_synced,
        })
    }

    fn collect_audio_files(
        &self,
        scan_root: &Path,
        progress_tx: &mpsc::Sender<ScanProgress>,
    ) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        self.walk_dir(scan_root, &mut files, progress_tx)?;
        Ok(files)
    }

    fn walk_dir(
        &self,
        dir: &Path,
        files: &mut Vec<PathBuf>,
        progress_tx: &mpsc::Sender<ScanProgress>,
    ) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| Error::Origin(format!("read_dir {}: {}", dir.display(), e)))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.walk_dir(&path, files, progress_tx)?;
            } else if is_audio_file(&path) {
                files.push(path.clone());
                let _ = progress_tx.try_send(ScanProgress {
                    phase: "scanning".to_string(),
                    current: files.len() as u32,
                    total: 0,
                    current_path: path.display().to_string(),
                    bytes_synced: 0,
                });
            }
        }

        Ok(())
    }
}

fn derive_virtual_path(meta: &AudioMeta, rel_path: &Path) -> VirtualPath {
    let artist = meta.artist.as_deref().unwrap_or("Unknown Artist");
    let album = meta.album.as_deref().unwrap_or("Unknown Album");
    let filename = rel_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    VirtualPath::new(format!("/{}/{}/{}", artist, album, filename))
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

struct DiscoveredFile {
    abs_path: PathBuf,
    rel_path: PathBuf,
    size: u64,
}
