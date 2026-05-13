//! Plugin trait definitions (FR-23.1-23.4)
//!
//! Per architecture.md section 4.3.4:
//! - Plugin trait: Base interface for all plugins
//! - OriginPlugin: Creates Origin instances for storage backends
//! - MetadataPlugin: Provides external metadata lookup
//! - FormatPlugin: Handles custom audio format parsing

use crate::error::Result;
use async_trait::async_trait;
use musicfs_core::AudioMeta;
use semver::Version;
use serde_json::Value;
use std::io::Read;

/// Current plugin API version
pub const PLUGIN_API_VERSION: &str = "0.1.0";

/// Unique identifier for a loaded plugin
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginId(pub u64);

impl PluginId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

/// Plugin metadata returned by plugins
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub id: PluginId,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub plugin_type: PluginType,
}

/// Type of plugin
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    Origin,
    Metadata,
    Format,
}

/// Base plugin interface (FR-23.1)
///
/// All plugins must implement this trait. It provides:
/// - Plugin identification (name, version)
/// - Lifecycle management (init, shutdown)
pub trait Plugin: Send + Sync {
    /// Unique plugin name (e.g., "s3-origin", "musicbrainz-metadata")
    fn name(&self) -> &str;

    /// Plugin version following semver
    fn version(&self) -> Version;

    /// Human-readable description
    fn description(&self) -> &str {
        ""
    }

    /// Plugin type for registry categorization
    fn plugin_type(&self) -> PluginType;

    /// Initialize plugin with configuration
    ///
    /// Called once after loading. The config value contains
    /// plugin-specific configuration from the main config file.
    fn init(&mut self, config: Value) -> Result<()>;

    /// Shutdown plugin and release resources
    ///
    /// Called before unloading. Plugins should clean up any
    /// resources (connections, file handles, etc).
    fn shutdown(&mut self) -> Result<()>;
}

/// Origin plugin interface (FR-23.3)
///
/// Per architecture.md section 4.3.4:
/// Origin plugins create `Box<dyn Origin>` instances for custom storage backends.
///
/// Example use cases:
/// - Google Drive origin
/// - Dropbox origin
/// - Custom NAS protocol
#[async_trait]
pub trait OriginPlugin: Plugin {
    /// Origin type identifier (e.g., "gdrive", "dropbox")
    fn origin_type(&self) -> &str;

    /// Create a new Origin instance with the given configuration
    ///
    /// The config contains origin-specific settings (credentials, paths, etc).
    /// Returns a boxed Origin that can be used by the OriginRouter.
    async fn create_origin(
        &self,
        id: &str,
        config: Value,
    ) -> Result<Box<dyn OriginInstance>>;
}

/// Instance created by OriginPlugin
///
/// This is a simplified async interface that maps to the full Origin trait.
/// The plugin host wraps this to provide the full Origin implementation.
#[async_trait]
pub trait OriginInstance: Send + Sync {
    /// List directory contents
    async fn readdir(&self, path: &str) -> Result<Vec<OriginDirEntry>>;

    /// Get file/directory stats
    async fn stat(&self, path: &str) -> Result<OriginStat>;

    /// Read file data
    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>>;

    /// Check if path exists
    async fn exists(&self, path: &str) -> Result<bool>;

    /// Health check
    async fn health(&self) -> OriginHealth;

    /// Watch path for changes (FR-10.2)
    async fn watch(
        &self,
        path: &str,
        callback: Box<dyn Fn(WatchEvent) + Send + Sync>,
    ) -> Result<WatchHandle>;
}

pub struct WatchHandle {
    _cancel: tokio::sync::oneshot::Sender<()>,
}

impl WatchHandle {
    pub fn new(cancel: tokio::sync::oneshot::Sender<()>) -> Self {
        Self { _cancel: cancel }
    }
}

#[derive(Debug, Clone)]
pub enum WatchEvent {
    Created(String),
    Modified(String),
    Deleted(String),
}

/// Directory entry from plugin origin
#[derive(Debug, Clone)]
pub struct OriginDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_secs: u64,
}

/// File stats from plugin origin
#[derive(Debug, Clone)]
pub struct OriginStat {
    pub size: u64,
    pub mtime_secs: u64,
    pub is_dir: bool,
}

