use crate::ops::SearchOps;
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen,
    Request,
};
use musicfs_cache::{VirtualNode, VirtualTree, ROOT_INODE};
use musicfs_cas::FileReader;
use musicfs_core::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::runtime::Handle;
use tracing::{debug, info, instrument, trace, warn};

const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 512;
const SEARCH_QUERY_INODE_BASE: u64 = 0xFFFF_FFFF_0000_0100;

pub struct MusicFs {
    tree: Arc<RwLock<VirtualTree>>,
    reader: Option<Arc<FileReader>>,
    runtime_handle: Handle,
    search_ops: Option<SearchOps>,
    query_inodes: RwLock<HashMap<String, u64>>,
    inode_queries: RwLock<HashMap<u64, String>>,
    next_query_inode: RwLock<u64>,
    uid: u32,
    gid: u32,
}

impl MusicFs {
    pub fn new(tree: Arc<RwLock<VirtualTree>>, runtime_handle: Handle) -> Self {
        Self {
            tree,
            reader: None,
            runtime_handle,
            search_ops: None,
            query_inodes: RwLock::new(HashMap::new()),
            inode_queries: RwLock::new(HashMap::new()),
            next_query_inode: RwLock::new(SEARCH_QUERY_INODE_BASE),
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
    }

    pub fn with_reader(tree: Arc<RwLock<VirtualTree>>, reader: Arc<FileReader>, runtime_handle: Handle) -> Self {
        Self {
            tree,
            reader: Some(reader),
            runtime_handle,
            search_ops: None,
            query_inodes: RwLock::new(HashMap::new()),
            inode_queries: RwLock::new(HashMap::new()),
            next_query_inode: RwLock::new(SEARCH_QUERY_INODE_BASE),
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
    }

    pub fn with_search(mut self, search_ops: SearchOps) -> Self {
        self.search_ops = Some(search_ops);
        self
    }

    fn get_or_create_query_inode(&self, query: &str) -> u64 {
        let query_inodes = self.query_inodes.read();
        if let Some(&inode) = query_inodes.get(query) {
            return inode;
        }
        drop(query_inodes);

        let mut query_inodes = self.query_inodes.write();
        let mut inode_queries = self.inode_queries.write();
        let mut next_inode = self.next_query_inode.write();

        if let Some(&inode) = query_inodes.get(query) {
            return inode;
        }

        let inode = *next_inode;
        *next_inode += 1;
        query_inodes.insert(query.to_string(), inode);
        inode_queries.insert(inode, query.to_string());
        inode
    }

    fn get_query_for_inode(&self, inode: u64) -> Option<String> {
        self.inode_queries.read().get(&inode).cloned()
    }

    pub fn mount(self, mountpoint: &Path) -> Result<()> {
        info!("Mounting MusicFS at {:?}", mountpoint);

        let options = vec![
            fuser::MountOption::RO,
            fuser::MountOption::FSName("musicfs".to_string()),
            fuser::MountOption::AutoUnmount,
            fuser::MountOption::AllowOther,
        ];

        fuser::mount2(self, mountpoint, &options).map_err(musicfs_core::Error::Io)?;

        Ok(())
    }

    pub fn spawn_mount(self, mountpoint: &Path) -> Result<fuser::BackgroundSession> {
        info!("Mounting MusicFS at {:?}", mountpoint);

        let options = vec![
            fuser::MountOption::RO,
            fuser::MountOption::FSName("musicfs".to_string()),
            fuser::MountOption::AutoUnmount,
            fuser::MountOption::AllowOther,
        ];

        let session =
            fuser::spawn_mount2(self, mountpoint, &options).map_err(musicfs_core::Error::Io)?;

        Ok(session)
    }

    fn node_to_attr(&self, node: &VirtualNode) -> FileAttr {
        match node {
            VirtualNode::Directory(dir) => FileAttr {
                ino: dir.inode,
                size: 0,
                blocks: 0,
                atime: dir.mtime,
                mtime: dir.mtime,
                ctime: dir.mtime,
                crtime: dir.mtime,
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: self.uid,
                gid: self.gid,
                rdev: 0,
                blksize: BLOCK_SIZE,
                flags: 0,
            },
            VirtualNode::File(file) => FileAttr {
                ino: file.inode,
                size: file.size,
                blocks: (file.size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64,
                atime: file.mtime,
                mtime: file.mtime,
                ctime: file.mtime,
                crtime: file.mtime,
                kind: FileType::RegularFile,
                perm: 0o644,
                nlink: 1,
                uid: self.uid,
                gid: self.gid,
                rdev: 0,
                blksize: BLOCK_SIZE,
                flags: 0,
            },
        }
    }
}

impl Filesystem for MusicFs {
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut fuser::KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        info!("MusicFS initialized");
        Ok(())
    }

    fn destroy(&mut self) {
        info!("MusicFS destroyed");
    }

    #[instrument(level = "debug", skip(self, reply))]
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy();

        if parent == ROOT_INODE && SearchOps::is_search_dir_name(&name_str) {
            trace!(parent, name = %name_str, "search_dir_name matched");
            if let Some(ref search_ops) = self.search_ops {
                search_ops.lookup_search_dir(reply);
                return;
            }
        }

        if parent == SearchOps::search_dir_inode() {
            trace!(parent, name = %name_str, "search_dir_inode matched");
            if let Some(ref search_ops) = self.search_ops {
                let inode = self.get_or_create_query_inode(&name_str);
                search_ops.lookup_query_dir(&name_str, inode, reply);
                return;
            }
        }

        if let Some(query) = self.get_query_for_inode(parent) {
            trace!(parent, name = %name_str, query = %query, "query_inode matched");
            if let Some(ref search_ops) = self.search_ops {
                let inode = self.get_or_create_query_inode(&format!("{}:{}", query, name_str));
                search_ops.lookup_result(inode, reply);
                return;
            }
        }

        let tree = self.tree.read();

        if let Some(inode) = tree.lookup(parent, name) {
            trace!(parent, name = %name_str, ino = inode, "file found in tree");
            if let Some(node) = tree.get(inode) {
                let attr = self.node_to_attr(node);
                reply.entry(&TTL, &attr, 0);
                return;
            }
        }

        trace!(parent, name = %name_str, "file not found");
        reply.error(libc::ENOENT);
    }

    #[instrument(level = "debug", skip(self, reply))]
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if ino == SearchOps::search_dir_inode() {
            trace!(ino, "search_dir_inode matched");
            if let Some(ref search_ops) = self.search_ops {
                search_ops.getattr_search_dir(reply);
                return;
            }
        }

        if SearchOps::is_search_inode(ino) {
            trace!(ino, "search_inode matched");
            if let Some(ref search_ops) = self.search_ops {
                search_ops.getattr_result(ino, reply);
                return;
            }
        }

        if self.get_query_for_inode(ino).is_some() {
            trace!(ino, "query_inode matched");
            if let Some(ref search_ops) = self.search_ops {
                search_ops.getattr_search_dir(reply);
                return;
            }
        }

        let tree = self.tree.read();

        if let Some(node) = tree.get(ino) {
            trace!(ino, "inode found in tree");
            let attr = self.node_to_attr(node);
            reply.attr(&TTL, &attr);
        } else {
            trace!(ino, "inode not found");
            reply.error(libc::ENOENT);
        }
    }

