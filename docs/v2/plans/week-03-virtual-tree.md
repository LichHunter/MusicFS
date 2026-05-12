# Week 3: Virtual Tree & Basic Ops

**Phase**: 1 (MVP)  
**Prerequisites**: Week 2 (Metadata Extraction)  
**Estimated effort**: 5 days

---

## Objective

Implement virtual path resolver, tree cache, and connect to FUSE operations (readdir, stat, read).

---

## Deliverables

| Task | Crate | Files | Done |
|------|-------|-------|------|
| Virtual path resolver | musicfs-core | `resolver.rs` | [ ] |
| Tree cache | musicfs-cache | `tree.rs` | [ ] |
| readdir implementation | musicfs-fuse | `ops/readdir.rs` | [ ] |
| stat implementation | musicfs-fuse | `ops/stat.rs` | [ ] |
| open/read implementation | musicfs-fuse | `ops/read.rs` | [ ] |

---

## Task 1: Virtual Path Resolver

### 1.1 Add to `musicfs-core/src/lib.rs`

```rust
pub mod resolver;
pub use resolver::{PathResolver, PathTemplate};
```

### 1.2 Create `musicfs-core/src/resolver.rs`

```rust
use crate::{AudioMeta, VirtualPath};
use std::path::PathBuf;

/// Path template configuration (per architecture 4.3.1)
/// Uses $variable syntax: $artist, $album, $title, $track, $year, $genre, 
/// $format, $format_upper, $disc
#[derive(Debug, Clone)]
pub struct PathTemplate {
    /// Template pattern using $var syntax
    /// Default: "$artist/$album ($year) [$format_upper]/$track - $title.$format"
    pub pattern: String,
    /// Fallback for missing artist
    pub fallback_artist: String,
    /// Fallback for missing album
    pub fallback_album: String,
    /// Fallback for missing title
    pub fallback_title: String,
    /// Fallback for missing year
    pub fallback_year: String,
}

impl Default for PathTemplate {
    fn default() -> Self {
        Self {
            // Architecture 4.3.1 default template
            pattern: "$artist/$album ($year) [$format_upper]/$track - $title.$format".to_string(),
            fallback_artist: "Unknown Artist".to_string(),
            fallback_album: "Unknown Album".to_string(),
            fallback_title: "Unknown Track".to_string(),
            fallback_year: "Unknown".to_string(),
        }
    }
}

/// Virtual path resolver (FR-5.1, FR-5.2)
pub struct PathResolver {
    template: PathTemplate,
}

impl PathResolver {
    pub fn new(template: PathTemplate) -> Self {
        Self { template }
    }
    
    /// Resolve real path + metadata to virtual path
    /// Uses $var syntax per architecture 4.3.1
    pub fn resolve(&self, meta: &AudioMeta, extension: &str) -> VirtualPath {
        let artist = meta.artist.as_deref()
            .unwrap_or(&self.template.fallback_artist);
        let album = meta.album.as_deref()
            .unwrap_or(&self.template.fallback_album);
        let title = meta.title.as_deref()
            .unwrap_or(&self.template.fallback_title);
        let year = meta.year
            .map(|y| y.to_string())
            .unwrap_or_else(|| self.template.fallback_year.clone());
        let track = meta.track.unwrap_or(0);
        let disc = meta.disc.unwrap_or(1);
        let genre = meta.genre.as_deref().unwrap_or("Unknown");
        let format = extension.to_lowercase();
        let format_upper = extension.to_uppercase();
        
        // Sanitize path components
        let artist = sanitize_path_component(artist);
        let album = sanitize_path_component(album);
        let title = sanitize_path_component(title);
        let genre = sanitize_path_component(genre);
        
        // Replace $var patterns (architecture 4.3.1 grammar)
        let path = self.template.pattern
            .replace("$artist", &artist)
            .replace("$album", &album)
            .replace("$title", &title)
            .replace("$track", &format!("{:02}", track))
            .replace("$disc", &disc.to_string())
            .replace("$year", &year)
            .replace("$genre", &genre)
            .replace("$format_upper", &format_upper)
            .replace("$format", &format);
        
        VirtualPath::new(path)
    }
    
    /// Parse template pattern to extract directory structure
    pub fn get_hierarchy(&self) -> Vec<&str> {
        // Returns ["artist", "album"] for default template
        self.template.pattern
            .split('/')
            .filter(|s| s.starts_with('{') && s.ends_with('}'))
            .map(|s| s.trim_matches(|c| c == '{' || c == '}'))
            .filter(|s| *s == "artist" || *s == "album")
            .collect()
    }
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new(PathTemplate::default())
    }
}

/// Sanitize string for use in file path
fn sanitize_path_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            '\0' => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioFormat;
    
    #[test]
    fn test_resolve_complete_metadata() {
        let resolver = PathResolver::default();
        let meta = AudioMeta {
            artist: Some("Metallica".to_string()),
            album: Some("Master of Puppets".to_string()),
            title: Some("Battery".to_string()),
            track: Some(1),
            year: Some(1986),
            format: AudioFormat::Flac,
            ..Default::default()
        };
        
        let path = resolver.resolve(&meta, "flac");
        // Default template: $artist/$album ($year) [$format_upper]/$track - $title.$format
        assert_eq!(path.as_str(), "Metallica/Master of Puppets (1986) [FLAC]/01 - Battery.flac");
    }
    
    #[test]
    fn test_resolve_missing_album() {
        let resolver = PathResolver::default();
        let meta = AudioMeta {
            artist: Some("Artist".to_string()),
            title: Some("Track".to_string()),
            track: Some(5),
            ..Default::default()
        };
        
        let path = resolver.resolve(&meta, "mp3");
        // Missing year uses fallback
        assert_eq!(path.as_str(), "Artist/Unknown Album (Unknown) [MP3]/05 - Track.mp3");
    }
    
    #[test]
    fn test_sanitize_special_chars() {
        let resolver = PathResolver::default();
        let meta = AudioMeta {
            artist: Some("AC/DC".to_string()),
            album: Some("Who Made Who?".to_string()),
            title: Some("Test:Track".to_string()),
            track: Some(1),
            year: Some(1986),
            ..Default::default()
        };
        
        let path = resolver.resolve(&meta, "flac");
        // Should not contain problematic characters
        assert!(!path.as_str().contains(':'));
        assert!(!path.as_str().contains('?'));
        // AC/DC becomes AC_DC
        assert!(path.as_str().contains("AC_DC"));
    }
    
    #[test]
    fn test_custom_template() {
        let template = PathTemplate {
            pattern: "$genre/$artist - $album/$track $title.$format".to_string(),
            ..Default::default()
        };
        let resolver = PathResolver::new(template);
        let meta = AudioMeta {
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            title: Some("Song".to_string()),
            genre: Some("Rock".to_string()),
            track: Some(3),
            ..Default::default()
        };
        
        let path = resolver.resolve(&meta, "flac");
        assert_eq!(path.as_str(), "Rock/Artist - Album/03 Song.flac");
    }
}
```

