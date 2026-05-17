mod artwork;
mod db;
mod eviction;
mod metadata;
mod patterns;
mod prefetch;
mod tree;

pub use artwork::{ArtworkCache, ArtworkError, CachedArtwork};
pub use db::Database;
pub use eviction::{EvictionError, EvictionPolicy, LruEviction};
pub use metadata::MetadataCache;
pub use patterns::{AccessContext, AccessPattern, PatternError, PatternStore};
pub use prefetch::{PrefetchConfig, PrefetchEngine, PrefetchHandle};
pub use tree::{
    DirNode, FileNode, Inode, RefreshPolicy, RenameError, TreeBuilder, VirtualNode, VirtualTree,
    ROOT_INODE,
};
