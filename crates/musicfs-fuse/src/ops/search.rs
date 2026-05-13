use fuser::{FileAttr, FileType, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry};
use moka::sync::Cache;
use musicfs_search::{SearchHit, SearchIndex};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const SEARCH_DIR_INODE: u64 = 0xFFFF_FFFF_0000_0001;
const SEARCH_RESULT_BASE: u64 = 0xFFFF_FFFF_1000_0000;
const RESULT_CACHE_MAX_ENTRIES: u64 = 1000;
const RESULT_CACHE_TTL_SECS: u64 = 300;
const INODE_CACHE_MAX_ENTRIES: u64 = 10000;
const MAX_QUERY_LENGTH: usize = 256;

pub struct SearchOps {
    index: Arc<SearchIndex>,
    result_cache: Cache<String, Vec<SearchHit>>,
    inode_to_result: Cache<u64, (String, usize)>,
    mount_point: String,
    uid: u32,
    gid: u32,
}

impl SearchOps {
    pub fn new(index: Arc<SearchIndex>, mount_point: &str, uid: u32, gid: u32) -> Self {
        let result_cache = Cache::builder()
            .max_capacity(RESULT_CACHE_MAX_ENTRIES)
            .time_to_live(Duration::from_secs(RESULT_CACHE_TTL_SECS))
            .build();

        let inode_to_result = Cache::builder()
            .max_capacity(INODE_CACHE_MAX_ENTRIES)
            .time_to_live(Duration::from_secs(RESULT_CACHE_TTL_SECS))
            .build();

        Self {
            index,
            result_cache,
            inode_to_result,
            mount_point: mount_point.to_string(),
            uid,
            gid,
        }
    }

    pub fn is_search_dir_name(name: &str) -> bool {
        name == ".search"
    }

    pub fn is_search_inode(inode: u64) -> bool {
        inode == SEARCH_DIR_INODE || inode >= SEARCH_RESULT_BASE
    }

    pub fn search_dir_inode() -> u64 {
        SEARCH_DIR_INODE
    }

    pub fn lookup_search_dir(&self, reply: ReplyEntry) {
        let attr = self.dir_attr(SEARCH_DIR_INODE);
        reply.entry(&Duration::from_secs(60), &attr, 0);
    }

    pub fn lookup_query_dir(&self, query: &str, inode: u64, reply: ReplyEntry) {
        let results = self.execute_query(query);
        if results.is_empty() {
            reply.error(libc::ENOENT);
            return;
        }

        let attr = self.dir_attr(inode);
        reply.entry(&Duration::from_secs(1), &attr, 0);
    }

    pub fn lookup_result(&self, inode: u64, reply: ReplyEntry) {
        if self.inode_to_result.contains_key(&inode) {
            let attr = self.symlink_attr(inode, 256);
            reply.entry(&Duration::from_secs(1), &attr, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    pub fn getattr_search_dir(&self, reply: ReplyAttr) {
        let attr = self.dir_attr(SEARCH_DIR_INODE);
        reply.attr(&Duration::from_secs(60), &attr);
    }

    pub fn getattr_result(&self, inode: u64, reply: ReplyAttr) {
        let attr = self.symlink_attr(inode, 256);
        reply.attr(&Duration::from_secs(1), &attr);
    }

    pub fn readdir_search_root(&self, offset: i64, mut reply: ReplyDirectory) {
        let entries = vec![
            (SEARCH_DIR_INODE, FileType::Directory, "."),
            (1, FileType::Directory, ".."),
        ];

        for (i, (inode, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*inode, (i + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    pub fn readdir_query(&self, query: &str, offset: i64, mut reply: ReplyDirectory) {
        let results = self.execute_query(query);

        let entries = vec![
            (SEARCH_DIR_INODE + 1, FileType::Directory, ".".to_string()),
            (SEARCH_DIR_INODE, FileType::Directory, "..".to_string()),
        ];

        for (i, (inode, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*inode, (i + 1) as i64, *kind, name) {
                reply.ok();
                return;
            }
        }

        let base_offset = entries.len();
        for (i, hit) in results.iter().enumerate() {
            let entry_offset = base_offset + i;
            if entry_offset < offset as usize {
                continue;
            }

            let inode = SEARCH_RESULT_BASE + i as u64;
            let name = self.result_filename(hit, i);

            self.inode_to_result.insert(inode, (query.to_string(), i));

            if reply.add(inode, (entry_offset + 1) as i64, FileType::Symlink, &name) {
                break;
            }
        }
        reply.ok();
    }

    pub fn readlink(&self, inode: u64, reply: ReplyData) {
        let (query, index) = match self.inode_to_result.get(&inode) {
            Some((q, i)) => (q, i),
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let results = self.execute_query(&query);
        if let Some(hit) = results.get(index) {
            if let Some(target) = self.safe_symlink_target(hit.virtual_path.as_str()) {
                reply.data(target.as_bytes());
            } else {
                reply.error(libc::EINVAL);
            }
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn safe_symlink_target(&self, virtual_path: &str) -> Option<String> {
        let normalized = Path::new(virtual_path).components().fold(
            std::path::PathBuf::new(),
            |mut acc, comp| {
                match comp {
                    std::path::Component::Normal(s) => acc.push(s),
                    std::path::Component::RootDir => acc.push("/"),
                    _ => {}
                }
                acc
            },
        );

        let path_str = normalized.to_string_lossy();
        if path_str.contains("..") {
            return None;
        }

        Some(format!("{}{}", self.mount_point, path_str))
    }

    fn execute_query(&self, query: &str) -> Vec<SearchHit> {
        let query = if query.len() > MAX_QUERY_LENGTH {
            &query[..MAX_QUERY_LENGTH]
        } else {
            query
        };

        if let Some(results) = self.result_cache.get(query) {
            return results;
        }

        let results = self.index.search(query, 1000).unwrap_or_default();
        self.result_cache.insert(query.to_string(), results.clone());
        results
    }

    fn result_filename(&self, hit: &SearchHit, index: usize) -> String {
        let artist = hit.artist.as_deref().unwrap_or("Unknown");
        let title = hit.title.as_deref().unwrap_or("Unknown");
        let ext = hit
            .virtual_path
            .as_str()
            .rsplit('.')
            .next()
            .unwrap_or("flac");
        format!("{:03}. {} - {}.{}", index + 1, artist, title, ext)
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

    fn symlink_attr(&self, inode: u64, target_len: u64) -> FileAttr {
        FileAttr {
            ino: inode,
            size: target_len,
            blocks: 0,
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind: FileType::Symlink,
            perm: 0o777,
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
    use musicfs_search::SearchIndex;
    use tempfile::TempDir;

    #[test]
    fn test_search_ops_new() {
        let dir = TempDir::new().unwrap();
        let index = Arc::new(SearchIndex::open(dir.path()).unwrap());
        let _ops = SearchOps::new(index, "/mnt/music", 1000, 1000);
    }

    #[test]
    fn test_is_search_inode() {
        assert!(SearchOps::is_search_inode(SEARCH_DIR_INODE));
        assert!(SearchOps::is_search_inode(SEARCH_RESULT_BASE));
        assert!(SearchOps::is_search_inode(SEARCH_RESULT_BASE + 100));
        assert!(!SearchOps::is_search_inode(1));
        assert!(!SearchOps::is_search_inode(1000));
    }
}