---

## Task 2: Tree Cache

### 2.1 Add to `musicfs-cache/src/lib.rs`

```rust
mod tree;
pub use tree::{VirtualTree, VirtualNode, TreeBuilder};
```

### 2.2 Create `musicfs-cache/src/tree.rs`

```rust
use musicfs_core::{FileId, FileMeta, VirtualPath};
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

/// Inode number type
pub type Inode = u64;

/// Root inode (FUSE convention)
pub const ROOT_INODE: Inode = 1;

/// Node in the virtual tree
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

/// Directory node
#[derive(Debug)]
pub struct DirNode {
    pub inode: Inode,
    pub name: OsString,
    pub children: BTreeMap<OsString, Inode>,
    pub mtime: SystemTime,
}

/// File node
#[derive(Debug)]
pub struct FileNode {
    pub inode: Inode,
    pub name: OsString,
    pub file_id: FileId,
    pub size: u64,
    pub mtime: SystemTime,
}

/// Refresh policy configuration (FR-9.3)
#[derive(Debug, Clone)]
pub struct RefreshPolicy {
    /// Time-to-live for cached entries
    pub ttl: Duration,
    /// Whether to refresh on access
    pub refresh_on_access: bool,
    /// Background refresh interval (None = disabled)
    pub background_interval: Option<Duration>,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(300),      // 5 minutes
            refresh_on_access: false,
            background_interval: None,
        }
    }
}

/// Virtual filesystem tree (FR-9.1-9.4)
pub struct VirtualTree {
    nodes: HashMap<Inode, VirtualNode>,
    path_to_inode: HashMap<VirtualPath, Inode>,
    next_inode: AtomicU64,
    /// Last refresh timestamp
    last_refresh: RwLock<SystemTime>,
    /// Refresh policy (FR-9.3)
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
        
        // Create root directory
        tree.nodes.insert(ROOT_INODE, VirtualNode::Directory(DirNode {
            inode: ROOT_INODE,
            name: OsString::from(""),
            children: BTreeMap::new(),
            mtime: SystemTime::now(),
        }));
        tree.path_to_inode.insert(VirtualPath::new("/"), ROOT_INODE);
        
        tree
    }
    
    /// Allocate a new inode number
    fn alloc_inode(&self) -> Inode {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }
    
    /// Get node by inode
    pub fn get(&self, inode: Inode) -> Option<&VirtualNode> {
        self.nodes.get(&inode)
    }
    
    /// Get node by path
    pub fn get_by_path(&self, path: &VirtualPath) -> Option<&VirtualNode> {
        self.path_to_inode.get(path).and_then(|ino| self.nodes.get(ino))
    }
    
    /// Lookup child in directory
    pub fn lookup(&self, parent_inode: Inode, name: &OsStr) -> Option<Inode> {
        if let Some(VirtualNode::Directory(dir)) = self.nodes.get(&parent_inode) {
            dir.children.get(name).copied()
        } else {
            None
        }
    }
    
    /// List directory children
    pub fn readdir(&self, inode: Inode) -> Option<Vec<(OsString, Inode, bool)>> {
        if let Some(VirtualNode::Directory(dir)) = self.nodes.get(&inode) {
            Some(dir.children.iter().map(|(name, &ino)| {
                let is_dir = self.nodes.get(&ino).map(|n| n.is_dir()).unwrap_or(false);
                (name.clone(), ino, is_dir)
            }).collect())
        } else {
            None
        }
    }
    
    /// Insert a file into the tree
    pub fn insert_file(&mut self, meta: &FileMeta) -> Inode {
        let path = &meta.virtual_path;
        
        // Ensure parent directories exist
        let parent_inode = self.ensure_parents(path);
        
        // Create file node
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
        
        // Add to parent
        if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&parent_inode) {
            dir.children.insert(name, inode);
        }
        
        inode
    }
    
    /// Ensure all parent directories exist for a path
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
        
        // Process all but the last component (which is the file)
        for component in &components[..components.len() - 1] {
            current_path.push_str(component);
            
            let vpath = VirtualPath::new(&current_path);
            
            if let Some(&existing) = self.path_to_inode.get(&vpath) {
                current_inode = existing;
            } else {
                // Create directory
                let new_inode = self.alloc_inode();
                let name = OsString::from(*component);
                
                let dir_node = DirNode {
                    inode: new_inode,
                    name: name.clone(),
                    children: BTreeMap::new(),
                    mtime: SystemTime::now(),
                };
                
                self.nodes.insert(new_inode, VirtualNode::Directory(dir_node));
                self.path_to_inode.insert(vpath, new_inode);
                
                // Add to parent
                if let Some(VirtualNode::Directory(parent)) = self.nodes.get_mut(&current_inode) {
                    parent.children.insert(name, new_inode);
                }
                
                current_inode = new_inode;
            }
            
            current_path.push('/');
        }
        
        current_inode
    }
    
    /// Remove a file from the tree
    pub fn remove_file(&mut self, path: &VirtualPath) -> Option<FileId> {
        let inode = self.path_to_inode.remove(path)?;
        
        if let Some(VirtualNode::File(file)) = self.nodes.remove(&inode) {
            // Remove from parent
            let parent_path = std::path::Path::new(path.as_str())
                .parent()
                .map(|p| VirtualPath::new(p.to_string_lossy()))
                .unwrap_or_else(|| VirtualPath::new("/"));
            
            if let Some(&parent_inode) = self.path_to_inode.get(&parent_path) {
                if let Some(VirtualNode::Directory(dir)) = self.nodes.get_mut(&parent_inode) {
                    dir.children.remove(&file.name);
                }
            }
            
            Some(file.file_id)
        } else {
            None
        }
    }
    
    /// Get total file count
    pub fn file_count(&self) -> usize {
        self.nodes.values().filter(|n| matches!(n, VirtualNode::File(_))).count()
    }
    
    /// Get total directory count
    pub fn dir_count(&self) -> usize {
        self.nodes.values().filter(|n| matches!(n, VirtualNode::Directory(_))).count()
    }
    
    // =========================================================================
    // Refresh Policy (FR-9.3, FR-9.4)
    // =========================================================================
    
    /// Check if tree needs refresh based on policy (FR-9.3)
    pub fn needs_refresh(&self) -> bool {
        let last = *self.last_refresh.read().unwrap();
        last.elapsed().unwrap_or(Duration::MAX) > self.refresh_policy.ttl
    }
    
    /// Force refresh - clears tree for rebuild (FR-9.4)
    /// Call this from signal handler or API endpoint
    pub fn force_refresh(&mut self) {
        // Keep root, clear everything else
        self.nodes.retain(|&ino, _| ino == ROOT_INODE);
        self.path_to_inode.retain(|p, _| p.as_str() == "/");
        
        // Reset root children
        if let Some(VirtualNode::Directory(root)) = self.nodes.get_mut(&ROOT_INODE) {
            root.children.clear();
        }
        
        *self.last_refresh.write().unwrap() = SystemTime::now();
    }
    
    /// Mark tree as refreshed
    pub fn mark_refreshed(&self) {
        *self.last_refresh.write().unwrap() = SystemTime::now();
    }
    
    /// Get current refresh policy
    pub fn refresh_policy(&self) -> &RefreshPolicy {
        &self.refresh_policy
    }
}

impl Default for VirtualTree {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for constructing tree from database
pub struct TreeBuilder {
    tree: VirtualTree,
}

impl TreeBuilder {
    pub fn new() -> Self {
        Self {
            tree: VirtualTree::new(),
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
    use musicfs_core::{AudioMeta, OriginId, RealPath};
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
        assert!(tree.get_by_path(&VirtualPath::new("/Artist/Album")).is_some());
        assert!(tree.get_by_path(&VirtualPath::new("/Artist/Album/Track.flac")).is_some());
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
}
```

