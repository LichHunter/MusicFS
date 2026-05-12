use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen,
    Request,
};
use musicfs_cache::{VirtualNode, VirtualTree, ROOT_INODE};
use musicfs_core::Result;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
use tracing::{debug, info};

const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 512;

pub struct MusicFs {
    tree: Arc<RwLock<VirtualTree>>,
    uid: u32,
    gid: u32,
}

impl MusicFs {
    pub fn new(tree: Arc<RwLock<VirtualTree>>) -> Self {
        Self {
            tree,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
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

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup(parent={}, name={:?})", parent, name);

        let tree = self.tree.read().unwrap();

        if let Some(inode) = tree.lookup(parent, name) {
            if let Some(node) = tree.get(inode) {
                let attr = self.node_to_attr(node);
                reply.entry(&TTL, &attr, 0);
                return;
            }
        }

        reply.error(libc::ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        debug!("getattr(ino={})", ino);

        let tree = self.tree.read().unwrap();

        if let Some(node) = tree.get(ino) {
            let attr = self.node_to_attr(node);
            reply.attr(&TTL, &attr);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir(ino={}, offset={})", ino, offset);

        let tree = self.tree.read().unwrap();

        if let Some(children) = tree.readdir(ino) {
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
            reply.error(libc::ENOENT);
        }
    }

    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        debug!("open(ino={}, flags={})", ino, flags);

        let write_flags = libc::O_WRONLY | libc::O_RDWR | libc::O_APPEND | libc::O_TRUNC;
        if flags & write_flags != 0 {
            reply.error(libc::EROFS);
            return;
        }

        let tree = self.tree.read().unwrap();

        if tree.get(ino).is_some() {
            reply.opened(0, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

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
        debug!("read(ino={}, offset={}, size={})", ino, offset, size);

        let tree = self.tree.read().unwrap();

        if let Some(VirtualNode::File(_file)) = tree.get(ino) {
            reply.data(&[]);
        } else {
            reply.error(libc::ENOENT);
        }
    }

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
        debug!("release(ino={})", ino);
        reply.ok();
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
        let mut builder = TreeBuilder::new();
        builder.add_file(&make_file_meta(1, "/Artist/Album/Track.flac", 30_000_000));
        let tree = Arc::new(RwLock::new(builder.build()));

        let _fs = MusicFs::new(tree.clone());

        let tree_read = tree.read().unwrap();
        assert!(tree_read.get(ROOT_INODE).is_some());
        assert!(tree_read.get_by_path(&VirtualPath::new("/Artist")).is_some());
    }
}
