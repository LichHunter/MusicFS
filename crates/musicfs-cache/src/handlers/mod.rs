//! Format-specific metadata handlers for audio file synthesis.
//!
//! Each handler implements the `FormatHandler` trait to support:
//! - Analyzing original files to find audio boundaries
//! - Synthesizing new headers from database metadata
//! - Extracting metadata from existing files

mod id3v2;

pub use id3v2::Id3v2Handler;