---

## Task 3: FUSE Operations

### 3.1 Refactor `musicfs-fuse/src/lib.rs`

```rust
mod filesystem;
mod ops;

pub use filesystem::MusicFs;
```

### 3.2 Create `musicfs-fuse/src/ops/mod.rs`

```rust
pub mod readdir;
pub mod stat;
pub mod read;
```

### 3.3 Update `musicfs-fuse/src/filesystem.rs`

```rust
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyEntry, ReplyOpen, Request, FUSE_ROOT_ID,
};
use musicfs_cache::{Database, MetadataCache, VirtualTree, VirtualNode, ROOT_INODE};
use musicfs_core::{Error, Result, VirtualPath};
use musicfs_origins::Origin;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 512;

/// Main FUSE filesystem implementation
pub struct MusicFs {
    tree: Arc<RwLock<VirtualTree>>,
    cache: Arc<MetadataCache>,
    origins: Arc<HashMap<String, Box<dyn Origin>>>,
    uid: u32,
    gid: u32,
}

impl MusicFs {
    pub fn new(
        tree: Arc<RwLock<VirtualTree>>,
        cache: Arc<MetadataCache>,
        origins: Arc<HashMap<String, Box<dyn Origin>>>,
    ) -> Self {
        Self {
            tree,
            cache,
            origins,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
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
            let mut entries = vec![
                (ino, FileType::Directory, "."),
                (if ino == ROOT_INODE { ROOT_INODE } else { ROOT_INODE }, FileType::Directory, ".."),
            ];
            
            for (name, child_ino, is_dir) in children {
                let kind = if is_dir { FileType::Directory } else { FileType::RegularFile };
                entries.push((child_ino, kind, name.to_str().unwrap_or("?")));
            }
            
            for (i, (inode, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                if reply.add(*inode, (i + 1) as i64, *kind, name) {
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
        
        // Check for write flags (FR-4.1)
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
        
        // Get file info from tree
        let file_id = {
            let tree = self.tree.read().unwrap();
            if let Some(VirtualNode::File(file)) = tree.get(ino) {
                file.file_id
            } else {
                reply.error(libc::ENOENT);
                return;
            }
        };
        
        // TODO: Read from cache/origin
        // For now, return empty data
        reply.data(&[]);
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
    
    // Write operations - always EROFS (FR-4.1-4.4)
    fn write(&mut self, _req: &Request, _ino: u64, _fh: u64, _offset: i64, _data: &[u8], _write_flags: u32, _flags: i32, _lock_owner: Option<u64>, reply: fuser::ReplyWrite) {
        reply.error(libc::EROFS);
    }
    
    fn mkdir(&mut self, _req: &Request, _parent: u64, _name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
        reply.error(libc::EROFS);
    }
    
    fn unlink(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }
    
    fn rmdir(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }
    
    fn rename(&mut self, _req: &Request, _parent: u64, _name: &OsStr, _newparent: u64, _newname: &OsStr, _flags: u32, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }
    
    fn create(&mut self, _req: &Request, _parent: u64, _name: &OsStr, _mode: u32, _umask: u32, _flags: i32, reply: fuser::ReplyCreate) {
        reply.error(libc::EROFS);
    }
}
```

