use musicfs_core::{FileId, FileMeta, VirtualPath};
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tracing::{debug, trace};

pub type Inode = u64;
pub const ROOT_INODE: Inode = 1;

#[derive(Debug)]
pub enum VirtualNode {
    Directory(DirNode),
    File(FileNode),
}

impl VirtualNode {
    pub fn inode(&self) -> Inode {
        match self {
            VirtualNode::Directory(d) => d.inode,
            VirtualNode::File(f) => f.inode,
        }
    }

    pub fn name(&self) -> &OsStr {
        match self {
            VirtualNode::Directory(d) => &d.name,
            VirtualNode::File(f) => &f.name,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, VirtualNode::Directory(_))
    }
}

#[derive(Debug)]
pub struct DirNode {
    pub inode: Inode,
    pub parent: Inode,
    pub name: OsString,
    pub children: BTreeMap<OsString, Inode>,
    pub mtime: SystemTime,
}

#[derive(Debug)]
pub struct FileNode {
    pub inode: Inode,
    pub name: OsString,
    pub file_id: FileId,
    pub size: u64,
    pub mtime: SystemTime,
}

#[derive(Debug, Clone)]
pub struct RefreshPolicy {
    pub ttl: Duration,
    pub refresh_on_access: bool,
    pub background_interval: Option<Duration>,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(300),
            refresh_on_access: false,
            background_interval: None,
        }
    }
}

pub struct VirtualTree {
    nodes: HashMap<Inode, VirtualNode>,
    path_to_inode: HashMap<VirtualPath, Inode>,
    next_inode: AtomicU64,
    last_refresh: RwLock<SystemTime>,
    refresh_policy: RefreshPolicy,
}

impl VirtualTree {
    pub fn new() -> Self {
        Self::with_policy(RefreshPolicy::default())
    }

    pub fn with_policy(policy: RefreshPolicy) -> Self {
        let mut tree = Self {
            nodes: HashMap::new(),
            path_to_inode: HashMap::new(),
            next_inode: AtomicU64::new(ROOT_INODE + 1),
            last_refresh: RwLock::new(SystemTime::now()),
            refresh_policy: policy,
        };

        tree.nodes.insert(
            ROOT_INODE,
            VirtualNode::Directory(DirNode {
                inode: ROOT_INODE,
                parent: ROOT_INODE,
                name: OsString::from(""),
                children: BTreeMap::new(),
                mtime: SystemTime::now(),
            }),
        );
        tree.path_to_inode.insert(VirtualPath::new("/"), ROOT_INODE);

        tree
    }

    fn alloc_inode(&self) -> Inode {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }

    pub fn get(&self, inode: Inode) -> Option<&VirtualNode> {
        self.nodes.get(&inode)
    }

    pub fn get_by_path(&self, path: &VirtualPath) -> Option<&VirtualNode> {
        self.path_to_inode
            .get(path)
            .and_then(|ino| self.nodes.get(ino))
    }

    pub fn lookup(&self, parent_inode: Inode, name: &OsStr) -> Option<Inode> {
        if let Some(VirtualNode::Directory(dir)) = self.nodes.get(&parent_inode) {
            let result = dir.children.get(name).copied();
            let hit = result.is_some();
            trace!(inode = parent_inode, name = ?name, hit, "tree lookup");
            result
        } else {
            trace!(inode = parent_inode, name = ?name, hit = false, "tree lookup");
            None
        }
    }

    pub fn readdir(&self, inode: Inode) -> Option<Vec<(OsString, Inode, bool)>> {
        if let Some(VirtualNode::Directory(dir)) = self.nodes.get(&inode) {
            Some(
                dir.children
                    .iter()
                    .map(|(name, &ino)| {
                        let is_dir = self.nodes.get(&ino).map(|n| n.is_dir()).unwrap_or(false);
                        (name.clone(), ino, is_dir)
                    })
                    .collect(),
            )
        } else {
            None
        }
    }

    pub fn get_parent(&self, inode: Inode) -> Option<Inode> {
        match self.nodes.get(&inode) {
            Some(VirtualNode::Directory(dir)) => Some(dir.parent),
            Some(VirtualNode::File(_)) => self.find_parent_by_path_lookup(inode),
            None => None,
        }
    }

