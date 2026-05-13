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
}