---

## Task 4: Benchmarks

### 4.1 Create `benches/tree_ops.rs`

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use musicfs_cache::{VirtualTree, TreeBuilder};
use musicfs_core::{FileId, FileMeta, OriginId, RealPath, VirtualPath};
use std::path::PathBuf;
use std::time::SystemTime;

fn make_file_meta(id: i64, artist: &str, album: &str, track: u32) -> FileMeta {
    let vpath = format!("/{}/{}/{:02} - Track.flac", artist, album, track);
    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(&vpath),
        real_path: RealPath {
            origin_id: OriginId::from("test"),
            path: PathBuf::from("/test"),
        },
        size: 30_000_000,
        mtime: SystemTime::now(),
        content_hash: None,
        audio: None,
    }
}

fn build_tree(n_artists: usize, albums_per_artist: usize, tracks_per_album: usize) -> VirtualTree {
    let mut builder = TreeBuilder::new();
    let mut id = 1i64;
    
    for a in 0..n_artists {
        for b in 0..albums_per_artist {
            for t in 0..tracks_per_album {
                let meta = make_file_meta(
                    id,
                    &format!("Artist {}", a),
                    &format!("Album {}", b),
                    t as u32 + 1,
                );
                builder.add_file(&meta);
                id += 1;
            }
        }
    }
    
    builder.build()
}

