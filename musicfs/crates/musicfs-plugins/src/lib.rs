pub mod error;
pub mod manager;
pub mod native;
pub mod traits;
pub mod wasm;

pub use error::{PluginError, Result};
pub use manager::{PluginConfig, PluginEntry, PluginManager, WasmConfig};
pub use native::NativePluginHost;
pub use traits::{
    ExternalMetadata, FormatPlugin, MetadataPlugin, MetadataQuery, MetadataQueryType,
    OriginDirEntry, OriginHealth, OriginInstance, OriginPlugin, OriginStat, Plugin, PluginId,
    PluginInfo, PluginType, WatchEvent, WatchHandle, PLUGIN_API_VERSION,
};
pub use wasm::{ResourceLimits, WasmPluginHost};
