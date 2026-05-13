use fuser::{FileAttr, FileType, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry};
use musicfs_cache::{PatternStore, PrefetchConfig, PrefetchEngine};
use musicfs_cas::ContentFetcher;
use musicfs_core::{EventBus, FileId};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const PREFETCH_DIR_INODE: u64 = 0xFFFF_FFFF_0000_0002;
const PREFETCH_STATUS_INODE: u64 = 0xFFFF_FFFF_0000_0003;
const PREFETCH_HINTS_BASE: u64 = 0xFFFF_FFFF_2000_0000;

pub struct PrefetchOps {
    pattern_store: Arc<PatternStore>,
    engine: Option<Arc<PrefetchEngine>>,
    uid: u32,
    gid: u32,
}

impl PrefetchOps {
    pub fn new(pattern_store: Arc<PatternStore>, uid: u32, gid: u32) -> Self {
        Self {
            pattern_store,
            engine: None,
            uid,
            gid,
        }
    }

    pub fn with_engine(
        pattern_store: Arc<PatternStore>,
        fetcher: Arc<ContentFetcher>,
        config: PrefetchConfig,
        uid: u32,
        gid: u32,
    ) -> Self {
        let engine = Arc::new(PrefetchEngine::new(config, pattern_store.clone(), fetcher));

        Self {
            pattern_store,
            engine: Some(engine),
            uid,
            gid,
        }
    }

    pub fn start_engine(
        &self,
        event_bus: Arc<EventBus>,
    ) -> Option<musicfs_cache::PrefetchHandle> {
        self.engine
            .as_ref()
            .map(|e| e.clone().start(event_bus, self.pattern_store.clone()))
    }

    pub fn is_prefetch_dir_name(name: &str) -> bool {
        name == ".prefetch"
    }

    pub fn is_prefetch_inode(inode: u64) -> bool {
        inode == PREFETCH_DIR_INODE
            || inode == PREFETCH_STATUS_INODE
            || inode >= PREFETCH_HINTS_BASE
    }

    pub fn prefetch_dir_inode() -> u64 {
        PREFETCH_DIR_INODE
    }

    pub fn lookup_prefetch_dir(&self, reply: ReplyEntry) {
        let attr = self.dir_attr(PREFETCH_DIR_INODE);
        reply.entry(&Duration::from_secs(60), &attr, 0);
    }

    pub fn lookup_status(&self, reply: ReplyEntry) {
        let status = self.generate_status();
        let attr = self.file_attr(PREFETCH_STATUS_INODE, status.len() as u64);
        reply.entry(&Duration::from_secs(1), &attr, 0);
    }