    #[instrument(level = "debug", skip(self, reply))]
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino == SearchOps::search_dir_inode() {
            trace!(ino, offset, "search_dir_inode matched");
            if let Some(ref search_ops) = self.search_ops {
                search_ops.readdir_search_root(offset, reply);
                return;
            }
        }

        if let Some(query) = self.get_query_for_inode(ino) {
            trace!(ino, offset, query = %query, "query_inode matched");
            if let Some(ref search_ops) = self.search_ops {
                search_ops.readdir_query(&query, offset, reply);
                return;
            }
        }

        let tree = self.tree.read();

        if let Some(children) = tree.readdir(ino) {
            trace!(ino, offset, children_count = children.len(), "directory found");
            let parent_ino = tree.get_parent(ino).unwrap_or(ROOT_INODE);

            let entries: Vec<(u64, FileType, &str)> = vec![
                (ino, FileType::Directory, "."),
                (parent_ino, FileType::Directory, ".."),
            ];

            let child_entries: Vec<(u64, FileType, String)> = children
                .iter()
                .map(|(name, child_ino, is_dir)| {
                    let kind = if *is_dir {
                        FileType::Directory
                    } else {
                        FileType::RegularFile
                    };
                    (*child_ino, kind, name.to_string_lossy().to_string())
                })
                .collect();

            for (i, (inode, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                if reply.add(*inode, (i + 1) as i64, *kind, name) {
                    reply.ok();
                    return;
                }
            }

            let base_offset = entries.len();
            for (i, (inode, kind, name)) in child_entries.iter().enumerate() {
                let entry_offset = base_offset + i;
                if entry_offset < offset as usize {
                    continue;
                }
                if reply.add(*inode, (entry_offset + 1) as i64, *kind, name) {
                    break;
                }
            }

            reply.ok();
        } else {
            trace!(ino, offset, "directory not found");
            reply.error(libc::ENOENT);
        }
    }

