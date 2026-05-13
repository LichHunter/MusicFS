use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Plugin not found: {0}")]
    NotFound(String),

    #[error("Plugin load failed: {0}")]
    LoadFailed(String),

    #[error("Plugin initialization failed: {0}")]
    InitFailed(String),

    #[error("Plugin API version mismatch: expected {expected}, got {actual}")]
    VersionMismatch { expected: String, actual: String },

    #[error("Plugin already loaded: {0}")]
    AlreadyLoaded(String),

    #[error("Plugin symbol not found: {0}")]
    SymbolNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Plugin execution error: {0}")]
    Execution(String),

    #[error("Plugin shutdown error: {0}")]
    Shutdown(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("WASM error: {0}")]
    Wasm(String),

    #[error("Resource limit exceeded: {0}")]
    ResourceLimit(String),
}

pub type Result<T> = std::result::Result<T, PluginError>;
