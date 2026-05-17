use crate::FormatLayout;
use musicfs_core::AudioMeta;
use std::collections::HashMap;
use std::sync::Arc;

/// Error types for format handling operations
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("Unsupported format")]
    UnsupportedFormat,

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Synthesis failed: {0}")]
    SynthesisFailed(String),
}

/// Trait for format-specific metadata handling.
///
/// Implementations handle:
/// 1. Analyzing original files to find audio boundaries
/// 2. Synthesizing new headers from database metadata
pub trait FormatHandler: Send + Sync + 'static {
    /// Unique identifier for this handler
    fn id(&self) -> &'static str;

    /// Human-readable name
    fn name(&self) -> &'static str;

    /// File extensions this handler supports
    fn extensions(&self) -> &[&'static str];

    /// MIME types this handler supports
    fn mime_types(&self) -> &[&'static str];

    /// Analyze file bytes to determine audio layout
    fn analyze(
        &self,
        data: &[u8],
        file_size: u64,
    ) -> std::result::Result<FormatLayout, FormatError>;

    /// Synthesize header bytes from metadata. Called on every read().
    fn synthesize(
        &self,
        metadata: &AudioMeta,
        layout: &FormatLayout,
    ) -> std::result::Result<Vec<u8>, FormatError>;

    /// Extract metadata from header bytes (for initial ingest)
    fn extract(&self, data: &[u8]) -> std::result::Result<AudioMeta, FormatError>;

    /// Estimate header size without full synthesis (for getattr)
    fn estimate_header_size(&self, _metadata: &AudioMeta) -> usize {
        10 * 1024 // 10KB default
    }
}

/// Registry for format handlers
pub struct FormatHandlerRegistry {
    handlers: HashMap<String, Arc<dyn FormatHandler>>,
    extension_map: HashMap<String, String>,
}

impl FormatHandlerRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            extension_map: HashMap::new(),
        }
    }

    /// Register a format handler
    pub fn register(&mut self, handler: Arc<dyn FormatHandler>) {
        let id = handler.id().to_string();

        // Map extensions to handler ID
        for ext in handler.extensions() {
            self.extension_map.insert(ext.to_string(), id.clone());
        }

        self.handlers.insert(id, handler);
    }

    /// Get handler by file extension
    pub fn get_by_extension(&self, ext: &str) -> Option<Arc<dyn FormatHandler>> {
        let id = self.extension_map.get(ext)?;
        self.handlers.get(id).cloned()
    }

    /// Get handler by format ID
    pub fn get_by_format(&self, format: &str) -> Option<Arc<dyn FormatHandler>> {
        self.handlers.get(format).cloned()
    }
}

impl Default for FormatHandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
