use crate::cdc::CdcChunker;
use musicfs_core::{ChunkHash, FileId, FileMeta, OriginId};
use musicfs_origins::Origin;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;
use tracing::{debug, info, trace};

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub origin_id: OriginId,
    pub size: u64,
    pub mtime: SystemTime,
}

#[derive(Debug, Default)]
pub struct ChangeSet {
    pub added: Vec<ScannedFile>,
    pub removed: Vec<FileId>,
    pub modified: Vec<(FileId, ManifestDiff)>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }
}

#[derive(Debug, Clone)]
pub struct ManifestChunk {
    pub hash: ChunkHash,
    pub offset: u64,
    pub size: u32,
}

#[derive(Debug)]
pub struct ManifestDiff {
    pub reuse: Vec<ManifestChunk>,
    pub fetch: Vec<ManifestChunk>,
    pub orphaned: Vec<ChunkHash>,
}

pub struct DeltaDetector {
    chunker: CdcChunker,
}

impl DeltaDetector {
    pub fn new() -> Self {
        Self {
            chunker: CdcChunker::default(),
        }
    }

    pub fn with_chunker(chunker: CdcChunker) -> Self {
        Self { chunker }
    }

    pub async fn detect_changes(
        &self,
        origin: &dyn Origin,
        cached: &HashMap<FileId, FileMeta>,
        manifests: &HashMap<FileId, Vec<ManifestChunk>>,
    ) -> Result<ChangeSet, DeltaError> {
        let origin_id = origin.id().clone();
        info!(origin_id = %origin_id, "Starting delta detection");
        
        let mut changes = ChangeSet::default();

        let origin_files = self.scan_origin(origin).await?;
        trace!(origin_id = %origin_id, scanned_count = origin_files.len(), "Completed origin scan");

        let cached_by_path: HashMap<_, _> = cached
            .values()
            .map(|m| (m.real_path.path.clone(), m))
            .collect();

        for scanned in &origin_files {
            if let Some(cached_file) = cached_by_path.get(&scanned.path) {
                if self.is_modified_scan(cached_file, scanned) {
                    debug!(origin_id = %origin_id, path = ?scanned.path, "File modified");

                    if let Some(old_chunks) = manifests.get(&cached_file.id) {
                        let new_chunks = self.compute_chunks_for_scan(origin, scanned).await?;
                        let diff = self.compute_diff(old_chunks, &new_chunks);
                        changes.modified.push((cached_file.id, diff));
                    }
                }
            } else {
                debug!(origin_id = %origin_id, path = ?scanned.path, "File added");
                changes.added.push(scanned.clone());
            }
        }

        let origin_paths: HashSet<_> = origin_files.iter().map(|f| &f.path).collect();

        for cached_file in cached.values() {
            if !origin_paths.contains(&cached_file.real_path.path) {
                debug!(origin_id = %origin_id, path = ?cached_file.real_path.path, "File removed");
                changes.removed.push(cached_file.id);
            }
        }

        info!(
            origin_id = %origin_id,
            files_added = changes.added.len(),
            files_removed = changes.removed.len(),
            files_modified = changes.modified.len(),
            "Delta detection complete"
        );

        Ok(changes)
    }

    fn is_modified_scan(&self, cached: &FileMeta, scanned: &ScannedFile) -> bool {
        cached.size != scanned.size || cached.mtime != scanned.mtime
    }

    async fn scan_origin(&self, origin: &dyn Origin) -> Result<Vec<ScannedFile>, DeltaError> {
        let mut files = Vec::new();
        let mut dirs_to_scan = vec![PathBuf::from("/")];

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = origin
                .readdir(&dir)
                .await
                .map_err(|e| DeltaError::OriginScan(e.to_string()))?;

            for entry in entries {
                let entry_path = dir.join(&entry.name);

                if entry.is_dir {
                    dirs_to_scan.push(entry_path);
                } else if Self::is_audio_file(&entry.name) {
                    let stat = origin
                        .stat(&entry_path)
                        .await
                        .map_err(|e| DeltaError::OriginScan(e.to_string()))?;

                    files.push(ScannedFile {
                        path: entry_path,
                        origin_id: origin.id().clone(),
                        size: stat.size,
                        mtime: stat.mtime,
                    });
                }
            }
        }

