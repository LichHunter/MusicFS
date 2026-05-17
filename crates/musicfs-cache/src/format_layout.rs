use musicfs_core::AudioFormat;
use serde::{Deserialize, Serialize};

/// Describes the byte layout of an audio file for overlay splicing.
///
/// This struct tracks where the audio data begins and ends in the origin file,
/// allowing the OverlayReader to splice synthetic headers with original audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatLayout {
    /// Byte offset where audio data begins in the origin file
    pub audio_start: u64,

    /// Byte offset where audio data ends in the origin file
    pub audio_end: u64,

    /// Audio format (from musicfs-core)
    pub format: AudioFormat,

    /// Format-specific data (e.g., FLAC STREAMINFO block, MP4 stco offsets)
    /// Stored as raw bytes, interpreted by format handlers
    pub format_data: Option<Vec<u8>>,
}
