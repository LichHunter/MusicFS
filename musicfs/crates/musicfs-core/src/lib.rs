pub mod config;
pub mod credentials;
pub mod error;
pub mod events;
pub mod metrics;
pub mod resolver;
pub mod types;

pub use config::{
    CacheConfig, Config, ConfigError, HealthConfig, LoggingConfig, OriginConfig, OriginType,
};

use std::path::Path;

pub fn sanitize_path(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        path.to_string_lossy().replace(&home, "~")
    } else {
        path.to_string_lossy().to_string()
    }
}
pub use credentials::{Credential, CredentialConfig, CredentialError, CredentialStore};
pub use error::{Error, Result};
pub use events::{Event, EventBus};
pub use metrics::{CacheMetrics, FuseOpsMetrics, Metrics, OriginsMetrics};
pub use resolver::{PathResolver, PathTemplate};
pub use types::*;