    #[instrument(level = "debug", skip(self, reply))]
    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        let write_flags = libc::O_WRONLY | libc::O_RDWR | libc::O_APPEND | libc::O_TRUNC;
        if flags & write_flags != 0 {
            trace!(ino, flags, "write flags detected");
            reply.error(libc::EROFS);
            return;
        }

        let tree = self.tree.read();

        if tree.get(ino).is_some() {
            trace!(ino, "inode found");
            reply.opened(0, 0);
        } else {
            trace!(ino, "inode not found");
            reply.error(libc::ENOENT);
        }
    }

    #[instrument(level = "debug", skip(self, reply))]
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let file_id = {
            let tree = self.tree.read();
            if let Some(VirtualNode::File(file)) = tree.get(ino) {
                trace!(ino, "file found in tree");
                file.file_id
            } else {
                trace!(ino, "file not found");
                reply.error(libc::ENOENT);
                return;
            }
        };

        let Some(reader) = &self.reader else {
            trace!(ino, "no reader available");
            reply.data(&[]);
            return;
        };

        let reader = reader.clone();
        let handle = self.runtime_handle.clone();
        let result = std::thread::scope(|_| {
            handle.block_on(async {
                reader.read(file_id, offset as u64, size).await
            })
        });

        match result {
            Ok(data) => {
                trace!(ino, offset, size_bytes = size, bytes_read = data.len(), "read successful");
                reply.data(&data);
            }
            Err(e) => {
                warn!(ino, offset, size_bytes = size, error = %e, "read failed");
                reply.error(libc::EIO);
            }
        }
    }

    #[instrument(level = "debug", skip(self, reply))]
    fn release(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        trace!(ino, "releasing file handle");
        reply.ok();
    }

    fn readlink(&mut self, _req: &Request, ino: u64, reply: ReplyData) {
        debug!("readlink(ino={})", ino);

        if SearchOps::is_search_inode(ino) {
            if let Some(ref search_ops) = self.search_ops {
                search_ops.readlink(ino, reply);
                return;
            }
        }

        reply.error(libc::EINVAL);
    }

    fn write(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        reply.error(libc::EROFS);
    }

    fn mkdir(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }

    fn unlink(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }

    fn rmdir(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }

    fn rename(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _newparent: u64,
        _newname: &OsStr,
        _flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        reply.error(libc::EROFS);
    }

    fn create(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        reply.error(libc::EROFS);
    }

    fn setattr(
        &mut self,
        _req: &Request,
        _ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        reply.error(libc::EROFS);
    }

    fn symlink(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _link: &Path,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }

    fn link(
        &mut self,
        _req: &Request,
        _ino: u64,
        _newparent: u64,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }

    fn mknod(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_cache::TreeBuilder;
    use musicfs_core::{FileId, FileMeta, OriginId, RealPath, VirtualPath};
    use std::path::PathBuf;

    fn make_file_meta(id: i64, vpath: &str, size: u64) -> FileMeta {
        FileMeta {
            id: FileId(id),
            virtual_path: VirtualPath::new(vpath),
            real_path: RealPath {
                origin_id: OriginId::from("test"),
                path: PathBuf::from("/test"),
            },
            size,
            mtime: SystemTime::now(),
            content_hash: None,
            audio: None,
        }
    }

    #[test]
    fn test_tree_integration() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let handle = runtime.handle().clone();
        
        let mut builder = TreeBuilder::new();
        builder.add_file(&make_file_meta(1, "/Artist/Album/Track.flac", 30_000_000));
        let tree = Arc::new(RwLock::new(builder.build()));

        let _fs = MusicFs::new(tree.clone(), handle);

        let tree_read = tree.read();
        assert!(tree_read.get(ROOT_INODE).is_some());
        assert!(tree_read.get_by_path(&VirtualPath::new("/Artist")).is_some());
    }
}