    fn find_parent_by_path_lookup(&self, inode: Inode) -> Option<Inode> {
        for (path, &ino) in &self.path_to_inode {
            if ino == inode {
                return std::path::Path::new(path.as_str()).parent().and_then(|p| {
                    self.path_to_inode
                        .get(&VirtualPath::new(p.to_string_lossy().into_owned()))
                        .copied()
                });
            }
        }
        None
    }

    pub fn insert_file(&mut self, meta: &FileMeta) -> Inode {
        let path = &meta.virtual_path;

        let parent_inode = self.ensure_parents(path);

        let inode = self.alloc_inode();
        let name = std::path::Path::new(path.as_str())
            .file_name()
            .unwrap_or_default()
            .to_os_string();

        let file_node = FileNode {
            inode,
            name: name.clone(),
            file_id: meta.id,
            size: meta.size,
            mtime: meta.mtime,
        };

        self.nodes.insert(inode, VirtualNode::File(file_node));
        self.path_to_inode.insert(path.clone(), inode);

        if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&parent_inode) {
            dir.children.insert(name, inode);
        }

        debug!(inode, path = path.as_str(), file_id = ?meta.id, "add file to tree");
        inode
    }

    fn ensure_parents(&mut self, path: &VirtualPath) -> Inode {
        let path_str = path.as_str();
        let components: Vec<&str> = path_str
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        if components.len() <= 1 {
            return ROOT_INODE;
        }

        let mut current_inode = ROOT_INODE;
        let mut current_path = String::from("/");

        for component in &components[..components.len() - 1] {
            current_path.push_str(component);

            let vpath = VirtualPath::new(&current_path);

            if let Some(&existing) = self.path_to_inode.get(&vpath) {
                current_inode = existing;
            } else {
                let new_inode = self.alloc_inode();
                let name = OsString::from(*component);

                let dir_node = DirNode {
                    inode: new_inode,
                    parent: current_inode,
                    name: name.clone(),
                    children: BTreeMap::new(),
                    mtime: SystemTime::now(),
                };

                self.nodes
                    .insert(new_inode, VirtualNode::Directory(dir_node));
                self.path_to_inode.insert(vpath, new_inode);

                if let Some(VirtualNode::Directory(parent)) = self.nodes.get_mut(&current_inode) {
                    parent.children.insert(name, new_inode);
                }

                current_inode = new_inode;
            }

            current_path.push('/');
        }

        current_inode
    }

    pub fn remove_file(&mut self, path: &VirtualPath) -> Option<FileId> {
        let inode = self.path_to_inode.remove(path)?;

        if let Some(VirtualNode::File(file)) = self.nodes.remove(&inode) {
            let parent_path = std::path::Path::new(path.as_str())
                .parent()
                .map(|p| VirtualPath::new(p.to_string_lossy().into_owned()))
                .unwrap_or_else(|| VirtualPath::new("/"));

            if let Some(&parent_inode) = self.path_to_inode.get(&parent_path) {
                if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&parent_inode) {
                    dir.children.remove(&file.name);
                }
            }

            debug!(inode, path = path.as_str(), file_id = ?file.file_id, "remove file from tree");
            Some(file.file_id)
        } else {
            None
        }
    }

    pub fn file_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| matches!(n, VirtualNode::File(_)))
            .count()
    }

    pub fn dir_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| matches!(n, VirtualNode::Directory(_)))
            .count()
    }

    pub fn needs_refresh(&self) -> bool {
        let last = *self.last_refresh.read();
        last.elapsed().unwrap_or(Duration::MAX) > self.refresh_policy.ttl
    }

    pub fn force_refresh(&mut self) {
        self.nodes.retain(|&ino, _| ino == ROOT_INODE);
        self.path_to_inode.retain(|p, _| p.as_str() == "/");

        if let Some(VirtualNode::Directory(root)) = self.nodes.get_mut(&ROOT_INODE) {
            root.children.clear();
        }

        *self.last_refresh.write() = SystemTime::now();
    }

    pub fn mark_refreshed(&self) {
        *self.last_refresh.write() = SystemTime::now();
    }

    pub fn refresh_policy(&self) -> &RefreshPolicy {
        &self.refresh_policy
    }

    pub fn path_to_inode_iter(&self) -> impl Iterator<Item = (&VirtualPath, &Inode)> {
        self.path_to_inode.iter()
    }

    pub fn mkdir(&mut self, path: &VirtualPath) -> std::result::Result<Inode, RenameError> {
        if self.path_to_inode.contains_key(path) {
            return Err(RenameError::TargetExists);
        }

        let parent_path = std::path::Path::new(path.as_str())
            .parent()
            .map(|p| {
                let s = p.to_string_lossy();
                if s.is_empty() {
                    VirtualPath::new("/")
                } else {
                    VirtualPath::new(s.into_owned())
                }
            })
            .unwrap_or_else(|| VirtualPath::new("/"));

        let parent_inode = self
            .path_to_inode
            .get(&parent_path)
            .copied()
            .ok_or(RenameError::ParentNotFound)?;

        if !self
            .nodes
            .get(&parent_inode)
            .map(|n| n.is_dir())
            .unwrap_or(false)
        {
            return Err(RenameError::ParentNotFound);
        }

        let inode = self.alloc_inode();
        let name = std::path::Path::new(path.as_str())
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();

        let dir_node = DirNode {
            inode,
            parent: parent_inode,
            name: name.clone(),
            children: BTreeMap::new(),
            mtime: SystemTime::now(),
        };

        self.nodes.insert(inode, VirtualNode::Directory(dir_node));
        self.path_to_inode.insert(path.clone(), inode);

        if let Some(VirtualNode::Directory(parent)) = self.nodes.get_mut(&parent_inode) {
            parent.children.insert(name, inode);
        }

        debug!(path = path.as_str(), inode, "created directory");
        Ok(inode)
    }

    pub fn rename_file(
        &mut self,
        old_path: &VirtualPath,
        new_path: &VirtualPath,
    ) -> std::result::Result<(), RenameError> {
        let old_inode = self
            .path_to_inode
            .get(old_path)
            .copied()
            .ok_or(RenameError::SourceNotFound)?;

        if self.path_to_inode.contains_key(new_path) {
            return Err(RenameError::TargetExists);
        }

        let node = self
            .nodes
            .get(&old_inode)
            .ok_or(RenameError::SourceNotFound)?;

        if node.is_dir() {
            return Err(RenameError::IsDirectory);
        }

        let new_parent_path = std::path::Path::new(new_path.as_str())
            .parent()
            .map(|p| {
                let s = p.to_string_lossy();
                if s.is_empty() {
                    VirtualPath::new("/")
                } else {
                    VirtualPath::new(s.into_owned())
                }
            })
            .unwrap_or_else(|| VirtualPath::new("/"));

        let new_parent_inode = self
            .path_to_inode
            .get(&new_parent_path)
            .copied()
            .ok_or(RenameError::ParentNotFound)?;

        if !self
            .nodes
            .get(&new_parent_inode)
            .map(|n| n.is_dir())
            .unwrap_or(false)
        {
            return Err(RenameError::ParentNotFound);
        }

        self.path_to_inode.remove(old_path);

        let old_parent_path = std::path::Path::new(old_path.as_str())
            .parent()
            .map(|p| VirtualPath::new(p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| VirtualPath::new("/"));

        if let Some(&old_parent_inode) = self.path_to_inode.get(&old_parent_path) {
            if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&old_parent_inode) {
                let old_name = std::path::Path::new(old_path.as_str())
                    .file_name()
                    .map(|n| n.to_os_string())
                    .unwrap_or_default();
                dir.children.remove(&old_name);
            }
        }

        let new_name = std::path::Path::new(new_path.as_str())
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();

        if let Some(VirtualNode::File(file)) = self.nodes.get_mut(&old_inode) {
            file.name = new_name.clone();
        }

        if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&new_parent_inode) {
            dir.children.insert(new_name, old_inode);
        }

        self.path_to_inode.insert(new_path.clone(), old_inode);

        debug!(
            old = old_path.as_str(),
            new = new_path.as_str(),
            inode = old_inode,
            "renamed file"
        );
        Ok(())
    }

    pub fn rename_directory(
        &mut self,
        old_path: &VirtualPath,
        new_path: &VirtualPath,
    ) -> std::result::Result<u64, RenameError> {
        let old_inode = self
            .path_to_inode
            .get(old_path)
            .copied()
            .ok_or(RenameError::SourceNotFound)?;

        if !self
            .nodes
            .get(&old_inode)
            .map(|n| n.is_dir())
            .unwrap_or(false)
        {
            return Err(RenameError::NotDirectory);
        }

        if self.path_to_inode.contains_key(new_path) {
            return Err(RenameError::TargetExists);
        }

        let new_parent_path = std::path::Path::new(new_path.as_str())
            .parent()
            .map(|p| {
                let s = p.to_string_lossy();
                if s.is_empty() {
                    VirtualPath::new("/")
                } else {
                    VirtualPath::new(s.into_owned())
                }
            })
            .unwrap_or_else(|| VirtualPath::new("/"));

        let new_parent_inode = self
            .path_to_inode
            .get(&new_parent_path)
            .copied()
            .ok_or(RenameError::ParentNotFound)?;

        if !self
            .nodes
            .get(&new_parent_inode)
            .map(|n| n.is_dir())
            .unwrap_or(false)
        {
            return Err(RenameError::ParentNotFound);
        }

        let old_prefix = old_path.as_str();
        let new_prefix = new_path.as_str();

        let paths_to_update: Vec<(VirtualPath, Inode)> = self
            .path_to_inode
            .iter()
            .filter(|(p, _)| p.as_str().starts_with(old_prefix))
            .map(|(p, &i)| (p.clone(), i))
            .collect();

        let count = paths_to_update.len() as u64;

        for (old_p, inode) in paths_to_update {
            self.path_to_inode.remove(&old_p);
            let new_p_str = format!("{}{}", new_prefix, &old_p.as_str()[old_prefix.len()..]);
            let new_p = VirtualPath::new(&new_p_str);
            self.path_to_inode.insert(new_p, inode);
        }

        let old_parent_path = std::path::Path::new(old_path.as_str())
            .parent()
            .map(|p| VirtualPath::new(p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| VirtualPath::new("/"));

        if let Some(&old_parent_inode) = self.path_to_inode.get(&old_parent_path) {
            if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&old_parent_inode) {
                let old_name = std::path::Path::new(old_path.as_str())
                    .file_name()
                    .map(|n| n.to_os_string())
                    .unwrap_or_default();
                dir.children.remove(&old_name);
            }
        }

        let new_name = std::path::Path::new(new_path.as_str())
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();

        if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&old_inode) {
            dir.name = new_name.clone();
            dir.parent = new_parent_inode;
        }

        if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&new_parent_inode) {
            dir.children.insert(new_name, old_inode);
        }

        debug!(
            old = old_path.as_str(),
            new = new_path.as_str(),
            count,
            "renamed directory"
        );
        Ok(count)
    }

    pub fn is_trash_path(path: &VirtualPath) -> bool {
        path.as_str().starts_with("/.trash") || path.as_str() == "/.trash"
    }

    pub fn ensure_trash_dir(&mut self) -> Inode {
        let trash_path = VirtualPath::new("/.trash");
        if let Some(&inode) = self.path_to_inode.get(&trash_path) {
            return inode;
        }

        let inode = self.alloc_inode();
        let dir_node = DirNode {
            inode,
            parent: ROOT_INODE,
            name: OsString::from(".trash"),
            children: BTreeMap::new(),
            mtime: SystemTime::now(),
        };

        self.nodes.insert(inode, VirtualNode::Directory(dir_node));
        self.path_to_inode.insert(trash_path, inode);

        if let Some(VirtualNode::Directory(root)) = self.nodes.get_mut(&ROOT_INODE) {
            root.children.insert(OsString::from(".trash"), inode);
        }

        debug!(inode, "created .trash directory");
        inode
    }

    pub fn mkdir_p(&mut self, path: &VirtualPath) -> std::result::Result<Inode, RenameError> {
        if let Some(&existing) = self.path_to_inode.get(path) {
            if self
                .nodes
                .get(&existing)
                .map(|n| n.is_dir())
                .unwrap_or(false)
            {
                return Ok(existing);
            }
            return Err(RenameError::TargetExists);
        }

        let components: Vec<&str> = path
            .as_str()
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut current_inode = ROOT_INODE;
        let mut current_path = String::from("/");

        for component in &components {
            if !current_path.ends_with('/') {
                current_path.push('/');
            }
            current_path.push_str(component);

            let vpath = VirtualPath::new(&current_path);

            if let Some(&existing) = self.path_to_inode.get(&vpath) {
                current_inode = existing;
            } else {
                let new_inode = self.alloc_inode();
                let name = OsString::from(*component);

                let dir_node = DirNode {
                    inode: new_inode,
                    parent: current_inode,
                    name: name.clone(),
                    children: BTreeMap::new(),
                    mtime: SystemTime::now(),
                };

                self.nodes
                    .insert(new_inode, VirtualNode::Directory(dir_node));
                self.path_to_inode.insert(vpath, new_inode);

                if let Some(VirtualNode::Directory(parent)) = self.nodes.get_mut(&current_inode) {
                    parent.children.insert(name, new_inode);
                }

                current_inode = new_inode;
            }
        }

        Ok(current_inode)
    }

    pub fn remove_directory(&mut self, path: &VirtualPath) -> std::result::Result<(), RemoveError> {
        let inode = self
            .path_to_inode
            .get(path)
            .copied()
            .ok_or(RemoveError::NotFound)?;

        let node = self.nodes.get(&inode).ok_or(RemoveError::NotFound)?;

        match node {
            VirtualNode::File(_) => return Err(RemoveError::NotDirectory),
            VirtualNode::Directory(dir) => {
                if !dir.children.is_empty() {
                    return Err(RemoveError::NotEmpty);
                }
            }
        }

        let parent_path = std::path::Path::new(path.as_str())
            .parent()
            .map(|p| VirtualPath::new(p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| VirtualPath::new("/"));

        if let Some(&parent_inode) = self.path_to_inode.get(&parent_path) {
            if let Some(VirtualNode::Directory(parent)) = self.nodes.get_mut(&parent_inode) {
                let name = std::path::Path::new(path.as_str())
                    .file_name()
                    .map(|n| n.to_os_string())
                    .unwrap_or_default();
                parent.children.remove(&name);
            }
        }

        self.path_to_inode.remove(path);
        self.nodes.remove(&inode);

        debug!(path = path.as_str(), inode, "removed directory");
        Ok(())
    }

    pub fn remove_directory_recursive(
        &mut self,
        path: &VirtualPath,
    ) -> std::result::Result<Vec<FileId>, RemoveError> {
        let inode = self
            .path_to_inode
            .get(path)
            .copied()
            .ok_or(RemoveError::NotFound)?;

        if !self.nodes.get(&inode).map(|n| n.is_dir()).unwrap_or(false) {
            return Err(RemoveError::NotDirectory);
        }

        let prefix = path.as_str();
        let paths_to_remove: Vec<(VirtualPath, Inode)> = self
            .path_to_inode
            .iter()
            .filter(|(p, _)| p.as_str().starts_with(prefix))
            .map(|(p, &i)| (p.clone(), i))
            .collect();

        let mut removed_files = Vec::new();

        for (p, ino) in &paths_to_remove {
            if let Some(VirtualNode::File(f)) = self.nodes.get(ino) {
                removed_files.push(f.file_id);
            }
            self.path_to_inode.remove(p);
            self.nodes.remove(ino);
        }

        let parent_path = std::path::Path::new(path.as_str())
            .parent()
            .map(|p| VirtualPath::new(p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| VirtualPath::new("/"));

        if let Some(&parent_inode) = self.path_to_inode.get(&parent_path) {
            if let Some(VirtualNode::Directory(parent)) = self.nodes.get_mut(&parent_inode) {
                let name = std::path::Path::new(path.as_str())
                    .file_name()
                    .map(|n| n.to_os_string())
                    .unwrap_or_default();
                parent.children.remove(&name);
            }
        }

        debug!(
            path = path.as_str(),
            file_count = removed_files.len(),
            "removed directory recursively"
        );
        Ok(removed_files)
    }

    pub fn is_directory_empty(&self, path: &VirtualPath) -> Option<bool> {
        let inode = self.path_to_inode.get(path)?;
        if let Some(VirtualNode::Directory(dir)) = self.nodes.get(inode) {
            Some(dir.children.is_empty())
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveError {
    NotFound,
    NotEmpty,
    NotDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameError {
    SourceNotFound,
    TargetExists,
    ParentNotFound,
    IsDirectory,
    NotDirectory,
}

impl Default for VirtualTree {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TreeBuilder {
    tree: VirtualTree,
}

impl TreeBuilder {
    pub fn new() -> Self {
        Self {
            tree: VirtualTree::new(),
        }
    }

    pub fn with_policy(policy: RefreshPolicy) -> Self {
        Self {
            tree: VirtualTree::with_policy(policy),
        }
    }

    pub fn add_file(&mut self, meta: &FileMeta) {
        self.tree.insert_file(meta);
    }

    pub fn build(self) -> VirtualTree {
        self.tree
    }
}

impl Default for TreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::{OriginId, RealPath};
    use std::path::PathBuf;

    fn make_file_meta(id: i64, vpath: &str) -> FileMeta {
        FileMeta {
            id: FileId(id),
            virtual_path: VirtualPath::new(vpath),
            real_path: RealPath {
                origin_id: OriginId::from("test"),
                path: PathBuf::from("/test"),
            },
            size: 1000,
            mtime: SystemTime::now(),
            content_hash: None,
            audio: None,
        }
    }

    #[test]
    fn test_tree_creation() {
        let tree = VirtualTree::new();
        assert!(tree.get(ROOT_INODE).is_some());
    }

    #[test]
    fn test_insert_file() {
        let mut tree = VirtualTree::new();
        let meta = make_file_meta(1, "/Artist/Album/Track.flac");
        tree.insert_file(&meta);

        assert!(tree.get_by_path(&VirtualPath::new("/Artist")).is_some());
        assert!(tree
            .get_by_path(&VirtualPath::new("/Artist/Album"))
            .is_some());
        assert!(tree
            .get_by_path(&VirtualPath::new("/Artist/Album/Track.flac"))
            .is_some());
    }

    #[test]
    fn test_readdir() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Album/Track1.flac"));
        tree.insert_file(&make_file_meta(2, "/Artist/Album/Track2.flac"));

        let root_children = tree.readdir(ROOT_INODE).unwrap();
        assert_eq!(root_children.len(), 1);
        assert_eq!(root_children[0].0, "Artist");
    }

    #[test]
    fn test_lookup() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Album/Track.flac"));

        let artist_inode = tree.lookup(ROOT_INODE, OsStr::new("Artist")).unwrap();
        assert!(tree.lookup(artist_inode, OsStr::new("Album")).is_some());
    }

    #[test]
    fn test_file_and_dir_count() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/A/B/Track1.flac"));
        tree.insert_file(&make_file_meta(2, "/A/B/Track2.flac"));
        tree.insert_file(&make_file_meta(3, "/A/C/Track3.flac"));

        assert_eq!(tree.file_count(), 3);
        assert_eq!(tree.dir_count(), 4);
    }

    #[test]
    fn test_remove_file() {
        let mut tree = VirtualTree::new();
        let path = VirtualPath::new("/Artist/Album/Track.flac");
        tree.insert_file(&make_file_meta(1, path.as_str()));

        assert!(tree.get_by_path(&path).is_some());

        let removed_id = tree.remove_file(&path);
        assert_eq!(removed_id, Some(FileId(1)));
        assert!(tree.get_by_path(&path).is_none());
    }

    #[test]
    fn test_tree_builder() {
        let mut builder = TreeBuilder::new();
        builder.add_file(&make_file_meta(1, "/A/Track1.flac"));
        builder.add_file(&make_file_meta(2, "/A/Track2.flac"));

        let tree = builder.build();
        assert_eq!(tree.file_count(), 2);
    }

    #[test]
    fn test_rename_file() {
        let mut tree = VirtualTree::new();
        let old_path = VirtualPath::new("/Artist/Album/Track.flac");
        let new_path = VirtualPath::new("/Artist/Album/Renamed.flac");

        tree.insert_file(&make_file_meta(1, old_path.as_str()));

        assert!(tree.get_by_path(&old_path).is_some());

        tree.rename_file(&old_path, &new_path).unwrap();

        assert!(tree.get_by_path(&old_path).is_none());
        assert!(tree.get_by_path(&new_path).is_some());
    }

    #[test]
    fn test_rename_file_to_new_dir() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Album/Track.flac"));

        tree.mkdir(&VirtualPath::new("/New Artist")).unwrap();
        tree.mkdir(&VirtualPath::new("/New Artist/New Album"))
            .unwrap();

        let result = tree.rename_file(
            &VirtualPath::new("/Artist/Album/Track.flac"),
            &VirtualPath::new("/New Artist/New Album/Track.flac"),
        );

        assert!(result.is_ok());
        assert!(tree
            .get_by_path(&VirtualPath::new("/New Artist/New Album/Track.flac"))
            .is_some());
    }

    #[test]
    fn test_rename_file_parent_not_found() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Album/Track.flac"));

        let result = tree.rename_file(
            &VirtualPath::new("/Artist/Album/Track.flac"),
            &VirtualPath::new("/NonExistent/Album/Track.flac"),
        );

        assert_eq!(result, Err(RenameError::ParentNotFound));
    }

    #[test]
    fn test_rename_file_target_exists() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/A/Track1.flac"));
        tree.insert_file(&make_file_meta(2, "/A/Track2.flac"));

        let result = tree.rename_file(
            &VirtualPath::new("/A/Track1.flac"),
            &VirtualPath::new("/A/Track2.flac"),
        );

        assert_eq!(result, Err(RenameError::TargetExists));
    }

    #[test]
    fn test_rename_file_source_not_found() {
        let mut tree = VirtualTree::new();

        let result = tree.rename_file(
            &VirtualPath::new("/Nonexistent.flac"),
            &VirtualPath::new("/New.flac"),
        );

        assert_eq!(result, Err(RenameError::SourceNotFound));
    }

    #[test]
    fn test_rename_directory() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Album/Track1.flac"));
        tree.insert_file(&make_file_meta(2, "/Artist/Album/Track2.flac"));
        tree.insert_file(&make_file_meta(3, "/Artist/Other/Track3.flac"));

        let count = tree
            .rename_directory(
                &VirtualPath::new("/Artist"),
                &VirtualPath::new("/Renamed Artist"),
            )
            .unwrap();

        assert_eq!(count, 6);

        assert!(tree.get_by_path(&VirtualPath::new("/Artist")).is_none());
        assert!(tree
            .get_by_path(&VirtualPath::new("/Renamed Artist"))
            .is_some());
        assert!(tree
            .get_by_path(&VirtualPath::new("/Renamed Artist/Album/Track1.flac"))
            .is_some());
        assert!(tree
            .get_by_path(&VirtualPath::new("/Renamed Artist/Album/Track2.flac"))
            .is_some());
        assert!(tree
            .get_by_path(&VirtualPath::new("/Renamed Artist/Other/Track3.flac"))
            .is_some());
    }

    #[test]
    fn test_rename_directory_parent_not_found() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Album/Track.flac"));

        let result = tree.rename_directory(
            &VirtualPath::new("/Artist"),
            &VirtualPath::new("/NonExistent/Renamed"),
        );

        assert_eq!(result, Err(RenameError::ParentNotFound));
    }

    #[test]
    fn test_rename_directory_not_directory() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Track.flac"));

        let result = tree.rename_directory(
            &VirtualPath::new("/Artist/Track.flac"),
            &VirtualPath::new("/New"),
        );

        assert_eq!(result, Err(RenameError::NotDirectory));
    }

    #[test]
    fn test_mkdir() {
        let mut tree = VirtualTree::new();

        let inode = tree.mkdir(&VirtualPath::new("/NewDir")).unwrap();
        assert!(inode > ROOT_INODE);
        assert!(tree.get_by_path(&VirtualPath::new("/NewDir")).is_some());
        assert!(tree
            .get_by_path(&VirtualPath::new("/NewDir"))
            .unwrap()
            .is_dir());
    }

    #[test]
    fn test_mkdir_nested() {
        let mut tree = VirtualTree::new();

        tree.mkdir(&VirtualPath::new("/A")).unwrap();
        tree.mkdir(&VirtualPath::new("/A/B")).unwrap();
        tree.mkdir(&VirtualPath::new("/A/B/C")).unwrap();

        assert!(tree.get_by_path(&VirtualPath::new("/A/B/C")).is_some());
    }

    #[test]
    fn test_mkdir_parent_not_found() {
        let mut tree = VirtualTree::new();

        let result = tree.mkdir(&VirtualPath::new("/A/B/C"));
        assert_eq!(result, Err(RenameError::ParentNotFound));
    }

    #[test]
    fn test_mkdir_already_exists() {
        let mut tree = VirtualTree::new();

        tree.mkdir(&VirtualPath::new("/Existing")).unwrap();
        let result = tree.mkdir(&VirtualPath::new("/Existing"));

        assert_eq!(result, Err(RenameError::TargetExists));
    }

    #[test]
    fn test_is_trash_path() {
        assert!(VirtualTree::is_trash_path(&VirtualPath::new("/.trash")));
        assert!(VirtualTree::is_trash_path(&VirtualPath::new(
            "/.trash/Artist/Track.flac"
        )));
        assert!(!VirtualTree::is_trash_path(&VirtualPath::new(
            "/Artist/Track.flac"
        )));
        assert!(!VirtualTree::is_trash_path(&VirtualPath::new(
            "/trash/Artist/Track.flac"
        )));
    }

    #[test]
    fn test_ensure_trash_dir() {
        let mut tree = VirtualTree::new();

        assert!(tree.get_by_path(&VirtualPath::new("/.trash")).is_none());

        let inode = tree.ensure_trash_dir();
        assert!(inode > ROOT_INODE);

        let node = tree.get_by_path(&VirtualPath::new("/.trash"));
        assert!(node.is_some());
        assert!(node.unwrap().is_dir());

        let inode2 = tree.ensure_trash_dir();
        assert_eq!(inode, inode2);
    }

    #[test]
    fn test_mkdir_p() {
        let mut tree = VirtualTree::new();

        tree.mkdir_p(&VirtualPath::new("/A/B/C/D")).unwrap();

        assert!(tree.get_by_path(&VirtualPath::new("/A")).is_some());
        assert!(tree.get_by_path(&VirtualPath::new("/A/B")).is_some());
        assert!(tree.get_by_path(&VirtualPath::new("/A/B/C")).is_some());
        assert!(tree.get_by_path(&VirtualPath::new("/A/B/C/D")).is_some());
    }

    #[test]
    fn test_mkdir_p_partial_exists() {
        let mut tree = VirtualTree::new();

        tree.mkdir(&VirtualPath::new("/A")).unwrap();
        tree.mkdir(&VirtualPath::new("/A/B")).unwrap();

        tree.mkdir_p(&VirtualPath::new("/A/B/C/D")).unwrap();

        assert!(tree.get_by_path(&VirtualPath::new("/A/B/C")).is_some());
        assert!(tree.get_by_path(&VirtualPath::new("/A/B/C/D")).is_some());
    }

    #[test]
    fn test_remove_directory_empty() {
        let mut tree = VirtualTree::new();

        tree.mkdir(&VirtualPath::new("/EmptyDir")).unwrap();
        assert!(tree.get_by_path(&VirtualPath::new("/EmptyDir")).is_some());

        tree.remove_directory(&VirtualPath::new("/EmptyDir"))
            .unwrap();
        assert!(tree.get_by_path(&VirtualPath::new("/EmptyDir")).is_none());
    }

    #[test]
    fn test_remove_directory_not_empty() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Track.flac"));

        let result = tree.remove_directory(&VirtualPath::new("/Artist"));
        assert_eq!(result, Err(RemoveError::NotEmpty));
    }

    #[test]
    fn test_remove_directory_not_found() {
        let mut tree = VirtualTree::new();

        let result = tree.remove_directory(&VirtualPath::new("/NonExistent"));
        assert_eq!(result, Err(RemoveError::NotFound));
    }

    #[test]
    fn test_remove_directory_is_file() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Track.flac"));

        let result = tree.remove_directory(&VirtualPath::new("/Track.flac"));
        assert_eq!(result, Err(RemoveError::NotDirectory));
    }

    #[test]
    fn test_remove_directory_recursive() {
        let mut tree = VirtualTree::new();
        tree.insert_file(&make_file_meta(1, "/Artist/Album/Track1.flac"));
        tree.insert_file(&make_file_meta(2, "/Artist/Album/Track2.flac"));
        tree.insert_file(&make_file_meta(3, "/Artist/Other/Track3.flac"));

        let removed = tree
            .remove_directory_recursive(&VirtualPath::new("/Artist"))
            .unwrap();

        assert_eq!(removed.len(), 3);
        assert!(tree.get_by_path(&VirtualPath::new("/Artist")).is_none());
    }

    #[test]
    fn test_is_directory_empty() {
        let mut tree = VirtualTree::new();

        tree.mkdir(&VirtualPath::new("/Empty")).unwrap();
        assert_eq!(
            tree.is_directory_empty(&VirtualPath::new("/Empty")),
            Some(true)
        );

        tree.insert_file(&make_file_meta(1, "/NonEmpty/Track.flac"));
        assert_eq!(
            tree.is_directory_empty(&VirtualPath::new("/NonEmpty")),
            Some(false)
        );

        assert_eq!(
            tree.is_directory_empty(&VirtualPath::new("/NonExistent")),
            None
        );
    }
}
