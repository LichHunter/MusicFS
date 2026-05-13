pub mod config;
pub mod credentials;
pub mod error;
pub mod events;
pub mod metrics;
pub mod resolver;
pub mod supervisor;
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

/// Install a custom panic hook that logs panics via tracing before the default behavior.
/// This ensures panics are captured in log files and journald.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");

        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };

        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());

        tracing::error!(
            thread = thread_name,
            location = %location,
            "PANIC: {}",
            message
        );

        default_hook(info);
    }));
}
pub use credentials::{Credential, CredentialConfig, CredentialError, CredentialStore};
pub use error::{Error, Result};
pub use events::{Event, EventBus};
pub use metrics::{CacheMetrics, FuseOpsMetrics, Metrics, OriginsMetrics};
pub use resolver::{PathResolver, PathTemplate};
pub use types::*;