    pub fn lookup_hint(&self, name: &str, reply: ReplyEntry) {
        if let Some(inode) = self.hint_name_to_inode(name) {
            let attr = self.file_attr(inode, 256);
            reply.entry(&Duration::from_secs(1), &attr, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    pub fn getattr_prefetch_dir(&self, reply: ReplyAttr) {
        let attr = self.dir_attr(PREFETCH_DIR_INODE);
        reply.attr(&Duration::from_secs(60), &attr);
    }

    pub fn getattr_status(&self, reply: ReplyAttr) {
        let status = self.generate_status();
        let attr = self.file_attr(PREFETCH_STATUS_INODE, status.len() as u64);
        reply.attr(&Duration::from_secs(1), &attr);
    }

    pub fn getattr_hint(&self, inode: u64, reply: ReplyAttr) {
        let attr = self.file_attr(inode, 256);
        reply.attr(&Duration::from_secs(1), &attr);
    }

    pub fn readdir_prefetch_root(&self, offset: i64, mut reply: ReplyDirectory) {
        let entries: Vec<(u64, FileType, &str)> = vec![
            (PREFETCH_DIR_INODE, FileType::Directory, "."),
            (1, FileType::Directory, ".."),
            (PREFETCH_STATUS_INODE, FileType::RegularFile, "status"),
        ];

        let recently_played = self.pattern_store.recently_played(7).unwrap_or_default();
        let predictions: Vec<(u64, FileType, String)> = recently_played
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, file_id)| {
                let inode = PREFETCH_HINTS_BASE + i as u64;
                let name = format!("hint_{:04}", file_id.0);
                (inode, FileType::RegularFile, name)
            })
            .collect();

        for (i, (inode, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*inode, (i + 1) as i64, *kind, *name) {
                reply.ok();
                return;
            }
        }

        let base_offset = entries.len();
        for (i, (inode, kind, name)) in predictions.iter().enumerate() {
            let entry_offset = base_offset + i;
            if entry_offset < offset as usize {
                continue;
            }
            if reply.add(*inode, (entry_offset + 1) as i64, *kind, name) {
                break;
            }
        }

        reply.ok();
    }

    pub fn read_status(&self, offset: i64, size: u32, reply: ReplyData) {
        let status = self.generate_status();
        let start = offset as usize;
        let end = std::cmp::min(start + size as usize, status.len());

        if start >= status.len() {
            reply.data(&[]);
        } else {
            reply.data(&status.as_bytes()[start..end]);
        }
    }

    pub fn read_hint(&self, inode: u64, offset: i64, size: u32, reply: ReplyData) {
        let file_id = self.inode_to_file_id(inode);
        let predictions = self.pattern_store.predict_next(file_id, 5);

        let content = predictions
            .iter()
            .map(|id| format!("{}", id.0))
            .collect::<Vec<_>>()
            .join("\n");

        let start = offset as usize;
        let end = std::cmp::min(start + size as usize, content.len());

        if start >= content.len() {
            reply.data(&[]);
        } else {
            reply.data(&content.as_bytes()[start..end]);
        }
    }

    fn generate_status(&self) -> String {
        let engine_status = if let Some(engine) = &self.engine {
            format!(
                "running: {}\nin_flight: {}",
                engine.is_running(),
                engine.in_flight_count()
            )
        } else {
            "engine: disabled".to_string()
        };

        let most_played = self
            .pattern_store
            .most_played(5)
            .unwrap_or_default()
            .iter()
            .map(|id| format!("{}", id.0))
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            "MusicFS Prefetch Status\n\
             =======================\n\
             {}\n\
             most_played: [{}]\n",
            engine_status, most_played
        )
    }

    fn hint_name_to_inode(&self, name: &str) -> Option<u64> {
        if name.starts_with("hint_") {
            let id_str = name.strip_prefix("hint_")?;
            let id: i64 = id_str.parse().ok()?;
            Some(PREFETCH_HINTS_BASE + id as u64)
        } else {
            None
        }
    }

    fn inode_to_file_id(&self, inode: u64) -> FileId {
        FileId((inode - PREFETCH_HINTS_BASE) as i64)
    }

    fn dir_attr(&self, inode: u64) -> FileAttr {
        FileAttr {
            ino: inode,
            size: 0,
            blocks: 0,
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 2,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    fn file_attr(&self, inode: u64, size: u64) -> FileAttr {
        FileAttr {
            ino: inode,
            size,
            blocks: (size + 511) / 512,
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_prefetch_ops_new() {
        let dir = TempDir::new().unwrap();
        let pattern_store = Arc::new(PatternStore::new(&dir.path().join("patterns.db"), 30).unwrap());
        let _ops = PrefetchOps::new(pattern_store, 1000, 1000);
    }

    #[test]
    fn test_is_prefetch_inode() {
        assert!(PrefetchOps::is_prefetch_inode(PREFETCH_DIR_INODE));
        assert!(PrefetchOps::is_prefetch_inode(PREFETCH_STATUS_INODE));
        assert!(PrefetchOps::is_prefetch_inode(PREFETCH_HINTS_BASE));
        assert!(PrefetchOps::is_prefetch_inode(PREFETCH_HINTS_BASE + 100));
        assert!(!PrefetchOps::is_prefetch_inode(1));
        assert!(!PrefetchOps::is_prefetch_inode(1000));
    }

    #[test]
    fn test_hint_name_to_inode() {
        let dir = TempDir::new().unwrap();
        let pattern_store = Arc::new(PatternStore::new(&dir.path().join("patterns.db"), 30).unwrap());
        let ops = PrefetchOps::new(pattern_store, 1000, 1000);

        assert_eq!(ops.hint_name_to_inode("hint_0001"), Some(PREFETCH_HINTS_BASE + 1));
        assert_eq!(ops.hint_name_to_inode("hint_9999"), Some(PREFETCH_HINTS_BASE + 9999));
        assert_eq!(ops.hint_name_to_inode("invalid"), None);
    }
}