fn bench_stat_cached(c: &mut Criterion) {
    // 100 artists * 10 albums * 12 tracks = 12,000 files
    let tree = build_tree(100, 10, 12);
    let path = VirtualPath::new("/Artist 50/Album 5/06 - Track.flac");
    
    c.bench_function("stat_cached", |b| {
        b.iter(|| {
            black_box(tree.get_by_path(&path))
        })
    });
    
    // Target: <1ms p99 (NFR-1.1)
}

fn bench_readdir_1000_entries(c: &mut Criterion) {
    // Create tree with 1000 artists (1000 entries in root)
    let tree = build_tree(1000, 1, 1);
    
    c.bench_function("readdir_1000", |b| {
        b.iter(|| {
            black_box(tree.readdir(1))  // ROOT_INODE
        })
    });
    
    // Target: <10ms p99 (NFR-1.2)
}

fn bench_lookup(c: &mut Criterion) {
    let tree = build_tree(100, 10, 12);
    
    c.bench_function("lookup", |b| {
        b.iter(|| {
            let artist = tree.lookup(1, std::ffi::OsStr::new("Artist 50"));
            if let Some(a) = artist {
                black_box(tree.lookup(a, std::ffi::OsStr::new("Album 5")));
            }
        })
    });
}

fn bench_mount_time(c: &mut Criterion) {
    c.bench_function("tree_build_12k", |b| {
        b.iter(|| {
            black_box(build_tree(100, 10, 12))
        })
    });
    
    // Target: <100ms, Max: <500ms (NFR-1.7)
}

criterion_group!(benches, bench_stat_cached, bench_readdir_1000_entries, bench_lookup, bench_mount_time);
criterion_main!(benches);
```

---

## Tests

### Integration Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_fuse_readdir() {
        // Build tree and verify readdir returns correct entries
    }
    
    #[test]
    fn test_fuse_stat() {
        // Verify stat returns correct size/mtime
    }
    
    #[test]
    fn test_read_only_enforcement() {
        // Verify write ops return EROFS
    }
}
```

---

## Exit Criteria

- [ ] Virtual tree built from metadata
- [ ] `ls /mnt/music` shows Artist directories
- [ ] `ls /mnt/music/Artist/Album` shows tracks
- [ ] `stat` returns correct size, mtime
- [ ] Write operations return EROFS (FR-4.1)
- [ ] stat benchmark <1ms p99 (NFR-1.1)
- [ ] readdir benchmark <10ms p99 (NFR-1.2)
- [ ] Mount completes in <500ms (NFR-1.7)

---

## Next Week

Week 4 will implement CAS storage and chunk caching, enabling actual file reads.
