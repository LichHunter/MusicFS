use crate::db::Database;
use musicfs_core::{AudioMeta, FileMeta, OriginId, Result, VirtualPath};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::trace;

pub struct MetadataCache {
    db: Arc<Database>,
}

impl MetadataCache {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn store(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
        virtual_path: &VirtualPath,
        audio_meta: &AudioMeta,
        origin_mtime: SystemTime,
        origin_size: u64,
    ) -> Result<()> {
        self.db.upsert_file(
            origin_id,
            real_path,
            virtual_path,
            audio_meta,
            origin_mtime,
            origin_size,
        )?;
        Ok(())
    }

    pub fn lookup(&self, path: &VirtualPath) -> Result<Option<FileMeta>> {
        let result = self.db.get_file_by_virtual_path(path)?;
        let hit = result.is_some();
        trace!(path = path.as_str(), hit, "metadata cache lookup");
        Ok(result)
    }

    pub fn is_fresh(
        &self,
        origin_id: &OriginId,
        real_path: &Path,
        current_mtime: SystemTime,
    ) -> Result<bool> {
        if let Some(cached_mtime) = self.db.get_mtime_by_real_path(origin_id, real_path)? {
            let current_secs = current_mtime
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            let cached_secs = cached_mtime
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            let hit = current_secs == cached_secs;
            trace!(path = ?real_path, hit, "metadata freshness check");
            Ok(hit)
        } else {
            trace!(path = ?real_path, hit = false, "metadata freshness check");
            Ok(false)
        }
    }

    pub fn invalidate(&self, path: &VirtualPath) -> Result<()> {
        if let Some(meta) = self.db.get_file_by_virtual_path(path)? {
            self.db.delete_file(meta.id)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::AudioFormat;

    #[test]
    fn test_metadata_cache_store_and_lookup() {
        let db = Arc::new(Database::open_memory().unwrap());
        let cache = MetadataCache::new(db);

        let origin_id = OriginId::from("local");
        let real_path = Path::new("/music/song.flac");
        let virtual_path = VirtualPath::new("/Artist/Album/Song.flac");
        let meta = AudioMeta {
            title: Some("Song".to_string()),
            artist: Some("Artist".to_string()),
            format: AudioFormat::Flac,
            ..Default::default()
        };

        cache
            .store(&origin_id, real_path, &virtual_path, &meta, UNIX_EPOCH, 5000)
            .unwrap();

        let retrieved = cache.lookup(&virtual_path).unwrap().unwrap();
        assert_eq!(
            retrieved.audio.as_ref().unwrap().title,
            Some("Song".to_string())
        );
    }

    #[test]
    fn test_metadata_cache_invalidate() {
        let db = Arc::new(Database::open_memory().unwrap());
        let cache = MetadataCache::new(db);

        let virtual_path = VirtualPath::new("/Test.flac");

        cache
            .store(
                &OriginId::from("local"),
                Path::new("/test.flac"),
                &virtual_path,
                &AudioMeta::default(),
                UNIX_EPOCH,
                100,
            )
            .unwrap();

        assert!(cache.lookup(&virtual_path).unwrap().is_some());

        cache.invalidate(&virtual_path).unwrap();

        assert!(cache.lookup(&virtual_path).unwrap().is_none());
    }
}