/// Origin health status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginHealth {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Metadata plugin interface (FR-23.3)
///
/// Metadata plugins provide external metadata lookup from services like
/// MusicBrainz, Discogs, Last.fm, etc.
#[async_trait]
pub trait MetadataPlugin: Plugin {
    /// Lookup metadata for a query
    ///
    /// Returns enriched metadata if found, None otherwise.
    async fn lookup(&self, query: &MetadataQuery) -> Result<Option<ExternalMetadata>>;

    /// Supported query types
    fn supported_queries(&self) -> &[MetadataQueryType] {
        &[MetadataQueryType::ByTitleArtist]
    }
}

/// Query for metadata lookup
#[derive(Debug, Clone)]
pub struct MetadataQuery {
    pub query_type: MetadataQueryType,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub fingerprint: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Type of metadata query
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataQueryType {
    ByTitleArtist,
    ByFingerprint,
    ByAlbum,
}

/// External metadata returned by plugins
#[derive(Debug, Clone, Default)]
pub struct ExternalMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub musicbrainz_id: Option<String>,
    pub artwork_url: Option<String>,
}

/// Format plugin interface (FR-24.1)
///
/// Format plugins handle custom audio formats not supported by symphonia.
///
/// Example use cases:
/// - Custom lossless codecs
/// - Proprietary formats
/// - Game audio formats
pub trait FormatPlugin: Plugin {
    /// File extensions this plugin handles
    fn extensions(&self) -> &[&str];

    /// Check if plugin can handle a specific extension
    fn can_handle(&self, extension: &str) -> bool {
        self.extensions()
            .iter()
            .any(|ext| ext.eq_ignore_ascii_case(extension))
    }

    /// Parse audio metadata from reader
    ///
    /// The reader provides the raw file bytes. Plugin should parse
    /// and return AudioMeta with whatever metadata it can extract.
    fn parse(&self, reader: &mut dyn Read) -> Result<AudioMeta>;

    /// Synthesize file header with updated metadata (FR-5.3)
    ///
    /// Creates a new file header containing the provided metadata.
    /// Used for metadata overlay - serving cached metadata without
    /// modifying the original file.
    fn synthesize_header(&self, metadata: &AudioMeta) -> Result<Vec<u8>>;
}

/// Declaration macro for native plugins
///
/// Native plugins must export a function with this signature:
/// ```ignore
/// #[no_mangle]
/// pub extern "C" fn musicfs_plugin_create() -> *mut dyn Plugin
/// ```
#[macro_export]
macro_rules! declare_plugin {
    ($plugin_type:ty, $constructor:expr) => {
        #[no_mangle]
        pub extern "C" fn musicfs_plugin_create() -> *mut dyn $crate::Plugin {
            let plugin = $constructor;
            let boxed: Box<dyn $crate::Plugin> = Box::new(plugin);
            Box::into_raw(boxed)
        }

        #[no_mangle]
        pub extern "C" fn musicfs_plugin_api_version() -> *const std::ffi::c_char {
            concat!($crate::PLUGIN_API_VERSION, "\0").as_ptr() as *const std::ffi::c_char
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestPlugin {
        name: String,
        initialized: bool,
    }

    impl Plugin for TestPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn version(&self) -> Version {
            Version::new(1, 0, 0)
        }

        fn plugin_type(&self) -> PluginType {
            PluginType::Origin
        }

        fn init(&mut self, _config: Value) -> Result<()> {
            self.initialized = true;
            Ok(())
        }

        fn shutdown(&mut self) -> Result<()> {
            self.initialized = false;
            Ok(())
        }
    }

    #[test]
    fn test_plugin_lifecycle() {
        let mut plugin = TestPlugin {
            name: "test".to_string(),
            initialized: false,
        };

        assert_eq!(plugin.name(), "test");
        assert!(!plugin.initialized);

        plugin.init(Value::Null).unwrap();
        assert!(plugin.initialized);

        plugin.shutdown().unwrap();
        assert!(!plugin.initialized);
    }

    #[test]
    fn test_plugin_id() {
        let id1 = PluginId::new(1);
        let id2 = PluginId::new(1);
        let id3 = PluginId::new(2);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }
}
