mod artwork;
mod db;
mod eviction;
mod format_handler;
mod format_layout;
pub mod handlers;
mod metadata;
mod overlay;
mod patterns;
mod prefetch;
mod tree;

pub use artwork::{ArtworkCache, ArtworkError, CachedArtwork};
pub use db::{Database, TrashedFile, TrashedFilter};
pub use eviction::{EvictionError, EvictionPolicy, LruEviction};
pub use format_handler::{FormatError, FormatHandler, FormatHandlerRegistry};
pub use format_layout::FormatLayout;
pub use handlers::{FlacHandler, Id3v2Handler};
pub use metadata::MetadataCache;
pub use overlay::{OverlayError, OverlayReader};
pub use patterns::{AccessContext, AccessPattern, PatternError, PatternStore};
pub use prefetch::{PrefetchConfig, PrefetchEngine, PrefetchHandle};
pub use tree::{
    DirNode, FileNode, Inode, RefreshPolicy, RemoveError, RenameError, TreeBuilder, VirtualNode,
    VirtualTree, ROOT_INODE,
};