        Ok(files)
    }

    fn is_audio_file(name: &str) -> bool {
        let lower = name.to_lowercase();
        lower.ends_with(".flac")
            || lower.ends_with(".mp3")
            || lower.ends_with(".ogg")
            || lower.ends_with(".wav")
            || lower.ends_with(".m4a")
            || lower.ends_with(".aac")
            || lower.ends_with(".opus")
    }

    async fn compute_chunks_for_scan(
        &self,
        origin: &dyn Origin,
        scanned: &ScannedFile,
    ) -> Result<Vec<ManifestChunk>, DeltaError> {
        let data = origin
            .read_full(&scanned.path)
            .await
            .map_err(|e| DeltaError::OriginRead(e.to_string()))?;

        let chunks = self.chunker.chunk_refs(&data);

        Ok(chunks
            .into_iter()
            .map(|c| ManifestChunk {
                hash: c.hash,
                offset: c.offset,
                size: c.length,
            })
            .collect())
    }

    fn compute_diff(&self, old_chunks: &[ManifestChunk], new_chunks: &[ManifestChunk]) -> ManifestDiff {
        let old_hashes: HashSet<_> = old_chunks.iter().map(|c| c.hash).collect();
        let new_hashes: HashSet<_> = new_chunks.iter().map(|c| c.hash).collect();

        ManifestDiff {
            reuse: new_chunks
                .iter()
                .filter(|c| old_hashes.contains(&c.hash))
                .cloned()
                .collect(),
            fetch: new_chunks
                .iter()
                .filter(|c| !old_hashes.contains(&c.hash))
                .cloned()
                .collect(),
            orphaned: old_chunks
                .iter()
                .filter(|c| !new_hashes.contains(&c.hash))
                .map(|c| c.hash)
                .collect(),
        }
    }
}

impl Default for DeltaDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeltaError {
    #[error("Origin read error: {0}")]
    OriginRead(String),

    #[error("Origin scan error: {0}")]
    OriginScan(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::{OriginId, RealPath, VirtualPath};
    use std::time::SystemTime;

    fn make_file_meta(id: i64, path: &str, size: u64) -> FileMeta {
        FileMeta {
            id: FileId(id),
            virtual_path: VirtualPath::new(format!("/test/{}", path)),
            real_path: RealPath {
                origin_id: OriginId::from("test"),
                path: PathBuf::from(path),
            },
            size,
            mtime: SystemTime::UNIX_EPOCH,
            content_hash: None,
            audio: None,
        }
    }

    fn make_scanned_file(path: &str, size: u64) -> ScannedFile {
        ScannedFile {
            path: PathBuf::from(path),
            origin_id: OriginId::from("test"),
            size,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_is_modified_size_change() {
        let detector = DeltaDetector::new();

        let cached = make_file_meta(1, "test.flac", 1000);
        let scanned = make_scanned_file("test.flac", 2000);

        assert!(detector.is_modified_scan(&cached, &scanned));
    }

    #[test]
    fn test_is_modified_same() {
        let detector = DeltaDetector::new();

        let cached = make_file_meta(1, "test.flac", 1000);
        let scanned = make_scanned_file("test.flac", 1000);

        assert!(!detector.is_modified_scan(&cached, &scanned));
    }

    #[test]
    fn test_is_audio_file() {
        assert!(DeltaDetector::is_audio_file("track.flac"));
        assert!(DeltaDetector::is_audio_file("song.MP3"));
        assert!(DeltaDetector::is_audio_file("audio.ogg"));
        assert!(!DeltaDetector::is_audio_file("readme.txt"));
        assert!(!DeltaDetector::is_audio_file("cover.jpg"));
    }

    #[test]
    fn test_compute_diff() {
        let detector = DeltaDetector::new();

        let old_chunks = vec![
            ManifestChunk {
                hash: ChunkHash::from_bytes(b"A"),
                offset: 0,
                size: 256,
            },
            ManifestChunk {
                hash: ChunkHash::from_bytes(b"B"),
                offset: 256,
                size: 256,
            },
            ManifestChunk {
                hash: ChunkHash::from_bytes(b"C"),
                offset: 512,
                size: 256,
            },
        ];

        let new_chunks = vec![
            ManifestChunk {
                hash: ChunkHash::from_bytes(b"A"),
                offset: 0,
                size: 256,
            },
            ManifestChunk {
                hash: ChunkHash::from_bytes(b"D"),
                offset: 256,
                size: 256,
            },
            ManifestChunk {
                hash: ChunkHash::from_bytes(b"C"),
                offset: 512,
                size: 256,
            },
        ];

        let diff = detector.compute_diff(&old_chunks, &new_chunks);

        assert_eq!(diff.reuse.len(), 2);
        assert_eq!(diff.fetch.len(), 1);
        assert_eq!(diff.orphaned.len(), 1);
    }
}
